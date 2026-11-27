/*
 * tako — Terminal emulator
 * Copyright (c) 2026 Alexander Panasenko
 *
 * Contact: alex@prod.codes
 * Author: https://prod.codes/about/
 * Project: https://github.com/alex09x/tako
 * SPDX-License-Identifier: MIT
 */

//! `Terminal::history` prints what session runtimes have always printed:
//! each case's bytes went into a 40x10 terminal of the reference runtime,
//! and its plain, VT and HTML history were recorded
//! (scripts/record-history-fixtures.py).

use tako_core::terminal::{HistoryFormat, Terminal};

const CASES: [&str; 5] = ["styles", "wrap", "scrollback", "alternate", "erase-bg"];

fn fixture(name: &str) -> Vec<u8> {
    let path = format!(
        "{}/tests/fixtures/history/{name}",
        env!("CARGO_MANIFEST_DIR")
    );
    std::fs::read(&path).unwrap_or_else(|e| panic!("{path}: {e}"))
}

fn shown(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).replace('\x1b', "⎋")
}

#[test]
fn history_matches_the_reference_in_every_format() {
    for case in CASES {
        let mut term = Terminal::with_scrollback(40, 10, 10_000);
        term.feed(&fixture(&format!("{case}.in")));
        for (format, ext) in [
            (HistoryFormat::Plain, "plain"),
            (HistoryFormat::Vt, "vt"),
            (HistoryFormat::Html, "html"),
        ] {
            let want = fixture(&format!("{case}.{ext}"));
            assert_eq!(shown(&term.history(format)), shown(&want), "{case}.{ext}");
        }
    }
}
