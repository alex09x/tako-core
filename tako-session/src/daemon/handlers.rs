/*
 * tako-session — session runtime for Tako
 * Copyright (c) 2026 Alexander Panasenko
 *
 * Contact: alex@prod.codes
 * Author: https://prod.codes/about/
 * Project: https://github.com/alex09x/tako-core
 * SPDX-License-Identifier: MIT
 */

//! What the daemon does with each request a client sends.

use super::{Daemon, inject, labels, unix_seconds};
use crate::ipc::{
    GuardReply, Info, MAX_CMD_LEN, Resize, Tag, labels_to_payload, validate_label_pairs,
};
use crate::pty_text::{rewrite_prompt_redraw, shell_needs_quoting, shell_quote};
use crate::{input, log};
use tako_core::terminal::HistoryFormat;

/// What the event loop does after a request.
#[derive(Debug, PartialEq, Eq)]
pub enum Flow {
    Continue,
    /// The client list changed; stop walking this poll's clients.
    ClientsChanged,
    /// End the session.
    Kill,
}

/// The longest destination path `write` takes (PATH_MAX).
pub const MAX_WRITE_PATH: usize = 4096;

/// The most cells a client may ask the screen to hold (e.g. 2048 x 2048).
pub const MAX_SCREEN_CELLS: usize = 1 << 22;

/// A size frame's size, unless it is malformed, empty or too large to
/// allocate a screen for.
pub fn usable_size(payload: &[u8]) -> Option<Resize> {
    let size = Resize::decode(payload).filter(|_| payload.len() == 8)?;
    let cells = usize::from(size.rows) * usize::from(size.cols);
    (size.rows > 0 && size.cols > 0 && cells <= MAX_SCREEN_CELLS).then_some(size)
}

fn set_winsize(pty_fd: i32, r: Resize) {
    let ws = libc::winsize {
        ws_row: r.rows,
        ws_col: r.cols,
        ws_xpixel: r.xpixel,
        ws_ypixel: r.ypixel,
    };
    // SAFETY: ws is a valid winsize for the PTY descriptor.
    unsafe { libc::ioctl(pty_fd, libc::TIOCSWINSZ, &ws) };
}

/// The PTY's foreground process group, when it has one.
pub fn foreground_pgrp(pty_fd: i32) -> Option<libc::pid_t> {
    let mut pgrp: libc::pid_t = 0;
    // SAFETY: pgrp is valid for TIOCGPGRP to write.
    let ok = pty_fd >= 0 && unsafe { libc::ioctl(pty_fd, libc::TIOCGPGRP, &mut pgrp) } == 0;
    (ok && pgrp > 0).then_some(pgrp)
}

impl Daemon {
    pub fn dispatch(&mut self, i: usize, tag: Tag, payload: &[u8]) -> Flow {
        match tag {
            Tag::INPUT => self.handle_input(i, payload),
            Tag::SEND => self.queue_pty_input(payload),
            Tag::OUTPUT => self.handle_output(payload),
            Tag::INIT => self.handle_init(i, payload),
            Tag::SWITCH => self.handle_switch(payload),
            Tag::RESIZE => self.handle_resize(i, payload),
            Tag::DETACH => {
                log::info(&format!(
                    "client detach session={} fd={}",
                    self.opts.session,
                    self.clients[i].fd()
                ));
                self.close_client(i, false);
                return Flow::ClientsChanged;
            }
            Tag::DETACH_ALL => {
                log::info(&format!("detach all clients={}", self.clients.len()));
                while !self.clients.is_empty() {
                    self.close_client(self.clients.len() - 1, false);
                }
                return Flow::ClientsChanged;
            }
            Tag::KILL => return Flow::Kill,
            Tag::KILL_IF_LABEL => return self.handle_kill_if_label(i, payload),
            Tag::ATTACH_IF_LABEL => self.handle_attach_if_label(i, payload),
            Tag::INFO => self.handle_info(i),
            Tag::LABEL_GET => {
                let out = labels_to_payload(&self.labels);
                self.send(i, Tag::LABEL_DATA, out.as_bytes());
            }
            Tag::LABEL_SET => {
                let text = String::from_utf8_lossy(payload).into_owned();
                log::info(&format!("handle label set payload={text}"));
                labels::apply(&mut self.labels, &text);
                self.send(i, Tag::ACK, b"");
            }
            Tag::LABEL_CLEAR => {
                self.labels.clear();
                self.send(i, Tag::ACK, b"");
            }
            Tag::ENV_GET => {
                let env = self
                    .leader
                    .and_then(|fd| self.client_index(fd))
                    .and_then(|l| self.clients[l].env_str.clone());
                self.send(i, Tag::ENV_DATA, env.as_deref().unwrap_or(b""));
            }
            Tag::ENV_SET => {
                // Only the size: tracked values can be secrets.
                log::info(&format!("handle env set bytes={}", payload.len()));
                self.clients[i].set_env(payload);
                self.send(i, Tag::ACK, b"");
            }
            Tag::HISTORY => {
                self.set_pwd();
                let format = match payload.first() {
                    Some(1) => HistoryFormat::Vt,
                    Some(2) => HistoryFormat::Html,
                    _ => HistoryFormat::Plain,
                };
                let out = self.term.history(format);
                self.send(i, Tag::HISTORY, &out);
            }
            Tag::RUN => self.handle_run(i, payload),
            Tag::WRITE => self.handle_write(i, payload),
            Tag::ACK | Tag::TASK_COMPLETE | Tag::LABEL_DATA | Tag::ENV_DATA | Tag::GUARD_STATUS => {
            }
            other => log::warn(&format!("ignoring unknown IPC tag={}", other.0)),
        }
        Flow::Continue
    }

    fn handle_input(&mut self, i: usize, payload: &[u8]) {
        if self.is_leader(i) {
            self.queue_pty_input(payload);
        } else if input::is_user_input(payload) {
            self.set_leader(i);
            self.queue_pty_input(payload);
        }
    }

    /// Output a client hands back (a restored screen): shown to everyone,
    /// and the program is told to redraw over it.
    fn handle_output(&mut self, payload: &[u8]) {
        self.feed(payload);
        self.broadcast(Tag::OUTPUT, payload);
        if !self.clients.is_empty() {
            // SAFETY: signalling the session's own child.
            unsafe { libc::kill(self.pid, libc::SIGWINCH) };
        }
    }

    /// Tells the leader to switch to another session, with this session's
    /// directory when it is local.
    fn handle_switch(&mut self, session: &[u8]) {
        let Some(l) = self.leader.and_then(|fd| self.client_index(fd)) else {
            log::warn("switch: no leader found");
            return;
        };
        let mut payload = session.to_vec();
        if !self.cwd.is_empty() && !self.cwd_path.is_empty() {
            payload.push(b'\n');
            payload.extend_from_slice(&self.cwd);
        }
        self.send(l, Tag::SWITCH, &payload);
    }

    fn handle_init(&mut self, i: usize, payload: &[u8]) {
        let Some(size) = usable_size(payload) else {
            return;
        };
        self.clients[i].is_terminal = true;
        if self.leader.is_none() {
            self.set_leader(i);
        }
        let leader = self.is_leader(i);
        // The screen takes the leader's size before it is serialized, so the
        // snapshot arrives laid out for the width it is shown at.
        if leader {
            self.term.resize(size.cols as usize, size.rows as usize);
        }
        let reattach = self.has_pty_output && self.has_had_client;
        if reattach {
            let state = self.term.replay_state();
            let data = rewrite_prompt_redraw(&state).unwrap_or(state);
            self.send(i, Tag::OUTPUT, &data);
        }
        if leader {
            set_winsize(self.pty_fd, size);
            // An unchanged size sends no SIGWINCH; repainting programs need one.
            if reattach && let Some(pgrp) = foreground_pgrp(self.pty_fd) {
                // SAFETY: signalling the session's foreground group.
                unsafe { libc::kill(-pgrp, libc::SIGWINCH) };
            }
            self.has_had_client = true;
        }
    }

    fn handle_resize(&mut self, i: usize, payload: &[u8]) {
        let Some(size) = usable_size(payload) else {
            return;
        };
        if self.leader.is_none() {
            self.set_leader(i);
        }
        if !self.is_leader(i) {
            return;
        }
        set_winsize(self.pty_fd, size);
        self.term.resize(size.cols as usize, size.rows as usize);
    }

    fn guard_reply(&mut self, i: usize, reply: GuardReply) {
        self.send(i, Tag::GUARD_STATUS, &[reply.byte()]);
    }

    /// Ends the session only when its labels match. The reply must reach
    /// the client before the daemon goes away, so it is written here.
    fn handle_kill_if_label(&mut self, i: usize, payload: &[u8]) -> Flow {
        let required = String::from_utf8_lossy(payload);
        if validate_label_pairs(&required, true, true).is_err() {
            self.guard_reply(i, GuardReply::Invalid);
            return Flow::Continue;
        }
        if !labels::matches(&self.labels, &required) {
            self.guard_reply(i, GuardReply::Mismatch);
            return Flow::Continue;
        }
        let c = &mut self.clients[i];
        if !c.has_flushed_bytes {
            c.write_buf.clear();
            c.has_pending_output = false;
        } else if c.write_buf.len() > 64 * 1024 {
            log::info(&format!(
                "conditional kill refused due to pre-existing write_buf={} session={}",
                c.write_buf.len(),
                self.opts.session
            ));
            return Flow::Continue;
        }
        log::info(&format!(
            "conditional kill matched session={}",
            self.opts.session
        ));
        crate::ipc::append_frame(
            &mut c.write_buf,
            Tag::GUARD_STATUS,
            &[GuardReply::Matched.byte()],
        );
        let flushed = super::event_loop::write_all_with_timeout(c.fd(), &c.write_buf, 200);
        c.write_buf.clear();
        c.has_pending_output = false;
        c.has_flushed_bytes = false;
        if let Err(err) = flushed {
            log::info(&format!(
                "conditional kill failed to flush GuardStatus session={} err={err}",
                self.opts.session
            ));
            return Flow::Continue;
        }
        Flow::Kill
    }

    /// Attaches only when the labels match, then applies the new ones.
    fn handle_attach_if_label(&mut self, i: usize, payload: &[u8]) {
        let text = String::from_utf8_lossy(payload).into_owned();
        let (required, set) = text.split_once('\n').unwrap_or((&text, ""));
        if validate_label_pairs(required, true, true).is_err()
            || validate_label_pairs(set, false, false).is_err()
        {
            return self.guard_reply(i, GuardReply::Invalid);
        }
        if !labels::matches(&self.labels, required) {
            return self.guard_reply(i, GuardReply::Mismatch);
        }
        if !set.is_empty() {
            labels::apply(&mut self.labels, set);
        }
        self.guard_reply(i, GuardReply::Matched);
    }

    fn handle_info(&mut self, i: usize) {
        self.set_pwd();
        let mut info = Info {
            clients_len: (self.clients.len() - 1) as u64,
            pid: self.pid,
            created_at: self.created_at,
            task_ended_at: self.task_ended_at.unwrap_or(0),
            task_exit_code: self.task_exit_code.unwrap_or(0),
            cwd: self.cwd.clone(),
            ..Info::default()
        };
        if let Some(args) = &self.opts.command {
            info.cmd = command_line(args);
        }
        self.send(i, Tag::INFO, &info.encode());
    }

    fn handle_run(&mut self, i: usize, payload: &[u8]) {
        self.task_exit_code = None;
        self.task_ended_at = None;
        self.task_mode = true;
        self.task_id = inject::generate_task_id();
        if payload.is_empty() {
            return;
        }
        let input = inject::run_input(payload, &self.task_id);
        let cmd_len = payload.len() - usize::from(payload.ends_with(b"\r"));
        self.queue_pty_input(&input[..cmd_len]);
        self.queue_pty_input(&input[cmd_len..]);
        self.send(i, Tag::ACK, b"");
        self.has_had_client = true;
    }

    /// Payload: u32 path length (native order), the path, the content.
    fn handle_write(&mut self, i: usize, payload: &[u8]) {
        let Some(len) = payload
            .get(..4)
            .map(|b| u32::from_ne_bytes([b[0], b[1], b[2], b[3]]) as usize)
        else {
            return log::warn("write: invalid payload");
        };
        // The path is repeated for every chunk of the file: a path longer
        // than any file system allows would only multiply memory.
        let Some(path) = payload.get(4..4 + len).filter(|_| len <= MAX_WRITE_PATH) else {
            return log::warn("write: invalid payload");
        };
        let input = inject::write_input(path, &payload[4 + len..]);
        self.queue_pty_input(&input);
        self.send(i, Tag::ACK, b"");
        self.has_had_client = true;
    }

    /// A finished task: remember its exit code and tell every client.
    pub fn complete_task(&mut self, code: u8) {
        self.task_exit_code = Some(code);
        self.task_ended_at = Some(unix_seconds());
        log::info(&format!("task completed exit_code={code}"));
        self.broadcast(Tag::TASK_COMPLETE, &[code]);
    }
}

/// The arguments joined, quoted where a shell needs it, cut with `...`
/// where the record has no room.
pub fn command_line(args: &[String]) -> Vec<u8> {
    let mut out: Vec<u8> = Vec::new();
    for (n, arg) in args.iter().enumerate() {
        let src = if shell_needs_quoting(arg) {
            shell_quote(arg)
        } else {
            arg.clone()
        };
        let need = src.len() + usize::from(n > 0);
        if out.len() + need > MAX_CMD_LEN {
            if out.len() + 3 <= MAX_CMD_LEN {
                out.extend_from_slice(b"...");
            }
            break;
        }
        if n > 0 {
            out.push(b' ');
        }
        out.extend_from_slice(src.as_bytes());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::{command_line, usable_size};
    use crate::ipc::Resize;

    #[test]
    fn sizes_must_be_non_empty_and_allocatable() {
        let size = |rows, cols| {
            Resize {
                rows,
                cols,
                xpixel: 0,
                ypixel: 0,
            }
            .encode()
        };
        assert_eq!(
            usable_size(&size(24, 80)).map(|s| (s.rows, s.cols)),
            Some((24, 80))
        );
        assert_eq!(usable_size(&size(2048, 2048)).map(|s| s.cols), Some(2048));
        assert!(usable_size(&size(0, 80)).is_none());
        assert!(usable_size(&size(24, 0)).is_none());
        assert!(usable_size(&size(u16::MAX, u16::MAX)).is_none());
        assert!(
            usable_size(&size(24, 80)[..4]).is_none(),
            "legacy 4-byte form"
        );
    }

    #[test]
    fn command_lines_quote_and_cut() {
        let args = ["echo".to_string(), "a b".to_string()];
        assert_eq!(command_line(&args), b"echo 'a b'");
        let long = vec!["x".repeat(200), "y".repeat(100)];
        let out = command_line(&long);
        assert_eq!(out.len(), 203);
        assert!(out.ends_with(b"..."));
    }
}
