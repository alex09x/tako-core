/*
 * tako-session — session runtime for Tako
 * Copyright (c) 2026 Alexander Panasenko
 *
 * Contact: alex@prod.codes
 * Author: https://prod.codes/about/
 * Project: https://github.com/alex09x/tako-core
 * SPDX-License-Identifier: MIT
 */

//! `detach`: detaches every client of the session this shell runs in.

use super::error::{CliError, CliResult, name_too_long};
use crate::client::{Conn, ProbeError};
use crate::ipc::Tag;
use crate::log;
use crate::socket::{cleanup_stale_socket, hung_up, socket_path};
use std::path::Path;

pub fn run(dir: &Path, session: &str) -> CliResult {
    if session.is_empty() {
        return Err(CliError::msg(
            "not inside a zmx session (ZMX_SESSION not set)",
        ));
    }
    log::info(&format!("detach all session={session}"));
    if !dir.is_dir() {
        return Err(CliError::Silent);
    }
    let path = socket_path(dir, session).ok_or_else(|| name_too_long(session, dir))?;
    let mut conn = match Conn::connect(&path) {
        Ok(conn) => conn,
        Err(err) => {
            log::error(&format!("session unresponsive: {}", err.name()));
            if err == ProbeError::ConnectionRefused {
                cleanup_stale_socket(dir, session);
            }
            return Ok(());
        }
    };
    match conn.send(Tag::DETACH_ALL, b"") {
        Ok(()) => Ok(()),
        Err(e) if hung_up(&e) => Ok(()),
        Err(_) => Err(CliError::Silent),
    }
}
