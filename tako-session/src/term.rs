/*
 * tako-session — session runtime for Tako
 * Copyright (c) 2026 Alexander Panasenko
 *
 * Contact: alex@prod.codes
 * Author: https://prod.codes/about/
 * Project: https://github.com/alex09x/tako-core
 * SPDX-License-Identifier: MIT
 */

//! The attaching client's terminal: its size, and raw mode while attached.

use crate::ipc::Resize;

/// Clears the screen before a session repaints it.
pub const CLEAR: &[u8] = b"\x1b[2J\x1b[H";
/// Written on detach: full reset, then the default foreground, background
/// and cursor colours back (OSC 110/111/112).
pub const RESTORE: &[u8] = b"\x1bc\x1b]110\x1b\\\x1b]111\x1b\\\x1b]112\x1b\\";

fn winsize(fd: i32) -> Option<Resize> {
    // SAFETY: a zeroed winsize is valid; TIOCGWINSZ fills it.
    let mut ws: libc::winsize = unsafe { std::mem::zeroed() };
    // SAFETY: ws is a valid winsize for the ioctl to write.
    let ok = unsafe { libc::ioctl(fd, libc::TIOCGWINSZ, &mut ws) } == 0;
    (ok && ws.ws_row > 0 && ws.ws_col > 0).then_some(Resize {
        rows: ws.ws_row,
        cols: ws.ws_col,
        xpixel: ws.ws_xpixel,
        ypixel: ws.ws_ypixel,
    })
}

/// The size of the terminal on `fd`, else on stdout, stdin, stderr (other
/// than `fd`), else `/dev/tty`, else 24x120.
pub fn terminal_size(fd: i32) -> Resize {
    if let Some(size) = winsize(fd) {
        return size;
    }
    for fallback in [1, 0, 2] {
        if fallback != fd
            && let Some(size) = winsize(fallback)
        {
            return size;
        }
    }
    if let Ok(tty) = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/tty")
    {
        use std::os::fd::AsRawFd;
        if let Some(size) = winsize(tty.as_raw_fd()) {
            return size;
        }
    }
    Resize {
        rows: 24,
        cols: 120,
        xpixel: 0,
        ypixel: 0,
    }
}

/// Raw mode on stdin while it lives; the original settings come back
/// (discarding unread input) when it drops. Not a TTY: nothing changes.
pub struct RawMode {
    original: Option<libc::termios>,
}

impl RawMode {
    pub fn enter() -> RawMode {
        // SAFETY: a zeroed termios is valid for tcgetattr to fill.
        let mut original: libc::termios = unsafe { std::mem::zeroed() };
        // SAFETY: original is a valid termios.
        if unsafe { libc::tcgetattr(0, &mut original) } != 0 {
            return RawMode { original: None };
        }
        let mut raw = original;
        // SAFETY: raw is a valid termios.
        unsafe { libc::cfmakeraw(&mut raw) };
        raw.c_cc[libc::VLNEXT] = libc::_POSIX_VDISABLE;
        raw.c_cc[libc::VQUIT] = libc::_POSIX_VDISABLE; // ctrl+\ is the detach key
        raw.c_cc[libc::VMIN] = 1;
        raw.c_cc[libc::VTIME] = 0;
        // SAFETY: raw is a valid termios.
        unsafe { libc::tcsetattr(0, libc::TCSANOW, &raw) };
        RawMode {
            original: Some(original),
        }
    }
}

impl Drop for RawMode {
    fn drop(&mut self) {
        if let Some(original) = &self.original {
            // SAFETY: original came from tcgetattr.
            unsafe { libc::tcsetattr(0, libc::TCSAFLUSH, original) };
        }
        write_stdout(RESTORE);
    }
}

/// Writes all of `bytes` to stdout, ignoring errors (a detached terminal).
pub fn write_stdout(bytes: &[u8]) {
    let mut rest = bytes;
    while !rest.is_empty() {
        // SAFETY: rest is valid for its length.
        let n = unsafe { libc::write(1, rest.as_ptr().cast(), rest.len()) };
        if n <= 0 {
            return;
        }
        rest = &rest[n as usize..];
    }
}
