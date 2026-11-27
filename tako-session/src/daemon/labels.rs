/*
 * tako-session — session runtime for Tako
 * Copyright (c) 2026 Alexander Panasenko
 *
 * Contact: alex@prod.codes
 * Author: https://prod.codes/about/
 * Project: https://github.com/alex09x/tako-core
 * SPDX-License-Identifier: MIT
 */

//! The session's labels as the daemon keeps them.

use crate::ipc::{parse_labels, validate_label_pairs};
use std::collections::BTreeMap;

/// Applies `k=v` pairs; `k=` removes `k`.
pub fn apply(labels: &mut BTreeMap<String, String>, payload: &str) {
    for (key, value) in parse_labels(payload) {
        if value.is_empty() {
            labels.remove(key);
        } else {
            labels.insert(key.to_string(), value.to_string());
        }
    }
}

/// Whether `required` is a valid, non-empty list with values and every pair
/// has the same value in `labels`.
pub fn matches(labels: &BTreeMap<String, String>, required: &str) -> bool {
    if validate_label_pairs(required, true, true).is_err() {
        return false;
    }
    required
        .split(' ')
        .all(|token| match token.split_once('=') {
            Some((key, value)) => labels.get(key).map(String::as_str) == Some(value),
            None => false,
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn apply_sets_replaces_and_removes() {
        let mut l = BTreeMap::new();
        apply(&mut l, "a=1 b=2");
        apply(&mut l, "b=3 a=");
        assert_eq!(
            l.into_iter().collect::<Vec<_>>(),
            vec![("b".to_string(), "3".to_string())]
        );
    }

    #[test]
    fn matching_needs_every_pair_and_a_valid_request() {
        let mut l = BTreeMap::new();
        apply(&mut l, "owner=tako pane=7");
        assert!(matches(&l, "owner=tako"));
        assert!(matches(&l, "pane=7 owner=tako"));
        assert!(!matches(&l, "owner=other"));
        assert!(!matches(&l, "missing=1"));
        assert!(!matches(&l, ""));
        assert!(!matches(&l, "owner="));
    }
}
