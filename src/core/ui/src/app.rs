//! The TUI's pure state model — no terminal, no I/O, so it is unit-tested.
//!
//! The `tui` render/event glue (in the binary) is a thin shell over this: key
//! events mutate the [`App`], each turn's answer is recorded, and the view is a
//! pure function of this state.

use crate::composer::Composer;

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
    /// The message being written, with its caret (#150).
    ///
    /// A [`Composer`] rather than a `String`: the old buffer's `pop` removed one
    /// `char`, so backspacing an emoji left half of it behind.
    pub composer: Composer,
    /// The conversation so far, oldest first.
    pub transcript: Vec<Entry>,
    /// Set when the user asked to quit.
    pub should_quit: bool,
    /// Submitted messages, oldest first (#151).
    ///
    /// Appended to on submit and **never mutated**, so a recalled entry that is
    /// then edited leaves the stored one alone: history holds what was sent, not
    /// what was later half-typed over it.
    ///
    /// Not persisted. The event log already holds every user message and reading
    /// it back is a `session/get` concern — a client keeping its own copy is a
    /// second source of truth for the one thing the log exists to be.
    history: Vec<String>,
    /// Where recall is in [`Self::history`], and the text it put in the
    /// composer.
    ///
    /// The text is what makes "unmodified" answerable: the composer is
    /// unmodified while it still holds exactly what recall placed there.
    recall: Option<(usize, String)>,
    /// Set while a turn is blocked on a confirmation. The next submission is that
    /// answer, not a new message — a turn is already running and typing a fresh
    /// message would go nowhere.
    pub pending_prompt: Option<Prompt>,
}

/// A confirmation a running turn is waiting on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Prompt {
    /// What is being asked.
    pub question: String,
    /// The answers core recognises.
    pub options: Vec<String>,
    /// What core assumes if nobody answers.
    pub default: String,
}

impl App {
    /// The text being written. Kept as an accessor so callers cannot reach past
    /// the composer and desynchronise its caret from its text.
    #[must_use]
    pub fn input(&self) -> &str {
        self.composer.text()
    }

    /// Insert a typed character at the caret.
    pub fn push_char(&mut self, c: char) {
        self.composer.push(c);
    }

    /// Delete the grapheme before the caret.
    pub fn backspace(&mut self) {
        self.composer.backspace();
    }

    /// Take a trimmed, non-empty submission: record it as a [`Who::You`] line,
    /// clear the input, and return the message to send. Whitespace-only input
    /// yields `None` (nothing to send).
    pub fn take_submission(&mut self) -> Option<String> {
        let message = self.composer.take()?;
        self.history.push(message.clone());
        self.recall = None;
        self.record(Who::You, message.clone());
        Some(message)
    }

    /// Whether `↑`/`↓` should walk history rather than move the caret.
    ///
    /// Two states count as unmodified, and the second is the one that is easy to
    /// miss: a fresh empty composer, **and** one still holding exactly what
    /// recall put there. Without the second, pressing `↑` twice would recall
    /// once and then start moving the caret.
    fn composer_unmodified(&self) -> bool {
        self.composer.is_empty()
            || self
                .recall
                .as_ref()
                .is_some_and(|(_, text)| text == self.composer.text())
    }

    /// `↑`: the previous message, or the caret up if history is not what this
    /// keypress means.
    pub fn history_up(&mut self, width: usize) {
        if !(self.composer.on_first_row(width) && self.composer_unmodified()) {
            self.composer.up(width);
            return;
        }
        // A cleared composer starts the walk over, rather than resuming from
        // wherever recall had reached. Otherwise clearing the line and pressing
        // `↑` would land on the entry *before* the one just discarded, which is
        // not what emptying a prompt means anywhere else.
        if self.composer.is_empty() {
            self.recall = None;
        }
        let next = match self.recall {
            // At the oldest already: stop rather than wrap. Wrapping hands the
            // user the newest message at the moment they asked for the oldest.
            Some((0, _)) => return,
            Some((i, _)) => i - 1,
            None => match self.history.len().checked_sub(1) {
                Some(last) => last,
                None => return,
            },
        };
        let text = self.history[next].clone();
        self.composer.set(&text);
        self.recall = Some((next, text));
    }

    /// `↓`: the next message, back to the empty line, or the caret down.
    pub fn history_down(&mut self, width: usize) {
        if !(self.composer.on_last_row(width) && self.composer_unmodified()) {
            self.composer.down(width);
            return;
        }
        let Some((i, _)) = self.recall else { return };
        if i + 1 < self.history.len() {
            let text = self.history[i + 1].clone();
            self.composer.set(&text);
            self.recall = Some((i + 1, text));
        } else {
            // Past the newest is the line the user was writing before they
            // started recalling, which is empty — they submitted the last one.
            self.composer.set("");
            self.recall = None;
        }
    }

    /// Record core's answer.
    pub fn record_answer(&mut self, text: impl Into<String>) {
        self.record(Who::Klod, text);
    }

    /// Append a streaming text delta to the in-progress assistant message,
    /// starting one if the last entry is not already a [`Who::Klod`] line.
    pub fn apply_delta(&mut self, text: &str) {
        if let Some(entry) = self.transcript.last_mut().filter(|e| e.who == Who::Klod) {
            entry.text.push_str(text);
        } else {
            self.record(Who::Klod, text.to_string());
        }
    }

    /// Finish a streaming turn with the authoritative answer. Replaces the
    /// partial streamed entry if one exists, otherwise records it fresh.
    pub fn finish_turn(&mut self, answer: String) {
        if let Some(entry) = self.transcript.last_mut().filter(|e| e.who == Who::Klod) {
            entry.text = answer;
        } else {
            self.record_answer(answer);
        }
    }

    /// Record that a turn is waiting on a confirmation, and show it.
    pub fn ask(&mut self, prompt: Prompt) {
        self.record(
            Who::Status,
            format!(
                "{} [{}] (default: {})",
                prompt.question,
                prompt.options.join("/"),
                prompt.default
            ),
        );
        self.pending_prompt = Some(prompt);
    }

    /// Take a pending confirmation's answer from the input, if one is pending.
    ///
    /// An empty submission answers with the prompt's own default rather than
    /// sending an empty string, so pressing Enter on a confirmation does the safe
    /// thing instead of something undefined.
    pub fn take_answer(&mut self) -> Option<String> {
        let prompt = self.pending_prompt.take()?;
        // `take` clears the composer and its caret together, which is the point
        // of the type: an answer left behind in the buffer would be sent as the
        // next message.
        let answer = self.composer.take().unwrap_or(prompt.default);
        self.record(Who::You, answer.clone());
        Some(answer)
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
        self.transcript.push(Entry {
            who,
            text: text.into(),
        });
    }
}
