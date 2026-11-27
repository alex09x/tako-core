/*
 * tako-session — session runtime for Tako
 * Copyright (c) 2026 Alexander Panasenko
 *
 * Contact: alex@prod.codes
 * Author: https://prod.codes/about/
 * Project: https://github.com/alex09x/tako-core
 * SPDX-License-Identifier: MIT
 */

//! `get`, `set` and `clear`: a session's labels.

use super::error::{CliError, CliResult, name_too_long};
use crate::client::{ProbeError, round_trip_for_tag};
use crate::ipc::{LabelError, Tag, assert_label, label_value, parse_labels, validate_label_pairs};
use crate::log;
use crate::socket::socket_path;
use std::io::Write;
use std::path::Path;

/// The message for a label exchange that failed.
fn exchange_error(session: &str, err: ProbeError) -> CliError {
    match err {
        ProbeError::Timeout => CliError::msg(format!(
            "session \"{session}\" does not support labels (daemon too old?)"
        )),
        ProbeError::ConnectionRefused | ProbeError::Unexpected => {
            CliError::msg(format!("session \"{session}\" not found or unresponsive"))
        }
        other => CliError::msg(other.name()),
    }
}

/// Checks each pair of a label list for `set` and plain `attach --labels`.
pub fn assert_labels(labels: &str) -> CliResult {
    const MSG: &str = "error: key-value kvs can only contain [a-z, A-Z, 0-9, -_.] characters";
    for (key, value) in parse_labels(labels) {
        if let Err(err) = assert_label(key, value) {
            return Err(match err {
                LabelError::KeyEmpty => CliError::msg("label key cannot be empty"),
                LabelError::KeyReservedName => {
                    CliError::msg(format!("\"{key}\" is a read-only built-in field"))
                }
                LabelError::KeyInvalidChar => CliError::Line(format!("{MSG}: key=[{key}]")),
                _ => CliError::Line(format!("{MSG}: value=[{value}]")),
            });
        }
    }
    Ok(())
}

/// Checks a guard (`--if-label`) or guarded label list strictly.
pub fn assert_guard_labels(raw: &str, require_values: bool) -> CliResult {
    validate_label_pairs(raw, true, require_values).map_err(|err| match err {
        LabelError::EmptyLabels => CliError::msg("label guard cannot be empty"),
        LabelError::EmptyValue => CliError::msg("--if-label requires non-empty values"),
        LabelError::MalformedPair => CliError::msg("malformed key=value label pair"),
        other => CliError::msg(format!("invalid label guard ({})", label_error_name(other))),
    })
}

/// The reference name of a label error, as printed.
pub fn label_error_name(err: LabelError) -> &'static str {
    match err {
        LabelError::KeyEmpty => "LabelKeyEmpty",
        LabelError::KeyInvalidChar => "LabelKeyInvalidChar",
        LabelError::ValueInvalidChar => "LabelValueInvalidChar",
        LabelError::KeyReservedName => "LabelKeyReservedName",
        LabelError::EmptyLabels => "EmptyLabels",
        LabelError::MalformedPair => "MalformedPair",
        LabelError::EmptyValue => "EmptyValue",
    }
}

fn path_for(dir: &Path, session: &str) -> Result<std::path::PathBuf, CliError> {
    socket_path(dir, session).ok_or_else(|| name_too_long(session, dir))
}

/// `get <session> [key]`: all labels as sent, or one value; no newline.
pub fn get(dir: &Path, session: &str, key: &str, out: &mut dyn Write) -> CliResult {
    log::info(&format!("label get session={session}"));
    let path = path_for(dir, session)?;
    let payload = round_trip_for_tag(&path, Tag::LABEL_GET, b"", Tag::LABEL_DATA)
        .map_err(|e| exchange_error(session, e))?;
    if key.is_empty() {
        out.write_all(&payload).map_err(|_| CliError::Silent)?;
        return out.flush().map_err(|_| CliError::Silent);
    }
    let text = String::from_utf8_lossy(&payload);
    let value = label_value(key, &text).ok_or_else(|| {
        CliError::msg(format!(
            "label key \"{key}\" not found in session \"{session}\""
        ))
    })?;
    out.write_all(value.as_bytes())
        .map_err(|_| CliError::Silent)?;
    out.flush().map_err(|_| CliError::Silent)
}

/// `set <session> key=value...`: adds or replaces labels.
pub fn set(dir: &Path, session: &str, labels: &str) -> CliResult {
    log::info(&format!("label set session={session}"));
    assert_labels(labels)?;
    let path = path_for(dir, session)?;
    round_trip_for_tag(&path, Tag::LABEL_SET, labels.as_bytes(), Tag::ACK)
        .map_err(|e| exchange_error(session, e))?;
    Ok(())
}

/// `clear <session>`: removes every label.
pub fn clear(dir: &Path, session: &str) -> CliResult {
    log::info(&format!("label clear session={session}"));
    let path = path_for(dir, session)?;
    round_trip_for_tag(&path, Tag::LABEL_CLEAR, b"", Tag::ACK)
        .map_err(|e| exchange_error(session, e))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn label_checks_give_the_reference_messages() {
        let text = |r: CliResult| r.unwrap_err().stderr_text();
        assert_eq!(
            text(assert_labels("bad/key=1")),
            "error: key-value kvs can only contain [a-z, A-Z, 0-9, -_.] characters: key=[bad/key]\n"
        );
        assert_eq!(
            text(assert_labels("name=1")),
            "error: \"name\" is a read-only built-in field\n"
        );
        assert_eq!(
            text(assert_labels("=1")),
            "error: label key cannot be empty\n"
        );
        assert!(text(assert_labels("k=a/b")).ends_with(": value=[a/b]\n"));
        assert_eq!(assert_labels("a=1 b=2"), Ok(()));
        assert_eq!(
            text(assert_guard_labels("tako-gen=", true)),
            "error: --if-label requires non-empty values\n"
        );
        assert_eq!(
            text(assert_guard_labels("", true)),
            "error: label guard cannot be empty\n"
        );
        assert_eq!(
            text(assert_guard_labels("x", true)),
            "error: malformed key=value label pair\n"
        );
        assert_eq!(
            text(assert_guard_labels("name=x", true)),
            "error: invalid label guard (LabelKeyReservedName)\n"
        );
    }

    #[test]
    fn a_missing_session_reads_as_not_found() {
        let dir = std::env::temp_dir().join(format!("ts-labels-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let err = get(&dir, "nosuch", "", &mut Vec::new()).unwrap_err();
        assert_eq!(
            err.stderr_text(),
            "error: session \"nosuch\" not found or unresponsive\n"
        );
        assert_eq!(set(&dir, "nosuch", "a=b").unwrap_err(), err);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
