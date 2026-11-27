/*
 * tako-session — session runtime for Tako
 * Copyright (c) 2026 Alexander Panasenko
 *
 * Contact: alex@prod.codes
 * Author: https://prod.codes/about/
 * Project: https://github.com/alex09x/tako-core
 * SPDX-License-Identifier: MIT
 */

//! `wait <name>...`: poll until every matching session's task has ended,
//! then report, with the end of a failed task's history.

use super::error::{CliError, CliResult};
use super::list::session_entries;
use crate::daemon::unix_seconds;
use crate::socket::SessionMatch;
use std::io::Write;
use std::path::Path;

pub fn run(
    dir: &Path,
    matchers: &[SessionMatch],
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> CliResult {
    let io = |_| CliError::Silent;
    let mut max_seen = 0usize;
    let mut zero_iters = 0;
    let mut agg: u8;
    let mut last_print = 0u64;
    let mut prev_done = 0usize;
    loop {
        agg = 0;
        let (mut total, mut done) = (0usize, 0usize);
        for s in session_entries(dir).map_err(|_| CliError::Silent)? {
            if !matchers.iter().any(|m| m.matches(&s.name)) {
                continue;
            }
            total += 1;
            if let Some(e) = s.error {
                // An unreachable daemon would otherwise be waited on forever.
                writeln!(
                    err,
                    "[{}] task unreachable: {} ({})",
                    unix_seconds(),
                    s.name,
                    e.name()
                )
                .map_err(io)?;
                agg = 1;
                done += 1;
                continue;
            }
            let ended = s.task_ended_at.unwrap_or(0);
            if ended == 0 {
                let now = unix_seconds();
                if now.saturating_sub(last_print) >= 5 {
                    writeln!(out, "[{now}] waiting task={}", s.name).map_err(io)?;
                    out.flush().map_err(io)?;
                    last_print = now;
                }
                continue;
            }
            let code = s.task_exit_code.unwrap_or(0);
            if done >= prev_done {
                writeln!(out, "[{ended}] completed task={} exit_code={code}", s.name)
                    .map_err(io)?;
                out.flush().map_err(io)?;
            }
            if code != 0 {
                agg = code;
            }
            done += 1;
        }
        // Before the completion check: N-1 finished and one gone is no success.
        if total < max_seen {
            writeln!(
                err,
                "error: {} session(s) disappeared before completing",
                max_seen - total
            )
            .map_err(io)?;
            return Err(CliError::Exit(1));
        }
        max_seen = total;
        if total > 0 && total == done {
            break;
        }
        if max_seen == 0 {
            zero_iters += 1;
            if zero_iters >= 3 {
                writeln!(err, "error: no matching sessions found").map_err(io)?;
                return Err(CliError::Exit(2));
            }
        }
        prev_done = done;
        std::thread::sleep(std::time::Duration::from_secs(1));
    }
    writeln!(
        out,
        "{}",
        if agg == 0 {
            "task(s) completed!"
        } else {
            "task(s) failed!"
        }
    )
    .map_err(io)?;
    for s in session_entries(dir).map_err(|_| CliError::Silent)? {
        if !matchers.iter().any(|m| m.matches(&s.name)) || s.task_exit_code.unwrap_or(0) == 0 {
            continue;
        }
        let (ended, code) = (s.task_ended_at.unwrap_or(0), s.task_exit_code.unwrap_or(0));
        writeln!(
            out,
            "---\n[{ended}] failed task={} exit_status={code}",
            s.name
        )
        .map_err(io)?;
        if let Some(text) = super::history::fetch(dir, &s.name) {
            writeln!(out, "\nLast 20 lines of {} history:", s.name).map_err(io)?;
            let text = String::from_utf8_lossy(&text);
            let lines: Vec<&str> = text.split('\n').collect();
            for line in &lines[lines.len().saturating_sub(20)..] {
                writeln!(out, "{line}").map_err(io)?;
            }
        }
        writeln!(
            out,
            "\nSee the logs:\nzmx history {0}\nzmx attach {0}",
            s.name
        )
        .map_err(io)?;
    }
    Err(CliError::Exit(i32::from(agg)))
}
