/*
 * tako-session — session runtime for Tako
 * Copyright (c) 2026 Alexander Panasenko
 *
 * Contact: alex@prod.codes
 * Author: https://prod.codes/about/
 * Project: https://github.com/alex09x/tako-core
 * SPDX-License-Identifier: MIT
 */

//! Whether a client's input is the person typing, which makes that client
//! the session's leader. Mouse reports and focus events are not.

use crate::keys::kitty_press;

#[derive(Clone, Copy, PartialEq, Eq)]
enum State {
    Ground,
    Escape,
    Csi,
    String,
    StringEsc,
}

/// Feeds one byte; Some(true/false) once the input is decided.
struct Scanner {
    state: State,
    params: usize,
    param_seen: bool,
}

impl Scanner {
    fn new() -> Scanner {
        Scanner {
            state: State::Ground,
            params: 0,
            param_seen: false,
        }
    }

    fn csi(&mut self, c: u8) -> Option<bool> {
        match c {
            b'0'..=b'9' => self.param_seen = true,
            b';' | b':' => {
                self.params += 1;
                self.param_seen = false;
            }
            0x20..=0x3f => {}
            0x40..=0x7e => {
                let params = self.params + usize::from(self.param_seen);
                self.state = State::Ground;
                return match c {
                    b'u' | b'~' => Some(true),
                    b'A'..=b'D' if params > 1 => Some(true),
                    b'M' | b'<' | b'I' | b'O' => Some(false),
                    _ => None,
                };
            }
            0x18 | 0x1a => self.state = State::Ground,
            0x1b => self.state = State::Escape,
            _ => return execute(c),
        }
        None
    }

    fn next(&mut self, c: u8) -> Option<bool> {
        match self.state {
            State::Ground => match c {
                0x1b => self.state = State::Escape,
                0x00..=0x1f | 0x7f => return execute(c),
                _ => return Some(true),
            },
            State::Escape => match c {
                b'[' => {
                    self.state = State::Csi;
                    self.params = 0;
                    self.param_seen = false;
                }
                b']' | b'P' | b'X' | b'^' | b'_' => self.state = State::String,
                0x1b => {}
                0x00..=0x1f => return execute(c),
                _ => self.state = State::Ground,
            },
            State::Csi => return self.csi(c),
            State::String => match c {
                0x07 => self.state = State::Ground,
                0x1b => self.state = State::StringEsc,
                _ => {}
            },
            State::StringEsc => {
                self.state = if c == b'\\' {
                    State::Ground
                } else {
                    State::String
                }
            }
        }
        None
    }
}

/// CR, LF, tab and backspace are typing; other C0 controls are not decided.
fn execute(c: u8) -> Option<bool> {
    matches!(c, 0x0d | 0x0a | 0x09 | 0x08).then_some(true)
}

pub fn is_user_input(payload: &[u8]) -> bool {
    let mut scanner = Scanner::new();
    let mut i = 0;
    while i < payload.len() {
        if payload[i] == 0x1b
            && i + 2 < payload.len()
            && payload[i + 1] == b'['
            && let Some((event, consumed)) = kitty_press(&payload[i + 2..])
        {
            if event != 3 {
                return true;
            }
            i += 2 + consumed;
            continue;
        }
        if let Some(decided) = scanner.next(payload[i]) {
            return decided;
        }
        i += 1;
    }
    false
}

#[cfg(test)]
mod tests {
    use super::is_user_input as typed;

    #[test]
    fn typing_counts_and_reports_do_not() {
        assert!(typed(b"a") && typed(b"\r") && typed(b"\n") && typed(b"\t") && typed(b"\x08"));
        assert!(typed(b"\x1b[97;5u"), "kitty press");
        assert!(!typed(b"\x1b[97;5:3u"), "kitty release alone");
        assert!(
            typed(b"\x1b[3~") && typed(b"\x1b[1;5C"),
            "delete, ctrl+right"
        );
        assert!(!typed(b"\x1b[A"), "a bare arrow has one parameter");
        assert!(!typed(b"\x1b[M !!"), "legacy mouse decides at M");
        assert!(!typed(b"\x1b[<0;10;5M"), "SGR mouse");
        assert!(!typed(b"\x1b[I") && !typed(b"\x1b[O"), "focus events");
        assert!(!typed(b"\x1b]11;?\x07"), "an OSC is not typing");
        assert!(!typed(b"") && !typed(b"\x01"));
    }
}
