/*
 * tako-session — session runtime for Tako
 * Copyright (c) 2026 Alexander Panasenko
 *
 * Contact: alex@prod.codes
 * Author: https://prod.codes/about/
 * Project: https://github.com/alex09x/tako-core
 * SPDX-License-Identifier: MIT
 */

//! Signals reach poll loops through a self-pipe: a handler writes one byte,
//! and the loop wakes on the pipe's read end.

use std::sync::atomic::{AtomicI32, Ordering};

static PIPE_READ: AtomicI32 = AtomicI32::new(-1);
static PIPE_WRITE: AtomicI32 = AtomicI32::new(-1);

extern "C" fn wake(_: libc::c_int) {
    // SAFETY: write(2) and errno access are async-signal-safe.
    unsafe {
        let saved = *errno_ptr();
        let fd = PIPE_WRITE.load(Ordering::Relaxed);
        if fd >= 0 {
            libc::write(fd, b"x".as_ptr().cast(), 1);
        }
        *errno_ptr() = saved;
    }
}

#[cfg(target_os = "linux")]
unsafe fn errno_ptr() -> *mut libc::c_int {
    // SAFETY: returns this thread's errno location.
    unsafe { libc::__errno_location() }
}

#[cfg(not(target_os = "linux"))]
unsafe fn errno_ptr() -> *mut libc::c_int {
    // SAFETY: returns this thread's errno location.
    unsafe { libc::__error() }
}

/// Opens the self-pipe, non-blocking and close-on-exec; returns its read end.
pub fn open_pipe() -> std::io::Result<i32> {
    let mut fds = [0; 2];
    // SAFETY: fds is a valid two-element array.
    if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    for fd in fds {
        // SAFETY: fd was just returned by pipe().
        unsafe {
            libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC);
            let flags = libc::fcntl(fd, libc::F_GETFL);
            libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK);
        }
    }
    PIPE_READ.store(fds[0], Ordering::Relaxed);
    PIPE_WRITE.store(fds[1], Ordering::Relaxed);
    Ok(fds[0])
}

/// The self-pipe's read end, or -1 before `open_pipe`.
pub fn pipe_read_fd() -> i32 {
    PIPE_READ.load(Ordering::Relaxed)
}

/// Makes `sig` wake the self-pipe. Interrupted reads and writes restart
/// (`poll` still returns, and the pipe wakes it), so a resize never reads
/// as an I/O error.
pub fn install_wake_handler(sig: libc::c_int) {
    // SAFETY: a zeroed sigaction with a valid handler and empty mask.
    unsafe {
        let mut act: libc::sigaction = std::mem::zeroed();
        act.sa_sigaction = wake as extern "C" fn(libc::c_int) as usize;
        act.sa_flags = libc::SA_RESTART;
        libc::sigemptyset(&mut act.sa_mask);
        libc::sigaction(sig, &act, std::ptr::null_mut());
    }
}

/// A peer that vanishes between a probe and a write must surface as
/// `BrokenPipe`, not kill the process. Inherited by a forked daemon.
pub fn ignore_sigpipe() {
    // SAFETY: setting a disposition has no memory-safety preconditions.
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_IGN);
    }
}

/// Empties the self-pipe after a wake.
pub fn drain_pipe() {
    let fd = pipe_read_fd();
    let mut buf = [0u8; 16];
    // SAFETY: buf is valid for its length; fd is non-blocking.
    while unsafe { libc::read(fd, buf.as_mut_ptr().cast(), buf.len()) } > 0 {}
}
