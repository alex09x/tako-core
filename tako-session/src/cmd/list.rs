/*
 * tako-session — session runtime for Tako
 * Copyright (c) 2026 Alexander Panasenko
 *
 * Contact: alex@prod.codes
 * Author: https://prod.codes/about/
 * Project: https://github.com/alex09x/tako-core
 * SPDX-License-Identifier: MIT
 */

//! `list`: one line per live session, sorted by name.

use crate::client::{ProbeError, probe_session};
use crate::ipc::parse_labels;
use crate::socket::{session_exists, socket_path};
use std::io::{self, Write};
use std::path::Path;

/// What one session socket answered.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionEntry {
    pub name: String,
    pub pid: Option<i32>,
    pub clients_len: Option<u64>,
    /// Set when the probe failed for a reason other than a refused connection.
    pub error: Option<ProbeError>,
    pub cmd: Option<Vec<u8>>,
    pub cwd: Option<Vec<u8>>,
    pub labels: Option<Vec<u8>>,
    pub created_at: u64,
    pub task_ended_at: Option<u64>,
    pub task_exit_code: Option<u8>,
}

/// Probes every socket in `dir`. A refused connection has no live daemon:
/// it is left out, and its file left alone (a listing never deletes).
pub fn session_entries(dir: &Path) -> io::Result<Vec<SessionEntry>> {
    let mut sessions = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if !matches!(session_exists(dir, &name), Ok(true)) {
            continue;
        }
        let Some(path) = socket_path(dir, &name) else {
            continue;
        };
        match probe_session(&path) {
            Err(ProbeError::ConnectionRefused) => {}
            Err(err) => sessions.push(SessionEntry {
                name,
                pid: None,
                clients_len: None,
                error: Some(err),
                cmd: None,
                cwd: None,
                labels: Some(Vec::new()),
                created_at: 0,
                task_ended_at: Some(0),
                task_exit_code: Some(1),
            }),
            Ok(probe) => {
                let info = probe.info;
                sessions.push(SessionEntry {
                    name,
                    pid: Some(info.pid),
                    clients_len: Some(info.clients_len),
                    error: None,
                    cmd: (!info.cmd.is_empty()).then_some(info.cmd),
                    cwd: (!info.cwd.is_empty()).then_some(info.cwd),
                    labels: probe.labels,
                    created_at: info.created_at,
                    task_ended_at: Some(info.task_ended_at),
                    task_exit_code: Some(info.task_exit_code),
                });
            }
        }
    }
    sessions.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(sessions)
}

/// One line of `list`. `current` is the session the caller runs in: its line
/// starts with `→ `, every other line with two spaces; with None there is no
/// prefix. `short` prints bare names and skips errors.
/// Bytes from the daemon (cwd, cmd, labels) are written as they came.
pub fn session_line(session: &SessionEntry, short: bool, current: Option<&str>) -> Vec<u8> {
    let prefix = match current {
        Some(cur) if cur == session.name => "→ ",
        Some(_) => "  ",
        None => "",
    };
    if short {
        return if session.error.is_some() {
            Vec::new()
        } else {
            format!("{}\n", session.name).into_bytes()
        };
    }
    if let Some(err) = session.error {
        let status = if err == ProbeError::ConnectionRefused {
            "cleaning up"
        } else {
            "unreachable"
        };
        return format!(
            "{prefix}name={}\terr={}\tstatus={status}\n",
            session.name,
            err.name()
        )
        .into_bytes();
    }
    let mut line = format!(
        "{prefix}name={}\tpid={}\tclients={}\tcreated={}",
        session.name,
        session.pid.unwrap_or_default(),
        session.clients_len.unwrap_or_default(),
        session.created_at
    )
    .into_bytes();
    if let Some(cwd) = &session.cwd {
        line.extend_from_slice(b"\tcwd=");
        line.extend_from_slice(cwd);
    }
    if let Some(cmd) = &session.cmd {
        line.extend_from_slice(b"\tcmd=");
        line.extend_from_slice(cmd);
    }
    if let Some(ended) = session.task_ended_at.filter(|&e| e > 0) {
        line.extend_from_slice(format!("\tended={ended}").as_bytes());
        if let Some(code) = session.task_exit_code {
            line.extend_from_slice(format!("\texit_code={code}").as_bytes());
        }
    }
    if let Some(labels) = &session.labels {
        for (k, v) in parse_labels(&String::from_utf8_lossy(labels)) {
            line.extend_from_slice(format!("\t{k}={v}").as_bytes());
        }
    }
    line.push(b'\n');
    line
}

/// Runs `list`. The current session comes from `ZMX_SESSION`, empty when
/// unset, so outside a session every line starts with two spaces.
pub fn run(
    dir: &Path,
    short: bool,
    current: &str,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> io::Result<()> {
    let sessions = session_entries(dir)?;
    if sessions.is_empty() {
        if !short {
            writeln!(err, "no sessions found in {}", dir.display())?;
        }
        return Ok(());
    }
    for session in &sessions {
        out.write_all(&session_line(session, short, Some(current)))?;
        out.flush()?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dev() -> SessionEntry {
        SessionEntry {
            name: "dev".into(),
            pid: Some(123),
            clients_len: Some(2),
            error: None,
            cmd: None,
            cwd: None,
            labels: None,
            created_at: 0,
            task_ended_at: None,
            task_exit_code: None,
        }
    }

    fn session_line(s: &SessionEntry, short: bool, current: Option<&str>) -> String {
        String::from_utf8(super::session_line(s, short, current)).unwrap()
    }

    #[test]
    fn lines_match_the_reference_for_current_and_short() {
        let s = dev();
        assert_eq!(
            session_line(&s, false, Some("dev")),
            "→ name=dev\tpid=123\tclients=2\tcreated=0\n"
        );
        assert_eq!(
            session_line(&s, false, Some("other")),
            "  name=dev\tpid=123\tclients=2\tcreated=0\n"
        );
        assert_eq!(
            session_line(&s, false, Some("")),
            "  name=dev\tpid=123\tclients=2\tcreated=0\n"
        );
        assert_eq!(
            session_line(&s, false, None),
            "name=dev\tpid=123\tclients=2\tcreated=0\n"
        );
        for current in [Some("dev"), Some("other"), None] {
            assert_eq!(session_line(&s, true, current), "dev\n");
        }
    }

    #[test]
    fn full_lines_carry_cwd_cmd_task_and_labels_in_order() {
        let mut s = dev();
        s.cwd = Some(b"file://h/tmp".to_vec());
        s.cmd = Some(b"/bin/sh".to_vec());
        s.task_ended_at = Some(9);
        s.task_exit_code = Some(3);
        s.labels = Some(b"tako-attempt=a1 tako-gen=g1".to_vec());
        assert_eq!(
            session_line(&s, false, Some("")),
            "  name=dev\tpid=123\tclients=2\tcreated=0\tcwd=file://h/tmp\tcmd=/bin/sh\tended=9\texit_code=3\ttako-attempt=a1\ttako-gen=g1\n"
        );
        s.task_ended_at = Some(0);
        assert!(!session_line(&s, false, None).contains("ended="));
    }

    #[test]
    fn errors_print_their_status_and_short_skips_them() {
        let mut s = dev();
        s.error = Some(ProbeError::Timeout);
        assert_eq!(
            session_line(&s, false, Some("")),
            "  name=dev\terr=Timeout\tstatus=unreachable\n"
        );
        s.error = Some(ProbeError::ConnectionRefused);
        assert!(session_line(&s, false, None).ends_with("status=cleaning up\n"));
        assert_eq!(session_line(&s, true, None), "");
    }
}
