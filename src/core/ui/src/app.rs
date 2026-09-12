//! The TUI's pure state model — no terminal, no I/O, so it is unit-tested.
//!
//! The `tui` render/event glue (in the binary) is a thin shell over this: key
//! events mutate the [`App`], each turn's answer is recorded, and the view is a
//! pure function of this state.

use crate::commands::{Availability, Command};
use crate::composer::Composer;
use crate::paths;

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

/// How a tool call is going.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolStatus {
    /// Invoked, no result yet.
    Running,
    /// Answered, with what it returned.
    Done(String),
    /// The turn ended with this call still open.
    ///
    /// A block left `Running` forever is a spinner that never stops, which
    /// reads as a hung client rather than as a turn that moved on.
    Interrupted,
}

/// A tool call and its result, as one thing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolBlock {
    /// Call id, which is what pairs the two halves.
    pub id: String,
    /// Tool name.
    pub name: String,
    /// JSON arguments, where the transport carried them — the SSE `tool` frame
    /// does not (#161), so this is `None` over REST.
    pub arguments: Option<String>,
    /// Running, done, or interrupted.
    pub status: ToolStatus,
    /// Whether the full arguments and result are shown.
    ///
    /// Collapsed by default; a **failure opens itself**, because hiding the
    /// reason one keystroke away is the opposite of what a failure needs.
    pub expanded: bool,
}

/// One entry in the transcript.
///
/// An enum rather than a `Who` with extra fields: a tool block is **not a
/// speaker**. It has an id, a name, arguments, a status and a result, and
/// squeezing that into `{ who, text }` would mean a `Who::Tool` whose `text` is
/// a rendering — which is exactly the "append it to a status line" shape #100
/// rules out, because then the model is a string and only the rendering knows
/// what it means.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Entry {
    /// Something somebody said.
    Message {
        /// Who authored it.
        who: Who,
        /// The text.
        text: String,
    },
    /// A tool call.
    Tool(ToolBlock),
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
    /// Which entry the `/` menu has highlighted, when it is open.
    ///
    /// `Some` **is** open, so there is no second flag to fall out of step with
    /// the first. What the menu *shows* is derived from the composer rather than
    /// stored (see [`Self::menu_query`]), which is what stops the list and the
    /// text disagreeing; only the highlight needs remembering.
    menu: Option<usize>,
    /// The open `@` completion: the fragment it was computed for, the
    /// highlight, and the result.
    ///
    /// The result is cached rather than recomputed per frame, because computing
    /// it walks the filesystem. The fragment it was computed *for* is stored
    /// beside it so a stale list cannot be shown against a newer fragment —
    /// which is the failure a cache of this shape invites.
    completion: Option<(String, usize, paths::Completion)>,
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
        self.maybe_open_menu();
        if c == '@' {
            // Opened here and nowhere else, so that deleting back to an older
            // `@` does not reopen a list the user dismissed.
            self.completion = Some((String::new(), 0, paths::Completion::default()));
        }
    }

    /// Delete the grapheme before the caret.
    pub fn backspace(&mut self) {
        self.composer.backspace();
        self.maybe_open_menu();
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

    /// The fragment the `/` menu is filtering on, when it is open.
    ///
    /// A command is the whole buffer — `/` then a run of non-whitespace. The
    /// moment a space or a newline arrives it has stopped being one, and the
    /// menu closes rather than filtering on something that can never match.
    #[must_use]
    pub fn menu_query(&self) -> Option<&str> {
        self.menu?;
        let text = self.composer.text();
        (text.starts_with('/') && !text.contains(char::is_whitespace)).then_some(text)
    }

    /// The entries the menu is showing. Empty means the empty state, **not**
    /// that the menu has closed.
    #[must_use]
    pub fn menu_entries(&self) -> Vec<&'static Command> {
        self.menu_query()
            .map(crate::commands::matching)
            .unwrap_or_default()
    }

    /// Which entry is highlighted, clamped to what is on offer.
    #[must_use]
    pub fn menu_selected(&self) -> usize {
        let len = self.menu_entries().len();
        self.menu.unwrap_or(0).min(len.saturating_sub(1))
    }

    /// Open the menu, if this keystroke is the one that opens it.
    fn maybe_open_menu(&mut self) {
        if self.composer.text() == "/" {
            self.menu = Some(0);
        } else if self.menu_query().is_none() {
            // It stopped looking like a command — a space, a newline, or the
            // slash was deleted.
            self.menu = None;
        }
    }

    /// `Esc` with the menu open: close it and leave the text alone.
    ///
    /// Only a keystroke reopens it, so this does not immediately undo itself.
    pub const fn menu_dismiss(&mut self) {
        self.menu = None;
    }

    /// Move the highlight, wrapping — a six-item list is short enough that
    /// wrapping is a convenience rather than the disorientation it is in
    /// history.
    pub fn menu_move(&mut self, delta: isize) {
        let len = self.menu_entries().len();
        if len == 0 {
            return;
        }
        let current = self.menu_selected();
        let len_i = isize::try_from(len).unwrap_or(1);
        let next = (isize::try_from(current).unwrap_or(0) + delta).rem_euclid(len_i);
        self.menu = Some(usize::try_from(next).unwrap_or(0));
    }

    /// Run the highlighted command, or explain why it cannot run yet.
    ///
    /// Returns whether anything was accepted — `false` on the empty state, so
    /// the caller knows `Enter` still means submit.
    pub fn menu_accept(&mut self) -> bool {
        let Some(command) = self.menu_entries().get(self.menu_selected()).copied() else {
            return false;
        };
        self.menu = None;
        self.composer.set("");
        match command.availability {
            Availability::Ready => match command.name {
                "/newline" => self.composer.push('\n'),
                "/quit" => self.should_quit = true,
                // Unreachable while the table and this match agree, and a status
                // line rather than a panic if they ever stop: a client that
                // aborts on its own menu is worse than one that says so.
                other => self.record(Who::Status, format!("{other} is not wired up")),
            },
            Availability::Pending(reason) => {
                self.record(Who::Status, format!("{} — {reason}", command.name));
            }
        }
        true
    }

    /// A tool is about to run: open a block for it.
    pub fn record_tool_invoked(&mut self, id: String, name: String, arguments: Option<String>) {
        self.transcript.push(Entry::Tool(ToolBlock {
            id,
            name,
            arguments,
            status: ToolStatus::Running,
            expanded: false,
        }));
    }

    /// A tool returned: close the block with the matching id.
    ///
    /// **An unmatched result becomes a muted line rather than vanishing.** It
    /// means the client and the core disagree about what is open, and a user
    /// watching a turn should see that something arrived — a result dropped
    /// silently is indistinguishable from one that never came.
    pub fn record_tool_result(&mut self, id: &str, content: String) {
        let open = self
            .transcript
            .iter_mut()
            .rev()
            .find_map(|entry| match entry {
                Entry::Tool(block) if block.id == id && block.status == ToolStatus::Running => {
                    Some(block)
                }
                _ => None,
            });
        match open {
            Some(block) => {
                block.expanded = crate::blocks::reads_as_failure(&content);
                block.status = ToolStatus::Done(content);
            }
            None => self.record_status(format!("tool result for an unknown call `{id}`")),
        }
    }

    /// The turn ended: nothing still open is still running.
    ///
    /// Called on `done` **and** on error, because a turn that failed leaves the
    /// same blocks open as one that finished, and a spinner that never stops
    /// reads as a hung client rather than as a turn that moved on.
    pub fn interrupt_open_tools(&mut self) {
        for entry in &mut self.transcript {
            if let Entry::Tool(block) = entry {
                if block.status == ToolStatus::Running {
                    block.status = ToolStatus::Interrupted;
                }
            }
        }
    }

    /// Whether the `@` completion list is open.
    #[must_use]
    pub const fn completion_open(&self) -> bool {
        self.completion.is_some()
    }

    /// The paths on offer. Empty means the empty state, not a closed list.
    #[must_use]
    pub fn completion_entries(&self) -> &[String] {
        self.completion
            .as_ref()
            .map_or(&[], |(_, _, found)| found.entries.as_slice())
    }

    /// Whether the list stopped at the cap. Shown, never silent — see #145.
    #[must_use]
    pub fn completion_truncated(&self) -> bool {
        self.completion
            .as_ref()
            .is_some_and(|(_, _, found)| found.truncated)
    }

    /// Which path is highlighted, clamped to what is on offer.
    #[must_use]
    pub fn completion_selected(&self) -> usize {
        let len = self.completion_entries().len();
        self.completion
            .as_ref()
            .map_or(0, |(_, i, _)| *i)
            .min(len.saturating_sub(1))
    }

    /// Recompute the list if the fragment moved, and close it if there is no
    /// fragment left.
    ///
    /// `root` is an argument rather than `std::env::current_dir()` so a test can
    /// state the tree it means — the same reason `theme::Theme::detect` takes
    /// its environment.
    pub fn refresh_completion(&mut self, root: &std::path::Path) {
        if self.completion.is_none() {
            return;
        }
        let Some(fragment) = paths::fragment(self.composer.text(), self.composer.caret()) else {
            self.completion = None;
            return;
        };
        let fragment = fragment.to_string();
        match &self.completion {
            Some((cached, _, _)) if *cached == fragment => {}
            _ => self.completion = Some((fragment.clone(), 0, paths::complete(root, &fragment))),
        }
    }

    /// Close the list and leave the text alone.
    pub fn completion_dismiss(&mut self) {
        self.completion = None;
    }

    /// Move the highlight, wrapping like the `/` menu's.
    pub fn completion_move(&mut self, delta: isize) {
        let len = self.completion_entries().len();
        if len == 0 {
            return;
        }
        let current = self.completion_selected();
        let len_i = isize::try_from(len).unwrap_or(1);
        let next = (isize::try_from(current).unwrap_or(0) + delta).rem_euclid(len_i);
        if let Some((_, selected, _)) = self.completion.as_mut() {
            *selected = usize::try_from(next).unwrap_or(0);
        }
    }

    /// Insert the highlighted path in place of the typed fragment.
    ///
    /// **No file is read.** The path is text in a message; what to do with it is
    /// the core's business, and a client that opened it would have grown policy.
    ///
    /// Returns whether anything was accepted, so the caller knows whether
    /// `Enter` still means submit.
    pub fn completion_accept(&mut self) -> bool {
        let Some(path) = self
            .completion_entries()
            .get(self.completion_selected())
            .cloned()
        else {
            return false;
        };
        // The `@` goes too: #101 asks for the accepted text to be "a plain path
        // in the message". Keeping the marker would leave the core a hint it has
        // no contract for — and this client's whole position is that it inserts
        // text and reads nothing, so the message should say what the user means
        // rather than carry a convention nobody has agreed to. `'@'` is one
        // byte, hence the `+ 1`.
        let typed = self
            .completion
            .as_ref()
            .map_or(0, |(fragment, _, _)| fragment.len() + 1);
        self.composer.replace_before(typed, &path);
        self.completion = None;
        true
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
        match self.transcript.last_mut() {
            Some(Entry::Message {
                who: Who::Klod,
                text: existing,
            }) => existing.push_str(text),
            _ => self.record(Who::Klod, text.to_string()),
        }
    }

    /// Finish a streaming turn with the authoritative answer. Replaces the
    /// partial streamed entry if one exists, otherwise records it fresh.
    pub fn finish_turn(&mut self, answer: String) {
        match self.transcript.last_mut() {
            Some(Entry::Message {
                who: Who::Klod,
                text,
            }) => *text = answer,
            _ => self.record_answer(answer),
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
        self.transcript.push(Entry::Message {
            who,
            text: text.into(),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::{App, Entry, ToolStatus, Who};

    fn tools(app: &App) -> Vec<&super::ToolBlock> {
        app.transcript
            .iter()
            .filter_map(|e| match e {
                Entry::Tool(block) => Some(block),
                Entry::Message { .. } => None,
            })
            .collect()
    }

    /// The first Acceptance line: one block, resolved.
    #[test]
    fn an_invocation_and_its_result_are_one_block() {
        let mut app = App::default();
        app.record_tool_invoked("c1".into(), "fs.read".into(), Some("{}".into()));
        app.record_tool_result("c1", "contents".into());

        let blocks = tools(&app);
        assert_eq!(blocks.len(), 1, "the result did not open a second block");
        assert_eq!(blocks[0].status, ToolStatus::Done("contents".into()));
        assert_eq!(app.transcript.len(), 1, "and nothing else was recorded");
    }

    /// Acceptance: **a failure opens itself.** A tool that failed is the one
    /// block whose contents a user certainly wants, so hiding the reason behind
    /// a keystroke they have to know about is the wrong default — and a
    /// successful call is the opposite, or a transcript of twelve reads is
    /// twelve screens of JSON nobody asked for.
    #[test]
    fn a_failed_call_arrives_expanded_and_a_successful_one_does_not() {
        let mut app = App::default();
        app.record_tool_invoked("c1".into(), "fs.read".into(), Some("{}".into()));
        app.record_tool_result("c1", "contents".into());
        app.record_tool_invoked("c2".into(), "fs.read".into(), Some("{}".into()));
        app.record_tool_result("c2", "tool `fs.read` error: NotFound".into());

        let blocks = tools(&app);
        assert!(!blocks[0].expanded, "a result nobody needs to read opened");
        assert!(
            blocks[1].expanded,
            "the reason a call failed is one keystroke away, and the user does \
             not know which keystroke"
        );
    }

    /// The second: an unmatched result is visible, not dropped.
    #[test]
    fn a_result_for_an_unknown_call_is_a_muted_line_and_not_a_panic() {
        let mut app = App::default();
        app.record_tool_result("nobody", "orphan".into());

        assert!(tools(&app).is_empty(), "no block was invented for it");
        match app.transcript.last() {
            Some(Entry::Message { who, text }) => {
                assert_eq!(*who, Who::Status);
                assert!(
                    text.contains("nobody"),
                    "the line does not say which call: {text:?}"
                );
            }
            other => panic!("expected a status line, got {other:?}"),
        }
    }

    /// The third: `done` with a block open marks it interrupted.
    #[test]
    fn a_block_still_open_when_the_turn_ends_is_interrupted() {
        let mut app = App::default();
        app.record_tool_invoked("c1".into(), "slow".into(), None);
        app.record_tool_invoked("c2".into(), "fast".into(), None);
        app.record_tool_result("c2", "done".into());

        app.interrupt_open_tools();
        let blocks = tools(&app);
        assert_eq!(blocks[0].status, ToolStatus::Interrupted, "the open one");
        assert_eq!(
            blocks[1].status,
            ToolStatus::Done("done".into()),
            "a finished block is not retroactively interrupted"
        );
    }

    /// Two calls in flight resolve to their own blocks, which is the whole
    /// point of pairing by id rather than by position.
    #[test]
    fn concurrent_calls_resolve_by_id_not_by_order() {
        let mut app = App::default();
        app.record_tool_invoked("a".into(), "first".into(), None);
        app.record_tool_invoked("b".into(), "second".into(), None);
        app.record_tool_result("b", "B".into());
        app.record_tool_result("a", "A".into());

        let blocks = tools(&app);
        assert_eq!(blocks[0].status, ToolStatus::Done("A".into()));
        assert_eq!(blocks[1].status, ToolStatus::Done("B".into()));
    }
}
