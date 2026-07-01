//! The TUI's pure state model — no terminal, no I/O, so it is unit-tested.
//!
//! The `tui` render/event glue (in the binary) is a thin shell over this: key
//! events mutate the [`App`], each turn's answer is recorded, and the view is a
//! pure function of this state.

/// Who authored a transcript line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Who {
    /// The user's message.
    You,
    /// Core's answer.
    Klod,
    /// A client/transport error.
    Error,
    /// A client status note (connect banner, hints).
    Status,
}

/// One line in the transcript.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// Who authored the line.
    pub who: Who,
    /// The line text.
    pub text: String,
}

/// The REPL/TUI state: the input buffer and the scrollback transcript.
#[derive(Debug, Default)]
pub struct App {
    /// The current input line.
    pub input: String,
    /// The conversation so far, oldest first.
    pub transcript: Vec<Entry>,
    /// Set when the user asked to quit.
    pub should_quit: bool,
}

impl App {
    /// Append a typed character to the input.
    pub fn push_char(&mut self, c: char) {
        self.input.push(c);
    }

    /// Delete the last input character (if any).
    pub fn backspace(&mut self) {
        self.input.pop();
    }

    /// Take a trimmed, non-empty submission: record it as a [`Who::You`] line,
    /// clear the input, and return the message to send. Whitespace-only input
    /// yields `None` (nothing to send).
    pub fn take_submission(&mut self) -> Option<String> {
        let message = self.input.trim().to_string();
        if message.is_empty() {
            return None;
        }
        self.input.clear();
        self.record(Who::You, message.clone());
        Some(message)
    }

    /// Record core's answer.
    pub fn record_answer(&mut self, text: impl Into<String>) {
        self.record(Who::Klod, text);
    }

    /// Record a client/transport error.
    pub fn record_error(&mut self, text: impl Into<String>) {
        self.record(Who::Error, text);
    }

    /// Record a status note.
    pub fn record_status(&mut self, text: impl Into<String>) {
        self.record(Who::Status, text);
    }

    /// Request quit.
    pub const fn quit(&mut self) {
        self.should_quit = true;
    }

    fn record(&mut self, who: Who, text: impl Into<String>) {
        self.transcript.push(Entry { who, text: text.into() });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typing_and_backspace_edit_the_input() {
        let mut app = App::default();
        for c in "hii".chars() {
            app.push_char(c);
        }
        app.backspace();
        assert_eq!(app.input, "hi");
    }

    #[test]
    fn submit_records_the_user_line_and_clears_input() {
        let mut app = App::default();
        "  hello  ".chars().for_each(|c| app.push_char(c));
        let sent = app.take_submission();
        assert_eq!(sent.as_deref(), Some("hello"), "trimmed message returned");
        assert!(app.input.is_empty(), "input cleared after submit");
        assert_eq!(app.transcript, vec![Entry { who: Who::You, text: "hello".into() }]);
    }

    #[test]
    fn blank_submit_sends_nothing() {
        let mut app = App::default();
        "   ".chars().for_each(|c| app.push_char(c));
        assert_eq!(app.take_submission(), None);
        assert!(app.transcript.is_empty());
    }

    #[test]
    fn answers_and_errors_append_to_the_transcript() {
        let mut app = App::default();
        app.take_submission(); // nothing (empty)
        "ask".chars().for_each(|c| app.push_char(c));
        app.take_submission();
        app.record_answer("the reply");
        app.record_error("boom");
        let kinds: Vec<Who> = app.transcript.iter().map(|e| e.who).collect();
        assert_eq!(kinds, vec![Who::You, Who::Klod, Who::Error]);
        assert_eq!(app.transcript[1].text, "the reply");
    }
}
