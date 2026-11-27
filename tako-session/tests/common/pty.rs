/*
 * tako-session — session runtime for Tako
 * Copyright (c) 2026 Alexander Panasenko
 *
 * Contact: alex@prod.codes
 * Author: https://prod.codes/about/
 * Project: https://github.com/alex09x/tako-core
 * SPDX-License-Identifier: MIT
 */

//! A client run on a pseudo-terminal, the way a terminal app runs it.

use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

pub struct PtyClient {
    pub master: Option<OwnedFd>,
    pub child: Child,
    pub screen: Vec<u8>,
}

fn winsize(rows: u16, cols: u16) -> libc::winsize {
    libc::winsize {
        ws_row: rows,
        ws_col: cols,
        ws_xpixel: 0,
        ws_ypixel: 0,
    }
}

impl PtyClient {
    /// Runs `cmd` with a new PTY of `rows` x `cols` as its controlling
    /// terminal and stdio.
    pub fn spawn(mut cmd: Command, rows: u16, cols: u16) -> PtyClient {
        let (mut master, mut slave) = (-1, -1);
        let mut ws = winsize(rows, cols);
        // SAFETY: valid out-pointers and winsize. macOS declares the window
        // size `*mut`, Linux `*const`.
        #[allow(clippy::unnecessary_mut_passed)]
        let rc = unsafe {
            libc::openpty(
                &mut master,
                &mut slave,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                &mut ws,
            )
        };
        assert_eq!(rc, 0, "openpty");
        // SAFETY: the descriptors were just returned by openpty.
        let (master, slave) =
            unsafe { (OwnedFd::from_raw_fd(master), OwnedFd::from_raw_fd(slave)) };
        // SAFETY: fcntl on our own descriptor.
        unsafe {
            let flags = libc::fcntl(master.as_raw_fd(), libc::F_GETFL);
            libc::fcntl(master.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK);
            libc::fcntl(master.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC);
        }
        let io = || Stdio::from(slave.try_clone().unwrap());
        cmd.stdin(io()).stdout(io()).stderr(io());
        // SAFETY: setsid and TIOCSCTTY are async-signal-safe.
        unsafe {
            cmd.pre_exec(|| {
                libc::setsid();
                libc::ioctl(0, libc::TIOCSCTTY as _, 0);
                Ok(())
            });
        }
        let child = cmd.spawn().unwrap();
        drop(slave);
        PtyClient {
            master: Some(master),
            child,
            screen: Vec::new(),
        }
    }

    /// The master side, or -1 once the terminal was closed.
    fn fd(&self) -> i32 {
        self.master.as_ref().map_or(-1, |m| m.as_raw_fd())
    }

    /// Closes the terminal, as an app that quits does.
    pub fn hang_up(&mut self) {
        self.master = None;
    }

    pub fn write(&self, bytes: &[u8]) {
        // SAFETY: bytes is valid for its length.
        let n = unsafe { libc::write(self.fd(), bytes.as_ptr().cast(), bytes.len()) };
        assert_eq!(n, bytes.len() as isize, "pty write");
    }

    /// Reads what is there without blocking.
    pub fn pump(&mut self) {
        let mut buf = [0u8; 65536];
        loop {
            // SAFETY: buf is valid for its length.
            let n = unsafe { libc::read(self.fd(), buf.as_mut_ptr().cast(), buf.len()) };
            if n <= 0 {
                return;
            }
            self.screen.extend_from_slice(&buf[..n as usize]);
        }
    }

    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.screen).into_owned()
    }

    pub fn wait_for(&mut self, needle: &str) {
        let start = Instant::now();
        loop {
            self.pump();
            if self.text().contains(needle) {
                return;
            }
            assert!(
                start.elapsed() < Duration::from_secs(10),
                "no {needle:?} in {:?}",
                self.text()
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    pub fn resize(&self, rows: u16, cols: u16) {
        let ws = winsize(rows, cols);
        // SAFETY: setting the size of our PTY; the kernel signals the group.
        unsafe { libc::ioctl(self.fd(), libc::TIOCSWINSZ, &ws) };
    }

    /// The exit status within `limit`, else None.
    pub fn exit_within(&mut self, limit: Duration) -> Option<i32> {
        let start = Instant::now();
        while start.elapsed() < limit {
            self.pump();
            if let Some(status) = self.child.try_wait().unwrap() {
                return Some(status.code().unwrap_or(-1));
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        None
    }
}

impl Drop for PtyClient {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
