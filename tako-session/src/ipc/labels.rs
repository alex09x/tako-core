/*
 * tako-session — session runtime for Tako
 * Copyright (c) 2026 Alexander Panasenko
 *
 * Contact: alex@prod.codes
 * Author: https://prod.codes/about/
 * Project: https://github.com/alex09x/tako-core
 * SPDX-License-Identifier: MIT
 */

//! Session labels: `key=value` pairs separated by single spaces, the payload
//! of `LabelSet`, `LabelData`, `KillIfLabel` and both halves of
//! `AttachIfLabel`. Keys and values use `[A-Za-z0-9._-]`.

use std::collections::BTreeMap;

const RESERVED_KEYS: [&str; 3] = ["name", "start_dir", "cmd"];

/// Why a label or a list of labels was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LabelError {
    KeyEmpty,
    KeyInvalidChar,
    ValueInvalidChar,
    KeyReservedName,
    EmptyLabels,
    MalformedPair,
    EmptyValue,
}

fn allowed(c: u8) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, b'-' | b'_' | b'.')
}

/// Checks one key and value.
pub fn assert_label(key: &str, value: &str) -> Result<(), LabelError> {
    if key.is_empty() {
        return Err(LabelError::KeyEmpty);
    }
    if RESERVED_KEYS.contains(&key) {
        return Err(LabelError::KeyReservedName);
    }
    if !key.bytes().all(allowed) {
        return Err(LabelError::KeyInvalidChar);
    }
    if !value.bytes().all(allowed) {
        return Err(LabelError::ValueInvalidChar);
    }
    Ok(())
}

/// Strictly checks a label list: no leading, trailing or doubled spaces, every
/// token a `key=value`, no duplicate keys. `require_non_empty` refuses an
/// empty list; `require_values` refuses `key=`.
pub fn validate_label_pairs(
    raw: &str,
    require_non_empty: bool,
    require_values: bool,
) -> Result<(), LabelError> {
    if raw.is_empty() {
        return if require_non_empty {
            Err(LabelError::EmptyLabels)
        } else {
            Ok(())
        };
    }
    let mut seen: Vec<&str> = Vec::new();
    for token in raw.split(' ') {
        let (key, value) = token.split_once('=').ok_or(LabelError::MalformedPair)?;
        assert_label(key, value)?;
        if require_values && value.is_empty() {
            return Err(LabelError::EmptyValue);
        }
        if seen.contains(&key) {
            return Err(LabelError::MalformedPair);
        }
        seen.push(key);
    }
    Ok(())
}

/// The `LabelData` payload for a label set: pairs sorted by key.
pub fn labels_to_payload(labels: &BTreeMap<String, String>) -> String {
    labels
        .iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join(" ")
}

/// The pairs of a label payload, read leniently and exactly as existing
/// daemons and clients read it: a key runs to the next `=` (spaces
/// included), its value on to the next space; reading stops when no `=` is
/// left. So `a=1 junk b=2` reads as `a=1` and a key `junk b` with value `2`.
/// Strict callers validate first (`validate_label_pairs`).
pub fn parse_labels(payload: &str) -> Vec<(&str, &str)> {
    let mut pairs = Vec::new();
    let mut rest = payload;
    while !rest.is_empty() {
        let Some(eq) = rest.find('=') else { break };
        let after = &rest[eq + 1..];
        let end = after.find(' ').unwrap_or(after.len());
        pairs.push((&rest[..eq], &after[..end]));
        rest = after.get(end + 1..).unwrap_or("");
    }
    pairs
}

/// The value `key` has in a label payload.
pub fn label_value<'a>(key: &str, payload: &'a str) -> Option<&'a str> {
    parse_labels(payload)
        .into_iter()
        .find(|(k, _)| *k == key)
        .map(|(_, v)| v)
}

/// Whether every pair of `required` (a valid, non-empty list with values) has
/// the same value in `actual`, a `LabelData` payload. `actual` must itself
/// be valid with values: a session carrying any `key=` label fails every
/// guard, as it does against existing clients, so all of them agree.
pub fn label_pairs_match(actual: &str, required: &str) -> bool {
    if validate_label_pairs(required, true, true).is_err()
        || validate_label_pairs(actual, false, true).is_err()
    {
        return false;
    }
    required
        .split(' ')
        .all(|token| match token.split_once('=') {
            Some((key, value)) => label_value(key, actual) == Some(value),
            None => false,
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validation_refuses_what_the_guarded_protocol_refuses() {
        assert_eq!(
            validate_label_pairs("", true, true),
            Err(LabelError::EmptyLabels)
        );
        assert_eq!(validate_label_pairs("", false, false), Ok(()));
        assert_eq!(
            validate_label_pairs("tako-gen", true, true),
            Err(LabelError::MalformedPair)
        );
        assert_eq!(
            validate_label_pairs("tako-gen=", true, true),
            Err(LabelError::EmptyValue)
        );
        assert_eq!(validate_label_pairs("tako-gen=", false, false), Ok(()));
        assert_eq!(
            validate_label_pairs("tako-gen=a bogus", true, true),
            Err(LabelError::MalformedPair)
        );
        assert_eq!(
            validate_label_pairs("tako-gen=a tako-gen=a", true, true),
            Err(LabelError::MalformedPair)
        );
        assert_eq!(
            validate_label_pairs("tako-gen=a ", true, true),
            Err(LabelError::MalformedPair)
        );
        assert_eq!(
            validate_label_pairs(" tako-gen=a", true, true),
            Err(LabelError::MalformedPair)
        );
        assert_eq!(
            validate_label_pairs("name=x", true, true),
            Err(LabelError::KeyReservedName)
        );
        assert_eq!(
            validate_label_pairs("a/b=x", true, true),
            Err(LabelError::KeyInvalidChar)
        );
        assert_eq!(
            validate_label_pairs("a=x/y", true, true),
            Err(LabelError::ValueInvalidChar)
        );
        assert_eq!(
            validate_label_pairs("=x", true, true),
            Err(LabelError::KeyEmpty)
        );
        assert_eq!(
            validate_label_pairs("tako-gen=gen1 tako-attempt=att1", true, true),
            Ok(())
        );
    }

    #[test]
    fn payload_is_sorted_by_key() {
        let labels = BTreeMap::from([
            ("tako-gen".to_string(), "g1".to_string()),
            ("tako-attempt".to_string(), "a1".to_string()),
        ]);
        assert_eq!(labels_to_payload(&labels), "tako-attempt=a1 tako-gen=g1");
        assert_eq!(labels_to_payload(&BTreeMap::new()), "");
    }

    #[test]
    fn parsing_reads_across_tokens_like_the_reference_reader() {
        assert_eq!(parse_labels("a=1 b=2"), [("a", "1"), ("b", "2")]);
        assert_eq!(parse_labels("a= b=2"), [("a", ""), ("b", "2")]);
        assert_eq!(parse_labels("a=1 junk"), [("a", "1")]);
        assert_eq!(parse_labels("a=1 junk b=2"), [("a", "1"), ("junk b", "2")]);
        assert_eq!(label_value("b", "a=1 junk b=2"), None);
        assert_eq!(label_value("b", "a=1 b=2"), Some("2"));
        assert_eq!(label_value("c", "a=1 b=2"), None);
    }

    #[test]
    fn matching_needs_every_required_pair() {
        let actual = "tako-attempt=a1 tako-gen=g1";
        assert!(label_pairs_match(actual, "tako-gen=g1"));
        assert!(label_pairs_match(actual, "tako-gen=g1 tako-attempt=a1"));
        assert!(!label_pairs_match(actual, "tako-gen=g2"));
        assert!(!label_pairs_match(actual, "tako-gen=g1 other=x"));
        assert!(
            !label_pairs_match(actual, ""),
            "an empty requirement never matches"
        );
        assert!(
            !label_pairs_match("tako-gen=g1 note=", "tako-gen=g1"),
            "an empty actual value fails the guard, as with existing clients"
        );
        assert!(
            !label_pairs_match("tako-gen=g1 bad", "tako-gen=g1"),
            "a malformed actual never matches"
        );
    }
}
