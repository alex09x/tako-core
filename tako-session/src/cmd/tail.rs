/*
 * tako-session — session runtime for Tako
 * Copyright (c) 2026 Alexander Panasenko
 *
 * Contact: alex@prod.codes
 * Author: https://prod.codes/about/
 * Project: https://github.com/alex09x/tako-core
 * SPDX-License-Identifier: MIT
 */

//! `tail`: follow sessions' output as plain text. Also the reading half of
//! `run`, which ends with the task's exit code.

use super::attach::error_name;
use super::error::{CliError, CliResult, name_too_long};
use crate::ipc::{FrameBuffer, Tag};
use crate::socket::{self, SessionMatch};
use crate::{ansi, log};
use std::io::{self, Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;
use std::path::Path;

/// Reads `streams` until they close or a task completes. `detached`: stop
/// at the first `Ack`. `is_run`: drop the echoed command line.
pub fn follow(
    streams: &mut [UnixStream],
    detached: bool,
    is_run: bool,
    out: &mut dyn Write,
) -> io::Result<u8> {
    // One decoder per session: a frame split across reads must not take in
    // another session's bytes.
    let mut bufs: Vec<FrameBuffer> = streams.iter().map(|_| FrameBuffer::new()).collect();
    let mut first_line = true;
    let mut task_code: Option<u8> = None;
    loop {
        let mut fds: Vec<libc::pollfd> = streams
            .iter()
            .map(|s| libc::pollfd {
                fd: s.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            })
            .collect();
        // SAFETY: fds is a valid array of its length.
        if unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, -1) } < 0 {
            let err = io::Error::last_os_error();
            if err.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(err);
        }
        for (i, fd) in fds.iter().enumerate() {
            if fd.revents & libc::POLLIN == 0 {
                continue;
            }
            let mut chunk = [0u8; 65536];
            let n = match streams[i].read(&mut chunk) {
                Ok(n) => n,
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => continue,
                Err(e) if socket::hung_up(&e) => return Ok(1),
                Err(e) => {
                    log::error(&format!("daemon read err={e}"));
                    return Err(e);
                }
            };
            if n == 0 {
                return Ok(task_code.unwrap_or(0));
            }
            let buf = &mut bufs[i];
            buf.push(&chunk[..n]);
            while let Some(frame) = buf.next_frame() {
                match frame.header.tag {
                    Tag::ACK if detached => {
                        out.write_all(b"command sent!\n")?;
                        return Ok(0);
                    }
                    Tag::OUTPUT if !frame.payload.is_empty() => {
                        let mut payload = frame.payload;
                        if !detached && is_run && first_line {
                            first_line = false;
                            payload = match payload.iter().position(|&c| c == b'\n') {
                                Some(nl) => &payload[nl + 1..],
                                None => &[],
                            };
                        }
                        if !payload.is_empty() {
                            out.write_all(&ansi::strip(payload))?;
                        }
                    }
                    Tag::TASK_COMPLETE => {
                        task_code = Some(frame.payload.first().copied().unwrap_or(0))
                    }
                    _ => {}
                }
            }
        }
        out.flush()?;
        if let Some(code) = task_code {
            return Ok(code);
        }
        if fds
            .iter()
            .any(|f| f.revents & (libc::POLLHUP | libc::POLLERR | libc::POLLNVAL) != 0)
        {
            return Ok(0);
        }
    }
}

/// `tail <name>...`: prefixes (`name*`) match the sessions that exist now.
pub fn run(dir: &Path, matchers: &[SessionMatch], out: &mut dyn Write) -> CliResult {
    let mut names: Vec<String> = Vec::new();
    if matchers.iter().any(|m| m.is_prefix) {
        let sessions = super::list::session_entries(dir).map_err(|_| CliError::Silent)?;
        for s in sessions {
            if matchers.iter().any(|m| m.matches(&s.name)) {
                names.push(s.name);
            }
        }
    }
    names.extend(
        matchers
            .iter()
            .filter(|m| !m.is_prefix)
            .map(|m| m.name.clone()),
    );
    if names.is_empty() {
        return Err(CliError::msg("no matching sessions found"));
    }
    let mut streams = Vec::new();
    for name in &names {
        let path = socket::socket_path(dir, name).ok_or_else(|| name_too_long(name, dir))?;
        let stream = socket::connect(&path).map_err(|e| {
            CliError::msg(format!(
                "cannot connect to session \"{name}\": {}",
                error_name(&e)
            ))
        })?;
        streams.push(stream);
    }
    follow(&mut streams, false, false, out)
        .map(|_| ())
        .map_err(|_| CliError::Silent)
}

#[cfg(test)]
mod tests {
    use super::follow;
    use crate::ipc::{Tag, encode_frame};
    use std::io::Write;
    use std::os::unix::net::UnixStream;
    use std::time::Duration;

    #[test]
    fn a_frame_split_across_reads_keeps_to_its_own_session() {
        let (mut a_tx, a_rx) = UnixStream::pair().unwrap();
        let (mut b_tx, b_rx) = UnixStream::pair().unwrap();
        let a = encode_frame(Tag::OUTPUT, b"from-a ");
        let writer = std::thread::spawn(move || {
            let pause = || std::thread::sleep(Duration::from_millis(50));
            a_tx.write_all(&a[..5]).unwrap();
            pause();
            b_tx.write_all(&encode_frame(Tag::OUTPUT, b"from-b "))
                .unwrap();
            pause();
            a_tx.write_all(&a[5..]).unwrap();
            pause();
            a_tx.write_all(&encode_frame(Tag::TASK_COMPLETE, &[7]))
                .unwrap();
            pause();
        });
        let mut out = Vec::new();
        let code = follow(&mut [a_rx, b_rx], false, false, &mut out).unwrap();
        writer.join().unwrap();
        assert_eq!(
            (code, String::from_utf8(out).unwrap()),
            (7, "from-b from-a ".to_string())
        );
    }
}
