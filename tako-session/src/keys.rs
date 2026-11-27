/*
 * tako-session — session runtime for Tako
 * Copyright (c) 2026 Alexander Panasenko
 *
 * Contact: alex@prod.codes
 * Author: https://prod.codes/about/
 * Project: https://github.com/alex09x/tako-core
 * SPDX-License-Identifier: MIT
 */

//! The detach key, ctrl+\, in each encoding a terminal may send it: the raw
//! byte, the kitty keyboard protocol (`CSI 92;5u`) and xterm
//! modifyOtherKeys (`CSI 27;5;92~`).

const BACKSLASH: u32 = 0x5c;
const CTRL: u32 = 0b100;
/// Lock modifiers (caps, num) are ambient state, not part of a combination.
const INTENTIONAL: u32 = 0b0011_1111;

/// Whether `buf` holds a ctrl+\ press.
pub fn is_ctrl_backslash(buf: &[u8]) -> bool {
    match buf.first() {
        None => false,
        Some(0x1c) => true,
        Some(_) => {
            is_key_pressed(buf, BACKSLASH, CTRL) || is_modify_other_key(buf, BACKSLASH, CTRL)
        }
    }
}

/// Whether `ZMX_NO_DETACH_KEY` turned the shortcut off.
pub fn detach_key_disabled(env: &dyn crate::cfg::Env) -> bool {
    env.get("ZMX_NO_DETACH_KEY").is_some()
}

/// Reads decimal digits at `*pos` (wrapping on overflow); None when there are none.
fn decimal(buf: &[u8], pos: &mut usize) -> Option<u32> {
    let start = *pos;
    let mut value: u32 = 0;
    while let Some(c) = buf.get(*pos).filter(|c| c.is_ascii_digit()) {
        value = value.wrapping_mul(10).wrapping_add(u32::from(c - b'0'));
        *pos += 1;
    }
    (*pos > start).then_some(value)
}

fn mods_match(raw: u32, expected: u32) -> bool {
    expected == 0 || expected == raw & INTENTIONAL
}

/// A parsed kitty `CSI code[:alt...];mods[:event][;text]u` (the bytes after
/// `CSI`): key code, raw modifiers, event type and bytes consumed.
struct CsiU {
    code: u32,
    mods: u32,
    event: u32,
    consumed: usize,
}

fn parse_csi_u(buf: &[u8]) -> Option<CsiU> {
    let mut pos = 0;
    let code = decimal(buf, &mut pos)?;
    while buf.get(pos) == Some(&b':') {
        pos += 1;
        decimal(buf, &mut pos);
    }
    if buf.get(pos) != Some(&b';') {
        return None;
    }
    pos += 1;
    let encoded = decimal(buf, &mut pos).filter(|&m| m >= 1)?;
    let mut event = 1;
    if buf.get(pos) == Some(&b':') {
        pos += 1;
        event = decimal(buf, &mut pos)?;
    }
    if buf.get(pos) == Some(&b';') {
        pos += 1;
        while buf
            .get(pos)
            .is_some_and(|c| c.is_ascii_digit() || *c == b':')
        {
            pos += 1;
        }
    }
    (buf.get(pos) == Some(&b'u')).then_some(CsiU {
        code,
        mods: encoded - 1,
        event,
        consumed: pos + 1,
    })
}

/// The event type and length of a kitty key report at `buf` (after `CSI`),
/// whatever the key: 1 press, 2 repeat, 3 release.
pub fn kitty_press(buf: &[u8]) -> Option<(u32, usize)> {
    parse_csi_u(buf).map(|k| (k.event, k.consumed))
}

/// A press or repeat (not a release) of `key` with `mods`.
fn csi_u_matches(buf: &[u8], key: u32, mods: u32) -> bool {
    parse_csi_u(buf).is_some_and(|k| k.code == key && mods_match(k.mods, mods) && k.event != 3)
}

fn is_key_pressed(buf: &[u8], key: u32, mods: u32) -> bool {
    (0..buf.len().saturating_sub(2))
        .any(|i| buf[i] == 0x1b && buf[i + 1] == b'[' && csi_u_matches(&buf[i + 2..], key, mods))
}

/// `CSI 27;mods;code~`.
fn modify_other_matches(buf: &[u8], key: u32, mods: u32) -> bool {
    let mut pos = 0;
    if decimal(buf, &mut pos) != Some(27) || buf.get(pos) != Some(&b';') {
        return false;
    }
    pos += 1;
    let Some(encoded) = decimal(buf, &mut pos).filter(|&m| m >= 1) else {
        return false;
    };
    if !mods_match(encoded - 1, mods) || buf.get(pos) != Some(&b';') {
        return false;
    }
    pos += 1;
    decimal(buf, &mut pos) == Some(key) && buf.get(pos) == Some(&b'~')
}

fn is_modify_other_key(buf: &[u8], key: u32, mods: u32) -> bool {
    (0..buf.len().saturating_sub(1)).any(|i| {
        buf[i] == 0x1b && buf[i + 1] == b'[' && modify_other_matches(&buf[i + 2..], key, mods)
    })
}

#[cfg(test)]
mod tests {
    use super::is_ctrl_backslash as hit;

    #[test]
    fn raw_byte_and_kitty_presses_detach() {
        assert!(hit(b"\x1c"));
        assert!(hit(b"\x1c rest"));
        assert!(hit(b"\x1b[92;5u"));
        assert!(
            hit(b"\x1b[92;5:1u") && hit(b"\x1b[92;5:2u"),
            "press and repeat"
        );
        assert!(!hit(b"\x1b[92;5:3u"), "release");
        assert!(hit(b"\x1b[92;69u"), "with caps lock");
        assert!(hit(b"\x1b[92:124;5u"), "with an alternate key");
        assert!(hit(b"\x1b[92;5;92u"), "with text codepoints");
        assert!(hit(b"abc\x1b[92;5u"), "anywhere in the read");
        assert!(!hit(b"\x1b[92;7u"), "ctrl+alt is another combination");
        assert!(!hit(b"\x1b[93;5u") && !hit(b"\\") && !hit(b""));
    }

    #[test]
    fn xterm_modify_other_keys_detach() {
        assert!(hit(b"\x1b[27;5;92~"));
        assert!(hit(b"\x1b[27;69;92~"));
        assert!(!hit(b"\x1b[27;5;93~") && !hit(b"\x1b[27;3;92~") && !hit(b"\x1b[28;5;92~"));
    }
}
