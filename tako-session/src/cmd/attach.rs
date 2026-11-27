/*
 * tako-session — session runtime for Tako
 * Copyright (c) 2026 Alexander Panasenko
 *
 * Contact: alex@prod.codes
 * Author: https://prod.codes/about/
 * Project: https://github.com/alex09x/tako-core
 * SPDX-License-Identifier: MIT
 */

//! `attach`: start or join a session and hand it this terminal, in three
//! modes — plain (create when missing), `--create-only` and `--attach-only
//! --if-label` (guarded).

use super::attach_args::{self, AttachArgs};
use super::error::{CliError, CliResult, name_too_long, session_name};
use super::labels::{self, assert_guard_labels, assert_labels};
use crate::cfg::{Cfg, Env, MAX_SCROLLBACK_LINES};
use crate::client::{Conn, HandshakeError, Hooks, LoopEnd, guarded_attach, session_loop};
use crate::daemon::{Options, Started, create_only, ensure_session};
use crate::ipc::{Tag, encode_frame};
use crate::{keys, log, socket, term};
use std::io::{self, Write};
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};

/// The name a runtime error goes by in messages.
pub fn error_name(e: &io::Error) -> &'static str {
    match e.kind() {
        io::ErrorKind::ConnectionRefused => "ConnectionRefused",
        io::ErrorKind::NotFound => "FileNotFound",
        io::ErrorKind::PermissionDenied => "AccessDenied",
        io::ErrorKind::AddrInUse => "AddressInUse",
        _ => "Unexpected",
    }
}

fn env_str(env: &dyn Env, key: &str) -> Option<String> {
    env.get(key).map(|v| v.to_string_lossy().into_owned())
}

/// `KEY=value` lines for each tracked variable, `-KEY` for an unset one.
pub fn tracked_env(env: &dyn Env, cfg: &Cfg) -> Vec<u8> {
    let keys = env_str(env, "ZMX_TRACK_ENV").unwrap_or_else(|| cfg.tracked_envs.clone());
    let mut out = String::new();
    for key in keys
        .split(',')
        .map(|k| k.trim_matches([' ', '\t', '\r', '\n']))
        .filter(|k| !k.is_empty())
    {
        match env_str(env, key) {
            Some(v) => out.push_str(&format!("{key}={v}\n")),
            None => out.push_str(&format!("-{key}\n")),
        }
    }
    out.into_bytes()
}

/// A test hook path from `var`, honoured only inside the socket directory.
fn scoped_test_path(env: &dyn Env, dir: &Path, var: &str) -> Option<PathBuf> {
    let raw = env_str(env, var)?;
    let d = dir.to_string_lossy();
    let rest = raw.strip_prefix(d.as_ref())?;
    (!rest.is_empty() && (d.ends_with('/') || rest.starts_with('/'))).then(|| PathBuf::from(raw))
}

struct Ctx<'a> {
    env: &'a dyn Env,
    cfg: &'a Cfg,
    env_str: Vec<u8>,
}

/// How a session named `session` would be started from here.
pub fn session_options(
    env: &dyn Env,
    cfg: &Cfg,
    session: &str,
    cwd: Option<Vec<u8>>,
) -> Result<Options, CliError> {
    let socket_path = socket::socket_path(&cfg.socket_dir, session)
        .ok_or_else(|| name_too_long(session, &cfg.socket_dir))?;
    Ok(Options {
        session: session.to_string(),
        socket_path,
        log_dir: cfg.log_dir.clone(),
        log_mode: cfg.log_mode,
        shell: env_str(env, "SHELL").unwrap_or_else(|| "/bin/sh".into()),
        cwd,
        max_scrollback_lines: MAX_SCROLLBACK_LINES,
        ..Options::default()
    })
}

impl Ctx<'_> {
    fn options(&self, session: &str, cwd: Option<Vec<u8>>) -> Result<Options, CliError> {
        session_options(self.env, self.cfg, session, cwd)
    }

    /// Takes over the terminal and runs the attached loop.
    fn attached(&self, sock: i32, unread: &[u8]) -> Result<LoopEnd, CliError> {
        let _raw = term::RawMode::enter();
        term::write_stdout(term::CLEAR);
        session_loop::run(
            sock,
            &self.env_str,
            unread,
            keys::detach_key_disabled(self.env),
        )
        .map_err(|_| CliError::Silent)
    }
}

pub fn current_dir() -> Option<Vec<u8>> {
    use std::os::unix::ffi::OsStringExt;
    std::env::current_dir()
        .ok()
        .map(|p| p.into_os_string().into_vec())
}

pub fn run(argv: &[String], env: &dyn Env, cfg: &Cfg, out: &mut dyn Write) -> CliResult {
    let p: AttachArgs = attach_args::parse(argv);
    if p.want_help {
        return out
            .write_all(crate::cli::HELP.as_bytes())
            .map_err(|_| CliError::Silent);
    }
    if p.missing_labels_value {
        return Err(CliError::msg("--labels requires \"key=value ...\""));
    }
    if p.missing_if_label_value {
        return Err(CliError::msg("--if-label requires \"key=value ...\""));
    }
    if p.create_only && p.attach_only {
        return Err(CliError::msg(
            "--create-only cannot be combined with --attach-only or --if-label",
        ));
    }
    if p.attach_only && p.if_label.is_none() {
        return Err(CliError::msg(
            "--attach-only requires --if-label \"key=value ...\"",
        ));
    }
    // Before anything starts, so a refused label leaves no session behind.
    if let Some(required) = &p.if_label {
        assert_guard_labels(required, true)?;
    }
    if let Some(kvs) = &p.labels {
        if p.attach_only {
            assert_guard_labels(kvs, false)?;
        } else if p.create_only {
            assert_guard_labels(kvs, true)?;
        } else {
            assert_labels(kvs)?;
        }
    }
    let command = (p.command_start < argv.len()).then(|| argv[p.command_start..].to_vec());
    if p.attach_only && command.is_some() {
        return Err(CliError::msg("--attach-only does not accept a command"));
    }
    if (p.attach_only || p.create_only) && p.session_name.is_empty() {
        return Err(CliError::msg("session name required"));
    }
    let session = session_name(env, &p.session_name)?;
    let ctx = Ctx {
        env,
        cfg,
        env_str: tracked_env(env, cfg),
    };
    let mut opts = ctx.options(&session, current_dir())?;
    opts.command = command;
    log::info(&format!("socket path={}", opts.socket_path.display()));
    if p.create_only {
        return create_then_attach(&ctx, opts, p.labels.as_deref(), out);
    }
    if let Some(required) = &p.if_label {
        return attach_if_label(&ctx, &opts, required, p.labels.as_deref(), true);
    }
    attach(&ctx, opts, p.labels.as_deref(), out)
}

fn inside_session_error(env: &dyn Env, flag: &str) -> CliResult {
    if socket::session_from_env(env).is_empty() {
        return Ok(());
    }
    Err(CliError::msg(format!(
        "cannot use {flag} inside an active zmx session"
    )))
}

/// Guarded attach: only to a running session whose labels match.
fn attach_if_label(
    ctx: &Ctx,
    opts: &Options,
    required: &str,
    set: Option<&str>,
    allow_legacy: bool,
) -> CliResult {
    inside_session_error(ctx.env, "--attach-only")?;
    let name = &opts.session;
    if !socket::session_exists(&ctx.cfg.socket_dir, name).map_err(|_| CliError::Silent)? {
        return Err(CliError::msg(format!("session \"{name}\" not found")));
    }
    let mut conn = Conn::connect(&opts.socket_path).map_err(|e| {
        CliError::msg(format!(
            "session \"{name}\" not found or unresponsive ({})",
            e.name()
        ))
    })?;
    let dir = &ctx.cfg.socket_dir;
    let hooks = if allow_legacy {
        Hooks {
            after_probe_delay_ms: env_str(ctx.env, "ZMX_TEST_LEGACY_ATTACH_DELAY_MS")
                .and_then(|v| v.parse().ok())
                .unwrap_or(0),
            probe_ready_path: scoped_test_path(
                ctx.env,
                dir,
                "ZMX_TEST_LEGACY_ATTACH_PROBE_READY_FILE",
            ),
            probe_resume_path: scoped_test_path(ctx.env, dir, "ZMX_TEST_LEGACY_ATTACH_RESUME_FILE"),
            ..Hooks::default()
        }
    } else {
        Hooks::default()
    };
    let unread =
        guarded_attach(&mut conn, required, set, allow_legacy, 1000, &hooks).map_err(|e| {
            CliError::msg(match e {
                HandshakeError::LabelMismatch => format!("session \"{name}\" label mismatch"),
                HandshakeError::InvalidGuard => {
                    format!("session \"{name}\" rejected guarded attach")
                }
                HandshakeError::UnreadOutputBudgetExceeded => {
                    format!("session \"{name}\" exceeded handshake output budget")
                }
                HandshakeError::Timeout => {
                    format!("session \"{name}\" does not support guarded attach (daemon too old?)")
                }
                HandshakeError::Unexpected => {
                    format!("session \"{name}\" not found or unresponsive")
                }
            })
        })?;
    log::info(&format!("guarded attach verified session={name}"));
    ctx.attached(conn.stream.as_raw_fd(), &unread).map(|_| ())
}

fn create_then_attach(
    ctx: &Ctx,
    mut opts: Options,
    labels: Option<&str>,
    out: &mut dyn Write,
) -> CliResult {
    inside_session_error(ctx.env, "--create-only")?;
    let name = opts.session.clone();
    opts.initial_labels = labels.map(str::to_string);
    let attach_opts = opts.clone();
    match create_only(opts, out) {
        Ok(Started::Daemon) => return Ok(()),
        Ok(Started::Client) => {}
        Err(e) if e.kind() == io::ErrorKind::AddrInUse => {
            return Err(CliError::msg(format!("session \"{name}\" already exists")));
        }
        Err(e) => {
            return Err(CliError::msg(format!(
                "cannot create session \"{name}\": {}",
                error_name(&e)
            )));
        }
    }
    if let Some(kvs) = labels {
        return attach_if_label(ctx, &attach_opts, kvs, None, false);
    }
    let stream = socket::connect(&attach_opts.socket_path).map_err(|e| {
        CliError::msg(format!(
            "cannot connect to session \"{name}\": {}",
            error_name(&e)
        ))
    })?;
    ctx.attached(stream.as_raw_fd(), b"").map(|_| ())
}

fn attach(ctx: &Ctx, opts: Options, labels: Option<&str>, out: &mut dyn Write) -> CliResult {
    let current = socket::session_from_env(ctx.env);
    if !current.is_empty() {
        return switch_session(ctx.cfg, &current, &opts.session);
    }
    let name = opts.session.clone();
    let socket_path = opts.socket_path.clone();
    let cwd = opts.cwd.clone();
    if ensure_session(opts, out).map_err(|_| CliError::Silent)? == Started::Daemon {
        return Ok(());
    }
    // Labels land before the terminal is taken over, so a supervisor that
    // dies right after cannot leave an unlabelled session behind.
    if let Some(kvs) = labels {
        labels::set(&ctx.cfg.socket_dir, &name, kvs)?;
    }
    let stream = socket::connect(&socket_path).map_err(|e| {
        CliError::msg(format!(
            "cannot connect to session \"{name}\": {}",
            error_name(&e)
        ))
    })?;
    log::info(&format!("attached session={name}"));
    let _raw = term::RawMode::enter();
    term::write_stdout(term::CLEAR);
    let end = session_loop::run(
        stream.as_raw_fd(),
        &ctx.env_str,
        b"",
        keys::detach_key_disabled(ctx.env),
    )
    .map_err(|_| CliError::Silent)?;
    match end {
        LoopEnd::Detach => Ok(()),
        LoopEnd::Switch {
            session,
            cwd: next_cwd,
        } => {
            // Leave the old session before joining the next one.
            drop(stream);
            term::write_stdout(term::RESTORE);
            let target_cwd = next_cwd.map(String::into_bytes).or(cwd);
            log::info(&format!(
                "switching to new session cwd={}",
                String::from_utf8_lossy(target_cwd.as_deref().unwrap_or(b""))
            ));
            let target = ctx.options(&session, target_cwd)?;
            attach(ctx, target, None, out)
        }
    }
}

/// Inside a session: ask that session's client to switch to `next`.
fn switch_session(cfg: &Cfg, current: &str, next: &str) -> CliResult {
    log::info(&format!("switch session cur={current} next={next}"));
    let dir = &cfg.socket_dir;
    let path = socket::socket_path(dir, current).ok_or_else(|| name_too_long(current, dir))?;
    if !socket::session_exists(dir, current).map_err(|_| CliError::Silent)? {
        return Err(CliError::msg(format!(
            "session \"{current}\" does not exist"
        )));
    }
    let mut stream = match socket::connect(&path) {
        Ok(s) => s,
        Err(e) => {
            if e.kind() == io::ErrorKind::ConnectionRefused {
                socket::cleanup_stale_socket(dir, current);
            }
            let kind = if e.kind() == io::ErrorKind::ConnectionRefused {
                "ConnectionRefused"
            } else {
                "Unexpected"
            };
            return Err(CliError::msg(format!(
                "session \"{current}\" is unresponsive ({kind})"
            )));
        }
    };
    match stream.write_all(&encode_frame(Tag::SWITCH, next.as_bytes())) {
        Ok(()) => Ok(()),
        Err(e)
            if matches!(
                e.kind(),
                io::ErrorKind::BrokenPipe | io::ErrorKind::ConnectionReset
            ) =>
        {
            Ok(())
        }
        Err(_) => Err(CliError::Silent),
    }
}
