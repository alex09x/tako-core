/*
 * tako-session — session runtime for Tako
 * Copyright (c) 2026 Alexander Panasenko
 *
 * Contact: alex@prod.codes
 * Author: https://prod.codes/about/
 * Project: https://github.com/alex09x/tako-core
 * SPDX-License-Identifier: MIT
 */

//! The command line: picks a command from the first argument, as the runtime
//! it replaces does, with the same aliases, messages and exit statuses.

use crate::cfg::{Cfg, Env};
use crate::cmd::error::{CliError, resolve_session};
use crate::cmd::kill::Io;
use crate::cmd::{attach, detach, kill, labels, list, tasks};
use crate::{log, socket};
use std::io::Write;

/// The protocol-compatible runtime version this build speaks.
pub const RUNTIME_VERSION: &str = "0.8.1";
pub const HELP: &str = include_str!("cmd/help.txt");

fn is_help(arg: &str) -> bool {
    matches!(arg, "--help" | "-h")
}

fn version_text(cfg: &Cfg) -> String {
    format!(
        "zmx\t\t{RUNTIME_VERSION}\ntako_core\t{}\nsocket_dir\t{}\nlog_dir\t\t{}\n",
        env!("CARGO_PKG_VERSION"),
        cfg.socket_dir.display(),
        cfg.log_dir.display()
    )
}

/// Runs one invocation; returns the exit status.
pub fn run(args: &[String], env: &dyn Env, out: &mut dyn Write, err: &mut dyn Write) -> i32 {
    let cfg = match Cfg::init(env) {
        Ok(cfg) => cfg,
        Err(_) => return 1,
    };
    let _ = log::init(&cfg.log_dir.join("zmx.log"), cfg.log_mode);
    let result = dispatch(args, env, &cfg, out, err);
    let _ = out.flush();
    match result {
        Ok(()) => 0,
        Err(CliError::Exit(code)) => code,
        Err(e) => {
            let _ = err.write_all(e.stderr_text().as_bytes());
            let _ = err.flush();
            1
        }
    }
}

fn help(out: &mut dyn Write) -> Result<(), CliError> {
    out.write_all(HELP.as_bytes()).map_err(|_| CliError::Silent)
}

fn dispatch(
    args: &[String],
    env: &dyn Env,
    cfg: &Cfg,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> Result<(), CliError> {
    let current = socket::session_from_env(env);
    let Some(cmd) = args.first() else {
        return list::run(&cfg.socket_dir, false, &current, out, err).map_err(|_| CliError::Silent);
    };
    let rest = &args[1..];
    match cmd.as_str() {
        "version" | "v" | "-v" | "--version" => out
            .write_all(version_text(cfg).as_bytes())
            .map_err(|_| CliError::Silent),
        "help" | "h" | "-h" => help(out),
        "list" | "l" | "ls" => {
            if rest.iter().any(|a| is_help(a)) {
                return help(out);
            }
            let short = rest.iter().any(|a| a == "--short");
            list::run(&cfg.socket_dir, short, &current, out, err).map_err(|_| CliError::Silent)
        }
        "get" | "g" => {
            let Some(name) = rest.first() else {
                return Err(CliError::msg(
                    "session name required (or run inside a zmx session)",
                ));
            };
            if is_help(name) {
                return help(out);
            }
            let session = resolve_session(env, Some(name))?;
            let key = rest.get(1).map(String::as_str).unwrap_or("");
            labels::get(&cfg.socket_dir, &session, key, out)
        }
        "set" => {
            let Some(name) = rest.first() else {
                return Err(CliError::msg(
                    "session name required (or run inside a zmx session)",
                ));
            };
            if is_help(name) {
                return help(out);
            }
            let session = resolve_session(env, Some(name))?;
            let kvs = rest[1..].join(" ");
            if kvs.is_empty() {
                return Err(CliError::msg("at least one key=value pair required"));
            }
            labels::set(&cfg.socket_dir, &session, &kvs)
        }
        "clear" | "cl" => {
            let Some(name) = rest.first() else {
                return Err(CliError::msg(
                    "session name required (or run inside a zmx session)",
                ));
            };
            if is_help(name) {
                return help(out);
            }
            let session = resolve_session(env, Some(name))?;
            labels::clear(&cfg.socket_dir, &session)
        }
        "attach" | "a" => attach::run(rest, env, cfg, out),
        "detach" | "d" => detach::run(&cfg.socket_dir, &current),
        "kill" | "k" => {
            if rest.iter().any(|a| is_help(a)) {
                return help(out);
            }
            kill::run(&cfg.socket_dir, env, rest, &mut Io { out, err })
        }
        other => tasks::dispatch(other, rest, env, cfg, out, err)
            .unwrap_or_else(|| Err(CliError::msg(format!("unknown command \"{other}\"")))),
    }
}
