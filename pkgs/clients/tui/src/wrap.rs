//! Wrapping by display cells, never splitting a grapheme (#147).
//!
//! Terminals draw by cells, not bytes or chars: an emoji is two cells, CJK
//! ideographs are two each, and combining marks take none. Widths come from
//! `unicode-width`; clusters from `unicode-segmentation`.
//!
//! [`wrap`] breaks on words and keeps leading indentation. [`hard_wrap`] keeps
//! every space.

use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

/// The display width of `text` in terminal cells.
#[must_use]
pub fn width(text: &str) -> usize {
    UnicodeWidthStr::width(text)
}

/// Wraps `text` to `max` cells, keeping its leading indentation. A blank line stays one row.
#[must_use]
pub fn wrap(text: &str, max: usize) -> Vec<String> {
    if max == 0 {
        // Nothing fits in zero cells; one empty line keeps the caller's row shape.
        return vec![String::new()];
    }

    let indent: String = text
        .chars()
        .take_while(|c| *c == ' ' || *c == '\t')
        .collect();
    // An indent as wide as the pane would leave no room and loop forever.
    let indent = if width(&indent) >= max {
        String::new()
    } else {
        indent
    };
    let hanging = width(&indent);

    let mut lines = Vec::new();
    let mut current = indent.clone();
    let mut current_width = hanging;

    for word in text.split_whitespace() {
        let word_width = width(word);
        let space = usize::from(current_width > hanging);

        if current_width + space + word_width <= max {
            if space == 1 {
                current.push(' ');
            }
            current.push_str(word);
            current_width += space + word_width;
            continue;
        }

        if current_width > hanging {
            lines.push(std::mem::take(&mut current));
            current.clone_from(&indent);
            current_width = hanging;
        }

        if word_width <= max - hanging {
            current.push_str(word);
            current_width = hanging + word_width;
            continue;
        }

        // Wider than the pane: break only between graphemes, or a combining mark is stranded.
        for cluster in word.graphemes(true) {
            let cluster_width = width(cluster);
            if current_width + cluster_width > max && current_width > hanging {
                lines.push(std::mem::take(&mut current));
                current.clone_from(&indent);
                current_width = hanging;
            }
            current.push_str(cluster);
            current_width += cluster_width;
        }
    }

    lines.push(current);
    lines
}

/// Breaks `text` every `max` cells, keeping all whitespace. Unlike [`wrap`], it does not collapse runs of spaces.
///
/// Diffs need this, since alignment is content (#156).
#[must_use]
pub fn hard_wrap(text: &str, max: usize) -> Vec<String> {
    if max == 0 {
        return vec![String::new()];
    }
    let mut lines = Vec::new();
    let mut current = String::new();
    let mut used = 0;
    for cluster in text.graphemes(true) {
        let cells = width(cluster);
        if used + cells > max && used > 0 {
            lines.push(std::mem::take(&mut current));
            used = 0;
        }
        current.push_str(cluster);
        used += cells;
    }
    // A blank line is still a line: a caller laying out rows needs it to exist.
    lines.push(current);
    lines
}

#[cfg(test)]
mod tests {
    use super::{hard_wrap, width, wrap};

    /// Every line a wrap produces must fit the pane, in **cells**.
    fn assert_fits(lines: &[String], max: usize) {
        for line in lines {
            assert!(
                width(line) <= max,
                "{line:?} is {} cells wide, pane is {max}",
                width(line)
            );
        }
    }

    #[test]
    fn wraps_on_word_boundaries_and_keeps_the_words() {
        let lines = wrap("the quick brown fox jumps", 11);
        assert_fits(&lines, 11);
        assert_eq!(lines.join(" ").split_whitespace().count(), 5);
        assert!(lines.len() > 1, "11 cells cannot hold 25");
    }

    #[test]
    fn continuation_lines_keep_the_leading_indentation() {
        let lines = wrap("    alpha beta gamma delta", 14);
        assert_fits(&lines, 14);
        assert!(lines.len() > 1);
        for line in &lines {
            assert!(
                line.starts_with("    "),
                "continuation lost the indent: {line:?}"
            );
        }
    }

    /// Counting `chars()` would overflow the pane by 2x.
    #[test]
    fn cjk_counts_two_cells_each_and_never_overflows() {
        let text = "日本語のテキストです";
        assert_eq!(text.chars().count(), 10);
        assert_eq!(width(text), 20, "ten ideographs are twenty cells");

        let lines = wrap(text, 10);
        assert_fits(&lines, 10);
        assert_eq!(lines.len(), 2, "twenty cells into a ten-cell pane");
        assert_eq!(lines.concat(), text, "no character was dropped");
    }

    /// Emoji clusters are never split.
    #[test]
    fn a_grapheme_cluster_is_never_split() {
        // Family emoji: multiple scalars joined by zero-width joiners.
        let family = "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}";
        assert!(family.chars().count() > 1, "this test needs a cluster");
        let text = format!("{family}{family}{family}");

        let lines = wrap(&text, 4);
        assert_fits(&lines, 4);
        for line in &lines {
            assert!(
                line.is_empty() || line.contains(family),
                "a cluster was split: {line:?}"
            );
            assert!(
                !line.starts_with('\u{200D}'),
                "a line begins with a joiner, so the split was mid-cluster: {line:?}"
            );
        }
        assert_eq!(lines.concat(), text, "no scalar was dropped");
    }

    #[test]
    fn a_word_wider_than_the_pane_is_broken_rather_than_overflowing() {
        let lines = wrap("supercalifragilistic", 7);
        assert_fits(&lines, 7);
        assert_eq!(lines.concat(), "supercalifragilistic");
    }

    #[test]
    fn degenerate_widths_do_not_hang_or_overflow() {
        assert_eq!(wrap("anything", 0), vec![String::new()]);
        assert_eq!(
            wrap("", 10),
            vec![String::new()],
            "a blank line stays a line"
        );
        let lines = wrap("        word", 4);
        assert_fits(&lines, 4);
        assert_eq!(lines.concat(), "word");
    }

    /// `hard_wrap` keeps what `wrap` collapses.
    #[test]
    fn a_hard_wrap_changes_nothing_but_where_the_line_ends() {
        let aligned = "let x = 1;      // two columns of padding";
        assert_eq!(hard_wrap(aligned, 80), vec![aligned.to_string()]);
        assert!(
            !wrap(aligned, 80)[0].contains("      "),
            "`wrap` collapses runs of spaces; if it stops, `hard_wrap` loses purpose"
        );

        let lines = hard_wrap(aligned, 12);
        assert_fits(&lines, 12);
        assert_eq!(lines.concat(), aligned, "a cell was invented or lost");

        // Leading whitespace is content here, not an indent to reproduce.
        assert_eq!(hard_wrap("    x", 10), vec!["    x".to_string()]);
        assert_eq!(hard_wrap("", 10), vec![String::new()], "a blank line stays");
        assert_eq!(hard_wrap("anything", 0), vec![String::new()]);

        // Wide clusters still cannot be halved.
        let cjk = hard_wrap("日本語のテキストです", 5);
        assert_fits(&cjk, 5);
        assert_eq!(cjk.concat(), "日本語のテキストです");
    }
}
