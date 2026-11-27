/*
 * tako-session — session runtime for Tako
 * Copyright (c) 2026 Alexander Panasenko
 *
 * Contact: alex@prod.codes
 * Author: https://prod.codes/about/
 * Project: https://github.com/alex09x/tako-core
 * SPDX-License-Identifier: MIT
 */

//! Text the daemon types into the session's shell: a task's command with
//! its exit marker, and files written through the PTY (so it works over SSH).

use crate::pty_text::task_exit_marker;
use std::io::Read;

/// Four lowercase hex digits from two random bytes.
pub fn generate_task_id() -> [u8; 4] {
    let mut bytes = [0u8; 2];
    let filled = std::fs::File::open("/dev/urandom").and_then(|mut f| f.read_exact(&mut bytes));
    if filled.is_err() {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.subsec_nanos())
            .unwrap_or(0);
        bytes = [(nanos >> 8) as u8, nanos as u8];
    }
    let hex = b"0123456789abcdef";
    [
        hex[(bytes[0] >> 4) as usize],
        hex[(bytes[0] & 15) as usize],
        hex[(bytes[1] >> 4) as usize],
        hex[(bytes[1] & 15) as usize],
    ]
}

/// The command (a trailing CR dropped) and the marker that reports its exit
/// code. The marker follows on the same line, or on the next one when the
/// command has a heredoc, whose delimiter must stand alone.
pub fn run_input(cmd: &[u8], id: &[u8; 4]) -> Vec<u8> {
    let mut out = cmd.strip_suffix(b"\r").unwrap_or(cmd).to_vec();
    let marker = task_exit_marker(id);
    let heredoc = cmd.windows(2).any(|w| w == b"<<");
    out.extend_from_slice(if heredoc { b"\r\necho " } else { b"; echo " });
    out.extend_from_slice(&marker);
    out.extend_from_slice(b"$?\r");
    out
}

/// 48000 is a multiple of 3 (no padding mid-file) and encodes to about
/// 64 KB, well under a command-line limit.
const CHUNK: usize = 48000;

/// `printf '%s' '<base64>' | base64 -d > '<path>'` per chunk, appending
/// after the first. An empty file is one empty chunk.
pub fn write_input(path: &[u8], content: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut chunks: Vec<&[u8]> = content.chunks(CHUNK).collect();
    if chunks.is_empty() {
        chunks.push(&[]);
    }
    for (i, chunk) in chunks.into_iter().enumerate() {
        out.extend_from_slice(b"printf '%s' '");
        out.extend_from_slice(base64(chunk).as_bytes());
        out.extend_from_slice(if i == 0 {
            b"' | base64 -d > '"
        } else {
            b"' | base64 -d >> '"
        });
        // Inside the single quotes: a quote in the name closes, escapes
        // and reopens them.
        for &c in path {
            if c == b'\'' {
                out.extend_from_slice(b"'\\''");
            } else {
                out.push(c);
            }
        }
        out.extend_from_slice(b"'\r");
    }
    out
}

pub fn base64(data: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for group in data.chunks(3) {
        let b = [
            group[0],
            *group.get(1).unwrap_or(&0),
            *group.get(2).unwrap_or(&0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        for k in 0..4 {
            if k <= group.len() {
                out.push(ALPHABET[((n >> (18 - 6 * k)) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_matches_the_standard_alphabet_and_padding() {
        let cases: [(&[u8], &str); 5] = [
            (b"", ""),
            (b"f", "Zg=="),
            (b"fo", "Zm8="),
            (b"foo", "Zm9v"),
            (b"\xff\xfe", "//4="),
        ];
        for (input, want) in cases {
            assert_eq!(base64(input), want);
        }
    }

    #[test]
    fn the_marker_follows_the_command() {
        assert_eq!(
            run_input(b"ls\r", b"ab12"),
            b"ls; echo ZMX_TASK_COMPLETED:ab12:$?\r"
        );
        assert_eq!(
            run_input(b"cat <<E\nx\nE", b"ab12"),
            b"cat <<E\nx\nE\r\necho ZMX_TASK_COMPLETED:ab12:$?\r"
        );
    }

    #[test]
    fn files_go_in_chunks_and_empty_files_still_get_created() {
        assert_eq!(
            write_input(b"/t/x", b""),
            b"printf '%s' '' | base64 -d > '/t/x'\r"
        );
        let big = vec![b'a'; CHUNK + 1];
        let text = String::from_utf8(write_input(b"f", &big)).unwrap();
        assert_eq!(text.matches("| base64 -d > 'f'").count(), 1);
        assert_eq!(text.matches("| base64 -d >> 'f'").count(), 1);
        assert_eq!(
            write_input(b"it's", b""),
            b"printf '%s' '' | base64 -d > 'it'\\''s'\r"
        );
    }

    #[test]
    fn task_ids_are_four_hex_digits() {
        let id = generate_task_id();
        assert!(
            id.iter()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()),
            "{id:?}"
        );
    }
}
