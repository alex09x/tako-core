/*
 * tako — Terminal emulator
 * Copyright (c) 2026 Alexander Panasenko
 *
 * Contact: alex@prod.codes
 * Author: https://prod.codes/about/
 * Project: https://github.com/alex09x/tako
 * SPDX-License-Identifier: MIT
 */

//! `Terminal::history`: scrollback and screen as plain text, VT or HTML,
//! the way session runtimes have always printed it. Rows are kept as they
//! are on screen (a soft-wrapped line stays split), blank rows and blank
//! cells are written only when text follows them, and plain text drops
//! trailing spaces.

use super::history_style::{HistoryStyle, html_char};
use super::{GraphemeWidthMethod, ScreenBuffer, Terminal};
use crate::charset::Charset;
use crate::grid::Cell;
use crate::modes::MouseTracking;

/// How `Terminal::history` renders the buffer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HistoryFormat {
    Plain,
    Vt,
    Html,
}

struct Rows<'t> {
    term: &'t Terminal,
    format: HistoryFormat,
    out: String,
    style: HistoryStyle,
    /// Rows ended but not yet terminated: written once text follows.
    blank_rows: usize,
    /// Blank cells not yet written: written once text follows on the row.
    blank_cells: usize,
}

impl<'t> Rows<'t> {
    fn new(term: &'t Terminal, format: HistoryFormat) -> Rows<'t> {
        Rows {
            term,
            format,
            out: String::new(),
            style: HistoryStyle::DEFAULT,
            blank_rows: 0,
            blank_cells: 0,
        }
    }

    fn styled(&self) -> bool {
        self.format != HistoryFormat::Plain
    }

    fn close_style(&mut self) {
        self.out.push_str(if self.format == HistoryFormat::Html {
            "</div>"
        } else {
            "\x1b[0m"
        });
    }

    fn row(&mut self, cells: &[Cell]) {
        if !cells.iter().any(|c| !c.is_wide_spacer && c.char != '\0') {
            self.blank_rows += 1;
            return;
        }
        if self.blank_rows > 0 {
            if !self.style.is_default() {
                self.close_style();
                self.style = HistoryStyle::DEFAULT;
            }
            let newline = if self.format == HistoryFormat::Vt {
                "\r\n"
            } else {
                "\n"
            };
            self.out.push_str(&newline.repeat(self.blank_rows));
            self.blank_rows = 0;
        }
        self.blank_rows += 1;
        self.blank_cells = 0;
        for cell in cells.iter().filter(|c| !c.is_wide_spacer) {
            let style = HistoryStyle::of(cell);
            let written = self.styled() && (cell.char != '\0' || !style.is_default());
            if !written && (cell.char == '\0' || cell.char == ' ') {
                self.blank_cells += 1;
                continue;
            }
            self.out.push_str(&" ".repeat(self.blank_cells));
            self.blank_cells = 0;
            if self.styled() && style != self.style {
                let html = self.format == HistoryFormat::Html;
                if !self.style.is_default() && (html || style.is_default()) {
                    self.close_style();
                }
                self.style = style;
                if !style.is_default() {
                    let open = if html {
                        format!("<div style=\"display: inline;{}\">", style.css())
                    } else {
                        style.vt()
                    };
                    self.out.push_str(&open);
                }
            }
            self.cell(cell);
        }
    }

    fn cell(&mut self, cell: &Cell) {
        if cell.char == '\0' {
            self.out.push(' ');
            return;
        }
        let grapheme = self.term.active_grid().grapheme(cell);
        for c in std::iter::once(cell.char).chain(grapheme.chars()) {
            if self.format == HistoryFormat::Html {
                html_char(c, &mut self.out);
            } else {
                self.out.push(c);
            }
        }
    }

    fn finish(&mut self) {
        if !self.style.is_default() {
            self.close_style();
            self.style = HistoryStyle::DEFAULT;
        }
    }
}

impl Terminal {
    /// Scrollback and screen as plain text, as VT (with the modes, scroll
    /// region, cursor, pen and working directory that restore it) or as HTML.
    pub fn history(&self, format: HistoryFormat) -> Vec<u8> {
        let mut rows = Rows::new(self, format);
        match format {
            HistoryFormat::Html => {
                rows.out.push_str("<style>:root{");
                for (i, (r, g, b)) in self.palette.colors().iter().enumerate() {
                    rows.out
                        .push_str(&format!("--vt-palette-{i}: #{r:02x}{g:02x}{b:02x};"));
                }
                rows.out
                    .push_str("}</style><div style=\"font-family: monospace; white-space: pre;\">");
            }
            HistoryFormat::Vt => rows.out.push_str(&self.history_modes()),
            HistoryFormat::Plain => {}
        }
        let grid = self.active_grid();
        for i in (0..grid.scrollback_len()).rev() {
            if let Some(line) = grid.scrollback_line(i) {
                rows.row(line);
            }
        }
        // The live screen, wherever the viewport is scrolled.
        for r in 0..grid.rows() {
            rows.row(grid.row_cells(r));
        }
        rows.finish();
        if format == HistoryFormat::Html {
            rows.out.push_str("</div>");
        }
        let mut out = rows.out;
        if format == HistoryFormat::Vt {
            self.history_state(&mut out);
        }
        out.into_bytes()
    }

    /// Modes that differ from their defaults, in mode-number order (ANSI
    /// modes first).
    fn history_modes(&self) -> String {
        let m = &self.modes;
        let mouse = |kind| m.mouse_tracking == kind;
        let modes: [(bool, bool, &str); 21] = [
            (m.insert, false, "4"),
            (m.linefeed_mode, false, "20"),
            (m.cursor_key_app_mode, false, "?1"),
            (m.origin_mode, false, "?6"),
            (m.autowrap, true, "?7"),
            (self.cursor_visible, true, "?25"),
            (m.reverse_wrap, false, "?45"),
            (m.left_right_margin_mode, false, "?69"),
            (mouse(MouseTracking::Normal), false, "?1000"),
            (mouse(MouseTracking::ButtonEvent), false, "?1002"),
            (mouse(MouseTracking::AnyEvent), false, "?1003"),
            (m.focus_events, false, "?1004"),
            (m.mouse_utf8, false, "?1005"),
            (m.mouse_sgr, false, "?1006"),
            (m.alternate_scroll, true, "?1007"),
            (m.reverse_wrap_extended, false, "?1045"),
            (
                self.active_screen() == ScreenBuffer::Alternate,
                false,
                "?1049",
            ),
            (m.bracketed_paste, false, "?2004"),
            (m.synchronized_output, false, "?2026"),
            (
                m.grapheme_cluster,
                self.grapheme_width_method == GraphemeWidthMethod::Unicode,
                "?2027",
            ),
            (m.color_scheme_updates, false, "?2031"),
        ];
        modes
            .iter()
            .filter(|(on, default, _)| on != default)
            .map(|(on, _, code)| format!("\x1b[{code}{}", if *on { 'h' } else { 'l' }))
            .collect()
    }

    /// After the text: scroll region, keyboard, cursor (re-printing the last
    /// cell when a wrap is pending), pen, protection, kitty keyboard,
    /// character sets, working directory.
    fn history_state(&self, out: &mut String) {
        let grid = self.active_grid();
        let (rows, cols) = (grid.rows(), grid.cols());
        if self.scroll_top != 0 || self.scroll_bottom + 1 != rows {
            out.push_str(&format!(
                "\x1b[{};{}r",
                self.scroll_top + 1,
                self.scroll_bottom + 1
            ));
        }
        if self.scroll_left != 0 || self.scroll_right + 1 != cols {
            out.push_str(&format!(
                "\x1b[{};{}s",
                self.scroll_left + 1,
                self.scroll_right + 1
            ));
        }
        if self.modes.modify_other_keys == 2 {
            out.push_str("\x1b[>4;2m");
        }
        let (row, col) = (
            self.cursor.row.min(rows.saturating_sub(1)),
            self.cursor.col.min(cols.saturating_sub(1)),
        );
        if !self.pending_wrap || col + 1 != cols {
            out.push_str(&format!("\x1b[{};{}H", row + 1, col + 1));
        } else {
            let line = grid.row_cells(row);
            let start = if line[col].is_wide_spacer && col > 0 {
                col - 1
            } else {
                col
            };
            out.push_str(&format!("\x1b[{};{}H", row + 1, start + 1));
            let mut cell = Rows::new(self, HistoryFormat::Vt);
            cell.row(&line[col..=col]);
            cell.finish();
            out.push_str(&cell.out);
        }
        let c = &self.cursor;
        out.push_str(
            &HistoryStyle::from_parts(c.fg, c.bg, c.attrs, c.underline_style, c.underline_color)
                .vt(),
        );
        if self.protected_mode != super::ProtectedMode::Off {
            out.push_str("\x1b[1\"q");
        }
        let flags = self.kitty_keyboard_flags();
        if flags != 0 {
            out.push_str(&format!("\x1b[={flags};1u"));
        }
        let slots = [
            ('(', self.g0),
            (')', self.g1),
            ('*', self.g2),
            ('+', self.g3),
        ];
        for (i, (slot, charset)) in slots.into_iter().enumerate() {
            let fin = match charset {
                Charset::DecSpecialGraphics => '0',
                Charset::British => 'A',
                Charset::Ascii if self.charsets_designated[i] => 'B',
                Charset::Ascii => continue,
            };
            out.push_str(&format!("\x1b{slot}{fin}"));
        }
        if self.shift_out {
            out.push('\x0e');
        }
        match self.gr_slot {
            1 => out.push_str("\x1b~"),
            3 => out.push_str("\x1b|"),
            _ => {}
        }
        if let Some(pwd) = self.working_directory().filter(|p| !p.is_empty()) {
            out.push_str(&format!("\x1b]7;{pwd}\x1b\\"));
        }
    }
}
