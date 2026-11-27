/*
 * tako-session — session runtime for Tako
 * Copyright (c) 2026 Alexander Panasenko
 *
 * Contact: alex@prod.codes
 * Author: https://prod.codes/about/
 * Project: https://github.com/alex09x/tako-core
 * SPDX-License-Identifier: MIT
 */

//! `history` (a session's scrollback and screen) and `print-env` (the
//! environment its leading client tracks).

use super::error::{CliError, CliResult, name_too_long};
use crate::client::{Conn, ProbeError, round_trip_for_tag};
use crate::ipc::Tag;
use crate::{log, socket};
use std::io::Write;
use std::path::Path;

/// 0 plain, 1 VT, 2 HTML.
pub fn format_byte(arg: &str) -> Option<u8> {
    match arg {
        "--vt" => Some(1),
        "--html" => Some(2),
        _ => None,
    }
}

/// The `History` payload, or None when the session did not answer in 5 s.
fn request(conn: &mut Conn, format: u8) -> Option<Vec<u8>> {
    conn.send(Tag::HISTORY, &[format]).ok()?;
    loop {
        if !conn.poll_readable(5000).ok()? {
            log::error("timeout waiting for history response");
            return None;
        }
        if conn.fill().ok()? == 0 {
            return None;
        }
        while let Some(frame) = conn.buf.next_frame() {
            if frame.header.tag == Tag::HISTORY {
                return Some(frame.payload.to_vec());
            }
        }
    }
}

fn connect(dir: &Path, name: &str) -> Result<Conn, CliError> {
    let path = socket::socket_path(dir, name).ok_or_else(|| name_too_long(name, dir))?;
    if !socket::session_exists(dir, name).map_err(|_| CliError::Silent)? {
        return Err(CliError::msg(format!("session \"{name}\" does not exist")));
    }
    Conn::connect(&path).map_err(|e| {
        log::error(&format!("session unresponsive: {}", e.name()));
        if e == ProbeError::ConnectionRefused {
            socket::cleanup_stale_socket(dir, name);
        }
        CliError::msg(format!("session \"{name}\" is unresponsive ({})", e.name()))
    })
}

pub fn history(dir: &Path, name: &str, format: u8, out: &mut dyn Write) -> CliResult {
    log::info(&format!("history session={name}"));
    let mut conn = connect(dir, name)?;
    if let Some(text) = request(&mut conn, format) {
        out.write_all(&text).map_err(|_| CliError::Silent)?;
    }
    Ok(())
}

/// Plain history for `wait`'s report; None on any failure.
pub fn fetch(dir: &Path, name: &str) -> Option<Vec<u8>> {
    let mut conn = connect(dir, name).ok()?;
    request(&mut conn, 0)
}

/// `KEY=value` lines and `-KEY` (unset) lines.
fn env_entries(payload: &str) -> impl Iterator<Item = (&str, Option<&str>)> {
    payload
        .split('\n')
        .filter(|l| !l.is_empty())
        .filter_map(|line| match line.strip_prefix('-') {
            Some(key) => Some((key, None)),
            None => line.split_once('=').map(|(k, v)| (k, Some(v))),
        })
}

/// `[A-Za-z_][A-Za-z0-9_]*`.
fn is_identifier(name: &str) -> bool {
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|c| c == '_' || c.is_ascii_alphabetic())
        && chars.all(|c| c == '_' || c.is_ascii_alphanumeric())
}

/// `print-env [-s] <name> [key]`.
pub fn print_env(dir: &Path, name: &str, key: &str, shell: bool, out: &mut dyn Write) -> CliResult {
    log::info(&format!("env get session={name}"));
    let path = socket::socket_path(dir, name).ok_or_else(|| name_too_long(name, dir))?;
    let payload = round_trip_for_tag(&path, Tag::ENV_GET, b"", Tag::ENV_DATA).map_err(|e| {
        CliError::msg(format!(
            "could not communicate with session \"{name}\" err={}",
            e.name()
        ))
    })?;
    let text = String::from_utf8_lossy(&payload).into_owned();
    let w =
        |out: &mut dyn Write, s: String| out.write_all(s.as_bytes()).map_err(|_| CliError::Silent);
    if !key.is_empty() {
        let value = env_entries(&text)
            .find(|(k, _)| *k == key)
            .and_then(|(_, v)| v);
        let value = value.ok_or_else(|| CliError::msg("environment variable not found"))?;
        return w(out, format!("{value}\n"));
    }
    if !shell {
        return out.write_all(&payload).map_err(|_| CliError::Silent);
    }
    for (k, v) in env_entries(&text) {
        match v {
            // A name that is no shell identifier would be shell syntax.
            _ if !is_identifier(k) => {}
            Some(v) => w(out, format!("export {k}='{}';\n", v.replace('\'', "'\\''")))?,
            None => w(out, format!("unset {k};\n"))?,
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{env_entries, is_identifier};

    #[test]
    fn only_shell_identifiers_become_exports() {
        assert!(is_identifier("SSH_AUTH_SOCK") && is_identifier("_x1"));
        for bad in ["", "1A", "A-B", "A;rm", "A B", "É"] {
            assert!(!is_identifier(bad), "{bad}");
        }
    }

    #[test]
    fn env_lines_set_and_unset() {
        let got: Vec<_> = env_entries("A=1\n-B\n\nC=x=y\nnoeq\n").collect();
        assert_eq!(got, vec![("A", Some("1")), ("B", None), ("C", Some("x=y"))]);
    }
}
