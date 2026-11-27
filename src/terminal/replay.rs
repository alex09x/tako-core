/*
 * tako — Terminal emulator
 * Copyright (c) 2026 Alexander Panasenko
 *
 * Contact: alex@prod.codes
 * Author: https://prod.codes/about/
 * Project: https://github.com/alex09x/tako
 * SPDX-License-Identifier: MIT
 */

//! The terminal as VT bytes: written to an empty terminal of the same size,
//! they bring back what this one shows (scrollback, screen, styles, modes,
//! cursor, title, working directory). A session runtime sends them to a
//! client that attaches to a running session.

use super::{GraphemeWidthMethod, ScreenBuffer, Terminal};
use crate::charset::Charset;
use crate::cursor_style::CursorShape;
use crate::grid::{Cell, CellAttrs, Color, Grid};
use crate::modes::MouseTracking;

type Style = (Color, Color, CellAttrs, u8, Color);

/// `;<params>` for a colour: the short 30-37/90-97 forms for the 16 basic
/// colours (when `base` has them), else `<extended>;5;n` or `;2;r;g;b`.
fn color_params(out: &mut Vec<u8>, color: Color, base: Option<u16>, extended: u16) {
    match (color, base) {
        (Color::Default, _) => {}
        (Color::Indexed(n), Some(b)) if n < 8 => param(out, b + u16::from(n)),
        (Color::Indexed(n), Some(b)) if n < 16 => param(out, b + 60 + u16::from(n) - 8),
        (Color::Indexed(n), _) => {
            param(out, extended);
            param(out, 5);
            param(out, u16::from(n));
        }
        (Color::Rgb(r, g, b), _) => {
            param(out, extended);
            param(out, 2);
            for c in [r, g, b] {
                param(out, u16::from(c));
            }
        }
    }
}

/// `;n` in decimal, without the formatting machinery (a replay writes one
/// SGR per style run, and `write!` was a fifth of its time).
fn param(out: &mut Vec<u8>, n: u16) {
    out.push(b';');
    let mut digits = [0u8; 5];
    let mut len = 0;
    let mut v = n;
    loop {
        digits[len] = b'0' + (v % 10) as u8;
        len += 1;
        v /= 10;
        if v == 0 {
            break;
        }
    }
    out.extend(digits[..len].iter().rev());
}

fn sgr(style: Style) -> Vec<u8> {
    let mut out = Vec::with_capacity(24);
    sgr_into(&mut out, style);
    out
}

/// One SGR sequence that resets the pen and sets `style`, appended to
/// `out` without intermediate strings: a replay writes one per style run.
fn sgr_into(out: &mut Vec<u8>, (fg, bg, attrs, ul_style, ul_color): Style) {
    out.extend_from_slice(b"\x1b[0");
    for (flag, code) in [
        (CellAttrs::BOLD, &b";1"[..]),
        (CellAttrs::DIM, b";2"),
        (CellAttrs::ITALIC, b";3"),
        (CellAttrs::BLINK, b";5"),
        (CellAttrs::REVERSE, b";7"),
        (CellAttrs::HIDDEN, b";8"),
        (CellAttrs::STRIKETHROUGH, b";9"),
        (CellAttrs::OVERLINE, b";53"),
    ] {
        if attrs.contains(flag) {
            out.extend_from_slice(code);
        }
    }
    if attrs.contains(CellAttrs::UNDERLINE) {
        out.extend_from_slice(b";4");
        if ul_style > 1 {
            out.push(b':');
            out.push(b'0' + ul_style.min(9));
        }
    }
    color_params(out, fg, Some(30), 38);
    color_params(out, bg, Some(40), 48);
    // No short form for underline colours: every index is `58;5;n`.
    color_params(out, ul_color, None, 58);
    out.push(b'm');
}

fn style_of(cell: &Cell) -> Style {
    (
        cell.fg,
        cell.bg,
        cell.attrs,
        cell.underline_style,
        cell.underline_color,
    )
}

fn is_blank(cell: &Cell) -> bool {
    (cell.char == '\0' || cell.char == ' ') && cell.attrs.is_empty() && cell.bg == Color::Default
}

impl Terminal {
    /// One line of cells as styled text. `keep_trailing` keeps the blanks at
    /// the end (a soft-wrapped line needs its full width).
    /// `grid` is the screen the cells belong to (it holds their graphemes).
    fn styled_cells(
        grid: &Grid,
        cells: &[Cell],
        keep_trailing: bool,
        styled: bool,
        out: &mut Vec<u8>,
    ) {
        let end = if keep_trailing {
            cells.len()
        } else {
            cells
                .iter()
                .rposition(|c| !is_blank(c) && !c.is_wide_spacer)
                .map_or(0, |i| i + 1)
        };
        let mut last = None;
        for cell in &cells[..end] {
            if cell.is_wide_spacer {
                continue;
            }
            if styled {
                let style = style_of(cell);
                if last != Some(style) {
                    sgr_into(out, style);
                    last = Some(style);
                }
            }
            let ch = if cell.char == '\0' { ' ' } else { cell.char };
            if ch.is_ascii() {
                out.push(ch as u8);
            } else {
                out.extend_from_slice(ch.encode_utf8(&mut [0u8; 4]).as_bytes());
            }
            if cell.grapheme != 0 {
                out.extend_from_slice(grid.grapheme(cell).as_bytes());
            }
        }
        if styled && last.is_some() {
            out.extend_from_slice(b"\x1b[0m");
        }
    }

    /// Scrollback lines, oldest first, soft-wrapped lines joined, each
    /// logical line ending in CRLF (LF for plain text).
    fn scrollback_text(grid: &Grid, styled: bool, out: &mut Vec<u8>) {
        let len = grid.scrollback_len();
        for i in (0..len).rev() {
            let Some(line) = grid.scrollback_line(i) else {
                continue;
            };
            // The next (newer) line continues this one: keep its full width.
            let continues = if i == 0 {
                grid.is_line_wrapped(0)
            } else {
                grid.scrollback_line_wrapped(i - 1)
            };
            Self::styled_cells(grid, line, continues, styled, out);
            if !continues {
                out.extend_from_slice(if styled { b"\r\n" } else { b"\n" });
            }
        }
    }

    /// The live screen (wherever the viewport is scrolled), rows joined
    /// unless soft-wrapped.
    /// `cursor_row` is where that screen's cursor is: the rows down to it are
    /// written even when blank, so the cursor can be placed there.
    fn screen_text(grid: &Grid, cursor_row: usize, styled: bool, out: &mut Vec<u8>) {
        let rows = grid.rows();
        let last = (0..rows)
            .rev()
            .find(|&r| grid.row_cells(r).iter().any(|c| !is_blank(c)))
            .unwrap_or(0)
            .max(if styled {
                cursor_row.min(rows.saturating_sub(1))
            } else {
                0
            });
        for row in 0..=last.min(rows.saturating_sub(1)) {
            let next_continues = row + 1 < rows && grid.is_line_wrapped(row + 1);
            if row > 0 && !grid.is_line_wrapped(row) {
                out.extend_from_slice(if styled { b"\r\n" } else { b"\n" });
            }
            Self::styled_cells(grid, grid.row_cells(row), next_continues, styled, out);
        }
    }

    /// A screen's history then its rows, as a fresh destination screen
    /// receives them: the newest history lines are scrolled off (a scroll
    /// keeps them in scrollback on every terminal, an erase only on some),
    /// then the rows are drawn from home.
    fn screen_with_history(grid: &Grid, cursor_row: usize, out: &mut Vec<u8>) {
        if grid.scrollback_len() > 0 {
            Self::scrollback_text(grid, true, out);
            // The last history line ends in CRLF, except when the screen's
            // first row continues it: then one more line break is needed.
            let breaks = grid.rows().saturating_sub(1) + usize::from(grid.is_line_wrapped(0));
            out.extend_from_slice(&b"\r\n".repeat(breaks));
            out.extend_from_slice(b"\x1b[H\x1b[0m");
        }
        Self::screen_text(grid, cursor_row, true, out);
    }

    /// While a full-screen program runs, the shell's screen behind it: its
    /// history and rows, then the cursor and pen saved on entering the
    /// alternate screen, so leaving it later (1049l) lands where it would
    /// have. Entering with 1049h saves exactly that cursor again.
    fn write_primary_behind_alternate(&self, out: &mut Vec<u8>) {
        let saved = self.cursor.saved;
        let (row, col) = saved.map_or((0, 0), |s| (s.row, s.col));
        Self::screen_with_history(&self.primary, row, out);
        let rows = self.primary.rows();
        let cols = self.primary.cols();
        out.extend_from_slice(
            format!(
                "\x1b[{};{}H",
                row.min(rows.saturating_sub(1)) + 1,
                col.min(cols.saturating_sub(1)) + 1
            )
            .as_bytes(),
        );
        if let Some(s) = saved {
            sgr_into(out, (s.fg, s.bg, s.attrs, 0, Color::Default));
        }
        out.extend_from_slice(b"\x1b[?1049h\x1b[H\x1b[0m");
    }

    fn write_modes(&self, out: &mut Vec<u8>) {
        let m = &self.modes;
        let mut set = |on: bool, default: bool, code: &str| {
            if on != default {
                out.extend_from_slice(
                    format!("\x1b[{code}{}", if on { 'h' } else { 'l' }).as_bytes(),
                );
            }
        };
        set(m.cursor_key_app_mode, false, "?1");
        set(m.reverse_wrap, false, "?45");
        set(m.reverse_wrap_extended, false, "?1045");
        set(m.mouse_utf8, false, "?1005");
        set(m.mouse_sgr, false, "?1006");
        set(m.focus_events, false, "?1004");
        set(m.bracketed_paste, false, "?2004");
        set(m.alternate_scroll, true, "?1007");
        set(m.color_scheme_updates, false, "?2031");
        set(m.insert, false, "4");
        set(m.linefeed_mode, false, "20");
        set(m.left_right_margin_mode, false, "?69");
        let mouse = match m.mouse_tracking {
            MouseTracking::Off => None,
            MouseTracking::Normal => Some(1000),
            MouseTracking::ButtonEvent => Some(1002),
            MouseTracking::AnyEvent => Some(1003),
        };
        if let Some(code) = mouse {
            out.extend_from_slice(format!("\x1b[?{code}h").as_bytes());
        }
        if m.modify_other_keys != 0 {
            out.extend_from_slice(format!("\x1b[>4;{}m", m.modify_other_keys).as_bytes());
        }
    }

    fn write_cursor(&self, out: &mut Vec<u8>) {
        let rows = self.active_grid().rows();
        let cols = self.active_grid().cols();
        if self.scroll_top != 0 || self.scroll_bottom + 1 != rows {
            out.extend_from_slice(
                format!("\x1b[{};{}r", self.scroll_top + 1, self.scroll_bottom + 1).as_bytes(),
            );
        }
        if self.modes.left_right_margin_mode
            && (self.scroll_left != 0 || self.scroll_right + 1 != cols)
        {
            out.extend_from_slice(
                format!("\x1b[{};{}s", self.scroll_left + 1, self.scroll_right + 1).as_bytes(),
            );
        }
        let (row, col) = (
            self.cursor.row.min(rows.saturating_sub(1)),
            self.cursor.col.min(cols.saturating_sub(1)),
        );
        let grid = self.active_grid();
        // A wide glyph ending at the right edge leaves the cursor on its spacer.
        let lead = match grid.get(row, col) {
            Some(c) if c.is_wide_spacer && col > 0 => col - 1,
            _ => col,
        };
        let reprint = self
            .pending_wrap
            .then(|| grid.get(row, lead))
            .flatten()
            .filter(|c| !c.is_wide_spacer);
        let at = if reprint.is_some() { lead } else { col };
        if self.modes.origin_mode {
            // CUP counts from the margins now. A cursor outside them (DECRC
            // can put it there) lands where the destination clamps it.
            let left = if self.modes.left_right_margin_mode {
                self.scroll_left
            } else {
                0
            };
            out.extend_from_slice(b"\x1b[?6h");
            out.extend_from_slice(
                format!(
                    "\x1b[{};{}H",
                    row.saturating_sub(self.scroll_top) + 1,
                    at.saturating_sub(left) + 1
                )
                .as_bytes(),
            );
        } else {
            out.extend_from_slice(format!("\x1b[{};{}H", row + 1, at + 1).as_bytes());
        }
        if let Some(cell) = reprint {
            // Reprinting the last glyph sets the pending wrap again; nothing
            // that moves the cursor may follow it.
            out.extend_from_slice(&sgr(style_of(cell)));
            let ch = if cell.char == '\0' { ' ' } else { cell.char };
            out.extend_from_slice(ch.encode_utf8(&mut [0u8; 4]).as_bytes());
            out.extend_from_slice(grid.grapheme(cell).as_bytes());
        }
        if !self.modes.autowrap {
            out.extend_from_slice(b"\x1b[?7l");
        }
        let c = &self.cursor;
        out.extend_from_slice(&sgr((
            c.fg,
            c.bg,
            c.attrs,
            c.underline_style,
            c.underline_color,
        )));
        if self.cursor_style_overridden {
            let base = match self.cursor_style.shape {
                CursorShape::Block => 1,
                CursorShape::Underline => 3,
                CursorShape::Bar => 5,
            };
            out.extend_from_slice(
                format!("\x1b[{} q", base + u8::from(!self.cursor_style.blinking)).as_bytes(),
            );
        }
        let flags = self.kitty_keyboard_flags();
        if flags != 0 {
            out.extend_from_slice(format!("\x1b[>{flags}u").as_bytes());
        }
        if !self.cursor_visible {
            out.extend_from_slice(b"\x1b[?25l");
        }
    }

    /// Character sets the program designated and shifted to, so what it
    /// prints after the replay is translated as before (line drawing).
    /// The replayed cells already hold translated glyphs: this goes last.
    fn write_charsets(&self, out: &mut Vec<u8>) {
        for (slot, charset) in [
            (b'(', self.g0),
            (b')', self.g1),
            (b'*', self.g2),
            (b'+', self.g3),
        ] {
            let fin = match charset {
                Charset::Ascii => continue,
                Charset::DecSpecialGraphics => b'0',
                Charset::British => b'A',
            };
            out.extend_from_slice(&[0x1b, slot, fin]);
        }
        if self.shift_out {
            out.push(0x0e);
        }
        match self.gr_slot {
            1 => out.extend_from_slice(b"\x1b~"),
            2 => out.extend_from_slice(b"\x1b}"),
            3 => out.extend_from_slice(b"\x1b|"),
            _ => {}
        }
    }

    /// Palette entries a program redefined (OSC 4), and the default
    /// foreground, background and cursor colours it set (OSC 10-12).
    fn write_color_overrides(&self, out: &mut Vec<u8>) {
        for i in 0..=255u8 {
            if self.palette.is_overridden(i) {
                let (r, g, b) = self.palette.get(i);
                out.extend_from_slice(
                    format!("\x1b]4;{i};rgb:{r:02x}/{g:02x}/{b:02x}\x1b\\").as_bytes(),
                );
            }
        }
        let overrides = [
            (10, self.palette.fg_overridden(), self.default_fg),
            (11, self.palette.bg_overridden(), self.default_bg),
            (12, self.palette.cursor_overridden(), self.cursor_color),
        ];
        for (code, overridden, color) in overrides {
            if let (true, Some((r, g, b))) = (overridden, color) {
                out.extend_from_slice(
                    format!("\x1b]{code};rgb:{r:02x}/{g:02x}/{b:02x}\x1b\\").as_bytes(),
                );
            }
        }
    }

    fn write_pwd(&self, out: &mut Vec<u8>) {
        if let Some(pwd) = self.last_cwd.as_deref().filter(|p| !p.is_empty()) {
            out.extend_from_slice(format!("\x1b]7;{pwd}\x1b\\").as_bytes());
        }
    }

    /// The working directory the program last reported (OSC 7), as sent.
    pub fn working_directory(&self) -> Option<&str> {
        self.last_cwd.as_deref()
    }

    /// The bytes that restore this terminal on an empty one of the same size.
    /// Synchronized output is left out: it belongs to the client a program
    /// was drawing for.
    pub fn replay_state(&self) -> Vec<u8> {
        let grid = self.active_grid();
        let lines = grid.scrollback_len() + grid.rows();
        let mut out = Vec::with_capacity(lines * (grid.cols() + 16));
        self.write_color_overrides(&mut out);
        // Cluster widths decide where every later cell lands: set them first.
        let cluster_default = self.grapheme_width_method == GraphemeWidthMethod::Unicode;
        if self.modes.grapheme_cluster != cluster_default {
            out.extend_from_slice(if self.modes.grapheme_cluster {
                b"\x1b[?2027h"
            } else {
                b"\x1b[?2027l"
            });
        }
        if self.active_screen() == ScreenBuffer::Alternate {
            self.write_primary_behind_alternate(&mut out);
            Self::screen_text(&self.alternate, self.cursor.row, true, &mut out);
        } else {
            Self::screen_with_history(&self.primary, self.cursor.row, &mut out);
        }
        self.write_modes(&mut out);
        self.write_cursor(&mut out);
        self.write_charsets(&mut out);
        self.write_pwd(&mut out);
        if !self.title.is_empty() {
            out.extend_from_slice(format!("\x1b]2;{}\x07", self.title).as_bytes());
        }
        out
    }
}
