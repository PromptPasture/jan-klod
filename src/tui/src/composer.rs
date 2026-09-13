//! The message buffer and its caret (#150).
//!
//! A model, not a widget: it holds text and a position and knows how to edit
//! them, and something else decides what that looks like. That split is what
//! lets every test here run without a terminal, the way `App` already does.
//!
//! # Graphemes, because `char` is the wrong unit and this repository has paid
//! for that once already
//!
//! The buffer this replaces was a `String` with `push` and `pop`. `pop` removes
//! one `char` — so backspacing an emoji left a stray joiner behind, and the next
//! keystroke appended to a cluster that no longer meant anything. Every motion
//! and every deletion here is over **grapheme clusters**, and the caret is a
//! byte index that is always on a cluster boundary.
//!
//! `unicode-segmentation` is already a dependency: it came with
//! [`crate::wrap`] and cost zero packages, being one of `ratatui-core`'s.
//!
//! # A word is a run of non-whitespace
//!
//! Stated rather than assumed, because "word" has several defensible meanings
//! and the one a reader expects from `Ctrl+W` is the shell's: delete back to the
//! last space. Unicode word boundaries would split `foo.bar` into three, which
//! is not what anybody pressing `Ctrl+W` after typing a path wants.

use unicode_segmentation::UnicodeSegmentation;

/// One wrapped row of the composer: where it starts, its text, and whether its
/// end is a newline (or the buffer's end) rather than a soft wrap.
struct Row {
    start: usize,
    text: String,
    hard_end: bool,
}

/// A multi-line message being written, and where the caret is in it.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Composer {
    text: String,
    /// Byte index, always on a grapheme boundary.
    caret: usize,
}

impl Composer {
    /// The text as it stands. May contain newlines.
    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }

    /// The caret's byte offset.
    #[must_use]
    pub const fn caret(&self) -> usize {
        self.caret
    }

    /// Whether there is anything to send.
    ///
    /// Whitespace-only counts as empty: it is what decides whether `Ctrl+U`
    /// scrolls the transcript or kills a line, and a buffer holding one space
    /// should behave like one holding nothing.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.text.trim().is_empty()
    }

    /// Take the message and reset, or `None` if there is nothing to send.
    pub fn take(&mut self) -> Option<String> {
        if self.is_empty() {
            self.text.clear();
            self.caret = 0;
            return None;
        }
        self.caret = 0;
        Some(std::mem::take(&mut self.text).trim().to_string())
    }

    /// Replace the whole buffer, caret at the end.
    ///
    /// For recall (#151), which swaps one message for another wholesale rather
    /// than editing the one that is there.
    pub fn set(&mut self, text: &str) {
        self.text.clear();
        self.text.push_str(text);
        self.caret = self.text.len();
    }

    /// Replace the `count` bytes immediately before the caret with `text`.
    ///
    /// For accepting a completion (#153), which swaps the typed fragment for the
    /// path it matched and leaves the caret after it — mid-message, so
    /// [`Composer::set`] is the wrong tool.
    pub fn replace_before(&mut self, count: usize, text: &str) {
        let start = self.caret.saturating_sub(count);
        self.text.replace_range(start..self.caret, text);
        self.caret = start + text.len();
    }

    /// Insert text at the caret and leave the caret after it.
    pub fn insert(&mut self, s: &str) {
        self.text.insert_str(self.caret, s);
        self.caret += s.len();
    }

    /// Insert one character.
    pub fn push(&mut self, c: char) {
        let mut buf = [0_u8; 4];
        self.insert(c.encode_utf8(&mut buf));
    }

    /// The byte index of the grapheme boundary before the caret.
    fn prev_boundary(&self) -> Option<usize> {
        self.text[..self.caret]
            .grapheme_indices(true)
            .next_back()
            .map(|(i, _)| i)
    }

    /// The byte index of the grapheme boundary after the caret.
    fn next_boundary(&self) -> Option<usize> {
        self.text[self.caret..]
            .graphemes(true)
            .next()
            .map(|g| self.caret + g.len())
    }

    /// Delete the grapheme before the caret.
    pub fn backspace(&mut self) {
        if let Some(start) = self.prev_boundary() {
            self.text.replace_range(start..self.caret, "");
            self.caret = start;
        }
    }

    /// Move one grapheme left.
    pub fn left(&mut self) {
        if let Some(i) = self.prev_boundary() {
            self.caret = i;
        }
    }

    /// Move one grapheme right.
    pub fn right(&mut self) {
        if let Some(i) = self.next_boundary() {
            self.caret = i;
        }
    }

    /// Start of the current line.
    #[must_use]
    pub fn line_start(&self) -> usize {
        self.text[..self.caret].rfind('\n').map_or(0, |i| i + 1)
    }

    /// End of the current line.
    #[must_use]
    pub fn line_end(&self) -> usize {
        self.text[self.caret..]
            .find('\n')
            .map_or(self.text.len(), |i| self.caret + i)
    }

    /// `Ctrl+A`.
    pub fn home(&mut self) {
        self.caret = self.line_start();
    }

    /// `Ctrl+E`.
    pub fn end(&mut self) {
        self.caret = self.line_end();
    }

    /// The byte index one word back: over any whitespace, then over the word.
    fn word_start(&self) -> usize {
        let head = &self.text[..self.caret];
        let trimmed = head.trim_end_matches(|c: char| c.is_whitespace());
        trimmed.rfind(char::is_whitespace).map_or(0, |i| i + 1)
    }

    /// The byte index one word forward: over any whitespace, then over the word.
    fn word_end(&self) -> usize {
        let tail = &self.text[self.caret..];
        let skipped = tail.len() - tail.trim_start_matches(|c: char| c.is_whitespace()).len();
        let rest = &tail[skipped..];
        let len = rest.find(char::is_whitespace).unwrap_or(rest.len());
        self.caret + skipped + len
    }

    /// `Alt+←`.
    pub fn word_left(&mut self) {
        self.caret = self.word_start();
    }

    /// `Alt+→`.
    pub fn word_right(&mut self) {
        self.caret = self.word_end();
    }

    /// `Ctrl+W`: delete the word before the caret.
    pub fn delete_word_back(&mut self) {
        let start = self.word_start();
        self.text.replace_range(start..self.caret, "");
        self.caret = start;
    }

    /// `Ctrl+K`: kill to the end of the line.
    pub fn kill_to_end(&mut self) {
        let end = self.line_end();
        self.text.replace_range(self.caret..end, "");
    }

    /// `Ctrl+U`: kill the whole line the caret is on.
    ///
    /// Reaches the composer only when there *is* something to kill — an empty
    /// composer leaves the key to the transcript's half-page scroll (#148), and
    /// the two sides share [`Composer::is_empty`] as their single condition.
    pub fn kill_line(&mut self) {
        let (start, end) = (self.line_start(), self.line_end());
        self.text.replace_range(start..end, "");
        self.caret = start;
    }

    /// The most rows the composer occupies before it scrolls instead of growing.
    pub const MAX_ROWS: usize = 8;

    /// One wrapped row: where it starts, its text, and whether its end is a
    /// **hard** one — a newline or the end of the buffer.
    ///
    /// The byte offset is what makes vertical motion possible: a row on screen
    /// has to be turned back into a position in the text, and a `Vec<String>`
    /// has thrown that away.
    ///
    /// The hard/soft flag is subtler and was found by a failing test. Where a
    /// row is *soft*-wrapped, its last byte and the next row's first byte are
    /// **the same offset** — there is no character between them — so a caret
    /// placed at "the end of row 1" is indistinguishable from one at "the start
    /// of row 2", and `up` from row 2 would land back on row 2. Vertical motion
    /// therefore stops one grapheme short on a soft-wrapped row.
    fn layout(&self, width: usize) -> (Vec<Row>, (usize, usize)) {
        let width = width.max(1);
        let mut rows: Vec<Row> = Vec::new();
        let mut caret = (0, 0);
        let mut base = 0;

        for logical in self.text.split('\n') {
            let mut row = String::new();
            let mut row_start = base;
            let mut used = 0;

            for (off, cluster) in logical.grapheme_indices(true) {
                let w = crate::wrap::width(cluster);
                if used + w > width && !row.is_empty() {
                    rows.push(Row {
                        start: row_start,
                        text: std::mem::take(&mut row),
                        hard_end: false,
                    });
                    row_start = base + off;
                    used = 0;
                }
                if base + off == self.caret {
                    caret = (rows.len(), used);
                }
                row.push_str(cluster);
                used += w;
            }
            if base + logical.len() == self.caret {
                caret = (rows.len(), used);
            }
            rows.push(Row {
                start: row_start,
                text: row,
                hard_end: true,
            });
            base += logical.len() + 1;
        }
        (rows, caret)
    }

    /// The wrapped rows, and the caret's `(row, column)` within them.
    ///
    /// **Not [`crate::wrap::wrap`].** That one is for display text: it breaks on
    /// word boundaries and collapses runs of whitespace, which is right for a
    /// rendered message and wrong for a buffer somebody is typing into — it
    /// would eat the second space of a double space and move the caret out from
    /// under their fingers. This wraps at the pane edge, between graphemes,
    /// preserving every byte.
    ///
    /// Columns are **display cells**, so a caret after a CJK character sits two
    /// columns along rather than one.
    #[must_use]
    pub fn rows(&self, width: usize) -> (Vec<String>, (usize, usize)) {
        let (rows, caret) = self.layout(width);
        (rows.into_iter().map(|r| r.text).collect(), caret)
    }

    /// The byte offset at `column` cells into `row`.
    ///
    /// A soft-wrapped row's end is the next row's start, so landing there would
    /// silently move the caret a row further than asked; on those rows the
    /// column clamps to the last grapheme instead.
    fn offset_in_row(row: &Row, column: usize) -> usize {
        let mut used = 0;
        let mut last = row.start;
        for (off, cluster) in row.text.grapheme_indices(true) {
            if used >= column {
                return row.start + off;
            }
            last = row.start + off;
            used += crate::wrap::width(cluster);
        }
        if row.hard_end || row.text.is_empty() {
            row.start + row.text.len()
        } else {
            last
        }
    }

    /// One wrapped row of the composer. See [`Composer::layout`].
    /// Whether the caret is on the first visual row.
    #[must_use]
    pub fn on_first_row(&self, width: usize) -> bool {
        self.layout(width).1 .0 == 0
    }

    /// Whether the caret is on the last visual row.
    #[must_use]
    pub fn on_last_row(&self, width: usize) -> bool {
        let (rows, (row, _)) = self.layout(width);
        row + 1 >= rows.len()
    }

    /// Move the caret one **visual row** up, keeping its column where it can.
    ///
    /// Visual rather than logical: the composer wraps, and somebody pressing
    /// `↑` on the second row of a wrapped line means the first row of that line,
    /// not the previous message line.
    ///
    /// The column is taken from where the caret is now rather than remembered
    /// across presses. A remembered column is nicer in a long editing session
    /// and is not what this slice is for; if it is added later, this is the one
    /// place it belongs.
    pub fn up(&mut self, width: usize) {
        let (rows, (row, col)) = self.layout(width);
        if row == 0 {
            return;
        }
        self.caret = Self::offset_in_row(&rows[row - 1], col);
    }

    /// Move the caret one visual row down. See [`Composer::up`].
    pub fn down(&mut self, width: usize) {
        let (rows, (row, col)) = self.layout(width);
        if row + 1 >= rows.len() {
            return;
        }
        self.caret = Self::offset_in_row(&rows[row + 1], col);
    }

    /// The rows actually on screen, and the caret within them.
    ///
    /// Grows to [`Self::MAX_ROWS`] and then scrolls rather than growing further,
    /// keeping the caret's row visible — a composer that grew without limit
    /// would eat the transcript it is a reply to.
    #[must_use]
    pub fn visible(&self, width: usize) -> (Vec<String>, (usize, usize)) {
        let (rows, (caret_row, caret_col)) = self.rows(width);
        if rows.len() <= Self::MAX_ROWS {
            return (rows, (caret_row, caret_col));
        }
        let last = caret_row.max(Self::MAX_ROWS - 1);
        let start = last + 1 - Self::MAX_ROWS;
        (rows[start..=last].to_vec(), (caret_row - start, caret_col))
    }

    /// How many lines the text occupies, at minimum one.
    #[must_use]
    pub fn line_count(&self) -> usize {
        self.text.split('\n').count()
    }
}

#[cfg(test)]
mod tests {
    use super::Composer;

    fn typed(s: &str) -> Composer {
        let mut c = Composer::default();
        c.insert(s);
        c
    }

    /// #150's first Acceptance line.
    #[test]
    fn type_move_by_word_and_delete_a_word() {
        let mut c = typed("alpha beta gamma");
        assert_eq!(c.caret(), c.text().len(), "the caret follows what is typed");

        c.word_left();
        assert_eq!(&c.text()[c.caret()..], "gamma");
        c.word_left();
        assert_eq!(&c.text()[c.caret()..], "beta gamma");

        c.end();
        c.delete_word_back();
        assert_eq!(c.text(), "alpha beta ");
        c.delete_word_back();
        assert_eq!(c.text(), "alpha ", "the space before the word goes too");
    }

    /// The bug the old `String::pop` buffer had, and the reason this module
    /// exists rather than a caret being bolted onto it.
    #[test]
    fn backspace_removes_a_grapheme_not_a_char() {
        let family = "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}";
        let mut c = typed(&format!("hi {family}"));
        assert!(family.chars().count() > 1, "this test needs a cluster");

        c.backspace();
        assert_eq!(c.text(), "hi ", "one press removed the whole cluster");
        assert!(
            !c.text().contains('\u{200D}'),
            "a joiner survived, so half an emoji is still in the buffer"
        );
    }

    #[test]
    fn motion_never_lands_inside_a_cluster() {
        let family = "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}";
        let mut c = typed(family);
        c.left();
        assert_eq!(c.caret(), 0, "one cluster back is the start");
        c.right();
        assert_eq!(c.caret(), family.len());
        // Overrunning either end is a no-op rather than a panic.
        c.right();
        c.right();
        assert_eq!(c.caret(), family.len());
        c.left();
        c.left();
        assert_eq!(c.caret(), 0);
    }

    #[test]
    fn lines_are_addressed_separately() {
        let mut c = typed("first line\nsecond line");
        c.home();
        assert_eq!(&c.text()[c.caret()..], "second line");
        c.end();
        assert_eq!(c.caret(), c.text().len());

        c.home();
        c.kill_to_end();
        assert_eq!(c.text(), "first line\n", "only the caret's line was killed");
        assert_eq!(c.line_count(), 2, "the newline survives a kill-to-end");
    }

    #[test]
    fn kill_line_empties_the_line_the_caret_is_on() {
        let mut c = typed("keep\ndrop");
        c.kill_line();
        assert_eq!(c.text(), "keep\n");
        assert_eq!(c.caret(), 5);
    }

    /// `Ctrl+U`'s two sides share exactly one condition, so this is what #148's
    /// scroll arm and this module's kill arm both ask.
    #[test]
    fn whitespace_only_counts_as_empty() {
        assert!(Composer::default().is_empty());
        assert!(
            typed("   \n  ").is_empty(),
            "a buffer of spaces is nothing to send"
        );
        assert!(!typed("x").is_empty());
    }

    #[test]
    fn taking_a_message_trims_and_resets() {
        let mut c = typed("  hello  ");
        assert_eq!(c.take().as_deref(), Some("hello"));
        assert_eq!(c.text(), "");
        assert_eq!(c.caret(), 0);

        let mut blank = typed("   ");
        assert_eq!(blank.take(), None, "nothing to send");
        assert_eq!(blank.text(), "", "and the whitespace does not linger");
    }

    /// Acceptance line 4: it grows, then scrolls rather than growing further.
    #[test]
    fn the_composer_grows_to_eight_rows_and_then_scrolls() {
        let mut c = Composer::default();
        for i in 0..20 {
            if i > 0 {
                c.push('\n');
            }
            c.insert(&format!("line{i}"));
        }
        let (all, _) = c.rows(40);
        assert_eq!(all.len(), 20, "twenty logical lines are twenty rows");

        let (shown, (row, _)) = c.visible(40);
        assert_eq!(
            shown.len(),
            Composer::MAX_ROWS,
            "it stopped growing at eight"
        );
        assert_eq!(shown.last().map(String::as_str), Some("line19"));
        assert_eq!(row, Composer::MAX_ROWS - 1, "the caret's row is on screen");

        // And scrolling follows the caret rather than pinning the end.
        c.home();
        for _ in 0..15 {
            c.left();
        }
        let (shown, (row, _)) = c.visible(40);
        assert!(row < Composer::MAX_ROWS, "the caret stayed visible: {row}");
        assert_eq!(shown.len(), Composer::MAX_ROWS);
    }

    /// The reason this does not reuse `wrap::wrap`.
    #[test]
    fn wrapping_the_buffer_preserves_every_byte() {
        let c = typed("a  b   c");
        let (rows, _) = c.rows(40);
        assert_eq!(rows.concat(), "a  b   c", "runs of spaces survived");

        let wide = typed("日本語です");
        let (rows, _) = wide.rows(4);
        assert_eq!(rows.concat(), "日本語です", "nothing was dropped");
        for row in &rows {
            assert!(crate::wrap::width(row) <= 4, "{row:?} overflows the pane");
        }
    }

    #[test]
    fn the_caret_column_is_cells_not_characters() {
        let mut c = typed("日本");
        let (_, (_, col)) = c.rows(40);
        assert_eq!(col, 4, "two ideographs are four columns");
        c.left();
        let (_, (_, col)) = c.rows(40);
        assert_eq!(col, 2);
    }

    #[test]
    fn vertical_motion_moves_by_visual_row_not_logical_line() {
        // One logical line, wrapped into three rows of four cells.
        let mut c = typed("abcdefghijkl");
        assert_eq!(c.rows(4).0.len(), 3);
        assert!(c.on_last_row(4) && !c.on_first_row(4));

        c.up(4);
        assert_eq!(c.rows(4).1 .0, 1, "up reached the middle row of one line");
        c.up(4);
        assert!(c.on_first_row(4), "and then the first");
        c.up(4);
        assert!(
            c.on_first_row(4),
            "up from the first row is a no-op, not a wrap"
        );

        c.down(4);
        c.down(4);
        assert!(c.on_last_row(4));
        c.down(4);
        assert!(c.on_last_row(4), "down from the last row is a no-op");
    }

    #[test]
    fn vertical_motion_keeps_the_column_and_clamps_to_a_short_row() {
        let mut c = typed("alpha\nxy\nbravo");
        c.end();
        assert_eq!(&c.text()[c.caret()..], "", "started at the end of `bravo`");

        c.up(40);
        assert_eq!(
            &c.text()[c.caret()..],
            "\nbravo",
            "column 5 on a two-character row clamps to its end"
        );
        // The column carried up is the one the caret has *now* — 2, the end of
        // `xy` — not the 5 it started with. `Composer::up` takes the column
        // fresh each press rather than remembering it across them, and this is
        // what that reads like from outside.
        c.up(40);
        assert_eq!(
            &c.text()[c.caret()..],
            "pha\nxy\nbravo",
            "column 2 of `alpha`, carried from where the caret sat on `xy`"
        );
    }

    /// Columns are cells, so a caret above a CJK row lands where it looks like
    /// it should rather than two characters early.
    #[test]
    fn vertical_motion_counts_cells() {
        let mut c = typed("日本語\nabcdef");
        c.end();
        c.up(40);
        // `日本語` is six cells; the caret was at column 6 on `abcdef`.
        assert_eq!(
            c.caret(),
            "日本語".len(),
            "clamped to the end of the wide row"
        );
    }

    #[test]
    fn inserting_mid_buffer_puts_the_caret_after_what_was_inserted() {
        let mut c = typed("ac");
        c.left();
        c.push('b');
        assert_eq!(c.text(), "abc");
        assert_eq!(&c.text()[c.caret()..], "c");
    }
}
