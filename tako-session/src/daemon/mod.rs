/*
 * tako-session — session runtime for Tako
 * Copyright (c) 2026 Alexander Panasenko
 *
 * Contact: alex@prod.codes
 * Author: https://prod.codes/about/
 * Project: https://github.com/alex09x/tako-core
 * SPDX-License-Identifier: MIT
 */

//! The per-session daemon: it owns the PTY and the program on it, keeps the
//! screen in a `tako_core` terminal, and serves any number of clients over
//! the session socket. One daemon per session, so a crash ends one session.

mod client;
mod event_loop;
mod handlers;
mod inject;
mod labels;
mod run;
pub mod spawn;

pub use client::Client;
pub use run::{Started, create_only, ensure_session};

use crate::ipc::{Tag, append_frame};
use crate::{log, osc7};
use std::collections::BTreeMap;
use std::path::PathBuf;
use tako_core::terminal::Terminal;

/// Most bytes queued for the program's input; more is dropped, newest first.
pub const PTY_WRITE_BUF_MAX: usize = 256 * 1024;

/// How a daemon is set up before it starts.
#[derive(Clone, Debug, Default)]
pub struct Options {
    pub session: String,
    pub socket_path: PathBuf,
    pub log_dir: PathBuf,
    pub log_mode: u32,
    /// The program to run; None runs a login shell.
    pub command: Option<Vec<String>>,
    pub shell: String,
    pub task_mode: bool,
    /// `k=v` pairs the session starts with.
    pub initial_labels: Option<String>,
    /// Where the program starts: an OSC 7 URI or an absolute path.
    pub cwd: Option<Vec<u8>>,
    pub max_scrollback_lines: usize,
}

pub struct Daemon {
    pub opts: Options,
    pub clients: Vec<Client>,
    pub labels: BTreeMap<String, String>,
    /// The client whose size and input lead the session, by socket fd.
    pub leader: Option<i32>,
    pub pty_write_buf: Vec<u8>,
    pub running: bool,
    pub pid: i32,
    pub pty_fd: i32,
    /// The working directory as OSC 7 reports it (`file://host/path`).
    pub cwd: Vec<u8>,
    /// The same directory as a local path; empty when it is on another host.
    pub cwd_path: Vec<u8>,
    pub has_pty_output: bool,
    pub has_had_client: bool,
    /// Unix seconds.
    pub created_at: u64,
    pub task_mode: bool,
    pub task_id: [u8; 4],
    pub task_exit_code: Option<u8>,
    pub task_ended_at: Option<u64>,
    pub term: Terminal,
}

pub fn unix_seconds() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

impl Daemon {
    pub fn new(opts: Options) -> Daemon {
        let task_mode = opts.task_mode;
        let mut d = Daemon {
            clients: Vec::new(),
            labels: BTreeMap::new(),
            leader: None,
            pty_write_buf: Vec::new(),
            running: true,
            pid: 0,
            pty_fd: -1,
            cwd: Vec::new(),
            cwd_path: Vec::new(),
            has_pty_output: false,
            has_had_client: false,
            created_at: unix_seconds(),
            task_mode,
            task_id: *b"0000",
            task_exit_code: None,
            task_ended_at: None,
            term: Terminal::with_scrollback(80, 24, opts.max_scrollback_lines),
            opts,
        };
        if let Some(cwd) = d.opts.cwd.clone() {
            d.set_cwd(&cwd);
        }
        d
    }

    /// Stores the working directory from an OSC 7 value or a plain path.
    pub fn set_cwd(&mut self, value: &[u8]) {
        let host = osc7::hostname();
        let Some(cwd) = osc7::parse(value, &host) else {
            log::warn(&format!(
                "ignoring unusable cwd={}",
                String::from_utf8_lossy(value)
            ));
            return;
        };
        self.cwd = if value.first() == Some(&b'/') {
            osc7::to_uri(value, &host)
        } else {
            value.to_vec()
        };
        self.cwd_path = if cwd.is_local { cwd.path } else { Vec::new() };
        log::info(&format!(
            "set cwd={} path={}",
            String::from_utf8_lossy(&self.cwd),
            String::from_utf8_lossy(&self.cwd_path)
        ));
    }

    /// Picks up a directory change the program reported.
    pub fn set_pwd(&mut self) {
        let Some(pwd) = self.term.working_directory().map(|p| p.as_bytes().to_vec()) else {
            return;
        };
        if pwd != self.cwd {
            self.set_cwd(&pwd);
        }
    }

    /// Feeds program output to the screen. Replies the terminal would send
    /// are dropped: clients answer for themselves (or the daemon does, see
    /// `respond_to_device_attributes`).
    pub fn feed(&mut self, bytes: &[u8]) {
        self.term.feed(bytes);
        let _ = self.term.take_output();
        let _ = self.term.take_events();
        self.set_pwd();
        self.has_pty_output = true;
    }

    pub fn queue_pty_input(&mut self, data: &[u8]) {
        if data.is_empty() {
            return;
        }
        if self.pty_write_buf.len() + data.len() > PTY_WRITE_BUF_MAX {
            log::warn(&format!(
                "pty input dropped {} bytes (buffer full, shell not reading)",
                data.len()
            ));
            return;
        }
        self.pty_write_buf.extend_from_slice(data);
    }

    pub fn has_terminal_client(&self) -> bool {
        self.clients.iter().any(|c| c.is_terminal)
    }

    pub fn client_index(&self, fd: i32) -> Option<usize> {
        self.clients.iter().position(|c| c.fd() == fd)
    }

    /// Queues a frame for the client at `i`.
    pub fn send(&mut self, i: usize, tag: Tag, payload: &[u8]) {
        let c = &mut self.clients[i];
        if c.write_closed {
            return;
        }
        append_frame(&mut c.write_buf, tag, payload);
        c.has_pending_output = true;
    }

    pub fn broadcast(&mut self, tag: Tag, payload: &[u8]) {
        for c in self.clients.iter_mut().filter(|c| !c.write_closed) {
            append_frame(&mut c.write_buf, tag, payload);
            c.has_pending_output = true;
        }
    }

    /// Makes the client at `i` the leader and asks it for its size.
    pub fn set_leader(&mut self, i: usize) {
        let fd = self.clients[i].fd();
        log::info(&format!("setting new leader client_fd={fd}"));
        self.leader = Some(fd);
        self.send(i, Tag::RESIZE, b"");
    }

    pub fn is_leader(&self, i: usize) -> bool {
        self.leader == Some(self.clients[i].fd())
    }

    /// Drops the client at `i`. With `shutdown_on_last`, the last one leaving
    /// ends the session; returns whether it did.
    pub fn close_client(&mut self, i: usize, shutdown_on_last: bool) -> bool {
        let fd = self.clients[i].fd();
        if self.leader == Some(fd) {
            log::info(&format!(
                "unsetting leader session={} fd={fd}",
                self.opts.session
            ));
            self.leader = None;
        }
        self.clients.remove(i);
        log::info(&format!(
            "client disconnected fd={fd} remaining={}",
            self.clients.len()
        ));
        if shutdown_on_last && self.clients.is_empty() {
            self.shutdown();
            return true;
        }
        false
    }

    pub fn shutdown(&mut self) {
        log::info(&format!(
            "shutting down daemon session={}",
            self.opts.session
        ));
        self.running = false;
        self.clients.clear();
    }
}
