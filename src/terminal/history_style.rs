/*
 * tako — Terminal emulator
 * Copyright (c) 2026 Alexander Panasenko
 *
 * Contact: alex@prod.codes
 * Author: https://prod.codes/about/
 * Project: https://github.com/alex09x/tako
 * SPDX-License-Identifier: MIT
 */

//! A cell's style as `Terminal::history` writes it: one SGR sequence per
//! attribute for VT, inline CSS for HTML.

use crate::grid::{Cell, CellAttrs, Color};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct HistoryStyle {
    fg: Color,
    bg: Color,
    attrs: CellAttrs,
    /// 0 none, 1 single, 2 double, 3 curly, 4 dotted, 5 dashed.
    underline: u8,
    underline_color: Color,
}

impl HistoryStyle {
    pub(super) const DEFAULT: HistoryStyle = HistoryStyle {
        fg: Color::Default,
        bg: Color::Default,
        attrs: CellAttrs::empty(),
        underline: 0,
        underline_color: Color::Default,
    };

    pub(super) fn of(cell: &Cell) -> HistoryStyle {
        Self::from_parts(
            cell.fg,
            cell.bg,
            cell.attrs,
            cell.underline_style,
            cell.underline_color,
        )
    }

    pub(super) fn from_parts(
        fg: Color,
        bg: Color,
        attrs: CellAttrs,
        underline_style: u8,
        underline_color: Color,
    ) -> HistoryStyle {
        let underline = if attrs.contains(CellAttrs::UNDERLINE) {
            underline_style.max(1)
        } else {
            0
        };
        HistoryStyle {
            fg,
            bg,
            attrs: attrs - CellAttrs::UNDERLINE,
            underline,
            underline_color,
        }
    }

    pub(super) fn is_default(&self) -> bool {
        *self == Self::DEFAULT
    }

    /// `ESC[0m`, then one sequence per attribute and colour.
    pub(super) fn vt(&self) -> String {
        let mut out = String::from("\x1b[0m");
        for (flag, code) in [
            (CellAttrs::BOLD, "1"),
            (CellAttrs::DIM, "2"),
            (CellAttrs::ITALIC, "3"),
            (CellAttrs::BLINK, "5"),
            (CellAttrs::REVERSE, "7"),
            (CellAttrs::HIDDEN, "8"),
            (CellAttrs::STRIKETHROUGH, "9"),
            (CellAttrs::OVERLINE, "53"),
        ] {
            if self.attrs.contains(flag) {
                out.push_str(&format!("\x1b[{code}m"));
            }
        }
        match self.underline {
            0 => {}
            1 => out.push_str("\x1b[4m"),
            n => out.push_str(&format!("\x1b[4:{n}m")),
        }
        for (prefix, color) in [(38, self.fg), (48, self.bg), (58, self.underline_color)] {
            match color {
                Color::Default => {}
                Color::Indexed(n) => out.push_str(&format!("\x1b[{prefix};5;{n}m")),
                Color::Rgb(r, g, b) => out.push_str(&format!("\x1b[{prefix};2;{r};{g};{b}m")),
            }
        }
        out
    }

    /// The CSS properties, each ending in `;`.
    pub(super) fn css(&self) -> String {
        let mut out = String::new();
        for (property, color) in [
            ("color", self.fg),
            ("background-color", self.bg),
            ("text-decoration-color", self.underline_color),
        ] {
            match color {
                Color::Default => {}
                Color::Indexed(n) => out.push_str(&format!("{property}: var(--vt-palette-{n});")),
                Color::Rgb(r, g, b) => out.push_str(&format!("{property}: rgb({r}, {g}, {b});")),
            }
        }
        let a = self.attrs;
        let lines = [
            (self.underline != 0, " underline"),
            (a.contains(CellAttrs::STRIKETHROUGH), " line-through"),
            (a.contains(CellAttrs::OVERLINE), " overline"),
            (a.contains(CellAttrs::BLINK), " blink"),
        ];
        if lines.iter().any(|(on, _)| *on) {
            out.push_str("text-decoration-line:");
            lines
                .iter()
                .filter(|(on, _)| *on)
                .for_each(|(_, name)| out.push_str(name));
            out.push(';');
        }
        let decoration = match self.underline {
            0 => "",
            2 => "double",
            3 => "wavy",
            4 => "dotted",
            5 => "dashed",
            _ => "solid",
        };
        if !decoration.is_empty() {
            out.push_str(&format!("text-decoration-style: {decoration};"));
        }
        for (flag, css) in [
            (CellAttrs::BOLD, "font-weight: bold;"),
            (CellAttrs::ITALIC, "font-style: italic;"),
            (CellAttrs::DIM, "opacity: 0.5;"),
            (CellAttrs::HIDDEN, "visibility: hidden;"),
            (CellAttrs::REVERSE, "filter: invert(100%);"),
        ] {
            if a.contains(flag) {
                out.push_str(css);
            }
        }
        out
    }
}

/// One character as HTML text.
pub(super) fn html_char(c: char, out: &mut String) {
    match c {
        '<' => out.push_str("&lt;"),
        '>' => out.push_str("&gt;"),
        '&' => out.push_str("&amp;"),
        '"' => out.push_str("&quot;"),
        '\'' => out.push_str("&#39;"),
        c if c.is_ascii() => out.push(c),
        c => out.push_str(&format!("&#{};", c as u32)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vt_writes_one_sequence_per_attribute() {
        let s = HistoryStyle::from_parts(
            Color::Indexed(1),
            Color::Rgb(1, 2, 3),
            CellAttrs::BOLD | CellAttrs::UNDERLINE,
            3,
            Color::Indexed(4),
        );
        assert_eq!(
            s.vt(),
            "\x1b[0m\x1b[1m\x1b[4:3m\x1b[38;5;1m\x1b[48;2;1;2;3m\x1b[58;5;4m"
        );
        assert!(
            HistoryStyle::from_parts(
                Color::Default,
                Color::Default,
                CellAttrs::empty(),
                3,
                Color::Default
            )
            .is_default()
        );
    }

    #[test]
    fn css_follows_colour_line_decoration_then_weight_order() {
        let s = HistoryStyle::from_parts(
            Color::Indexed(9),
            Color::Default,
            CellAttrs::UNDERLINE | CellAttrs::STRIKETHROUGH | CellAttrs::BOLD,
            1,
            Color::Default,
        );
        assert_eq!(
            s.css(),
            "color: var(--vt-palette-9);text-decoration-line: underline line-through;text-decoration-style: solid;font-weight: bold;"
        );
        let mut out = String::new();
        "<'é&".chars().for_each(|c| html_char(c, &mut out));
        assert_eq!(out, "&lt;&#39;&#233;&amp;");
    }
}
