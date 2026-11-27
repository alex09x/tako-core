/*
 * tako-session — session runtime for Tako
 * Copyright (c) 2026 Alexander Panasenko
 *
 * Contact: alex@prod.codes
 * Author: https://prod.codes/about/
 * Project: https://github.com/alex09x/tako-core
 * SPDX-License-Identifier: MIT
 */

//! Terminal output as plain text, for `run` and `tail`: escape sequences
//! and control characters go, except CR, LF, TAB and backspace.

#[derive(Clone, Copy, PartialEq, Eq)]
enum State {
    Ground,
    Escape,
    /// Intermediate bytes of an escape sequence, until its final byte.
    EscapeIntermediate,
    Csi,
    /// OSC, DCS, SOS, PM, APC: a string until BEL or ST.
    String,
    /// ESC inside a string: `\` ends it.
    StringEscape,
}

pub fn strip(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len());
    let mut state = State::Ground;
    for &c in data {
        // CAN and SUB cancel any sequence; ESC starts a new one.
        if matches!(c, 0x18 | 0x1a) {
            state = State::Ground;
            continue;
        }
        state = match state {
            State::Ground => match c {
                0x1b => State::Escape,
                b'\r' | b'\n' | b'\t' | 0x08 => {
                    out.push(c);
                    State::Ground
                }
                0x00..=0x1f | 0x7f => State::Ground,
                _ => {
                    out.push(c);
                    State::Ground
                }
            },
            State::Escape => match c {
                b'[' => State::Csi,
                b']' | b'P' | b'X' | b'^' | b'_' => State::String,
                0x1b => State::Escape,
                0x20..=0x2f => State::EscapeIntermediate,
                _ => State::Ground,
            },
            State::EscapeIntermediate => match c {
                0x1b => State::Escape,
                0x20..=0x2f => State::EscapeIntermediate,
                _ => State::Ground,
            },
            State::Csi => match c {
                0x1b => State::Escape,
                0x40..=0x7e => State::Ground,
                _ => State::Csi,
            },
            State::String => match c {
                0x07 => State::Ground,
                0x1b => State::StringEscape,
                _ => State::String,
            },
            State::StringEscape => match c {
                b'\\' => State::Ground,
                0x1b => State::StringEscape,
                _ => State::String,
            },
        };
    }
    out
}

#[cfg(test)]
mod tests {
    use super::strip;

    #[test]
    fn sequences_and_controls_go_text_and_whitespace_stay() {
        let input = b"\x1b[1;31mred\x1b[0m\r\n\x1b]0;title\x07tab\there\x1b]7;file://h/x\x1b\\ \x1b(Bok\x07\x08";
        assert_eq!(strip(input), b"red\r\ntab\there ok\x08");
        assert_eq!(strip("é\x1b[2Jz".as_bytes()), "éz".as_bytes());
        assert_eq!(strip(b"\x1bP1$r\x1b\\after"), b"after");
        assert_eq!(strip(b"\x1b[1\x18x"), b"x", "CAN cancels");
    }
}
