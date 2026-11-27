/*
 * tako-session — session runtime for Tako
 * Copyright (c) 2026 Alexander Panasenko
 *
 * Contact: alex@prod.codes
 * Author: https://prod.codes/about/
 * Project: https://github.com/alex09x/tako-core
 * SPDX-License-Identifier: MIT
 */

//! Session names and their sockets: `<socket dir>/<name>`.

use crate::cfg::Env;
use std::io;
use std::os::unix::fs::FileTypeExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};

/// Usable bytes of a Unix socket path on this platform (the field less its NUL).
pub const MAX_SOCKET_PATH_LEN: usize = {
    // SAFETY: an all-zero sockaddr_un is a valid value of the type.
    let addr: libc::sockaddr_un = unsafe { std::mem::zeroed() };
    addr.sun_path.len() - 1
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NameError {
    /// Neither a name nor `ZMX_SESSION_PREFIX` was given.
    Required,
    /// The name holds `/` or NUL, or is `.` or `..`.
    Invalid,
}

/// `ZMX_SESSION_PREFIX`, or empty.
pub fn prefix(env: &dyn Env) -> String {
    env.get("ZMX_SESSION_PREFIX")
        .map(|v| v.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// `ZMX_SESSION`, the session a shell runs in, or empty.
pub fn session_from_env(env: &dyn Env) -> String {
    env.get("ZMX_SESSION")
        .map(|v| v.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// The full session name: the prefix, then `name`. Names become file names
/// in the socket directory, so they never reach outside it.
pub fn session_name(env: &dyn Env, name: &str) -> Result<String, NameError> {
    let prefix = prefix(env);
    if prefix.is_empty() && name.is_empty() {
        return Err(NameError::Required);
    }
    let full = format!("{prefix}{name}");
    if full.contains('/') || full.contains('\0') || full == "." || full == ".." {
        return Err(NameError::Invalid);
    }
    Ok(full)
}

/// A name argument where `.` means the current session (`ZMX_SESSION`), and
/// no argument falls back to it too.
pub fn resolve_session_or_env(env: &dyn Env, name: Option<&str>) -> Result<String, NameError> {
    let current = session_from_env(env);
    let raw = match name {
        Some(".") | None if !current.is_empty() => current,
        Some(".") | None => return Err(NameError::Required),
        Some(name) => name.to_string(),
    };
    session_name(env, &raw)
}

/// A session argument: an exact name, or a prefix when it ends in `*`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionMatch {
    pub name: String,
    pub is_prefix: bool,
}

impl SessionMatch {
    pub fn parse(env: &dyn Env, raw: &str) -> Result<SessionMatch, NameError> {
        if let Some(stem) = raw.strip_suffix('*') {
            let name = if stem.is_empty() && prefix(env).is_empty() {
                String::new()
            } else {
                session_name(env, stem)?
            };
            return Ok(SessionMatch {
                name,
                is_prefix: true,
            });
        }
        Ok(SessionMatch {
            name: session_name(env, raw)?,
            is_prefix: false,
        })
    }

    pub fn matches(&self, session: &str) -> bool {
        if self.is_prefix {
            session.starts_with(&self.name)
        } else {
            session == self.name
        }
    }
}

/// `<dir>/<name>`, or None when it does not fit in a socket address.
pub fn socket_path(dir: &Path, name: &str) -> Option<PathBuf> {
    let len = dir.as_os_str().len() + 1 + name.len();
    (len <= MAX_SOCKET_PATH_LEN).then(|| PathBuf::from(format!("{}/{name}", dir.display())))
}

/// The longest session name `dir` leaves room for, or None when `dir` alone
/// is too long.
pub fn max_session_name_len(dir: &Path) -> Option<usize> {
    let overhead = dir.as_os_str().len() + 1;
    (overhead < MAX_SOCKET_PATH_LEN).then(|| MAX_SOCKET_PATH_LEN - overhead)
}

/// The message for a name that does not fit.
pub fn name_too_long_message(name: &str, dir: &Path) -> String {
    match max_session_name_len(dir) {
        Some(max) => format!(
            "error: session name is too long ({} bytes, max {max} for socket directory \"{}\")",
            name.len(),
            dir.display()
        ),
        None => format!(
            "error: socket directory path is too long (\"{}\")",
            dir.display()
        ),
    }
}

/// Whether `<dir>/<name>` exists; an existing file that is not a socket is
/// an error, never a session.
pub fn session_exists(dir: &Path, name: &str) -> io::Result<bool> {
    match std::fs::metadata(dir.join(name)) {
        Ok(meta) if meta.file_type().is_socket() => Ok(true),
        Ok(_) => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "FileNotUnixSocket",
        )),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e),
    }
}

/// Removes a socket nobody listens on any more.
pub fn cleanup_stale_socket(dir: &Path, name: &str) {
    log::warn(&format!("stale socket found, cleaning up session={name}"));
    if let Err(err) = std::fs::remove_file(dir.join(name)) {
        log::warn(&format!("failed to delete stale socket err={err}"));
    }
}

/// Connects to a session (close-on-exec, blocking).
pub fn connect(path: &Path) -> io::Result<UnixStream> {
    UnixStream::connect(path)
}

/// Listens on a new session socket (close-on-exec, non-blocking, backlog 128).
pub fn listen(path: &Path) -> io::Result<UnixListener> {
    let listener = UnixListener::bind(path)?;
    listener.set_nonblocking(true)?;
    Ok(listener)
}

use crate::log;

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;

    fn env(pairs: &'static [(&'static str, &'static str)]) -> impl Fn(&str) -> Option<OsString> {
        move |k| {
            pairs
                .iter()
                .find(|(key, _)| *key == k)
                .map(|(_, v)| OsString::from(v))
        }
    }

    #[test]
    fn names_get_the_prefix_and_never_leave_the_directory() {
        let none = env(&[]);
        assert_eq!(session_name(&none, "a"), Ok("a".into()));
        assert_eq!(session_name(&none, ""), Err(NameError::Required));
        for bad in ["a/b", "..", ".", "a\0b"] {
            assert_eq!(session_name(&none, bad), Err(NameError::Invalid), "{bad:?}");
        }
        let pre = env(&[("ZMX_SESSION_PREFIX", "p-")]);
        assert_eq!(session_name(&pre, "a"), Ok("p-a".into()));
        assert_eq!(session_name(&pre, ""), Ok("p-".into()));
    }

    #[test]
    fn dot_and_absence_mean_the_current_session() {
        let inside = env(&[("ZMX_SESSION", "cur")]);
        assert_eq!(resolve_session_or_env(&inside, Some(".")), Ok("cur".into()));
        assert_eq!(resolve_session_or_env(&inside, None), Ok("cur".into()));
        assert_eq!(resolve_session_or_env(&inside, Some("x")), Ok("x".into()));
        let outside = env(&[]);
        assert_eq!(
            resolve_session_or_env(&outside, Some(".")),
            Err(NameError::Required)
        );
    }

    #[test]
    fn a_star_matches_a_prefix() {
        let e = env(&[]);
        let all = SessionMatch::parse(&e, "*").unwrap();
        assert!(all.is_prefix && all.name.is_empty() && all.matches("anything"));
        let some = SessionMatch::parse(&e, "dev*").unwrap();
        assert!(some.matches("dev-1") && !some.matches("prod"));
        let exact = SessionMatch::parse(&e, "dev").unwrap();
        assert!(exact.matches("dev") && !exact.matches("dev-1"));
    }

    #[test]
    fn socket_paths_respect_the_platform_limit() {
        assert_eq!(
            socket_path(Path::new("/tmp/zmx"), "s"),
            Some(PathBuf::from("/tmp/zmx/s"))
        );
        let dir = "d".repeat(MAX_SOCKET_PATH_LEN - 2);
        assert!(socket_path(Path::new(&dir), "x").is_some());
        assert!(socket_path(Path::new(&dir), "xx").is_none());
        assert_eq!(max_session_name_len(Path::new(&dir)), Some(1));
        assert_eq!(
            max_session_name_len(Path::new(&"f".repeat(MAX_SOCKET_PATH_LEN))),
            None
        );
        assert!(name_too_long_message("xx", Path::new(&dir)).contains("max 1"));
    }

    #[test]
    fn exists_refuses_a_file_that_is_not_a_socket() {
        let dir = std::env::temp_dir().join(format!("ts-sock-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        assert!(!session_exists(&dir, "none").unwrap());
        std::fs::write(dir.join("plain"), b"").unwrap();
        assert!(session_exists(&dir, "plain").is_err());
        let _listener = listen(&dir.join("live")).unwrap();
        assert!(session_exists(&dir, "live").unwrap());
        assert!(connect(&dir.join("live")).is_ok());
        cleanup_stale_socket(&dir, "live");
        assert!(!session_exists(&dir, "live").unwrap());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
