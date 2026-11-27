/*
 * tako-session — session runtime for Tako
 * Copyright (c) 2026 Alexander Panasenko
 *
 * Contact: alex@prod.codes
 * Author: https://prod.codes/about/
 * Project: https://github.com/alex09x/tako-core
 * SPDX-License-Identifier: MIT
 */

//! `run <name> [-d] [command...]`: type a command into a session (started
//! as a task session when missing), print its output and exit with its code.

use super::attach::error_name;
use super::error::{CliError, CliResult};
use super::send::read_stdin;
use crate::daemon::{Options, Started, ensure_session};
use crate::ipc::{Tag, encode_frame};
use crate::pty_text::{shell_needs_quoting, shell_quote};
use crate::{log, socket, term};
use std::io::Write;

/// The command line to type: the words (quoted where needed) and CR, which
/// a shell at its prompt accepts; or standard input with its last newline
/// made a CR.
pub fn command_text(words: &[String], stdin: Vec<u8>) -> Option<Vec<u8>> {
    if !words.is_empty() {
        let quoted: Vec<String> = words
            .iter()
            .map(|w| {
                if shell_needs_quoting(w) {
                    shell_quote(w)
                } else {
                    w.clone()
                }
            })
            .collect();
        let mut text = quoted.join(" ").into_bytes();
        text.push(b'\r');
        return Some(text);
    }
    let mut text = stdin;
    match text.last_mut() {
        None => return None,
        Some(last) if *last == b'\n' => *last = b'\r',
        Some(_) => text.push(b'\r'),
    }
    Some(text)
}

pub fn run(opts: Options, detached: bool, words: &[String], out: &mut dyn Write) -> CliResult {
    let socket_path = opts.socket_path.clone();
    if ensure_session(opts, out).map_err(|_| CliError::Silent)? == Started::Daemon {
        return Ok(());
    }
    let stdin = if words.is_empty() {
        read_stdin()
    } else {
        Vec::new()
    };
    let Some(cmd) = command_text(words, stdin) else {
        return Err(CliError::msg(
            "run requires a command (or pipe input via stdin)",
        ));
    };
    let mut stream = socket::connect(&socket_path).map_err(|e| {
        log::error(&format!("session not ready: {}", error_name(&e)));
        CliError::msg(format!("session not ready: {}", error_name(&e)))
    })?;
    let size = term::terminal_size(1);
    let _ = stream.write_all(&encode_frame(Tag::RESIZE, &size.encode()));
    if let Err(e) = stream.write_all(&encode_frame(Tag::RUN, &cmd)) {
        return if socket::hung_up(&e) {
            Ok(())
        } else {
            Err(CliError::Silent)
        };
    }
    let code = super::tail::follow(std::slice::from_mut(&mut stream), detached, true, out)
        .map_err(|_| CliError::Silent)?;
    Err(CliError::Exit(i32::from(code)))
}

#[cfg(test)]
mod tests {
    use super::command_text;

    #[test]
    fn words_are_quoted_and_stdin_ends_in_cr() {
        let words = vec!["echo".to_string(), "a b".to_string()];
        assert_eq!(command_text(&words, Vec::new()).unwrap(), b"echo 'a b'\r");
        assert_eq!(command_text(&[], b"ls\n".to_vec()).unwrap(), b"ls\r");
        assert_eq!(command_text(&[], b"ls".to_vec()).unwrap(), b"ls\r");
        assert_eq!(command_text(&[], Vec::new()), None);
    }
}
