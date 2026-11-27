/*
 * tako-session — session runtime for Tako
 * Copyright (c) 2026 Alexander Panasenko
 *
 * Contact: alex@prod.codes
 * Author: https://prod.codes/about/
 * Project: https://github.com/alex09x/tako-core
 * SPDX-License-Identifier: MIT
 */

//! One connection to a daemon: an attached terminal, or a short command.

use crate::ipc::FrameBuffer;
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;

pub struct Client {
    pub stream: UnixStream,
    /// Frames queued for the client; flushed when the socket is writable.
    pub write_buf: Vec<u8>,
    pub read_buf: FrameBuffer,
    pub has_pending_output: bool,
    /// Some bytes of `write_buf` already went out (a partial flush).
    pub has_flushed_bytes: bool,
    /// Sent `Init`: a real attached terminal, not run/send/tail.
    pub is_terminal: bool,
    /// The tracked environment the client sent (`EnvSet`).
    pub env_str: Option<Vec<u8>>,
}

impl Client {
    pub fn new(stream: UnixStream) -> Client {
        Client {
            stream,
            write_buf: Vec::with_capacity(65536),
            read_buf: FrameBuffer::new(),
            has_pending_output: false,
            has_flushed_bytes: false,
            is_terminal: false,
            env_str: None,
        }
    }

    pub fn fd(&self) -> i32 {
        self.stream.as_raw_fd()
    }

    pub fn set_env(&mut self, env_str: &[u8]) {
        self.env_str = (!env_str.is_empty()).then(|| env_str.to_vec());
    }
}
