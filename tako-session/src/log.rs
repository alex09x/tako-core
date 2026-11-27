/*
 * tako-session — session runtime for Tako
 * Copyright (c) 2026 Alexander Panasenko
 *
 * Contact: alex@prod.codes
 * Author: https://prod.codes/about/
 * Project: https://github.com/alex09x/tako-core
 * SPDX-License-Identifier: MIT
 */

//! The runtime's log: one line per event, `[<unix seconds>] [<level>]
//! (default): <message>`, appended to `zmx.log` (clients) or `<session>.log`
//! (a daemon) in the log directory. Past 2 MiB the file starts over.

use std::fs::{File, OpenOptions};
use std::io::{Seek, SeekFrom, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

pub const MAX_SIZE: u64 = 2 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Level {
    Error,
    Warn,
    Info,
    Debug,
}

impl Level {
    fn text(self) -> &'static str {
        match self {
            Level::Error => "error",
            Level::Warn => "warning",
            Level::Info => "info",
            Level::Debug => "debug",
        }
    }
}

struct Sink {
    file: File,
    size: u64,
}

static SINK: Mutex<Option<Sink>> = Mutex::new(None);

/// Append mode: every client writes `zmx.log` at the same time, and each
/// line must land at the end, not over another process's line.
fn open(path: &Path, mode: u32) -> std::io::Result<File> {
    OpenOptions::new()
        .read(true)
        .append(true)
        .create(true)
        .mode(mode)
        .open(path)
}

/// Sends the log to `path`, created with `mode` if missing, appending at its
/// end. Replaces any earlier destination.
pub fn init(path: &Path, mode: u32) -> std::io::Result<()> {
    let mut file = open(path, mode)?;
    // At the true end, as seen now: a forked parent may still be writing.
    let size = file.seek(SeekFrom::End(0))?;
    *SINK.lock().unwrap_or_else(|e| e.into_inner()) = Some(Sink { file, size });
    Ok(())
}

/// Closes the log; later lines go to standard error.
pub fn deinit() {
    *SINK.lock().unwrap_or_else(|e| e.into_inner()) = None;
}

/// The descriptor of the open log, for a daemon that keeps it across a fork.
pub fn raw_fd() -> Option<i32> {
    use std::os::fd::AsRawFd;
    SINK.lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
        .map(|s| s.file.as_raw_fd())
}

/// One log line, in the on-disk format.
pub fn format_line(seconds: u64, level: Level, message: &str) -> String {
    format!("[{seconds}] [{}] (default): {message}\n", level.text())
}

pub fn log(level: Level, message: &str) {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let line = format_line(seconds, level, message);
    let mut guard = SINK.lock().unwrap_or_else(|e| e.into_inner());
    let Some(sink) = guard.as_mut() else {
        eprintln!("{}: {message}", level.text());
        return;
    };
    if sink.size >= MAX_SIZE {
        match sink.file.set_len(0) {
            Ok(()) => sink.size = 0,
            Err(err) => eprintln!("Log wipe failed: {err}"),
        }
    }
    sink.size += line.len() as u64;
    let _ = sink.file.write_all(line.as_bytes());
}

pub fn error(message: &str) {
    log(Level::Error, message);
}

pub fn warn(message: &str) {
    log(Level::Warn, message);
}

pub fn info(message: &str) {
    log(Level::Info, message);
}

pub fn debug(message: &str) {
    log(Level::Debug, message);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lines_have_the_on_disk_shape() {
        assert_eq!(
            format_line(1_700_000_000, Level::Warn, "stale socket"),
            "[1700000000] [warning] (default): stale socket\n"
        );
        assert_eq!(
            format_line(1, Level::Error, "x"),
            "[1] [error] (default): x\n"
        );
    }

    #[test]
    fn appends_then_starts_over_past_the_limit() {
        let path = std::env::temp_dir().join(format!("ts-log-{}.log", std::process::id()));
        std::fs::write(&path, b"old\n").unwrap();
        init(&path, 0o600).unwrap();
        info("first");
        let text = std::fs::read_to_string(&path).unwrap();
        // Other tests may log concurrently into the same global sink.
        assert!(
            text.starts_with("old\n") && text.contains("[info] (default): first\n"),
            "{text}"
        );

        SINK.lock().unwrap().as_mut().unwrap().size = MAX_SIZE;
        warn("after wipe");
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(
            !text.contains("old\n") && text.contains("[warning] (default): after wipe\n"),
            "{text}"
        );
        deinit();
        std::fs::remove_file(&path).unwrap();
    }
}
