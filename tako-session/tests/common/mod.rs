/*
 * tako-session — session runtime for Tako
 * Copyright (c) 2026 Alexander Panasenko
 *
 * Contact: alex@prod.codes
 * Author: https://prod.codes/about/
 * Project: https://github.com/alex09x/tako-core
 * SPDX-License-Identifier: MIT
 */

//! A private session directory, the binary run against it, and a raw
//! protocol client for talking to its daemons.

#![allow(dead_code)]

pub mod pty;

use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};
use tako_session::ipc::{FrameBuffer, Resize, Tag, encode_frame};

static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// The binary under test, or `TAKO_SESSION_BIN` (e.g. another build).
pub fn binary() -> PathBuf {
    std::env::var_os("TAKO_SESSION_BIN")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_BIN_EXE_tako-session")))
}

/// A short private directory (socket paths are limited) whose sessions are
/// killed and which is removed on drop.
pub struct Dir {
    pub path: PathBuf,
    pub bin: PathBuf,
}

impl Dir {
    pub fn new(tag: &str) -> Dir {
        Dir::with_binary(tag, binary())
    }

    pub fn with_binary(tag: &str, bin: PathBuf) -> Dir {
        let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let path = PathBuf::from(format!("/tmp/ts-{tag}-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).unwrap();
        Dir { path, bin }
    }

    pub fn socket(&self, name: &str) -> PathBuf {
        self.path.join(name)
    }

    /// The command with a clean environment: no user shell config, history
    /// or session of the caller.
    pub fn command(&self, bin: &Path, args: &[&str]) -> Command {
        let mut c = Command::new(bin);
        c.args(args)
            .env_clear()
            .env("ZMX_DIR", &self.path)
            .env("HOME", &self.path)
            .env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin")
            .env("SHELL", "/bin/sh")
            .env("TERM", "xterm-256color")
            .env("HISTFILE", "/dev/null")
            .env("ENV", "/dev/null")
            .current_dir(&self.path)
            .stdin(Stdio::null());
        c
    }

    pub fn run(&self, args: &[&str]) -> Output {
        self.command(&self.bin, args).output().unwrap()
    }

    /// Starts a session with its default shell and leaves it detached.
    pub fn create(&self, name: &str) {
        let out = self.run(&["attach", "--create-only", name]);
        assert!(out.status.success(), "create {name}: {out:?}");
        assert_eq!(
            String::from_utf8_lossy(&out.stdout).lines().next(),
            Some(format!("session \"{name}\" created").as_str())
        );
        wait_until(Duration::from_secs(5), || {
            UnixStream::connect(self.socket(name)).is_ok()
        });
    }
}

impl Drop for Dir {
    fn drop(&mut self) {
        if let Ok(entries) = std::fs::read_dir(&self.path) {
            for e in entries.flatten() {
                if let Ok(mut s) = UnixStream::connect(e.path()) {
                    let _ = s.write_all(&encode_frame(Tag::KILL, b""));
                }
            }
        }
        std::thread::sleep(Duration::from_millis(50));
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

pub fn wait_until(limit: Duration, mut ok: impl FnMut() -> bool) {
    let start = Instant::now();
    while !ok() {
        assert!(
            start.elapsed() < limit,
            "condition not met within {limit:?}"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// A protocol client on one session socket.
pub struct Peer {
    pub stream: UnixStream,
    buf: FrameBuffer,
    /// Every `Output` payload seen so far.
    pub output: Vec<u8>,
}

impl Peer {
    pub fn connect(path: &Path) -> Peer {
        let stream = UnixStream::connect(path).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_millis(100)))
            .unwrap();
        Peer {
            stream,
            buf: FrameBuffer::new(),
            output: Vec::new(),
        }
    }

    pub fn send(&mut self, tag: Tag, payload: &[u8]) {
        self.stream.write_all(&encode_frame(tag, payload)).unwrap();
    }

    /// Attaches as a terminal of `rows` x `cols`.
    pub fn init(&mut self, rows: u16, cols: u16) {
        self.send(
            Tag::INIT,
            &Resize {
                rows,
                cols,
                xpixel: 0,
                ypixel: 0,
            }
            .encode(),
        );
    }

    /// Next frame other than `Output` (kept in `output`), within `limit`.
    pub fn next(&mut self, limit: Duration) -> Option<(Tag, Vec<u8>)> {
        let start = Instant::now();
        loop {
            while let Some(f) = self.buf.next_frame() {
                if f.header.tag == Tag::OUTPUT {
                    self.output.extend_from_slice(f.payload);
                    continue;
                }
                return Some((f.header.tag, f.payload.to_vec()));
            }
            if start.elapsed() > limit {
                return None;
            }
            let mut b = [0u8; 65536];
            match self.stream.read(&mut b) {
                Ok(0) => return None,
                Ok(n) => self.buf.push(&b[..n]),
                Err(e)
                    if matches!(
                        e.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                    ) => {}
                Err(_) => return None,
            }
        }
    }

    /// The payload of the next `tag` frame.
    pub fn expect(&mut self, tag: Tag) -> Vec<u8> {
        let start = Instant::now();
        while start.elapsed() < Duration::from_secs(5) {
            if let Some((t, p)) = self.next(Duration::from_millis(200))
                && t == tag
            {
                return p;
            }
        }
        panic!("no {:?} frame", tag.name());
    }

    /// Reads until the output so far contains `text`.
    pub fn wait_output(&mut self, text: &str) {
        let start = Instant::now();
        while !String::from_utf8_lossy(&self.output).contains(text) {
            assert!(
                start.elapsed() < Duration::from_secs(10),
                "no {text:?} in {:?}",
                String::from_utf8_lossy(&self.output)
            );
            self.next(Duration::from_millis(100));
        }
    }
}

/// Whether the process is gone (or a zombie nobody waits on).
pub fn process_gone(pid: i32) -> bool {
    // SAFETY: signal 0 only checks for existence.
    unsafe { libc::kill(pid, 0) != 0 }
}
