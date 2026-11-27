/*
 * tako-session — session runtime for Tako
 * Copyright (c) 2026 Alexander Panasenko
 *
 * Contact: alex@prod.codes
 * Author: https://prod.codes/about/
 * Project: https://github.com/alex09x/tako-core
 * SPDX-License-Identifier: MIT
 */

//! Where sessions live and how their files are created, from the environment.

use std::ffi::OsString;
use std::io;
use std::os::unix::fs::DirBuilderExt;
use std::path::{Path, PathBuf};

/// Environment variables forwarded to a session by default (`ZMX_TRACK_ENV`
/// replaces the list).
pub const DEFAULT_TRACKED_ENVS: &str = "DISPLAY,SSH_AUTH_SOCK,SSH_AGENT_PID,SSH_CONNECTION,WINDOWID,XAUTHORITY,KITTY_LISTEN_ON,KITTY_PID,KITTY_WINDOW_ID";
pub const DEFAULT_DIR_MODE: u32 = 0o750;
pub const DEFAULT_LOG_MODE: u32 = 0o640;
pub const MAX_SCROLLBACK_LINES: usize = 10_000;

/// The runtime's configuration.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cfg {
    pub socket_dir: PathBuf,
    pub log_dir: PathBuf,
    pub dir_mode: u32,
    pub log_mode: u32,
    pub tracked_envs: String,
}

/// An environment lookup, so tests need not touch the process environment.
pub trait Env {
    fn get(&self, key: &str) -> Option<OsString>;
}

/// The process environment.
pub struct ProcessEnv;

impl Env for ProcessEnv {
    fn get(&self, key: &str) -> Option<OsString> {
        std::env::var_os(key)
    }
}

impl<F: Fn(&str) -> Option<OsString>> Env for F {
    fn get(&self, key: &str) -> Option<OsString> {
        self(key)
    }
}

fn tmpdir(env: &dyn Env) -> String {
    let raw = env.get("TMPDIR").map(|v| v.to_string_lossy().into_owned());
    let raw = raw.unwrap_or_else(|| "/tmp".to_string());
    raw.trim_end_matches('/').to_string()
}

fn uid() -> u32 {
    // SAFETY: getuid has no preconditions and cannot fail.
    unsafe { libc::getuid() }
}

/// `$ZMX_DIR`, else `$XDG_RUNTIME_DIR/zmx`, else `${TMPDIR:-/tmp}/zmx-<uid>`.
pub fn socket_dir(env: &dyn Env) -> PathBuf {
    if let Some(dir) = env.get("ZMX_DIR") {
        return PathBuf::from(dir);
    }
    if let Some(xdg) = env.get("XDG_RUNTIME_DIR") {
        return Path::new(&xdg).join("zmx");
    }
    PathBuf::from(format!("{}/zmx-{}", tmpdir(env), uid()))
}

/// `$ZMX_DIR/logs`, else `$XDG_STATE_HOME/zmx/logs`, else
/// `$HOME/.local/state/zmx/logs`, else `${TMPDIR:-/tmp}/zmx-<uid>`.
pub fn log_dir(env: &dyn Env) -> PathBuf {
    if let Some(dir) = env.get("ZMX_DIR") {
        return Path::new(&dir).join("logs");
    }
    if let Some(state) = env.get("XDG_STATE_HOME") {
        return Path::new(&state).join("zmx/logs");
    }
    if let Some(home) = env.get("HOME") {
        return Path::new(&home).join(".local/state/zmx/logs");
    }
    PathBuf::from(format!("{}/zmx-{}", tmpdir(env), uid()))
}

/// An octal mode from `key`, or `default` when unset or not octal.
fn mode(env: &dyn Env, key: &str, default: u32) -> u32 {
    env.get(key)
        .and_then(|v| u32::from_str_radix(&v.to_string_lossy(), 8).ok())
        .unwrap_or(default)
}

impl Cfg {
    /// Reads the configuration without touching the file system.
    pub fn from_env(env: &dyn Env) -> Cfg {
        Cfg {
            socket_dir: socket_dir(env),
            log_dir: log_dir(env),
            dir_mode: mode(env, "ZMX_DIR_MODE", DEFAULT_DIR_MODE),
            log_mode: mode(env, "ZMX_LOG_MODE", DEFAULT_LOG_MODE),
            tracked_envs: DEFAULT_TRACKED_ENVS.to_string(),
        }
    }

    /// Reads the configuration and creates both directories (and their
    /// missing parents) with `dir_mode`. Existing directories keep their mode.
    pub fn init(env: &dyn Env) -> io::Result<Cfg> {
        let cfg = Cfg::from_env(env);
        cfg.mkdir()?;
        Ok(cfg)
    }

    pub fn mkdir(&self) -> io::Result<()> {
        for dir in [&self.socket_dir, &self.log_dir] {
            std::fs::DirBuilder::new()
                .recursive(true)
                .mode(self.dir_mode)
                .create(dir)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn env(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<OsString> + use<> {
        let map: HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        move |k| map.get(k).map(OsString::from)
    }

    #[test]
    fn zmx_dir_wins_for_sockets_and_logs() {
        let e = env(&[("ZMX_DIR", "/s"), ("XDG_RUNTIME_DIR", "/x"), ("HOME", "/h")]);
        let cfg = Cfg::from_env(&e);
        assert_eq!(cfg.socket_dir, PathBuf::from("/s"));
        assert_eq!(cfg.log_dir, PathBuf::from("/s/logs"));
    }

    #[test]
    fn xdg_dirs_come_next() {
        let e = env(&[
            ("XDG_RUNTIME_DIR", "/run/u"),
            ("XDG_STATE_HOME", "/st"),
            ("HOME", "/h"),
        ]);
        let cfg = Cfg::from_env(&e);
        assert_eq!(cfg.socket_dir, PathBuf::from("/run/u/zmx"));
        assert_eq!(cfg.log_dir, PathBuf::from("/st/zmx/logs"));
    }

    #[test]
    fn fallbacks_use_tmpdir_without_its_trailing_slash_and_home() {
        let e = env(&[("TMPDIR", "/var/t//"), ("HOME", "/h")]);
        let cfg = Cfg::from_env(&e);
        assert_eq!(
            cfg.socket_dir,
            PathBuf::from(format!("/var/t/zmx-{}", uid()))
        );
        assert_eq!(cfg.log_dir, PathBuf::from("/h/.local/state/zmx/logs"));
        let bare = Cfg::from_env(&env(&[]));
        assert_eq!(
            bare.socket_dir,
            PathBuf::from(format!("/tmp/zmx-{}", uid()))
        );
        assert_eq!(bare.log_dir, bare.socket_dir);
    }

    #[test]
    fn modes_are_octal_with_defaults() {
        let cfg = Cfg::from_env(&env(&[("ZMX_DIR_MODE", "700"), ("ZMX_LOG_MODE", "600")]));
        assert_eq!((cfg.dir_mode, cfg.log_mode), (0o700, 0o600));
        let bad = Cfg::from_env(&env(&[("ZMX_DIR_MODE", "9z"), ("ZMX_LOG_MODE", "")]));
        assert_eq!(
            (bad.dir_mode, bad.log_mode),
            (DEFAULT_DIR_MODE, DEFAULT_LOG_MODE)
        );
    }

    #[test]
    fn init_creates_missing_directories_with_the_mode() {
        use std::os::unix::fs::PermissionsExt;
        let root = std::env::temp_dir().join(format!("ts-cfg-{}", std::process::id()));
        let zdir = root.join("a/b");
        let e = env(&[("ZMX_DIR", zdir.to_str().unwrap()), ("ZMX_DIR_MODE", "700")]);
        let cfg = Cfg::init(&e).unwrap();
        let mode = std::fs::metadata(&cfg.socket_dir)
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o700);
        assert!(cfg.log_dir.is_dir());
        std::fs::remove_dir_all(&root).unwrap();
    }
}
