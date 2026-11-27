/*
 * tako-session — session runtime for Tako
 * Copyright (c) 2026 Alexander Panasenko
 *
 * Contact: alex@prod.codes
 * Author: https://prod.codes/about/
 * Project: https://github.com/alex09x/tako-core
 * SPDX-License-Identifier: MIT
 */

//! `send` (input to the program), `print` (text onto the session's screen)
//! and `write` (a file created through the session's shell).

use super::error::{CliError, CliResult};
use crate::client::{Probe, ProbeError, await_tag, monotonic_millis, probe_session};
use crate::ipc::{Tag, encode_frame};
use crate::{log, socket};
use std::io::{self, IsTerminal, Read, Write};
use std::path::Path;

/// All of standard input, unless it is a terminal.
pub fn read_stdin() -> Vec<u8> {
    let mut data = Vec::new();
    if !io::stdin().is_terminal() {
        let _ = io::stdin().read_to_end(&mut data);
    }
    data
}

/// A probed connection, or the message the session's state calls for (a
/// refused one is a stale socket, and is removed).
pub fn probe_or_report(dir: &Path, name: &str, path: &Path) -> Result<Probe, CliError> {
    probe_session(path).map_err(|err| {
        log::error(&format!("session unresponsive: {}", err.name()));
        if err == crate::client::ProbeError::ConnectionRefused {
            socket::cleanup_stale_socket(dir, name);
            CliError::Line(format!("cleaned up stale session {name}"))
        } else {
            CliError::Line(format!(
                "session {name} is unresponsive ({})\ndaemon may be busy: try again",
                err.name()
            ))
        }
    })
}

/// `send` with `Tag::SEND`, `print` with `Tag::OUTPUT`: the words joined by
/// spaces, else standard input (for `send` without its final newline).
pub fn send(dir: &Path, name: &str, path: &Path, words: &[String], tag: Tag) -> CliResult {
    log::info(&format!("send session={name}"));
    let mut payload = words.join(" ").into_bytes();
    if words.is_empty() {
        payload = read_stdin();
        if tag != Tag::OUTPUT && payload.last() == Some(&b'\n') {
            payload.pop();
        }
    }
    if payload.is_empty() {
        return Err(CliError::msg(
            "text argument required (or pipe input via stdin)",
        ));
    }
    let mut probe = probe_or_report(dir, name, path)?;
    match probe.conn.stream.write_all(&encode_frame(tag, &payload)) {
        Ok(()) => Ok(()),
        Err(e) if socket::hung_up(&e) => Ok(()),
        Err(_) => Err(CliError::Silent),
    }
}

/// `write <name> <path>`: standard input becomes `path` in the session.
pub fn write(
    dir: &Path,
    name: &str,
    path: &Path,
    file_path: &str,
    out: &mut dyn Write,
) -> CliResult {
    let content = read_stdin();
    let mut probe = probe_or_report(dir, name, path)?;
    let mut payload = (file_path.len() as u32).to_ne_bytes().to_vec();
    payload.extend_from_slice(file_path.as_bytes());
    payload.extend_from_slice(&content);
    let stream = &mut probe.conn.stream;
    match stream.write_all(&encode_frame(Tag::WRITE, &payload)) {
        Ok(()) => {}
        Err(e) if socket::hung_up(&e) => return Ok(()),
        Err(_) => return Err(CliError::Silent),
    }
    // The ACK may come in pieces or after other frames: wait for it whole.
    let deadline = monotonic_millis() + ACK_TIMEOUT_MS;
    match await_tag(&mut probe.conn, Tag::ACK, deadline, None) {
        Ok(_) => writeln!(out, "file created {file_path}").map_err(|_| CliError::Silent),
        Err(ProbeError::Unexpected) => Err(CliError::msg("write failed: ConnectionClosed")),
        Err(_) => Err(CliError::msg("write failed: NoAckReceived")),
    }
}

/// How long `write` waits for the daemon to take the file.
const ACK_TIMEOUT_MS: i64 = 60_000;
