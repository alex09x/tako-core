/*
 * tako-session — session runtime for Tako
 * Copyright (c) 2026 Alexander Panasenko
 *
 * Contact: alex@prod.codes
 * Author: https://prod.codes/about/
 * Project: https://github.com/alex09x/tako-core
 * SPDX-License-Identifier: MIT
 */

//! The commands that drive a session without attaching to it: run, send,
//! print, write, wait, tail, history, print-env, and completions.

use super::attach::{current_dir, session_options};
use super::error::{CliError, CliResult, name_too_long, resolve_session, session_name};
use super::{history, run, send, tail, wait};
use crate::cfg::{Cfg, Env};
use crate::ipc::Tag;
use crate::socket::{self, SessionMatch};
use crate::{cli, log};
use std::io::Write;

fn is_help(arg: &str) -> bool {
    matches!(arg, "--help" | "-h")
}

fn help(out: &mut dyn Write) -> CliResult {
    out.write_all(cli::HELP.as_bytes())
        .map_err(|_| CliError::Silent)
}

fn matchers(env: &dyn Env, args: &[String]) -> Result<Vec<SessionMatch>, CliError> {
    let m: Vec<SessionMatch> = args
        .iter()
        .map(|a| SessionMatch::parse(env, a).map_err(|_| CliError::Silent))
        .collect::<Result<_, _>>()?;
    if m.is_empty() {
        return Err(CliError::msg("session name required"));
    }
    Ok(m)
}

fn completion(shell: &str) -> Option<&'static str> {
    Some(match shell {
        "bash" => include_str!("completions/bash.txt"),
        "zsh" => include_str!("completions/zsh.txt"),
        "fish" => include_str!("completions/fish.txt"),
        "nu" => include_str!("completions/nu.txt"),
        _ => return None,
    })
}

/// Runs `cmd` if it is one of these commands.
pub fn dispatch(
    cmd: &str,
    rest: &[String],
    env: &dyn Env,
    cfg: &Cfg,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> Option<CliResult> {
    let dir = &cfg.socket_dir;
    let first = rest.first().map(String::as_str).unwrap_or("");
    Some(match cmd {
        "run" | "r" => {
            if is_help(first) {
                return Some(help(out));
            }
            let (flags, words): (Vec<&String>, Vec<&String>) =
                rest.iter().skip(1).partition(|a| a.starts_with("-d"));
            let words: Vec<String> = words.into_iter().cloned().collect();
            session_name(env, first).and_then(|name| {
                let mut opts = session_options(env, cfg, &name, current_dir())?;
                opts.task_mode = true;
                log::info(&format!("socket path={}", opts.socket_path.display()));
                run::run(opts, !flags.is_empty(), &words, out)
            })
        }
        "send" | "s" | "print" | "p" => {
            if is_help(first) {
                return Some(help(out));
            }
            if first.is_empty() {
                return Some(Err(CliError::msg("session name required")));
            }
            let (tag, verb) = if cmd.starts_with('s') {
                (Tag::SEND, "send")
            } else {
                (Tag::OUTPUT, "print")
            };
            session_name(env, first).and_then(|name| {
                let path =
                    socket::socket_path(dir, &name).ok_or_else(|| name_too_long(&name, dir))?;
                send::send(dir, &name, &path, &rest[1..], tag).map_err(|e| match e {
                    CliError::Silent => CliError::msg(format!("{verb} failed: Unexpected")),
                    other => other,
                })
            })
        }
        "write" | "wr" => {
            if is_help(first) {
                return Some(help(out));
            }
            if first.is_empty() {
                return Some(Err(CliError::msg("session name required")));
            }
            let file = rest.get(1).map(String::as_str).unwrap_or("");
            if is_help(file) {
                return Some(help(out));
            }
            if file.is_empty() {
                return Some(Err(CliError::msg("file path required")));
            }
            session_name(env, first).and_then(|name| {
                let mut opts = session_options(env, cfg, &name, current_dir())?;
                opts.task_mode = true;
                let path = opts.socket_path.clone();
                if crate::daemon::ensure_session(opts, out).map_err(|_| CliError::Silent)?
                    == crate::daemon::Started::Daemon
                {
                    return Ok(());
                }
                send::write(dir, &name, &path, file, out)
            })
        }
        "wait" | "w" | "tail" | "t" => {
            if rest.iter().any(|a| is_help(a)) {
                return Some(help(out));
            }
            matchers(env, rest).and_then(|m| {
                if cmd.starts_with('w') {
                    wait::run(dir, &m, out, err)
                } else {
                    tail::run(dir, &m, out)
                }
            })
        }
        "history" | "hi" => {
            let mut name: Option<&str> = None;
            let mut format = 0u8;
            for arg in rest {
                if is_help(arg) {
                    return Some(help(out));
                } else if let Some(f) = history::format_byte(arg) {
                    format = f;
                } else if name.is_none() {
                    name = Some(arg);
                }
            }
            let current = socket::session_from_env(env);
            let raw = match name {
                Some(n) => n.to_string(),
                None if !current.is_empty() => current,
                None => {
                    return Some(Err(CliError::msg(
                        "session name required (or run inside a zmx session)",
                    )));
                }
            };
            session_name(env, &raw).and_then(|s| history::history(dir, &s, format, out))
        }
        "print-env" => {
            let (mut shell, mut name, mut key) = (false, None, "");
            for arg in rest {
                if is_help(arg) {
                    return Some(help(out));
                }
                if arg == "-s" || arg == "--shell" {
                    shell = true;
                } else if name.is_none() {
                    name = Some(arg.as_str());
                } else if key.is_empty() {
                    key = arg;
                }
            }
            let Some(raw) = name else {
                return Some(Err(CliError::msg(
                    "session name required (or run inside a zmx session)",
                )));
            };
            resolve_session(env, Some(raw))
                .and_then(|s| history::print_env(dir, &s, key, shell, out))
        }
        "completions" | "c" => {
            let Some(shell) = rest.first() else {
                return Some(Err(CliError::msg(
                    "completions requires a shell argument (bash, zsh, fish, nu)",
                )));
            };
            if is_help(shell) {
                return Some(help(out));
            }
            match completion(shell) {
                Some(script) => out
                    .write_all(script.as_bytes())
                    .map_err(|_| CliError::Silent),
                None => Err(CliError::msg(format!(
                    "unknown shell \"{shell}\" (valid: bash, zsh, fish, nu)"
                ))),
            }
        }
        _ => return None,
    })
}
