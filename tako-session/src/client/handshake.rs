/*
 * tako-session — session runtime for Tako
 * Copyright (c) 2026 Alexander Panasenko
 *
 * Contact: alex@prod.codes
 * Author: https://prod.codes/about/
 * Project: https://github.com/alex09x/tako-core
 * SPDX-License-Identifier: MIT
 */

//! The guarded attach: one socket, labels verified before the client takes
//! the terminal, `Output` that arrives meanwhile kept for the attached loop.

use super::conn::{Conn, ProbeError, monotonic_millis};
use super::probe::{await_tag_until, keep_output};
use crate::ipc::{
    GuardReply, MAX_HANDSHAKE_UNREAD_BYTES, Tag, guard_payload, label_pairs_match,
    validate_label_pairs,
};
use std::path::PathBuf;
use std::time::Duration;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HandshakeError {
    Timeout,
    Unexpected,
    LabelMismatch,
    InvalidGuard,
    UnreadOutputBudgetExceeded,
}

impl From<ProbeError> for HandshakeError {
    fn from(e: ProbeError) -> Self {
        match e {
            ProbeError::Timeout => HandshakeError::Timeout,
            ProbeError::UnreadOutputBudgetExceeded => HandshakeError::UnreadOutputBudgetExceeded,
            _ => HandshakeError::Unexpected,
        }
    }
}

/// Test hooks for the legacy path, so a test can change the session between
/// the client's two label checks.
#[derive(Clone, Debug)]
pub struct Hooks {
    pub after_probe_delay_ms: u32,
    pub max_unread_bytes: usize,
    pub probe_ready_path: Option<PathBuf>,
    pub probe_resume_path: Option<PathBuf>,
}

impl Default for Hooks {
    fn default() -> Hooks {
        Hooks {
            after_probe_delay_ms: 0,
            max_unread_bytes: MAX_HANDSHAKE_UNREAD_BYTES,
            probe_ready_path: None,
            probe_resume_path: None,
        }
    }
}

fn send(conn: &mut Conn, tag: Tag, payload: &[u8]) -> Result<(), HandshakeError> {
    conn.send(tag, payload)
        .map_err(|_| HandshakeError::Unexpected)
}

/// The rest of the buffered stream, appended to the kept output.
fn finish(conn: &mut Conn, mut unread: Vec<u8>, max: usize) -> Result<Vec<u8>, HandshakeError> {
    let rest = conn.buf.unread();
    if unread.len() + rest.len() > max {
        return Err(HandshakeError::UnreadOutputBudgetExceeded);
    }
    unread.extend_from_slice(rest);
    Ok(unread)
}

/// Pauses after the first legacy check: until the resume file appears (the
/// pause then extends the deadline) or for the fixed delay.
fn legacy_pause(hooks: &Hooks, deadline: &mut i64) {
    if let Some(ready) = &hooks.probe_ready_path {
        let _ = std::fs::File::create(ready);
    }
    if hooks.after_probe_delay_ms == 0 {
        return;
    }
    let start = monotonic_millis();
    match &hooks.probe_resume_path {
        Some(resume) => {
            while monotonic_millis() < start + i64::from(hooks.after_probe_delay_ms)
                && !resume.exists()
            {
                std::thread::sleep(Duration::from_millis(10));
            }
            *deadline += monotonic_millis() - start;
        }
        None => {
            std::thread::sleep(Duration::from_millis(u64::from(hooks.after_probe_delay_ms)));
            *deadline += i64::from(hooks.after_probe_delay_ms);
        }
    }
}

/// Runs the handshake on `conn`. With `allow_legacy` it pipelines
/// `AttachIfLabel` and `LabelGet`: a protocol-3 daemon answers `GuardStatus`
/// first; an older one ignores the guard and answers only `LabelData`, and
/// the client checks the labels itself, again right before attaching, then
/// sets `set_labels`. Returns the frames to hand to the attached loop.
pub fn guarded_attach(
    conn: &mut Conn,
    required: &str,
    set_labels: Option<&str>,
    allow_legacy: bool,
    timeout_ms: i32,
    hooks: &Hooks,
) -> Result<Vec<u8>, HandshakeError> {
    validate_label_pairs(required, true, true).map_err(|_| HandshakeError::InvalidGuard)?;
    validate_label_pairs(set_labels.unwrap_or(""), false, false)
        .map_err(|_| HandshakeError::InvalidGuard)?;
    send(
        conn,
        Tag::ATTACH_IF_LABEL,
        &guard_payload(required, set_labels.unwrap_or("")),
    )?;
    if allow_legacy {
        send(conn, Tag::LABEL_GET, b"")?;
    }
    let max = hooks.max_unread_bytes;
    let mut unread = Vec::new();
    let mut deadline = monotonic_millis() + i64::from(timeout_ms.max(0));
    loop {
        while let Some(frame) = conn.buf.next_frame() {
            match frame.header.tag {
                Tag::OUTPUT => keep_output(&mut unread, frame.payload, max)?,
                Tag::GUARD_STATUS => {
                    let reply =
                        GuardReply::decode(frame.payload).ok_or(HandshakeError::Unexpected)?;
                    match reply {
                        GuardReply::Matched => {}
                        GuardReply::Mismatch => return Err(HandshakeError::LabelMismatch),
                        _ => return Err(HandshakeError::InvalidGuard),
                    }
                    if allow_legacy {
                        await_tag_until(conn, Tag::LABEL_DATA, deadline, Some(&mut unread), max)?;
                    }
                    return finish(conn, unread, max);
                }
                Tag::LABEL_DATA => {
                    if !allow_legacy {
                        return Err(HandshakeError::Unexpected);
                    }
                    let actual = String::from_utf8_lossy(frame.payload).into_owned();
                    if !label_pairs_match(&actual, required) {
                        return Err(HandshakeError::LabelMismatch);
                    }
                    legacy_pause(hooks, &mut deadline);
                    send(conn, Tag::LABEL_GET, b"")?;
                    let again =
                        await_tag_until(conn, Tag::LABEL_DATA, deadline, Some(&mut unread), max)?;
                    if !label_pairs_match(&String::from_utf8_lossy(&again), required) {
                        return Err(HandshakeError::LabelMismatch);
                    }
                    if let Some(set) = set_labels.filter(|s| !s.is_empty()) {
                        send(conn, Tag::LABEL_SET, set.as_bytes())?;
                        await_tag_until(conn, Tag::ACK, deadline, Some(&mut unread), max)?;
                    }
                    return finish(conn, unread, max);
                }
                _ => {}
            }
        }
        if unread.len() + conn.buf.unread().len() > max {
            return Err(HandshakeError::UnreadOutputBudgetExceeded);
        }
        let left = deadline - monotonic_millis();
        if left <= 0 {
            return Err(HandshakeError::Timeout);
        }
        if !conn
            .poll_readable(left.min(i64::from(i32::MAX)) as i32)
            .map_err(|_| HandshakeError::Unexpected)?
        {
            return Err(HandshakeError::Timeout);
        }
        match conn.fill() {
            Ok(0) => return Err(HandshakeError::Unexpected),
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(_) => return Err(HandshakeError::Unexpected),
        }
    }
}
