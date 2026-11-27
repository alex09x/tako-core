/*
 * tako-session — session runtime for Tako
 * Copyright (c) 2026 Alexander Panasenko
 *
 * Contact: alex@prod.codes
 * Author: https://prod.codes/about/
 * Project: https://github.com/alex09x/tako-core
 * SPDX-License-Identifier: MIT
 */

//! A client's connection to a session daemon: framed writes, and reads into
//! a frame buffer with a poll timeout.

use crate::ipc::{FrameBuffer, Tag, encode_frame};
use std::io::{self, Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::Instant;

/// Why a request to a daemon failed, named as the CLI reports it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProbeError {
    Timeout,
    ConnectionRefused,
    Unexpected,
    InfoSizeMismatch,
    UnreadOutputBudgetExceeded,
}

impl ProbeError {
    pub fn name(self) -> &'static str {
        match self {
            ProbeError::Timeout => "Timeout",
            ProbeError::ConnectionRefused => "ConnectionRefused",
            ProbeError::Unexpected => "Unexpected",
            ProbeError::InfoSizeMismatch => "InfoSizeMismatch",
            ProbeError::UnreadOutputBudgetExceeded => "UnreadOutputBudgetExceeded",
        }
    }
}

/// Milliseconds on a monotonic clock, for deadlines.
pub fn monotonic_millis() -> i64 {
    use std::sync::OnceLock;
    static START: OnceLock<Instant> = OnceLock::new();
    START.get_or_init(Instant::now).elapsed().as_millis() as i64
}

#[derive(Debug)]
pub struct Conn {
    pub stream: UnixStream,
    pub buf: FrameBuffer,
}

impl Conn {
    /// Connects; a socket nobody listens on is `ConnectionRefused`, anything
    /// else `Unexpected`.
    pub fn connect(path: &Path) -> Result<Conn, ProbeError> {
        match UnixStream::connect(path) {
            Ok(stream) => Ok(Conn::from_stream(stream)),
            Err(e) if e.kind() == io::ErrorKind::ConnectionRefused => {
                Err(ProbeError::ConnectionRefused)
            }
            Err(_) => Err(ProbeError::Unexpected),
        }
    }

    pub fn from_stream(stream: UnixStream) -> Conn {
        Conn {
            stream,
            buf: FrameBuffer::new(),
        }
    }

    pub fn send(&mut self, tag: Tag, payload: &[u8]) -> io::Result<()> {
        self.stream.write_all(&encode_frame(tag, payload))
    }

    /// Waits up to `timeout_ms` (negative: forever) for the socket to be
    /// readable. True when it is.
    pub fn poll_readable(&self, timeout_ms: i32) -> io::Result<bool> {
        let mut fds = libc::pollfd {
            fd: self.stream.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        loop {
            // SAFETY: one valid pollfd for the duration of the call.
            let n = unsafe { libc::poll(&mut fds, 1, timeout_ms) };
            if n >= 0 {
                return Ok(n > 0);
            }
            let err = io::Error::last_os_error();
            if err.kind() != io::ErrorKind::Interrupted {
                return Err(err);
            }
        }
    }

    /// Reads what is available into the frame buffer; 0 at end of stream.
    pub fn fill(&mut self) -> io::Result<usize> {
        let mut tmp = [0u8; 4096];
        let n = self.stream.read(&mut tmp)?;
        self.buf.push(&tmp[..n]);
        Ok(n)
    }
}
