/*
 * tako — Terminal emulator
 * Copyright (c) 2026 Alexander Panasenko
 *
 * Contact: alex@prod.codes
 * Author: https://prod.codes/about/
 * Project: https://github.com/alex09x/tako
 * SPDX-License-Identifier: MIT
 */

//! `Terminal::replay_state` written to an empty terminal of the same size
//! restores what the source shows.

use tako_core::terminal::{HistoryFormat, ScreenBuffer, Terminal};

fn source(cols: usize, rows: usize, bytes: &[u8]) -> Terminal {
    let mut t = Terminal::new(cols, rows);
    t.feed(bytes);
    t
}

fn replayed(src: &Terminal) -> Terminal {
    let grid = src.active_grid();
    let mut t = Terminal::new(grid.cols(), grid.rows());
    t.feed(&src.replay_state());
    t
}

/// Each visible row as (text, styles per cell).
fn screen(t: &Terminal) -> Vec<(String, Vec<String>)> {
    (0..t.active_grid().rows())
        .map(|r| {
            let cells = t.viewport_row(r);
            let text: String = cells
                .iter()
                .map(|c| if c.char == '\0' { ' ' } else { c.char })
                .collect();
            let styles = cells
                .iter()
                .map(|c| format!("{:?}{:?}{:?}", c.fg, c.bg, c.attrs))
                .collect();
            (text.trim_end().to_string(), styles)
        })
        .collect()
}

fn assert_same(src: &Terminal, dst: &Terminal) {
    assert_eq!(screen(dst), screen(src), "screen");
    assert_eq!(dst.cursor(), src.cursor(), "cursor");
    assert_eq!(dst.pending_wrap(), src.pending_wrap(), "pending wrap");
    assert_eq!(
        dst.cursor_visible(),
        src.cursor_visible(),
        "cursor visibility"
    );
    assert_eq!(dst.title(), src.title(), "title");
    assert_eq!(
        dst.working_directory(),
        src.working_directory(),
        "working directory"
    );
    assert_eq!(dst.active_screen(), src.active_screen(), "active screen");
}

#[test]
fn styled_text_and_cursor_come_back() {
    let src = source(20, 5, b"plain \x1b[1;31mbold red\x1b[0m \x1b[48;5;100mbg\x1b[0m\r\n\x1b[4:3mcurly\x1b[0m x\x1b[2;3H");
    assert_same(&src, &replayed(&src));
}

#[test]
fn soft_wrapped_lines_rewrap_at_the_same_width() {
    let src = source(10, 5, b"0123456789abcdefghij-wraps\r\nnext");
    let dst = replayed(&src);
    assert_same(&src, &dst);
    assert!(
        dst.viewport_line_wrapped(1),
        "the continuation stays a soft wrap"
    );
}

#[test]
fn scrollback_comes_back_before_the_screen() {
    let lines: Vec<u8> = (0..12)
        .flat_map(|i| format!("line {i}\r\n").into_bytes())
        .collect();
    let src = source(20, 4, &lines);
    assert!(src.active_grid().scrollback_len() > 0);
    let dst = replayed(&src);
    assert_same(&src, &dst);
    let history = String::from_utf8(dst.history(HistoryFormat::Plain)).unwrap();
    assert!(
        history.contains("line 0") && history.contains("line 11"),
        "{history}"
    );
}

#[test]
fn modes_region_keyboard_and_cursor_style_come_back() {
    let src = source(
        30,
        8,
        b"\x1b[?1h\x1b[?2004h\x1b[?1002h\x1b[?1006h\x1b[?1004h\x1b[2;6r\x1b[>5u\x1b[6 q\x1b[?25l\x1b[3;4Hhi",
    );
    let dst = replayed(&src);
    assert_same(&src, &dst);
    let (m, n) = (src.modes(), dst.modes());
    assert_eq!(
        (
            n.cursor_key_app_mode,
            n.bracketed_paste,
            n.mouse_sgr,
            n.focus_events
        ),
        (
            m.cursor_key_app_mode,
            m.bracketed_paste,
            m.mouse_sgr,
            m.focus_events
        )
    );
    assert_eq!(n.mouse_tracking, m.mouse_tracking);
    assert_eq!(dst.kitty_keyboard_flags(), 5);
    assert_eq!(dst.cursor_style(), src.cursor_style());
}

#[test]
fn the_alternate_screen_comes_back_active() {
    let src = source(20, 5, b"shell\r\n\x1b[?1049h\x1b[Htop row\x1b[3;1Hthird");
    assert_eq!(src.active_screen(), ScreenBuffer::Alternate);
    assert_same(&src, &replayed(&src));
}

#[test]
fn a_pending_wrap_survives() {
    let src = source(10, 3, b"0123456789");
    assert!(src.pending_wrap());
    let mut dst = replayed(&src);
    assert_same(&src, &dst);
    dst.feed(b"X");
    assert_eq!(
        screen(&dst)[1].0,
        "X",
        "the next character wraps as it would have"
    );
}

#[test]
fn title_working_directory_and_colour_overrides_come_back() {
    let src = source(
        20,
        3,
        b"\x1b]2;my title\x07\x1b]7;file://host/tmp\x1b\\\x1b]11;rgb:10/20/30\x1b\\text",
    );
    let dst = replayed(&src);
    assert_same(&src, &dst);
    assert_eq!(dst.default_colors().1, Some((0x10, 0x20, 0x30)));
}

#[test]
fn every_history_line_survives_including_the_newest() {
    let mut bytes: Vec<u8> = (0..12)
        .flat_map(|i| format!("line {i}\r\n").into_bytes())
        .collect();
    // One logical line that wraps from the history into the screen.
    bytes.extend_from_slice(b"\x1b[2J\x1b[H");
    let mut wrapped = (0..30)
        .flat_map(|i| format!("w{i:02}").into_bytes())
        .collect::<Vec<u8>>();
    wrapped.extend_from_slice(b"\r\nend");
    for input in [bytes.clone(), [bytes, wrapped].concat()] {
        let src = source(20, 4, &input);
        let dst = replayed(&src);
        assert_same(&src, &dst);
        let text = |t: &Terminal| String::from_utf8(t.history(HistoryFormat::Plain)).unwrap();
        assert_eq!(text(&dst), text(&src));
    }
}

#[test]
fn an_origin_mode_cursor_above_the_region_replays_without_panicking() {
    // DECRC puts back a cursor that the new top margin now lies below.
    let src = source(20, 8, b"\x1b[?6h\x1b[1;1H\x1b7\x1b[5;8r\x1b8x");
    assert!(
        src.modes().origin_mode && src.cursor().0 < 4,
        "{:?}",
        src.cursor()
    );
    let dst = replayed(&src);
    assert!(dst.modes().origin_mode);
}

#[test]
fn a_wide_glyph_at_the_right_edge_keeps_its_pending_wrap() {
    let src = source(10, 3, "12345678漢".as_bytes());
    assert!(src.pending_wrap());
    let mut dst = replayed(&src);
    assert_same(&src, &dst);
    dst.feed(b"X");
    assert_eq!(screen(&dst)[0].0, "12345678漢");
    assert_eq!(screen(&dst)[1].0, "X");
}

#[test]
fn palette_underline_colour_and_cluster_width_come_back() {
    for cluster in [b"\x1b[?2027l", b"\x1b[?2027h"] {
        let mut bytes = b"\x1b]4;1;rgb:12/34/56\x1b\\\x1b[4;58;5;3mU\x1b[0m ".to_vec();
        bytes.extend_from_slice(cluster);
        bytes.extend_from_slice("❤\u{fe0f}x".as_bytes());
        let src = source(20, 3, &bytes);
        let dst = replayed(&src);
        assert_same(&src, &dst);
        assert_eq!(dst.palette().get(1), (0x12, 0x34, 0x56));
        assert_eq!(
            dst.viewport_row(0)[0].underline_color,
            src.viewport_row(0)[0].underline_color
        );
    }
}

#[test]
fn the_shell_screen_behind_a_full_screen_program_comes_back_when_it_exits() {
    // A shell with history, then vim-like full screen; the client returns
    // while it runs, and the program exits afterwards.
    let mut bytes: Vec<u8> = (0..30)
        .flat_map(|i| format!("\x1b[3{}mold {i}\x1b[0m\r\n", i % 8).into_bytes())
        .collect();
    bytes.extend_from_slice(b"$ vim notes\x1b[?1049h\x1b[2J\x1b[H~\r\n~\r\n\x1b[1;5Hediting");
    let mut src = source(30, 6, &bytes);
    let mut dst = replayed(&src);
    assert_same(&src, &dst);
    for t in [&mut src, &mut dst] {
        t.feed(b"\x1b[?1049l\r\n$ ");
    }
    assert_same(&src, &dst);
    let text = |t: &Terminal| String::from_utf8(t.history(HistoryFormat::Plain)).unwrap();
    assert_eq!(text(&dst), text(&src));
    assert!(
        text(&dst).contains("old 0") && text(&dst).contains("$ vim notes"),
        "{}",
        text(&dst)
    );
}

#[test]
fn line_drawing_stays_on_after_a_replay() {
    // A program draws a border, and keeps drawing after the client returns.
    let src = source(20, 3, b"\x1b(0lqqk");
    let mut dst = replayed(&src);
    assert_same(&src, &dst);
    let mut live = source(20, 3, b"\x1b(0lqqk");
    dst.feed(b"q");
    live.feed(b"q");
    assert_eq!(screen(&dst), screen(&live));
    assert_eq!(screen(&dst)[0].0, "┌──┐─");
}

#[test]
fn plain_history_has_no_escape_sequences() {
    let src = source(20, 3, b"\x1b[1mbold\x1b[0m\r\nsecond");
    let plain = src.history(HistoryFormat::Plain);
    assert!(!plain.contains(&0x1b));
    assert_eq!(String::from_utf8(plain).unwrap(), "bold\nsecond");
}
