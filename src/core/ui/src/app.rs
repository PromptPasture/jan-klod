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
    /// Answered, with what it returned and whether it failed.
    ///
    /// A struct variant rather than `Done(String)` plus a flag on
    /// [`ToolBlock`], because "did it fail" is only a question once a result
    /// exists: `Running` has no answer yet and `Interrupted` never got one, and
    /// a field beside the status would have invited both to carry one anyway.
    Done {
        /// What the tool returned.
        content: String,
        /// Whether the core reported the call as failed (#162), rather than
        /// this client inferring it from `content`'s wording.
        failed: bool,
    },
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
    /// JSON arguments, where the transport carried them.
    ///
    /// The SSE `tool` frame used to drop them, which is what made this an
    /// `Option`; #161 fixed that, so both surfaces send them now and `None`
    /// means an older core rather than a transport that cannot. Still an
    /// `Option`, because "not sent" and "the call took none" are different
    /// facts and the collapsed line says so differently.
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

/// Where the turn is (#158).
///
/// Four states, and only three of them are stored — see [`App::turn`].
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum Turn {
    /// Nothing running. The composer's text is a new message.
    #[default]
    Idle,
    /// A turn is running and text is arriving.
    Streaming,
    /// A cancel has been *asked for*. Deliberately not "the turn stopped":
    /// over stdio the client cannot send `turn/cancel` at all yet (#157), and
    /// over REST + SSE the cancel is the dropped stream, which the conductor
    /// notices in its own time. The state says what the user asked, and the
    /// stream ending is a separate event.
    Cancelling,
    /// A confirmation is waiting. The turn underneath is still streaming.
    Blocked,
}

/// The stored part of [`Turn`] — every state except `Blocked`.
///
/// `Blocked` is missing on purpose. `App::pending_prompt` already answers "is a
/// confirmation waiting", so a stored `Blocked` would be a second field with an
/// opinion about the same fact, free to disagree with the first. This is the
/// shape `menu: Option<usize>` already uses: one field, and `Some` *means* open.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
enum Phase {
    #[default]
    Idle,
    Streaming,
    Cancelling,
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
    /// Which tool block `Ctrl+O` acts on (#155).
    ///
    /// An index into [`Self::transcript`], and it only ever addresses an
    /// [`Entry::Tool`]: a message has nothing to open, so a cursor that could
    /// land on one would have positions where the only key it exists for does
    /// nothing.
    ///
    /// `None` is the resting state and the common one. A transcript that is
    /// being *read* does not need a selection, and one that appears
    /// uninvited — following the newest call, say — would mean a highlight
    /// moving down the screen on its own while the user types.
    cursor: Option<usize>,
    /// The turn, minus the one state that is derived. See [`Phase`].
    phase: Phase,
    /// Set when something asked for the running turn to stop and nothing has
    /// sent that ask to the core yet.
    ///
    /// The same shape as [`Self::should_quit`], and for the same reason: the
    /// model records the intent and the caller performs it, because the caller
    /// is the only thing holding a transport. Without it `/cancel` could not
    /// act at all — commands are dispatched here, and this type has never known
    /// what a transport is.
    cancel_requested: bool,
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
    /// Which entry the cursor is on, if the user has picked one.
    #[must_use]
    pub const fn cursor(&self) -> Option<usize> {
        self.cursor
    }

    /// Tool block indices, oldest first — the positions the cursor can occupy.
    fn stops(&self) -> Vec<usize> {
        self.transcript
            .iter()
            .enumerate()
            .filter_map(|(i, entry)| matches!(entry, Entry::Tool(_)).then_some(i))
            .collect()
    }

    /// Move the cursor to the previous tool block, or to the newest one.
    ///
    /// Starting at the newest rather than the oldest is the whole reason this is
    /// `Alt+Up`: the transcript grows downwards, the call a user wants to open
    /// is nearly always the one that just happened, and walking up from it is
    /// the same direction as scrolling back through what was said.
    ///
    /// Returns whether it moved, so the caller can scroll to what it selected
    /// without having to work out whether anything changed.
    pub fn cursor_up(&mut self) -> bool {
        let stops = self.stops();
        let moved = self.cursor.map_or_else(
            || stops.last().copied(),
            |at| stops.iter().rev().find(|&&i| i < at).copied(),
        );
        // At the oldest block already: hold there rather than wrapping to the
        // newest. A selection that jumps to the far end of the transcript when
        // a user presses the same key once too often is a selection they then
        // have to find again.
        if moved.is_some() {
            self.cursor = moved;
            return true;
        }
        false
    }

    /// Move the cursor to the next tool block, or off the end of the transcript.
    ///
    /// Past the newest block it clears rather than sticking, so `Alt+Down` is
    /// how a user puts the selection away — the same key that made it, which
    /// means there is nothing extra to know.
    pub fn cursor_down(&mut self) -> bool {
        let Some(at) = self.cursor else { return false };
        self.cursor = self.stops().iter().find(|&&i| i > at).copied();
        true
    }

    /// Open or close the block under the cursor.
    ///
    /// Returns whether anything happened. **Nothing under the cursor is a
    /// no-op**, not a panic and not a guess at which block was meant: the index
    /// is re-checked against the transcript here rather than trusted, so a
    /// cursor that somehow outlived the entry it addressed costs a keystroke
    /// instead of the client.
    pub fn toggle_expanded(&mut self) -> bool {
        let Some(at) = self.cursor else { return false };
        match self.transcript.get_mut(at) {
            Some(Entry::Tool(block)) => {
                block.expanded = !block.expanded;
                true
            }
            _ => false,
        }
    }

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
                // Sets the ask; the caller sends it, because commands are
                // dispatched here and this type holds no transport.
                "/cancel" => {
                    if !self.cancel() {
                        self.record(Who::Status, "no turn is running".to_string());
                    }
                }
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
    pub fn record_tool_result(&mut self, id: &str, content: String, failed: bool) {
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
                // A failure opens itself. This used to ask
                // `blocks::reads_as_failure(&content)`, which matched the
                // sentences the core happens to write; since #162 the core says
                // so and this reads the fact.
                block.expanded = failed;
                block.status = ToolStatus::Done { content, failed };
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

    /// Where the turn is.
    ///
    /// `Blocked` wins over whatever is stored, because it is a fact about a
    /// question that has been asked rather than about the stream: the turn
    /// underneath a confirmation is still streaming, and it goes back to being
    /// visibly so the moment the question is answered.
    #[must_use]
    pub const fn turn(&self) -> Turn {
        if self.pending_prompt.is_some() {
            return Turn::Blocked;
        }
        match self.phase {
            Phase::Idle => Turn::Idle,
            Phase::Streaming => Turn::Streaming,
            Phase::Cancelling => Turn::Cancelling,
        }
    }

    /// A turn has started.
    pub const fn begin_turn(&mut self) {
        self.phase = Phase::Streaming;
    }

    /// Ask for the running turn to stop.
    ///
    /// Returns whether this is the **first** such ask, which is what the caller
    /// uses to decide whether to act. A second `Ctrl+C` while already
    /// `Cancelling` returns `false` and does nothing: cancelling is not
    /// idempotent at the transport, and hammering the key must not queue
    /// messages at a core that is already stopping.
    ///
    /// A pending confirmation is dropped, because a question about a turn that
    /// is ending is one nobody is going to answer — and leaving it would leave
    /// the client in `Blocked` with no turn behind it.
    pub fn cancel(&mut self) -> bool {
        if !matches!(self.phase, Phase::Streaming) {
            return false;
        }
        self.phase = Phase::Cancelling;
        self.cancel_requested = true;
        self.pending_prompt = None;
        // A tool block left `Running` forever is a spinner that never stops,
        // which reads as a hung client rather than as a turn that was stopped.
        self.interrupt_open_tools();
        true
    }

    /// Take the pending "stop this turn" ask, if there is one.
    ///
    /// Drains, so a cancel is sent **once**: cancelling is not idempotent at
    /// the transport, and a second message to a core that is already stopping
    /// is noise at best. [`Self::cancel`] is what sets it, and it already
    /// refuses to act twice.
    pub const fn take_cancel_request(&mut self) -> bool {
        let asked = self.cancel_requested;
        self.cancel_requested = false;
        asked
    }

    /// The stream ended without an authoritative answer.
    ///
    /// **The transcript is left exactly as it is**, and that is the whole point
    /// of the state: whatever arrived before the cancel is what the user asked
    /// to keep. Somebody who stops a turn wanted it to stop, not to lose what
    /// it had already said.
    pub fn end_turn(&mut self) {
        self.phase = Phase::Idle;
        self.pending_prompt = None;
        self.interrupt_open_tools();
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
        self.phase = Phase::Idle;
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
    use super::{App, Entry, Prompt, ToolStatus, Turn, Who};

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
        app.record_tool_result("c1", "contents".into(), false);

        let blocks = tools(&app);
        assert_eq!(blocks.len(), 1, "the result did not open a second block");
        assert_eq!(
            blocks[0].status,
            ToolStatus::Done {
                content: "contents".into(),
                failed: false
            }
        );
        assert_eq!(app.transcript.len(), 1, "and nothing else was recorded");
    }

    /// Acceptance: the whole cycle, and the partial answer survives it.
    #[test]
    fn a_cancelled_turn_ends_idle_and_keeps_what_it_had_already_said() {
        let mut app = App::default();
        assert_eq!(app.turn(), Turn::Idle, "nothing is running yet");

        app.begin_turn();
        assert_eq!(app.turn(), Turn::Streaming);
        app.apply_delta("the answer so ");
        app.apply_delta("far");
        app.record_tool_invoked("c1".into(), "read".into(), None);

        assert!(app.cancel(), "the first ask is the one that acts");
        assert_eq!(app.turn(), Turn::Cancelling);

        app.end_turn();
        assert_eq!(app.turn(), Turn::Idle, "the stream ended");
        match app.transcript.first() {
            Some(Entry::Message { who, text }) => {
                assert_eq!(*who, Who::Klod);
                assert_eq!(
                    text, "the answer so far",
                    "a user who stops a turn wanted it to stop, not to lose it"
                );
            }
            other => panic!("the partial answer is gone: {other:?}"),
        }
        assert_eq!(
            tools(&app)[0].status,
            ToolStatus::Interrupted,
            "a block left running after a cancel is a spinner that never stops"
        );
    }

    /// Acceptance: a second `Ctrl+C` while already cancelling does nothing.
    #[test]
    fn cancelling_twice_asks_once() {
        let mut app = App::default();
        assert!(!app.cancel(), "there is no turn to cancel");

        app.begin_turn();
        assert!(app.cancel());
        assert!(
            !app.cancel(),
            "cancelling is not idempotent at the transport, so hammering the \
             key must not queue a second message"
        );
        assert_eq!(app.turn(), Turn::Cancelling, "and the state did not move");

        app.end_turn();
        assert!(!app.cancel(), "nor after it ended");
    }

    /// Acceptance: `Blocked` is entered on a prompt and left when answered.
    #[test]
    fn a_confirmation_blocks_the_turn_and_answering_it_resumes() {
        let mut app = App::default();
        app.begin_turn();
        app.ask(Prompt {
            question: "run `rm -rf /`?".into(),
            options: vec!["yes".into(), "no".into()],
            default: "no".into(),
        });
        assert_eq!(app.turn(), Turn::Blocked);

        assert_eq!(app.take_answer().as_deref(), Some("no"), "the default");
        assert_eq!(
            app.turn(),
            Turn::Streaming,
            "the turn underneath a confirmation never stopped streaming"
        );
    }

    /// A confirmation nobody will answer must not outlive the turn it is about.
    #[test]
    fn cancelling_drops_the_question_rather_than_leaving_it_unanswerable() {
        let mut app = App::default();
        app.begin_turn();
        app.ask(Prompt {
            question: "proceed?".into(),
            options: vec!["y".into(), "n".into()],
            default: "n".into(),
        });
        assert_eq!(app.turn(), Turn::Blocked);

        assert!(app.cancel(), "a blocked turn is still cancellable");
        assert_eq!(
            app.turn(),
            Turn::Cancelling,
            "a dropped prompt must not leave the client blocked on nothing"
        );
        assert!(app.take_answer().is_none(), "the question went with it");
    }

    /// The two states that can both claim "a confirmation is waiting" are one
    /// field, so they cannot disagree.
    #[test]
    fn blocked_is_derived_from_the_prompt_and_not_stored_beside_it() {
        let mut app = App::default();
        app.begin_turn();
        for _ in 0..2 {
            app.ask(Prompt {
                question: "again?".into(),
                options: vec!["y".into()],
                default: "y".into(),
            });
            assert_eq!(app.turn(), Turn::Blocked);
            app.take_answer();
            assert_eq!(app.turn(), Turn::Streaming);
        }
        app.finish_turn("done".into());
        assert_eq!(app.turn(), Turn::Idle, "an answered turn is over");
    }

    /// The cursor only stops where there is something to open.
    #[test]
    fn the_cursor_walks_tool_blocks_and_ignores_everything_else() {
        let mut app = App::default();
        app.record(Who::You, "do it");
        app.record_tool_invoked("c1".into(), "read".into(), None);
        app.record(Who::Klod, "thinking");
        app.record_tool_invoked("c2".into(), "edit".into(), None);
        app.record(Who::Klod, "done");

        assert_eq!(app.cursor(), None, "a transcript at rest has no selection");
        assert!(app.cursor_up(), "the first press selects something");
        assert_eq!(
            app.cursor(),
            Some(3),
            "and it is the newest call, not the oldest"
        );
        assert!(app.cursor_up());
        assert_eq!(
            app.cursor(),
            Some(1),
            "the message between them is not a stop"
        );
        assert!(
            !app.cursor_up(),
            "the oldest block holds rather than wrapping to the far end"
        );
        assert_eq!(app.cursor(), Some(1));

        assert!(app.cursor_down());
        assert_eq!(app.cursor(), Some(3));
        assert!(app.cursor_down());
        assert_eq!(
            app.cursor(),
            None,
            "past the newest is how the selection is put away"
        );
        assert!(!app.cursor_down(), "and there is nothing below that");
    }

    /// Acceptance: a toggle with nothing under the cursor is a no-op.
    #[test]
    fn toggling_opens_the_selected_block_and_does_nothing_with_no_selection() {
        // The block is entry **zero**, deliberately: a toggle that treated no
        // selection as "the first one" would pass a test where index 0 was a
        // message, and open the wrong block in the client.
        let mut app = App::default();
        app.record_tool_invoked("c1".into(), "read".into(), None);
        app.record(Who::You, "hello");
        assert!(
            !app.toggle_expanded(),
            "an empty selection toggled something"
        );
        assert!(!tools(&app)[0].expanded, "and nothing opened");

        app.cursor_up();
        assert!(app.toggle_expanded());
        assert!(tools(&app)[0].expanded, "it opened");
        assert!(app.toggle_expanded());
        assert!(!tools(&app)[0].expanded, "and the same key closed it");

        // A cursor that outlived its entry costs a keystroke, not the client.
        app.transcript.clear();
        assert!(!app.toggle_expanded(), "a stale index must not panic");
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
        app.record_tool_result("c1", "contents".into(), false);
        app.record_tool_invoked("c2".into(), "fs.read".into(), Some("{}".into()));
        app.record_tool_result("c2", "tool `fs.read` error: NotFound".into(), true);

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
        app.record_tool_result("nobody", "orphan".into(), false);

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
        app.record_tool_result("c2", "done".into(), false);

        app.interrupt_open_tools();
        let blocks = tools(&app);
        assert_eq!(blocks[0].status, ToolStatus::Interrupted, "the open one");
        assert_eq!(
            blocks[1].status,
            ToolStatus::Done {
                content: "done".into(),
                failed: false
            },
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
        app.record_tool_result("b", "B".into(), false);
        app.record_tool_result("a", "A".into(), false);

        let blocks = tools(&app);
        assert_eq!(
            blocks[0].status,
            ToolStatus::Done {
                content: "A".into(),
                failed: false
            }
        );
        assert_eq!(
            blocks[1].status,
            ToolStatus::Done {
                content: "B".into(),
                failed: false
            }
        );
    }
}
