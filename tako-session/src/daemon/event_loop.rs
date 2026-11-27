/*
 * tako-session — session runtime for Tako
 * Copyright (c) 2026 Alexander Panasenko
 *
 * Contact: alex@prod.codes
 * Author: https://prod.codes/about/
 * Project: https://github.com/alex09x/tako-core
 * SPDX-License-Identifier: MIT
 */

//! The daemon's poll loop: the listen socket, the PTY, the signal pipe and
//! every client, in that order.

use super::handlers::Flow;
use super::{Client, Daemon, PTY_WRITE_BUF_MAX};
use crate::ipc::{Tag, expected_length};
use crate::pty_text::{find_task_exit_marker, respond_to_device_attributes, rewrite_prompt_redraw};
use crate::{log, signal};
use std::io;
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixListener;

const ERR_EVENTS: i16 = libc::POLLHUP | libc::POLLERR | libc::POLLNVAL;

/// The largest frame a client may send (a file for `write` is the largest
/// request); a bigger one closes the connection.
pub const MAX_REQUEST_LEN: usize = 256 * 1024 * 1024;

/// PTY output up to this size is written to clients in the same round.
const EAGER_MAX: usize = 256;

fn pollfd(fd: i32, events: i16) -> libc::pollfd {
    libc::pollfd {
        fd,
        events,
        revents: 0,
    }
}

/// Writes all of `data` to a non-blocking `fd`, waiting at most
/// `timeout_ms` for it to drain.
pub fn write_all_with_timeout(fd: i32, mut data: &[u8], timeout_ms: i32) -> io::Result<()> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(timeout_ms as u64);
    while !data.is_empty() {
        // SAFETY: data is valid for its length.
        let n = unsafe { libc::write(fd, data.as_ptr().cast(), data.len()) };
        if n > 0 {
            data = &data[n as usize..];
            continue;
        }
        let err = io::Error::last_os_error();
        if n < 0
            && err.kind() != io::ErrorKind::WouldBlock
            && err.kind() != io::ErrorKind::Interrupted
        {
            return Err(err);
        }
        let left = deadline
            .saturating_duration_since(std::time::Instant::now())
            .as_millis() as i32;
        if left <= 0 {
            return Err(io::ErrorKind::TimedOut.into());
        }
        let mut p = [pollfd(fd, libc::POLLOUT)];
        // SAFETY: p is a valid one-element array.
        unsafe { libc::poll(p.as_mut_ptr(), 1, left) };
    }
    Ok(())
}

fn read_fd(fd: i32, buf: &mut [u8]) -> io::Result<usize> {
    // SAFETY: buf is valid for its length.
    let n = unsafe { libc::read(fd, buf.as_mut_ptr().cast(), buf.len()) };
    if n < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(n as usize)
    }
}

fn write_fd(fd: i32, buf: &[u8]) -> io::Result<usize> {
    // SAFETY: buf is valid for its length.
    let n = unsafe { libc::write(fd, buf.as_ptr().cast(), buf.len()) };
    if n < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(n as usize)
    }
}

impl Daemon {
    /// Program output: to the screen, the task tracker, and every client.
    fn on_pty_output(&mut self, data: &[u8], carry: &mut Vec<u8>) {
        self.feed(data);
        // Until a real terminal attaches, answer the queries a shell waits on.
        if !self.has_terminal_client() && self.pty_write_buf.len() < PTY_WRITE_BUF_MAX {
            respond_to_device_attributes(&mut self.pty_write_buf, data);
        }
        // The marker can straddle two reads; search with the previous tail.
        if self.task_mode && self.task_exit_code.is_none() {
            let mut scan = std::mem::take(carry);
            scan.extend_from_slice(data);
            if let Some(code) = find_task_exit_marker(&scan, &self.task_id) {
                self.complete_task(code);
            }
            let keep = scan.len().min(32);
            *carry = scan[scan.len() - keep..].to_vec();
        }
        let out = rewrite_prompt_redraw(data);
        self.broadcast(Tag::OUTPUT, out.as_deref().unwrap_or(data));
    }

    /// Runs until the program exits, a client kills the session, or SIGTERM.
    pub fn event_loop(&mut self, listener: &UnixListener) -> io::Result<()> {
        log::info(&format!(
            "daemon started session={} pty_fd={}",
            self.opts.session, self.pty_fd
        ));
        signal::open_pipe()?;
        signal::install_wake_handler(libc::SIGTERM);
        let size = crate::term::terminal_size(self.pty_fd);
        self.term.resize(size.cols as usize, size.rows as usize);
        let mut carry = Vec::new();
        // One read buffer for the daemon's life: a fresh zeroed array per
        // read cost a 64 KiB memset per keystroke.
        let mut scratch = vec![0u8; 65536];
        let mut had_terminal = self.has_terminal_client();
        let mut fds: Vec<libc::pollfd> = Vec::with_capacity(8);

        'daemon: while self.running {
            // A program that asked for focus reports hears when the last
            // attached client leaves and when one comes back.
            let has_terminal = self.has_terminal_client();
            if has_terminal != had_terminal && self.term.modes().focus_events {
                self.queue_pty_input(if has_terminal { b"\x1b[I" } else { b"\x1b[O" });
            }
            had_terminal = has_terminal;

            let mut pty_bytes = 0;
            fds.clear();
            fds.push(pollfd(listener.as_raw_fd(), libc::POLLIN));
            let pty_events = if self.pty_write_buf.is_empty() {
                libc::POLLIN
            } else {
                libc::POLLIN | libc::POLLOUT
            };
            fds.push(pollfd(self.pty_fd, pty_events));
            fds.push(pollfd(signal::pipe_read_fd(), libc::POLLIN));
            for c in &self.clients {
                let events = if c.has_pending_output {
                    libc::POLLIN | libc::POLLOUT
                } else {
                    libc::POLLIN
                };
                fds.push(pollfd(c.fd(), events));
            }
            // SAFETY: fds is a valid array of its length.
            if unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, -1) } < 0 {
                let err = io::Error::last_os_error();
                if err.kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(err);
            }

            if fds[2].revents & libc::POLLIN != 0 {
                signal::drain_pipe();
                log::info(&format!(
                    "SIGTERM received, shutting down gracefully session={}",
                    self.opts.session
                ));
                break;
            }
            if fds[0].revents & ERR_EVENTS != 0 {
                log::error(&format!("server socket error revents={}", fds[0].revents));
                break;
            } else if fds[0].revents & libc::POLLIN != 0 {
                match listener.accept() {
                    Ok((stream, _)) => {
                        stream.set_nonblocking(true)?;
                        let client = Client::new(stream);
                        let fd = client.fd();
                        self.clients.push(client);
                        log::info(&format!(
                            "client connected fd={fd} total={}",
                            self.clients.len()
                        ));
                    }
                    Err(e) if e.kind() == io::ErrorKind::WouldBlock => {}
                    Err(e) => return Err(e),
                }
            }

            if fds[1].revents & (libc::POLLIN | ERR_EVENTS) != 0 {
                // A PTY read returns at most 4096 bytes (the line discipline).
                let buf = &mut scratch[..4096];
                match read_fd(self.pty_fd, buf) {
                    Ok(0) => {
                        log::info(&format!("shell exited pty_fd={}", self.pty_fd));
                        // Finish this round so queued output still reaches clients.
                        self.running = false;
                    }
                    Ok(n) => {
                        pty_bytes = n;
                        self.on_pty_output(&buf[..n], &mut carry);
                    }
                    Err(e) if e.kind() == io::ErrorKind::WouldBlock => {}
                    Err(_) => {
                        log::info(&format!("shell exited pty_fd={}", self.pty_fd));
                        self.running = false;
                    }
                }
            }
            if fds[1].revents & libc::POLLOUT != 0 {
                while !self.pty_write_buf.is_empty() {
                    match write_fd(self.pty_fd, &self.pty_write_buf) {
                        Ok(0) => break,
                        Ok(n) => drop(self.pty_write_buf.drain(..n)),
                        Err(e) => {
                            if e.kind() != io::ErrorKind::WouldBlock {
                                log::warn(&format!("pty write failed: {e}"));
                                self.pty_write_buf.clear();
                            }
                            break;
                        }
                    }
                }
            }

            // Only the clients polled this round, newest first.
            let mut i = self.clients.len().min(fds.len() - 3);
            'clients: while i > 0 {
                i -= 1;
                let revents = fds[i + 3].revents;
                if revents & libc::POLLIN != 0 {
                    let buf = &mut scratch[..];
                    match read_fd(self.clients[i].fd(), buf) {
                        Err(e) if e.kind() == io::ErrorKind::WouldBlock => continue,
                        Ok(0) | Err(_) => {
                            self.close_client(i, false);
                            continue;
                        }
                        Ok(n) => self.clients[i].read_buf.push(&buf[..n]),
                    }
                    while let Some((tag, payload)) = self.clients[i]
                        .read_buf
                        .next_frame()
                        .map(|f| (f.header.tag, f.payload.to_vec()))
                    {
                        match self.dispatch(i, tag, &payload) {
                            Flow::Continue => {}
                            Flow::ClientsChanged => break 'clients,
                            Flow::Kill => break 'daemon,
                        }
                    }
                    // A frame announced larger than any request would only
                    // pile up unread: drop the client instead.
                    let pending = expected_length(self.clients[i].read_buf.unread());
                    if pending.is_some_and(|n| n > MAX_REQUEST_LEN) {
                        log::warn(&format!(
                            "dropping client fd={}: frame of {} bytes",
                            self.clients[i].fd(),
                            pending.unwrap_or(0)
                        ));
                        self.close_client(i, false);
                        continue;
                    }
                }
                if revents & libc::POLLOUT != 0 && self.flush_client(i) {
                    continue;
                }
                if revents & ERR_EVENTS != 0 {
                    self.close_client(i, false);
                }
            }
            // Write what this round queued now instead of after another
            // poll: a keystroke's echo reaches its client one wakeup sooner.
            // Only for small output (an echo, a prompt, a reply): while the
            // program streams, waiting a round batches reads into fewer
            // writes per client, which is faster with several clients.
            let mut i = if pty_bytes <= EAGER_MAX {
                self.clients.len()
            } else {
                0
            };
            while i > 0 {
                i -= 1;
                if self.clients[i].has_pending_output {
                    self.flush_client(i);
                }
            }
        }
        Ok(())
    }

    /// One non-blocking write of the client's queue; closes it on a write
    /// error and returns whether it did.
    fn flush_client(&mut self, i: usize) -> bool {
        let c = &mut self.clients[i];
        match write_fd(c.fd(), &c.write_buf) {
            Ok(n) if n > 0 => {
                c.has_flushed_bytes = true;
                c.write_buf.drain(..n);
            }
            Ok(_) => {}
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => {}
            // The client hung up, maybe right after a request (`attach`
            // inside a session sends Switch and exits): what it sent is
            // still to be read. Nothing more goes out; the read closes it.
            Err(e) if crate::socket::hung_up(&e) => {
                c.write_buf.clear();
                c.write_closed = true;
            }
            Err(_) => {
                self.close_client(i, false);
                return true;
            }
        }
        let c = &mut self.clients[i];
        if c.write_buf.is_empty() {
            c.has_pending_output = false;
            c.has_flushed_bytes = false;
        }
        false
    }
}
