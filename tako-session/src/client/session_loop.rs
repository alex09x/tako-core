/*
 * tako-session — session runtime for Tako
 * Copyright (c) 2026 Alexander Panasenko
 *
 * Contact: alex@prod.codes
 * Author: https://prod.codes/about/
 * Project: https://github.com/alex09x/tako-core
 * SPDX-License-Identifier: MIT
 */

//! The attached client: stdin to the daemon as `Input`, `Output` to stdout,
//! window size changes as `Resize`, until detach, hang-up or a switch.

use crate::ipc::{FrameBuffer, Tag, append_frame, append_size_frames};
use crate::{keys, log, signal, term};
use std::io;

/// How the loop ended.
#[derive(Debug, PartialEq, Eq)]
pub enum LoopEnd {
    Detach,
    /// The daemon asked this client to attach to another session instead.
    Switch {
        session: String,
        cwd: Option<String>,
    },
}

fn set_nonblocking(fd: i32) -> io::Result<libc::c_int> {
    // SAFETY: fcntl on a caller-owned descriptor.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: as above.
    unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) };
    Ok(flags)
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

fn switch(payload: &[u8]) -> LoopEnd {
    let text = String::from_utf8_lossy(payload);
    match text.split_once('\n') {
        None => LoopEnd::Switch {
            session: text.into_owned(),
            cwd: None,
        },
        Some((session, cwd)) => LoopEnd::Switch {
            session: session.to_string(),
            cwd: (!cwd.is_empty()).then(|| cwd.to_string()),
        },
    }
}

/// Handles the frames buffered so far; Some when one ends the loop.
fn drain_frames(
    read: &mut FrameBuffer,
    stdout: &mut Vec<u8>,
    sock_out: &mut Vec<u8>,
) -> Option<LoopEnd> {
    while let Some(frame) = read.next_frame() {
        match frame.header.tag {
            Tag::OUTPUT => stdout.extend_from_slice(frame.payload),
            // The daemon wants this client's size, usually as its new leader.
            Tag::RESIZE => append_size_frames(sock_out, Tag::RESIZE, term::terminal_size(1)),
            Tag::SWITCH => return Some(switch(frame.payload)),
            _ => {}
        }
    }
    None
}

fn gone(e: &io::Error) -> bool {
    matches!(
        e.kind(),
        io::ErrorKind::ConnectionReset | io::ErrorKind::BrokenPipe
    )
}

/// Runs the attached client on `sock` with the frames already received in
/// `unread`. `env` is the tracked environment to send first, if any.
pub fn run(sock: i32, env_str: &[u8], unread: &[u8], detach_disabled: bool) -> io::Result<LoopEnd> {
    log::info(&format!("client loop fd={sock}"));
    let sig = signal::open_pipe()?;
    signal::install_wake_handler(libc::SIGWINCH);
    set_nonblocking(sock)?;

    let mut sock_out = Vec::with_capacity(4096);
    if !env_str.is_empty() {
        append_frame(&mut sock_out, Tag::ENV_SET, env_str);
    }
    append_size_frames(&mut sock_out, Tag::INIT, term::terminal_size(1));
    let mut read = FrameBuffer::new();
    read.push(unread);
    let mut stdout = Vec::with_capacity(4096);

    let stdin_flags = set_nonblocking(0)?;
    let _restore = Restore(stdin_flags);

    loop {
        if let Some(end) = drain_frames(&mut read, &mut stdout, &mut sock_out) {
            return Ok(end);
        }
        let sock_events = libc::POLLIN
            | if sock_out.is_empty() {
                0
            } else {
                libc::POLLOUT
            };
        let mut fds = vec![
            libc::pollfd {
                fd: 0,
                events: libc::POLLIN,
                revents: 0,
            },
            libc::pollfd {
                fd: sock,
                events: sock_events,
                revents: 0,
            },
            libc::pollfd {
                fd: sig,
                events: libc::POLLIN,
                revents: 0,
            },
        ];
        if !stdout.is_empty() {
            fds.push(libc::pollfd {
                fd: 1,
                events: libc::POLLOUT,
                revents: 0,
            });
        }
        // SAFETY: fds is a valid array of pollfd for its length.
        if unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, -1) } < 0 {
            let err = io::Error::last_os_error();
            if err.kind() != io::ErrorKind::Interrupted {
                return Err(err);
            }
            continue;
        }
        if fds[2].revents & libc::POLLIN != 0 {
            signal::drain_pipe();
            append_size_frames(&mut sock_out, Tag::RESIZE, term::terminal_size(1));
        }
        if fds[0].revents & (libc::POLLIN | libc::POLLHUP | libc::POLLERR | libc::POLLNVAL) != 0 {
            let mut buf = [0u8; 4096];
            match read_fd(0, &mut buf) {
                Ok(0) => {
                    log::info("eof stdin");
                    return Ok(LoopEnd::Detach);
                }
                Ok(n) if !detach_disabled && keys::is_ctrl_backslash(&buf[..n]) => {
                    log::info("detach key detected");
                    append_frame(&mut sock_out, Tag::DETACH, b"");
                }
                Ok(n) => append_frame(&mut sock_out, Tag::INPUT, &buf[..n]),
                Err(e) if retry(&e) => {}
                Err(e) => return Err(e),
            }
        }
        if fds[1].revents & libc::POLLIN != 0 {
            let mut buf = [0u8; 4096];
            match read_fd(sock, &mut buf) {
                Ok(0) => {
                    log::info("server closed connection");
                    return Ok(LoopEnd::Detach);
                }
                Ok(n) => {
                    read.push(&buf[..n]);
                    if let Some(end) = drain_frames(&mut read, &mut stdout, &mut sock_out) {
                        return Ok(end);
                    }
                }
                Err(e) if retry(&e) => continue,
                Err(e) if gone(&e) => return Ok(LoopEnd::Detach),
                Err(e) => {
                    log::error(&format!("daemon read err={e}"));
                    return Err(e);
                }
            }
        }
        if fds[1].revents & libc::POLLOUT != 0 && !sock_out.is_empty() {
            match write_fd(sock, &sock_out) {
                Ok(n) => drop(sock_out.drain(..n)),
                Err(e) if retry(&e) => {}
                Err(e) if gone(&e) => {
                    log::info("connection reset or broken pipe");
                    return Ok(LoopEnd::Detach);
                }
                Err(e) => return Err(e),
            }
        }
        if !stdout.is_empty() {
            match write_fd(1, &stdout) {
                Ok(n) => drop(stdout.drain(..n)),
                Err(e) if retry(&e) => {}
                Err(e) => return Err(e),
            }
        }
        if fds[1].revents & (libc::POLLHUP | libc::POLLERR | libc::POLLNVAL) != 0 {
            log::info("poll hup|err|nval");
            return Ok(LoopEnd::Detach);
        }
    }
}

/// Nothing to do yet: try again on the next poll round.
fn retry(e: &io::Error) -> bool {
    matches!(
        e.kind(),
        io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
    )
}

/// Puts stdin's original flags back: O_NONBLOCK is shared with the parent shell.
struct Restore(libc::c_int);

impl Drop for Restore {
    fn drop(&mut self) {
        // SAFETY: restores flags read from the same descriptor.
        unsafe { libc::fcntl(0, libc::F_SETFL, self.0) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ipc::encode_frame;

    #[test]
    fn switch_payload_is_name_then_optional_cwd() {
        assert_eq!(
            switch(b"next"),
            LoopEnd::Switch {
                session: "next".into(),
                cwd: None
            }
        );
        assert_eq!(
            switch(b"next\n"),
            LoopEnd::Switch {
                session: "next".into(),
                cwd: None
            }
        );
        assert_eq!(
            switch(b"next\nfile://h/tmp"),
            LoopEnd::Switch {
                session: "next".into(),
                cwd: Some("file://h/tmp".into())
            }
        );
    }

    #[test]
    fn frames_route_output_answer_resize_and_stop_on_switch() {
        let mut read = FrameBuffer::new();
        let mut stream = encode_frame(Tag::OUTPUT, b"$ ");
        stream.extend(encode_frame(Tag::RESIZE, b""));
        stream.extend(encode_frame(Tag::LABEL_DATA, b"ignored"));
        stream.extend(encode_frame(Tag::SWITCH, b"other"));
        stream.extend(encode_frame(Tag::OUTPUT, b"after"));
        read.push(&stream);
        let (mut stdout, mut sock_out) = (Vec::new(), Vec::new());
        let end = drain_frames(&mut read, &mut stdout, &mut sock_out);
        assert_eq!(
            end,
            Some(LoopEnd::Switch {
                session: "other".into(),
                cwd: None
            })
        );
        assert_eq!(stdout, b"$ ");
        assert_eq!(
            sock_out.len(),
            2 * 8 + 8 + 4,
            "Resize answered in both forms"
        );
        assert_eq!(read.unread(), encode_frame(Tag::OUTPUT, b"after"));
    }
}
