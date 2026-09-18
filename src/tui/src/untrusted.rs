//! Text an extension wrote, made safe to draw.
//!
//! Every other string this client renders comes from the core or from the
//! person typing: a turn's answer, a tool result, a command in
//! [`crate::commands`]. Contributions are the first text from a **sandboxed
//! third party** (`wit/client-surface.wit`), and the contract there says every
//! renderer treats them as data. This is where the TUI does.
//!
//! # What a terminal lets a string do
//!
//! A frame is bytes on a stream, so text that reaches it unchecked is not text
//! — it is instructions. `\x1b[2J` clears the screen, `\x1b[6n` makes the
//! terminal *write back* into the input this client is reading, `\x1b]0;…\x07`
//! retitles the window, and a lone `\r` redraws a line over itself. None of
//! these need a bug to work; they are the terminal behaving correctly on text
//! that should never have been sent.
//!
//! # What is removed, and why each
//!
//! - **C0 controls and `DEL`.** Includes `ESC`, which is the introducer for
//!   every CSI and OSC sequence — removing it is what makes the rest inert
//!   rather than trying to recognise sequences and getting the grammar wrong.
//! - **C1 controls (`U+0080`–`U+009F`).** Some terminals accept `U+009B` as a
//!   CSI introducer directly, so dropping `ESC` alone is not enough.
//! - **Bidi overrides** (`U+202A`–`U+202E`, `U+2066`–`U+2069`). These reorder
//!   what is drawn without changing what is stored — the Trojan Source trick —
//!   so a label can read as one thing and be another. A contribution has no
//!   legitimate use for them; text that genuinely needs bidi is laid out by
//!   its own characters' direction, which is untouched.
//!
//! Everything else survives, including emoji, CJK and combining marks — the
//! point is to remove *instructions*, not to reduce contributions to ASCII.
//!
//! # Width is a safety property here
//!
//! A label wider than its cell does not overflow into the next widget in
//! `ratatui`; it is clipped. But a caller that measures the *unclipped* string
//! lays out around a width that is never drawn, so the truncation happens here,
//! in cells and never mid-grapheme.

use unicode_segmentation::UnicodeSegmentation;

use crate::wrap::width;

/// Whether `c` must not reach a frame.
const fn is_hostile(c: char) -> bool {
    // C0 + DEL, then C1. `is_control` covers both ranges; naming them is for
    // the reader, since the *reason* differs (introducers vs. a second
    // introducer some terminals honour).
    if c.is_control() {
        return true;
    }
    matches!(c, '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
}

/// `text` with everything a terminal would obey removed.
///
/// Characters are dropped rather than replaced: a visible `^[` would be the
/// renderer inventing content for a string that carried none, and a client
/// showing `^[[2J` teaches a user to read attacker text as a glyph soup rather
/// than seeing the label an honest extension meant.
#[must_use]
pub fn inert(text: &str) -> String {
    text.chars().filter(|c| !is_hostile(*c)).collect()
}

/// [`inert`], then cut to `cells` display columns, marked with `marker`.
///
/// Cutting is by grapheme, so a two-cell emoji is kept or dropped whole. When
/// something is cut, the tail carries `marker` — a reader has to be able to
/// tell a short label from a shortened one, or a truncated command name looks
/// like the name.
///
/// The marker is a parameter rather than a constant because the caller owns
/// the theme: an ASCII theme renders elision as `...`, and a widget that cut
/// some rows with `…` and others with `...` would show unicode to the one
/// user who asked for none (#206).
#[must_use]
pub fn inert_within(text: &str, cells: usize, marker: &str) -> String {
    let clean = inert(text);
    if width(&clean) <= cells {
        return clean;
    }
    if cells == 0 {
        return String::new();
    }
    // The marker's own width comes out of the budget, so the result still
    // fits `cells` when the marker is wider than one column.
    let Some(budget) = cells.checked_sub(width(marker)) else {
        // No room for even the marker: cut to the marker itself, truncated.
        return marker.chars().take(cells).collect();
    };
    let mut out = String::new();
    let mut used = 0;
    for grapheme in clean.graphemes(true) {
        let w = width(grapheme);
        if used + w > budget {
            break;
        }
        out.push_str(grapheme);
        used += w;
    }
    out.push_str(marker);
    out
}

#[cfg(test)]
mod tests {
    use super::{inert, inert_within};
    use crate::wrap::width;

    /// The sequences this exists for, each named by what it would do.
    #[test]
    fn a_screen_clear_is_not_passed_through() {
        assert_eq!(inert("before\x1b[2Jafter"), "before[2Jafter");
    }

    /// The nastiest one: a cursor-position report makes the terminal write
    /// into this client's own stdin.
    #[test]
    fn a_cursor_report_request_cannot_reach_the_terminal() {
        let cleaned = inert("\x1b[6n");
        assert!(!cleaned.contains('\x1b'), "{cleaned:?}");
    }

    #[test]
    fn an_osc_title_sequence_loses_its_introducer_and_its_terminator() {
        let cleaned = inert("\x1b]0;pwned\x07");
        assert_eq!(cleaned, "]0;pwned");
    }

    /// A lone carriage return redraws the line over itself, which is how a
    /// label hides what was already on the row.
    #[test]
    fn carriage_returns_and_newlines_do_not_survive() {
        assert_eq!(inert("one\rtwo\nthree"), "onetwothree");
    }

    /// Some terminals take `U+009B` as CSI with no `ESC` in front of it, so
    /// dropping the escape character alone would not be enough.
    #[test]
    fn the_c1_csi_introducer_is_removed_too() {
        let cleaned = inert("\u{9b}2J");
        assert_eq!(cleaned, "2J");
    }

    /// Trojan Source: the stored order and the drawn order differ, so a label
    /// can read as one thing and be another.
    #[test]
    fn bidi_overrides_cannot_reorder_a_label() {
        let cleaned = inert("safe\u{202e}txet desrever");
        assert!(!cleaned.contains('\u{202e}'), "{cleaned:?}");
        assert!(cleaned.starts_with("safe"), "{cleaned:?}");
    }

    /// Removing instructions must not mean reducing text to ASCII.
    #[test]
    fn ordinary_text_including_emoji_and_cjk_is_untouched() {
        for text in ["plain", "日本語", "e\u{301}", "🚀 ship"] {
            assert_eq!(inert(text), text, "{text:?} should survive intact");
        }
    }

    #[test]
    fn a_label_that_fits_is_returned_whole() {
        assert_eq!(inert_within("short", 10, "…"), "short");
    }

    #[test]
    fn a_long_label_is_cut_to_its_budget_and_says_so() {
        let cut = inert_within("a very long contributed label", 10, "…");
        assert_eq!(width(&cut), 10, "{cut:?} must fill exactly its cells");
        assert!(cut.ends_with('…'), "{cut:?} should show it was cut");
    }

    /// A two-cell grapheme is kept or dropped whole: half of an emoji is a
    /// broken cell, not a narrower one.
    #[test]
    fn a_wide_grapheme_is_never_split() {
        let cut = inert_within("🚀🚀🚀", 4, "…");
        assert!(width(&cut) <= 4, "{cut:?}");
        assert!(!cut.contains('\u{fffd}'), "{cut:?}");
    }

    /// The width budget is counted *after* cleaning, so a label padded with
    /// escape sequences cannot use them to push real text out of view.
    #[test]
    fn control_characters_do_not_consume_the_width_budget() {
        assert_eq!(inert_within("\x1b\x1b\x1babc", 4, "…"), "abc");
    }

    #[test]
    fn a_zero_cell_budget_renders_nothing() {
        assert!(inert_within("anything", 0, "…").is_empty());
    }
}
