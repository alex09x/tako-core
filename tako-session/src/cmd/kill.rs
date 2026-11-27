/*
 * tako-session — session runtime for Tako
 * Copyright (c) 2026 Alexander Panasenko
 *
 * Contact: alex@prod.codes
 * Author: https://prod.codes/about/
 * Project: https://github.com/alex09x/tako-core
 * SPDX-License-Identifier: MIT
 */

//! `kill`: ends sessions, unconditionally or only when their labels match.

use super::error::{CliError, CliResult, name_too_long};
use super::labels::assert_guard_labels;
use super::list::session_entries;
use crate::client::{Conn, ProbeError, await_guard_status};
use crate::ipc::{GuardReply, Tag};
use crate::log;
use crate::socket::{SessionMatch, cleanup_stale_socket, session_exists, socket_path};
use std::io::{self, Read, Write};
use std::path::Path;

/// Why one kill failed; printed by name.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KillError {
    SessionNotFound,
    SessionUnresponsive,
    FileNotUnixSocket,
    LabelMismatch,
    InvalidGuard,
    UnsupportedByDaemon,
    Unexpected,
    BrokenPipe,
}

impl KillError {
    pub fn name(self) -> &'static str {
        match self {
            KillError::SessionNotFound => "SessionNotFound",
            KillError::SessionUnresponsive => "SessionUnresponsive",
            KillError::FileNotUnixSocket => "FileNotUnixSocket",
            KillError::LabelMismatch => "LabelMismatch",
            KillError::InvalidGuard => "InvalidGuard",
            KillError::UnsupportedByDaemon => "UnsupportedByDaemon",
            KillError::Unexpected => "Unexpected",
            KillError::BrokenPipe => "BrokenPipe",
        }
    }
}

/// The command's streams.
pub struct Io<'a> {
    pub out: &'a mut dyn Write,
    pub err: &'a mut dyn Write,
}

fn exists(dir: &Path, name: &str) -> Result<(), KillError> {
    match session_exists(dir, name) {
        Ok(true) => Ok(()),
        Ok(false) => Err(KillError::SessionNotFound),
        Err(e) if e.kind() == io::ErrorKind::InvalidInput => Err(KillError::FileNotUnixSocket),
        Err(_) => Err(KillError::Unexpected),
    }
}

/// Reads until the daemon hangs up: it unlinks its socket first, so the
/// name is free for reuse once this returns.
fn drain(conn: &mut Conn) {
    let mut buf = [0u8; 256];
    while matches!(conn.stream.read(&mut buf), Ok(n) if n > 0) {}
}

fn killed(io: &mut Io, name: &str) {
    let _ = writeln!(io.out, "killed session {name}");
    let _ = io.out.flush();
}

/// Kills one session. `force` also clears a socket whose daemon cannot be
/// reached.
pub fn kill_one(dir: &Path, name: &str, force: bool, io: &mut Io) -> Result<(), KillError> {
    log::info(&format!("kill session={name}"));
    let path = socket_path(dir, name).ok_or(KillError::Unexpected)?;
    exists(dir, name)?;
    let mut conn = match Conn::connect(&path) {
        Ok(conn) => conn,
        Err(err) => {
            log::error(&format!("session unresponsive: {}", err.name()));
            if force || err == ProbeError::ConnectionRefused {
                cleanup_stale_socket(dir, name);
                let _ = writeln!(io.err, "cleaned up stale session {name}");
                return Ok(());
            }
            return Err(KillError::SessionUnresponsive);
        }
    };
    if let Err(e) = conn.send(Tag::KILL, b"") {
        return match e.kind() {
            io::ErrorKind::BrokenPipe | io::ErrorKind::ConnectionReset => Ok(()),
            _ => Err(KillError::Unexpected),
        };
    }
    drain(&mut conn);
    killed(io, name);
    Ok(())
}

/// Kills one session only if its labels match `required`. Never removes a
/// socket: without a verified match nothing changes.
pub fn kill_if_label(dir: &Path, name: &str, required: &str, io: &mut Io) -> Result<(), KillError> {
    log::info(&format!("conditional kill session={name}"));
    let path = socket_path(dir, name).ok_or(KillError::Unexpected)?;
    exists(dir, name)?;
    let mut conn = Conn::connect(&path).map_err(|e| match e {
        ProbeError::ConnectionRefused => KillError::SessionNotFound,
        _ => KillError::SessionUnresponsive,
    })?;
    conn.send(Tag::KILL_IF_LABEL, required.as_bytes())
        .map_err(|_| KillError::BrokenPipe)?;
    match await_guard_status(&mut conn, 1000) {
        Ok(GuardReply::Matched) => {}
        Ok(GuardReply::Mismatch) => return Err(KillError::LabelMismatch),
        Ok(_) => return Err(KillError::InvalidGuard),
        Err(ProbeError::Timeout) => return Err(KillError::UnsupportedByDaemon),
        Err(_) => return Err(KillError::Unexpected),
    }
    drain(&mut conn);
    killed(io, name);
    Ok(())
}

/// Parses and runs `kill [--force] [--if-label LABELS] <session|prefix*>...`.
pub fn run(
    dir: &Path,
    env: &dyn crate::cfg::Env,
    args: &[String],
    io: &mut Io,
) -> Result<(), CliError> {
    let mut matchers = Vec::new();
    let (mut force, mut if_label, mut missing_value) = (false, None::<String>, false);
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        if arg == "--help" || arg == "-h" {
            return Err(CliError::Silent); // the caller prints help
        }
        if arg == "--force" {
            force = true;
        } else if let Some(v) = arg.strip_prefix("--if-label=") {
            if_label = Some(v.to_string());
        } else if arg == "--if-label" {
            match it.next() {
                Some(v) => if_label = Some(v.clone()),
                None => missing_value = true,
            }
        } else {
            matchers.push(SessionMatch::parse(env, arg).map_err(|_| CliError::Silent)?);
        }
    }
    if missing_value {
        return Err(CliError::msg("--if-label requires \"key=value ...\""));
    }
    if matchers.is_empty() {
        return Err(CliError::msg("session name required"));
    }
    if let Some(required) = if_label {
        if force {
            return Err(CliError::msg("--force cannot be combined with --if-label"));
        }
        assert_guard_labels(&required, true)?;
        if matchers.len() != 1 || matchers[0].is_prefix {
            return Err(CliError::msg(
                "--if-label requires a single exact session name",
            ));
        }
        let name = &matchers[0].name;
        if socket_path(dir, name).is_none() {
            return Err(name_too_long(name, dir));
        }
        return kill_if_label(dir, name, &required, io)
            .map_err(|e| CliError::msg(format!("failed to kill session={name}: {}", e.name())));
    }
    kill_matching(dir, &matchers, force, io)
}

fn kill_matching(dir: &Path, matchers: &[SessionMatch], force: bool, io: &mut Io) -> CliResult {
    let sessions = session_entries(dir).map_err(|_| CliError::Silent)?;
    let mut killed_any = false;
    for session in &sessions {
        // Each matching pattern tries once more after a forced kill found
        // nothing; any other outcome ends this session's turn.
        for _ in matchers.iter().filter(|m| m.matches(&session.name)) {
            match kill_one(dir, &session.name, force, io) {
                Err(KillError::SessionNotFound) if force => {
                    killed_any = true;
                    continue;
                }
                Err(e) => {
                    let _ = writeln!(
                        io.err,
                        "failed to kill session={}: {}",
                        session.name,
                        e.name()
                    );
                }
                Ok(()) => {}
            }
            killed_any = true;
            break;
        }
    }
    if killed_any {
        return Ok(());
    }
    for m in matchers.iter().filter(|m| !m.is_prefix) {
        if socket_path(dir, &m.name).is_none() {
            return Err(name_too_long(&m.name, dir));
        }
        match kill_one(dir, &m.name, force, io) {
            Ok(()) => killed_any = true,
            Err(KillError::SessionNotFound) if force => killed_any = true,
            Err(e) => {
                return Err(CliError::msg(format!(
                    "failed to kill session={}: {}",
                    m.name,
                    e.name()
                )));
            }
        }
    }
    if !killed_any {
        return Err(CliError::msg("no matching sessions found"));
    }
    Ok(())
}
