/*
 * tako-session — session runtime for Tako
 * Copyright (c) 2026 Alexander Panasenko
 *
 * Contact: alex@prod.codes
 * Author: https://prod.codes/about/
 * Project: https://github.com/alex09x/tako-core
 * SPDX-License-Identifier: MIT
 */

//! Becoming a daemon and starting the session's program on a PTY.
//!
//! The client forks; the child leaves the client's session (`setsid`), drops
//! its stdio and inherited descriptors, and forks the program on a new PTY.
//! The program's process is not a session leader's session leader, so the
//! daemon never gains a controlling terminal.

use crate::ipc::Resize;
use crate::{log, term};
use std::ffi::CString;
use std::io;

/// What to execute: the file and its argv (argv[0] `-name` for a login shell).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cmd {
    pub file: String,
    pub argv: Vec<String>,
}

/// The session's program: `command` as given, else a login shell (`bash`
/// for a task session).
pub fn create_cmd(shell: &str, task_mode: bool, command: Option<&[String]>) -> Cmd {
    if let Some(args) = command.filter(|a| !a.is_empty()) {
        return Cmd {
            file: args[0].clone(),
            argv: args.to_vec(),
        };
    }
    let file = if task_mode { "bash" } else { shell }.to_string();
    let base = file.rsplit('/').next().unwrap_or(&file).to_string();
    Cmd {
        argv: vec![format!("-{base}")],
        file,
    }
}

/// The daemon's end of the PTY and the program's pid.
#[derive(Clone, Copy, Debug)]
pub struct PtyInfo {
    pub master_fd: i32,
    pub pid: i32,
}

/// Fork outcome for the calling process.
pub enum Forked {
    /// This is still the client; the daemon runs in the child.
    Client,
    /// This is the daemon, with its program running.
    Daemon(PtyInfo),
}

fn child_exec(session: &str, cmd: &Cmd) -> ! {
    // SAFETY: only async-signal-safe calls and exec in the forked child.
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
        let name = CString::new("ZMX_SESSION").unwrap();
        let value = CString::new(session).unwrap_or_default();
        libc::setenv(name.as_ptr(), value.as_ptr(), 1);
        let term_key = CString::new("TERM").unwrap();
        let term = libc::getenv(term_key.as_ptr());
        let dumb = !term.is_null() && std::ffi::CStr::from_ptr(term).to_bytes() == b"dumb";
        if term.is_null() || dumb {
            let v = CString::new("xterm-256color").unwrap();
            libc::setenv(term_key.as_ptr(), v.as_ptr(), 1);
        }
        let file = CString::new(cmd.file.as_str()).unwrap_or_default();
        let args: Vec<CString> = cmd
            .argv
            .iter()
            .map(|a| CString::new(a.as_str()).unwrap_or_default())
            .collect();
        let mut argv: Vec<*const libc::c_char> = args.iter().map(|a| a.as_ptr()).collect();
        argv.push(std::ptr::null());
        libc::execvp(file.as_ptr(), argv.as_ptr());
        log::error(&format!(
            "execvpe failed: cmd={} err={}",
            cmd.file,
            io::Error::last_os_error()
        ));
        libc::_exit(1);
    }
}

/// Starts `cmd` on a new PTY of `size`; the master is non-blocking.
pub fn spawn_pty(session: &str, cmd: &Cmd, size: Resize) -> io::Result<PtyInfo> {
    let mut ws = libc::winsize {
        ws_row: size.rows,
        ws_col: size.cols,
        ws_xpixel: size.xpixel,
        ws_ypixel: size.ypixel,
    };
    let mut master: libc::c_int = -1;
    // SAFETY: master and ws are valid for forkpty to write and read.
    // macOS declares the window size `*mut`, Linux `*const`.
    #[allow(clippy::unnecessary_mut_passed)]
    let pid = unsafe {
        libc::forkpty(
            &mut master,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &mut ws,
        )
    };
    if pid < 0 {
        return Err(io::Error::last_os_error());
    }
    if pid == 0 {
        child_exec(session, cmd);
    }
    log::info(&format!("pty spawned session={session} pid={pid}"));
    // SAFETY: master is the PTY descriptor forkpty returned.
    unsafe {
        let flags = libc::fcntl(master, libc::F_GETFL);
        libc::fcntl(master, libc::F_SETFL, flags | libc::O_NONBLOCK);
    }
    Ok(PtyInfo {
        master_fd: master,
        pid,
    })
}

/// Forks the daemon. In the client this returns `Client` after a short
/// pause; in the daemon it returns the running program. `keep` lists the
/// descriptors the daemon needs (its listen socket, its log).
pub fn daemonize(session: &str, cmd: &Cmd, keep: &[i32]) -> io::Result<Forked> {
    // SAFETY: fork; the child continues single-threaded below.
    let pid = unsafe { libc::fork() };
    if pid < 0 {
        return Err(io::Error::last_os_error());
    }
    if pid > 0 {
        std::thread::sleep(std::time::Duration::from_millis(10));
        return Ok(Forked::Client);
    }
    // SAFETY: setsid in the new child cannot fail as a group leader would.
    unsafe { libc::setsid() };
    let size = term::terminal_size(1);
    let devnull = CString::new("/dev/null").unwrap();
    // SAFETY: plain descriptor operations in the child.
    unsafe {
        let fd = libc::open(devnull.as_ptr(), libc::O_RDWR);
        if fd < 0 {
            let err = io::Error::last_os_error();
            log::warn(&format!("failed to open /dev/null: {err}"));
            return Err(err);
        }
        for target in 0..3 {
            libc::dup2(fd, target);
        }
        if fd > 2 && !keep.contains(&fd) {
            libc::close(fd);
        }
        for fd in 3..64 {
            if !keep.contains(&fd) {
                libc::close(fd);
            }
        }
    }
    spawn_pty(session, cmd, size).map(Forked::Daemon)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_program_is_a_login_shell() {
        let cmd = create_cmd("/usr/local/bin/fish", false, None);
        assert_eq!(
            cmd,
            Cmd {
                file: "/usr/local/bin/fish".into(),
                argv: vec!["-fish".into()]
            }
        );
        assert_eq!(
            create_cmd("/bin/zsh", true, None),
            Cmd {
                file: "bash".into(),
                argv: vec!["-bash".into()]
            }
        );
        let args = vec!["vim".to_string(), "x".to_string()];
        assert_eq!(
            create_cmd("/bin/sh", false, Some(&args)),
            Cmd {
                file: "vim".into(),
                argv: args.clone()
            }
        );
    }
}
