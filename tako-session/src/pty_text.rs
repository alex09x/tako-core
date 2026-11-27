/*
 * tako-session — session runtime for Tako
 * Copyright (c) 2026 Alexander Panasenko
 *
 * Contact: alex@prod.codes
 * Author: https://prod.codes/about/
 * Project: https://github.com/alex09x/tako-core
 * SPDX-License-Identifier: MIT
 */

//! Byte-level work on what flows through the PTY.

const DA1_QUERY: &[u8] = b"\x1b[c";
const DA1_QUERY_EXPLICIT: &[u8] = b"\x1b[0c";
const DA2_QUERY: &[u8] = b"\x1b[>c";
const DA2_QUERY_EXPLICIT: &[u8] = b"\x1b[>0c";
pub const DA1_RESPONSE: &[u8] = b"\x1b[?62;22c";
pub const DA2_RESPONSE: &[u8] = b"\x1b[>1;10;0c";
const OSC_133_A: &[u8] = b"\x1b]133;A";

/// Answers device-attribute queries in PTY output while no terminal is
/// attached, so a shell (fish) does not wait for them. Responses (`CSI ? ...`)
/// in the output are not queries.
pub fn respond_to_device_attributes(out: &mut Vec<u8>, data: &[u8]) {
    let mut i = 0;
    while i < data.len() {
        // Jump to the next ESC; text between escapes cannot hold a query.
        let Some(p) = memchr(0x1b, &data[i..]) else {
            return;
        };
        i += p;
        if data.get(i + 1) == Some(&b'[') {
            if data.get(i + 2) == Some(&b'?') {
                i += 3;
                continue;
            }
            let at = &data[i..];
            if at.starts_with(DA2_QUERY) || at.starts_with(DA2_QUERY_EXPLICIT) {
                out.extend_from_slice(DA2_RESPONSE);
            } else if at.starts_with(DA1_QUERY) || at.starts_with(DA1_QUERY_EXPLICIT) {
                out.extend_from_slice(DA1_RESPONSE);
            }
        }
        i += 1;
    }
}

/// The first `byte` in `hay`, through libc's `memchr` (vectorised in every
/// libc we run on), since output is mostly text with few escapes.
fn memchr(byte: u8, hay: &[u8]) -> Option<usize> {
    // SAFETY: memchr reads at most `hay.len()` bytes from `hay`.
    let p = unsafe { libc::memchr(hay.as_ptr().cast(), i32::from(byte), hay.len()) };
    (!p.is_null()).then(|| p as usize - hay.as_ptr() as usize)
}

/// Byte search that jumps (memchr) to each occurrence of the needle's last
/// byte: for an escape sequence that is a printable byte, far rarer than
/// ESC in styled output (a screen replay has an ESC every few dozen bytes).
fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    let last = needle.len() - 1;
    let mut from = last;
    while from < hay.len() {
        let at = from + memchr(needle[last], &hay[from..])?;
        let start = at - last;
        if &hay[start..=at] == needle {
            return Some(start);
        }
        from = at + 1;
    }
    None
}

fn rfind(hay: &[u8], needle: &[u8]) -> Option<usize> {
    let mut end = hay.len();
    while let Some(at) = hay[..end].iter().rposition(|&b| b == needle[0]) {
        if hay[at..].starts_with(needle) {
            return Some(at);
        }
        end = at;
    }
    None
}

/// Rewrites each OSC 133;A prompt mark to carry `redraw=0`, so the outer
/// terminal does not clear prompt lines on resize: the session's shell
/// cannot redraw them for it. None when nothing changes.
pub fn rewrite_prompt_redraw(data: &[u8]) -> Option<Vec<u8>> {
    find(data, OSC_133_A)?;
    let mut out = data.to_vec();
    let mut search_end = out.len();
    while search_end > 0 {
        let Some(pos) = rfind(&out[..search_end], OSC_133_A) else {
            break;
        };
        search_end = pos;
        let after = pos + OSC_133_A.len();
        if after >= out.len() {
            continue;
        }
        let Some(end) = (after..out.len())
            .find(|&j| out[j] == 0x07 || (out[j] == 0x1b && out.get(j + 1) == Some(&b'\\')))
        else {
            continue;
        };
        let params = &out[after..end];
        if find(params, b"redraw=0").is_some() {
            continue;
        }
        if let Some(off) = find(params, b"redraw=") {
            let value_start = after + off + b"redraw=".len();
            let value_end = (value_start..end).find(|&k| out[k] == b';').unwrap_or(end);
            out.splice(value_start..value_end, *b"0");
            continue;
        }
        out.splice(end..end, b";redraw=0".iter().copied());
    }
    (out != data).then_some(out)
}

/// `ZMX_TASK_COMPLETED:<id>:`, followed by the exit code once the shell runs it.
pub fn task_exit_marker(id: &[u8; 4]) -> Vec<u8> {
    let mut m = b"ZMX_TASK_COMPLETED:".to_vec();
    m.extend_from_slice(id);
    m.push(b':');
    m
}

/// The exit code after the first marker that has one; the PTY echoes the
/// command line, so earlier occurrences still read `$?`.
pub fn find_task_exit_marker(output: &[u8], id: &[u8; 4]) -> Option<u8> {
    let marker = task_exit_marker(id);
    let mut start = 0;
    while let Some(rel) = find(&output[start..], &marker) {
        let after = &output[start + rel + marker.len()..];
        let end = after
            .iter()
            .position(|&c| c == b'\n' || c == b'\r')
            .unwrap_or(after.len());
        if let Some(code) = std::str::from_utf8(&after[..end])
            .ok()
            .and_then(|s| s.parse::<u8>().ok())
        {
            return Some(code);
        }
        start += rel + marker.len();
    }
    None
}

pub fn shell_needs_quoting(arg: &str) -> bool {
    arg.is_empty()
        || arg
            .bytes()
            .any(|c| b" \t\"'\\$`!(){}[]|&;<>?*~#\n".contains(&c))
}

/// Single-quotes `arg`, with `'` as `'\''`.
pub fn shell_quote(arg: &str) -> String {
    format!("'{}'", arg.replace('\'', "'\\''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn device_attribute_queries_get_answers_and_responses_do_not() {
        let mut out = Vec::new();
        respond_to_device_attributes(&mut out, b"x\x1b[c y\x1b[>0c \x1b[?62;22c");
        assert_eq!(out, [DA1_RESPONSE, DA2_RESPONSE].concat());
    }

    #[test]
    fn prompt_marks_gain_redraw_zero() {
        assert_eq!(rewrite_prompt_redraw(b"plain"), None);
        assert_eq!(
            rewrite_prompt_redraw(b"\x1b]133;A\x07").unwrap(),
            b"\x1b]133;A;redraw=0\x07"
        );
        assert_eq!(
            rewrite_prompt_redraw(b"\x1b]133;A\x1b\\").unwrap(),
            b"\x1b]133;A;redraw=0\x1b\\"
        );
        assert_eq!(
            rewrite_prompt_redraw(b"\x1b]133;A;redraw=1\x07").unwrap(),
            b"\x1b]133;A;redraw=0\x07"
        );
        assert_eq!(
            rewrite_prompt_redraw(b"\x1b]133;A;redraw=last;k=v\x07").unwrap(),
            b"\x1b]133;A;redraw=0;k=v\x07"
        );
        assert_eq!(rewrite_prompt_redraw(b"\x1b]133;A;redraw=0\x07"), None);
        assert_eq!(rewrite_prompt_redraw(b"\x1b]133;B\x07"), None);
        assert_eq!(
            rewrite_prompt_redraw(b"a\x1b]133;A\x07b\x1b]133;A\x07").unwrap(),
            b"a\x1b]133;A;redraw=0\x07b\x1b]133;A;redraw=0\x07"
        );
    }

    #[test]
    fn task_marker_skips_the_echoed_command_line() {
        let id = *b"ab12";
        assert_eq!(
            find_task_exit_marker(
                b"echo ZMX_TASK_COMPLETED:ab12:$?\r\nZMX_TASK_COMPLETED:ab12:3\r\n",
                &id
            ),
            Some(3)
        );
        assert_eq!(
            find_task_exit_marker(b"ZMX_TASK_COMPLETED:ab12:$?", &id),
            None
        );
        assert_eq!(
            find_task_exit_marker(b"ZMX_TASK_COMPLETED:zz99:0\r\n", &id),
            None
        );
    }

    #[test]
    fn quoting_matches_the_reference() {
        assert!(
            shell_needs_quoting("") && shell_needs_quoting("a b") && !shell_needs_quoting("ls")
        );
        assert_eq!(shell_quote("it's"), "'it'\\''s'");
    }
}
