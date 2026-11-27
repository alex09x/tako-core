/*
 * tako-session — session runtime for Tako
 * Copyright (c) 2026 Alexander Panasenko
 *
 * Contact: alex@prod.codes
 * Author: https://prod.codes/about/
 * Project: https://github.com/alex09x/tako-core
 * SPDX-License-Identifier: MIT
 */

//! Working directories as OSC 7 reports them, `file://<host><path>`.

/// A directory from an OSC 7 value or a plain path.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cwd {
    /// Percent-decoded, without scheme or host.
    pub path: Vec<u8>,
    /// Whether the host is this machine, so the path can be entered here.
    pub is_local: bool,
}

/// This machine's host name.
pub fn hostname() -> String {
    let mut buf = [0u8; 256];
    // SAFETY: buf is valid for its length.
    if unsafe { libc::gethostname(buf.as_mut_ptr().cast(), buf.len()) } != 0 {
        return String::new();
    }
    let end = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
    String::from_utf8_lossy(&buf[..end]).into_owned()
}

fn hex(c: u8) -> Option<u8> {
    (c as char).to_digit(16).map(|d| d as u8)
}

fn percent_decode(raw: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(raw.len());
    let mut i = 0;
    while i < raw.len() {
        if raw[i] == b'%'
            && let (Some(h), Some(l)) = (
                raw.get(i + 1).and_then(|&c| hex(c)),
                raw.get(i + 2).and_then(|&c| hex(c)),
            )
        {
            out.push(h << 4 | l);
            i += 3;
            continue;
        }
        out.push(raw[i]);
        i += 1;
    }
    out
}

fn is_local_host(host: Option<&[u8]>, hostname: &str) -> bool {
    let Some(raw) = host else { return true };
    let value = percent_decode(raw);
    let value = String::from_utf8_lossy(&value);
    if value.is_empty()
        || value.eq_ignore_ascii_case("localhost")
        || value.eq_ignore_ascii_case(hostname)
    {
        return true;
    }
    let first = |s: &str| s.split('.').next().unwrap_or("").to_string();
    let host_label = first(hostname);
    !host_label.is_empty() && first(&value).eq_ignore_ascii_case(&host_label)
}

/// Parses an OSC 7 value (`file:` or `kitty-shell-cwd:` URI) or an absolute
/// path. None when it names no usable absolute path.
pub fn parse(value: &[u8], hostname: &str) -> Option<Cwd> {
    if value.is_empty() {
        return None;
    }
    if value[0] == b'/' {
        return Some(Cwd {
            path: value.to_vec(),
            is_local: true,
        });
    }
    let colon = value.iter().position(|&c| c == b':')?;
    let scheme = &value[..colon];
    if !scheme.eq_ignore_ascii_case(b"file") && !scheme.eq_ignore_ascii_case(b"kitty-shell-cwd") {
        return None;
    }
    let rest = &value[colon + 1..];
    let (host, path_part) = match rest.strip_prefix(b"//") {
        Some(auth_and_path) => {
            let slash = auth_and_path
                .iter()
                .position(|&c| c == b'/')
                .unwrap_or(auth_and_path.len());
            let authority = &auth_and_path[..slash];
            // Drop user info and port, keeping the host.
            let host = authority.rsplit(|&c| c == b'@').next().unwrap_or(authority);
            let host = match host.iter().rposition(|&c| c == b':') {
                Some(p) if !host.contains(&b']') || host.ends_with(b"]") => &host[..p],
                _ => host,
            };
            (Some(host), &auth_and_path[slash..])
        }
        None => (None, rest),
    };
    let path_end = path_part
        .iter()
        .position(|&c| c == b'?' || c == b'#')
        .unwrap_or(path_part.len());
    let path = percent_decode(&path_part[..path_end]);
    if path.first() != Some(&b'/') {
        return None;
    }
    Some(Cwd {
        path,
        is_local: is_local_host(host, hostname),
    })
}

fn is_path_char(c: u8) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, b'-' | b'.' | b'_' | b'~' | b'/' | b':' | b'@')
}

/// `file://<hostname><path>`, the path percent-encoded so it round-trips.
pub fn to_uri(path: &[u8], hostname: &str) -> Vec<u8> {
    let mut out = format!("file://{hostname}").into_bytes();
    for &c in path {
        if is_path_char(c) {
            out.push(c);
        } else {
            out.extend_from_slice(format!("%{c:02X}").as_bytes());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_uris_and_plain_paths() {
        let c = parse(b"file://box/home/me/a%20b", "box").unwrap();
        assert_eq!(
            (c.path.as_slice(), c.is_local),
            (&b"/home/me/a b"[..], true)
        );
        assert!(!parse(b"file://other/x", "box").unwrap().is_local);
        assert!(
            parse(b"file://box.lan/x", "box").unwrap().is_local,
            "first label"
        );
        assert!(parse(b"file://BOX/x", "box.lan").unwrap().is_local);
        assert!(
            parse(b"file:///x", "box").unwrap().is_local,
            "no host is local"
        );
        assert!(parse(b"file://localhost/x", "box").unwrap().is_local);
        assert!(parse(b"kitty-shell-cwd://box/x", "box").is_some());
        assert_eq!(
            parse(b"/plain/p", "box").unwrap(),
            Cwd {
                path: b"/plain/p".to_vec(),
                is_local: true
            }
        );
        assert!(parse(b"", "box").is_none() && parse(b"http://box/x", "box").is_none());
        assert!(parse(b"file://box", "box").is_none(), "no path");
    }

    #[test]
    fn builds_uris_that_parse_back() {
        assert_eq!(to_uri(b"/tmp/a b%", "box"), b"file://box/tmp/a%20b%25");
        let back = parse(&to_uri(b"/x/[y]", "box"), "box").unwrap();
        assert_eq!(back.path, b"/x/[y]");
    }
}
