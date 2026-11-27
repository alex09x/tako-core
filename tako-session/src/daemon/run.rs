/*
 * tako-session — session runtime for Tako
 * Copyright (c) 2026 Alexander Panasenko
 *
 * Contact: alex@prod.codes
 * Author: https://prod.codes/about/
 * Project: https://github.com/alex09x/tako-core
 * SPDX-License-Identifier: MIT
 */

//! Starting a session's daemon, and ending the session when it stops.

use super::handlers::foreground_pgrp;
use super::spawn::{self, Forked};
use super::{Daemon, Options, labels};
use crate::{log, socket};
use std::io::{self, Write};
use std::os::fd::AsRawFd;
use std::path::Path;

/// Which process returned from starting a session.
#[derive(Debug, PartialEq, Eq)]
pub enum Started {
    /// The caller: the session exists (or was just made) and can be attached.
    Client,
    /// The daemon, after its session ended; it must exit now.
    Daemon,
}

/// Uses the running session, or starts one when there is none or only a
/// stale socket is left.
pub fn ensure_session(opts: Options, out: &mut dyn Write) -> io::Result<Started> {
    let name = opts.session.clone();
    log::info(&format!("ensure session session={name}"));
    let dir = opts
        .socket_path
        .parent()
        .unwrap_or(Path::new("/"))
        .to_path_buf();
    if socket::session_exists(&dir, &name)? {
        match socket::connect(&opts.socket_path) {
            Ok(_) => {
                if opts.command.is_some() {
                    log::warn(&format!(
                        "session already exists, ignoring command session={name}"
                    ));
                }
                return Ok(Started::Client);
            }
            Err(e) if e.kind() == io::ErrorKind::ConnectionRefused => {
                socket::cleanup_stale_socket(&dir, &name)
            }
            Err(e) => {
                log::warn(&format!(
                    "connect failed ({e}), proceeding to attach session={name}"
                ));
                return Ok(Started::Client);
            }
        }
    }
    run(opts, out)
}

/// Starts a new session; never attaches to or replaces an existing socket
/// (`AddrInUse` when one is there).
pub fn create_only(opts: Options, out: &mut dyn Write) -> io::Result<Started> {
    log::info(&format!("create-only session={}", opts.session));
    run(opts, out)
}

fn run(opts: Options, out: &mut dyn Write) -> io::Result<Started> {
    let name = opts.session.clone();
    log::info(&format!("creating session={name}"));
    let listener = socket::listen(&opts.socket_path)?;
    let cmd = spawn::create_cmd(&opts.shell, opts.task_mode, opts.command.as_deref());
    let mut daemon = Daemon::new(opts);
    // The program starts where the client is, when that is a local directory.
    if !daemon.cwd_path.is_empty() {
        let path = String::from_utf8_lossy(&daemon.cwd_path).into_owned();
        match std::env::set_current_dir(&path) {
            Ok(()) => log::info(&format!("set directory dir={path}")),
            Err(e) => log::warn(&format!("failed to open dir={path} err={e}")),
        }
    }
    let mut keep = vec![listener.as_raw_fd()];
    keep.extend(log::raw_fd());
    let pty = match spawn::daemonize(&name, &cmd, &keep)? {
        Forked::Client => {
            let _ = writeln!(out, "session \"{name}\" created");
            let _ = out.flush();
            // Closing the descriptor leaves the socket file to the daemon.
            drop(listener);
            return Ok(Started::Client);
        }
        Forked::Daemon(pty) => pty,
    };
    daemon.pid = pty.pid;
    daemon.pty_fd = pty.master_fd;
    log::deinit();
    let _ = log::init(
        &daemon.opts.log_dir.join(format!("{name}.log")),
        daemon.opts.log_mode,
    );
    if let Some(initial) = daemon.opts.initial_labels.clone() {
        labels::apply(&mut daemon.labels, &initial);
    }
    if let Err(e) = daemon.event_loop(&listener) {
        log::error(&format!("daemon loop failed: {e}"));
    }
    log::info("daemon loop shutdown");
    // New connections are refused at once, before the grace period below;
    // the path is not unlinked, since another daemon may own it by now.
    drop(listener);
    log::info(&format!(
        "closed listen socket without pathname unlink session={name}"
    ));
    daemon.kill_program();
    // SAFETY: the PTY master is ours; the pid is our child.
    unsafe {
        libc::close(pty.master_fd);
        libc::waitpid(pty.pid, std::ptr::null_mut(), 0);
    }
    Ok(Started::Daemon)
}

/// Signals `target` (a process, or a group when negative). 0, -1 and 1
/// would reach the caller's group, everything, or init: never sent.
fn signal(target: libc::pid_t, sig: libc::c_int) {
    if target.unsigned_abs() <= 1 {
        return;
    }
    // SAFETY: sending a signal has no memory-safety preconditions.
    unsafe { libc::kill(target, sig) };
}

impl Daemon {
    /// Ends the program and whatever runs in its foreground: SIGHUP (shells
    /// ignore SIGTERM), half a second, then SIGKILL.
    fn kill_program(&mut self) {
        log::info(&format!("kill received session={}", self.opts.session));
        self.shutdown();
        if self.pid <= 1 {
            return;
        }
        let pgrp = foreground_pgrp(self.pty_fd).filter(|&p| p > 1);
        if let Some(p) = pgrp {
            signal(-p, libc::SIGHUP);
            signal(p, libc::SIGHUP);
        }
        log::info(&format!(
            "sending SIGHUP session={} pid={}",
            self.opts.session, self.pid
        ));
        signal(-self.pid, libc::SIGHUP);
        signal(self.pid, libc::SIGHUP);
        std::thread::sleep(std::time::Duration::from_millis(500));
        if let Some(p) = pgrp {
            signal(-p, libc::SIGKILL);
            signal(p, libc::SIGKILL);
        }
        signal(-self.pid, libc::SIGKILL);
        signal(self.pid, libc::SIGKILL);
    }
}
