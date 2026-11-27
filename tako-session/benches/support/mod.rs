/*
 * tako-session — session runtime for Tako
 * Copyright (c) 2026 Alexander Panasenko
 *
 * Contact: alex@prod.codes
 * Author: https://prod.codes/about/
 * Project: https://github.com/alex09x/tako-core
 * SPDX-License-Identifier: MIT
 */

//! Sessions of one runtime binary in a private directory, a protocol client
//! for them, and what a daemon costs (CPU time, resident memory).

use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};
use tako_session::ipc::{FrameBuffer, Info, Resize, Tag, encode_frame};

/// A runtime under test: this build, or the old one.
pub struct Runtime {
    pub name: &'static str,
    pub bin: PathBuf,
}

/// This build (or `TAKO_SESSION_BIN`, for a bench binary copied to another
/// machine), then the old runtime when `TAKO_TEST_NEW_ZMX` names it.
pub fn runtimes() -> Vec<Runtime> {
    let ours = std::env::var_os("TAKO_SESSION_BIN")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_BIN_EXE_tako-session")));
    let mut all = vec![Runtime {
        name: "rust",
        bin: ours,
    }];
    if let Some(bin) = std::env::var_os("TAKO_TEST_NEW_ZMX")
        .map(PathBuf::from)
        .filter(|p| p.exists())
    {
        all.push(Runtime { name: "zig", bin });
    }
    all
}

static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// A short private session directory; every session in it ends on drop.
pub struct Sessions {
    pub dir: PathBuf,
    bin: PathBuf,
}

impl Sessions {
    pub fn new(rt: &Runtime) -> Sessions {
        let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = PathBuf::from(format!("/tmp/tsb-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Sessions {
            dir,
            bin: rt.bin.clone(),
        }
    }

    /// Starts `name` running `program`, detached, and waits until it answers.
    pub fn start(&self, name: &str, program: &[&str]) {
        let mut args = vec!["attach", "--create-only", name];
        args.extend_from_slice(program);
        let status = Command::new(&self.bin)
            .args(&args)
            .env_clear()
            .env("ZMX_DIR", &self.dir)
            .env("HOME", &self.dir)
            .env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin")
            .env("SHELL", "/bin/sh")
            .env("TERM", "xterm-256color")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .unwrap();
        assert!(status.success(), "start {name}");
        let start = Instant::now();
        while UnixStream::connect(self.dir.join(name)).is_err() {
            assert!(
                start.elapsed() < Duration::from_secs(5),
                "{name} never answered"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    pub fn connect(&self, name: &str) -> Peer {
        Peer::connect(&self.dir.join(name))
    }

    /// The daemon's pid: the parent of the program it runs.
    pub fn daemon_pid(&self, name: &str) -> i32 {
        let mut p = self.connect(name);
        p.send(Tag::INFO, b"");
        let info = Info::decode(&p.expect(Tag::INFO)).unwrap();
        let out = Command::new("ps")
            .args(["-o", "ppid=", "-p", &info.pid.to_string()])
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stdout).trim().parse().unwrap()
    }
}

impl Drop for Sessions {
    fn drop(&mut self) {
        if let Ok(entries) = std::fs::read_dir(&self.dir) {
            for e in entries.flatten() {
                if let Ok(mut s) = UnixStream::connect(e.path()) {
                    let _ = s.write_all(&encode_frame(Tag::KILL, b""));
                }
            }
        }
        std::thread::sleep(Duration::from_millis(100));
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// One protocol connection.
pub struct Peer {
    pub stream: UnixStream,
    buf: FrameBuffer,
}

impl Peer {
    pub fn connect(path: &Path) -> Peer {
        let stream = UnixStream::connect(path).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(30)))
            .unwrap();
        Peer {
            stream,
            buf: FrameBuffer::new(),
        }
    }

    pub fn send(&mut self, tag: Tag, payload: &[u8]) {
        self.stream.write_all(&encode_frame(tag, payload)).unwrap();
    }

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

    /// The next whole frame.
    pub fn next(&mut self) -> (Tag, Vec<u8>) {
        loop {
            if let Some(f) = self.buf.next_frame() {
                return (f.header.tag, f.payload.to_vec());
            }
            let mut chunk = [0u8; 65536];
            let n = self.stream.read(&mut chunk).expect("daemon went quiet");
            assert!(n > 0, "daemon closed the connection");
            self.buf.push(&chunk[..n]);
        }
    }

    /// The payload of the next `tag` frame, skipping others.
    pub fn expect(&mut self, tag: Tag) -> Vec<u8> {
        loop {
            let (t, p) = self.next();
            if t == tag {
                return p;
            }
        }
    }

    /// Reads `Output` until `n` payload bytes have arrived.
    pub fn read_output(&mut self, n: usize) {
        let mut got = 0;
        let mut chunk = vec![0u8; 1 << 20];
        while got < n {
            while let Some(f) = self.buf.next_frame() {
                if f.header.tag == Tag::OUTPUT {
                    got += f.payload.len();
                }
            }
            if got >= n {
                return;
            }
            let k = self.stream.read(&mut chunk).expect("daemon went quiet");
            assert!(k > 0, "daemon closed the connection");
            self.buf.push(&chunk[..k]);
        }
    }
}

/// CPU time (ms) and resident memory (KiB) of `pid` so far.
pub fn process_cost(pid: i32) -> (f64, u64) {
    let out = Command::new("ps")
        .args(["-o", "rss=", "-p", &pid.to_string()])
        .output()
        .unwrap();
    let rss = String::from_utf8_lossy(&out.stdout)
        .trim()
        .parse()
        .unwrap_or(0);
    (cpu_ms(pid), rss)
}

#[cfg(target_os = "linux")]
fn cpu_ms(pid: i32) -> f64 {
    // utime and stime, fields 14 and 15, in clock ticks.
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).unwrap_or_default();
    let after = stat.rsplit_once(')').map(|(_, r)| r).unwrap_or("");
    let fields: Vec<&str> = after.split_whitespace().collect();
    let ticks: f64 = [11, 12]
        .iter()
        .filter_map(|&i| fields.get(i)?.parse::<f64>().ok())
        .sum();
    // SAFETY: sysconf has no preconditions.
    let hz = unsafe { libc::sysconf(libc::_SC_CLK_TCK) } as f64;
    ticks * 1000.0 / hz
}

#[cfg(not(target_os = "linux"))]
fn cpu_ms(pid: i32) -> f64 {
    // `[[hh:]mm:]ss.cc` from ps.
    let out = Command::new("ps")
        .args(["-o", "time=", "-p", &pid.to_string()])
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    text.trim().split(':').fold(0.0, |acc, part| {
        acc * 60.0 + part.parse::<f64>().unwrap_or(0.0)
    }) * 1000.0
}
