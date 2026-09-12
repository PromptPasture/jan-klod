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

    #[test]
    fn inserting_mid_buffer_puts_the_caret_after_what_was_inserted() {
        let mut c = typed("ac");
        c.left();
        c.push('b');
        assert_eq!(c.text(), "abc");
        assert_eq!(&c.text()[c.caret()..], "c");
    }
}
