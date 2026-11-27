/*
 * tako-session — session runtime for Tako
 * Copyright (c) 2026 Alexander Panasenko
 *
 * Contact: alex@prod.codes
 * Author: https://prod.codes/about/
 * Project: https://github.com/alex09x/tako-core
 * SPDX-License-Identifier: MIT
 */

//! Short request/reply exchanges with a daemon, each under a deadline.

use super::conn::{Conn, ProbeError, monotonic_millis};
use crate::ipc::{
    GuardReply, HEADER_LEN, INFO_LEN, Info, MAX_HANDSHAKE_UNREAD_BYTES, Tag, append_frame,
};
use std::io;
use std::path::Path;

const PROBE_TIMEOUT_MS: i32 = 1000;

/// What `list` learns about a live session.
#[derive(Debug)]
pub struct Probe {
    pub conn: Conn,
    pub info: Info,
    /// None from a daemon too old to know labels.
    pub labels: Option<Vec<u8>>,
}

fn wait_ms(deadline: i64) -> Result<i32, ProbeError> {
    let left = deadline - monotonic_millis();
    if left <= 0 {
        return Err(ProbeError::Timeout);
    }
    Ok(left.min(i32::MAX as i64) as i32)
}

/// Reads once more after a readable poll. Would-block is not an error.
fn fill(conn: &mut Conn) -> Result<(), ProbeError> {
    match conn.fill() {
        Ok(0) => Err(ProbeError::Unexpected),
        Ok(_) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::WouldBlock => Ok(()),
        Err(_) => Err(ProbeError::Unexpected),
    }
}

/// Asks for `Info` and the labels on one connection.
pub fn probe_session(path: &Path) -> Result<Probe, ProbeError> {
    let mut conn = Conn::connect(path)?;
    conn.send(Tag::INFO, b"")
        .map_err(|_| ProbeError::Unexpected)?;
    let _ = conn.send(Tag::LABEL_GET, b"");
    if !conn
        .poll_readable(PROBE_TIMEOUT_MS)
        .map_err(|_| ProbeError::Unexpected)?
    {
        return Err(ProbeError::Timeout);
    }
    if conn.fill().map_err(|_| ProbeError::Unexpected)? == 0 {
        return Err(ProbeError::Unexpected);
    }
    let (mut info, mut labels) = (None, None);
    loop {
        if let Some(frame) = conn.buf.next_frame() {
            if frame.header.tag == Tag::INFO {
                if frame.payload.len() != INFO_LEN {
                    return Err(ProbeError::InfoSizeMismatch);
                }
                info = Info::decode(frame.payload);
            }
            if frame.header.tag == Tag::LABEL_DATA {
                labels = Some(frame.payload.to_vec());
            }
            if info.is_some() && labels.is_some() {
                break;
            }
            continue;
        }
        match conn.poll_readable(50) {
            Ok(true) => {}
            _ => break,
        }
        match conn.fill() {
            Ok(n) if n > 0 => {}
            _ => break,
        }
    }
    match info {
        Some(info) => Ok(Probe { conn, info, labels }),
        None => Err(ProbeError::Unexpected),
    }
}

/// Sends one request and returns the payload of the first `expected` frame
/// that arrives within the probe timeout, however many reads it takes.
pub fn round_trip_for_tag(
    path: &Path,
    request: Tag,
    payload: &[u8],
    expected: Tag,
) -> Result<Vec<u8>, ProbeError> {
    let mut conn = Conn::connect(path)?;
    conn.send(request, payload)
        .map_err(|_| ProbeError::Unexpected)?;
    let deadline = monotonic_millis() + i64::from(PROBE_TIMEOUT_MS);
    await_tag(&mut conn, expected, deadline, None)
}

/// Waits for `GuardStatus`, skipping other frames; later frames stay
/// buffered. A daemon that ignores the guarded request never answers, so
/// this times out: the caller fails closed.
pub fn await_guard_status(conn: &mut Conn, timeout_ms: i32) -> Result<GuardReply, ProbeError> {
    let deadline = monotonic_millis() + timeout_ms.max(0) as i64;
    loop {
        while let Some(frame) = conn.buf.next_frame() {
            if frame.header.tag == Tag::GUARD_STATUS {
                return GuardReply::decode(frame.payload).ok_or(ProbeError::Unexpected);
            }
        }
        if !conn
            .poll_readable(wait_ms(deadline)?)
            .map_err(|_| ProbeError::Unexpected)?
        {
            return Err(ProbeError::Timeout);
        }
        fill(conn)?;
    }
}

/// Appends an `Output` frame to `unread`, unless that passes `max` bytes.
pub fn keep_output(unread: &mut Vec<u8>, payload: &[u8], max: usize) -> Result<(), ProbeError> {
    if unread.len() + HEADER_LEN + payload.len() > max {
        return Err(ProbeError::UnreadOutputBudgetExceeded);
    }
    append_frame(unread, Tag::OUTPUT, payload);
    Ok(())
}

/// Waits until `expected` arrives or the monotonic `deadline` passes. `Output`
/// frames meanwhile are kept in `unread` when given (within `max` bytes), or
/// dropped.
pub fn await_tag_until(
    conn: &mut Conn,
    expected: Tag,
    deadline: i64,
    mut unread: Option<&mut Vec<u8>>,
    max: usize,
) -> Result<Vec<u8>, ProbeError> {
    loop {
        while let Some(frame) = conn.buf.next_frame() {
            if frame.header.tag == expected {
                return Ok(frame.payload.to_vec());
            }
            if frame.header.tag == Tag::OUTPUT
                && let Some(out) = unread.as_deref_mut()
            {
                keep_output(out, frame.payload, max)?;
            }
        }
        let kept = unread.as_deref().map_or(0, Vec::len);
        if kept + conn.buf.unread().len() > max {
            return Err(ProbeError::UnreadOutputBudgetExceeded);
        }
        if !conn
            .poll_readable(wait_ms(deadline)?)
            .map_err(|_| ProbeError::Unexpected)?
        {
            return Err(ProbeError::Timeout);
        }
        fill(conn)?;
    }
}

/// `await_tag_until` with the handshake budget.
pub fn await_tag(
    conn: &mut Conn,
    expected: Tag,
    deadline: i64,
    unread: Option<&mut Vec<u8>>,
) -> Result<Vec<u8>, ProbeError> {
    await_tag_until(conn, expected, deadline, unread, MAX_HANDSHAKE_UNREAD_BYTES)
}
