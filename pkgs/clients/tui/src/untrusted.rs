//! Makes text from a sandboxed extension safe to draw.
//!
//! Contributions are the first text from a sandboxed third party
//! (`wit/client-surface.wit`), so every renderer must treat them as data. A frame
//! is a byte stream, so any escape sequence that reaches it is an instruction.
//! Removed:
//!
//! - C0 controls and `DEL`, including `ESC`, which introduces every CSI and OSC sequence.
//! - C1 controls (`U+0080`–`U+009F`), since some terminals take `U+009B` as CSI alone.
//! - Bidi overrides (`U+202A`–`U+202E`, `U+2066`–`U+2069`), which reorder the drawn text (Trojan Source).
//!
//! Everything else survives, including emoji, CJK and combining marks.
//! Truncation happens here, in cells and never mid-grapheme.

use unicode_segmentation::UnicodeSegmentation;

use crate::wrap::width;

/// Whether `c` must not reach a frame.
const fn is_hostile(c: char) -> bool {
    if c.is_control() {
        return true;
    }
    matches!(c, '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
}

/// `text` with every character a terminal would obey removed. Dropped, not replaced: a visible `^[` would invent content.
#[must_use]
pub fn inert(text: &str) -> String {
    text.chars().filter(|c| !is_hostile(*c)).collect()
}

/// [`inert`], then cut by grapheme to `cells` columns, ending in `marker` when cut.
///
/// `marker` is a parameter because the theme owns it: an ASCII theme must not show `…` (#206).
#[must_use]
pub fn inert_within(text: &str, cells: usize, marker: &str) -> String {
    let clean = inert(text);
    if width(&clean) <= cells {
        return clean;
    }
    if cells == 0 {
        return String::new();
    }
    let Some(budget) = cells.checked_sub(width(marker)) else {
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

    #[test]
    fn a_screen_clear_is_not_passed_through() {
        assert_eq!(inert("before\x1b[2Jafter"), "before[2Jafter");
    }

    /// A cursor-position report makes the terminal write into this client's stdin.
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

    /// A lone CR redraws the row over itself, hiding what was there.
    #[test]
    fn carriage_returns_and_newlines_do_not_survive() {
        assert_eq!(inert("one\rtwo\nthree"), "onetwothree");
    }

    /// Some terminals take `U+009B` as CSI without an `ESC`.
    #[test]
    fn the_c1_csi_introducer_is_removed_too() {
        let cleaned = inert("\u{9b}2J");
        assert_eq!(cleaned, "2J");
    }

    /// Trojan Source: stored and drawn order differ, so a label can read as something else.
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

    /// A two-cell grapheme is kept or dropped whole, never split.
    #[test]
    fn a_wide_grapheme_is_never_split() {
        let cut = inert_within("🚀🚀🚀", 4, "…");
        assert!(width(&cut) <= 4, "{cut:?}");
        assert!(!cut.contains('\u{fffd}'), "{cut:?}");
    }

    /// The budget is counted after cleaning, so escapes cannot push real text out of view.
    #[test]
    fn control_characters_do_not_consume_the_width_budget() {
        assert_eq!(inert_within("\x1b\x1b\x1babc", 4, "…"), "abc");
    }

    #[test]
    fn a_zero_cell_budget_renders_nothing() {
        assert_eq!(inert_within("anything", 0, "…"), "");
    }
}
