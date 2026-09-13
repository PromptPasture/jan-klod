//! The TUI's pure state model — no terminal, no I/O, so it is unit-tested.
//!
//! The `tui` binary is a thin shell: key events mutate this state, answers
//! record, and rendering is a pure function of it.

use std::collections::VecDeque;

use crate::commands::{Availability, Command};
use crate::composer::Composer;
use crate::paths;

/// Who authored a transcript line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Who {
    /// User's message.
    You,
    /// Core's answer.
    Klod,
    /// Client/transport error.
    Error,
    /// Client status note (connect banner, hints).
    Status,
}

/// How a tool call is going.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolStatus {
    /// Invoked, no result yet.
    Running,
    /// Answered, with what it returned and whether it failed.
    ///
    /// Struct variant not `Done(String)` + flag: "failed" only makes sense
    /// with a result; a separate field would let `Running`/`Interrupted`
    /// carry it pointlessly.
    Done {
        /// Tool's return value.
        content: String,
        /// Whether core reported the call failed (#162), not inferred from
        /// `content`.
        failed: bool,
    },
    /// The turn ended with this call still open.
    ///
    /// A `Running` block forever reads as hung, not as a stopped turn.
    Interrupted,
}

/// A tool call and its result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolBlock {
    /// Call id; pairs the two halves.
    pub id: String,
    /// Tool name.
    pub name: String,
    /// JSON arguments. Option because "not sent" and "call took none"
    /// differ. #161: both send them; `None` = older core or omitted.
    pub arguments: Option<String>,
    /// Running, done, or interrupted.
    pub status: ToolStatus,
    /// Whether full arguments and result show. Collapsed by default;
    /// **failure opens itself** (hiding the reason one keystroke away
    /// is wrong for failures).
    pub expanded: bool,
}

/// One entry in the transcript.
///
/// Enum not `Who`+fields: tool blocks aren't speakers. Squeezing into
/// `{ who, text }` would make `text` a rendering, hiding the model's meaning
/// from the client (#100).
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
/// Four states, only three stored — see [`App::turn`].
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum Turn {
    /// Nothing running. The composer's text is a new message.
    #[default]
    Idle,
    /// A turn is running and text is arriving.
    Streaming,
    /// A cancel has been *asked for*, not "the turn stopped". Over stdio,
    /// `turn/cancel` can't send yet (#157); over REST+SSE it's the dropped
    /// stream the conductor notices asynchronously. State ≠ stream.
    Cancelling,
    /// A confirmation is waiting. The turn underneath is still streaming.
    Blocked,
}

/// The stored part of [`Turn`] — every state except `Blocked`.
///
/// `Blocked` is omitted: `App::pending_prompt` answers "waiting?", so
/// storing it would duplicate state. Pattern: `menu: Option<usize>` — one
/// field, `Some` *means* open.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
enum Phase {
    #[default]
    Idle,
    Streaming,
    Cancelling,
}

/// The REPL/TUI state: the input buffer and the scrollback transcript.
///
/// Four separate facts (`should_quit`, `dialog_open`, `cancel_requested`,
/// `disconnected`) need no shared state machine; an enum per field would
/// invent false constraints.
#[derive(Debug, Default)]
#[allow(clippy::struct_excessive_bools)]
pub struct App {
    /// The message being written, with its caret (#150).
    ///
    /// [`Composer`] not `String`: emoji backspace leaves halves behind
    /// with plain `pop`.
    pub composer: Composer,
    /// The conversation so far, oldest first.
    pub transcript: Vec<Entry>,
    /// Set when the user asked to quit.
    pub should_quit: bool,
    /// Submitted messages, oldest first (#151). Appended, never mutated;
    /// edited recalls don't affect it. Not persisted; the event log is truth.
    history: Vec<String>,
    /// Where recall is in [`Self::history`], and the text it placed in the
    /// composer. The text enables "unmodified": composer is unmodified while
    /// holding exactly what recall placed there.
    recall: Option<(usize, String)>,
    /// Which entry the `/` menu has highlighted, when it is open.
    ///
    /// `Some` **is** open, no separate flag. Menu shows derives from
    /// composer (see [`Self::menu_query`]), only highlight is stored.
    menu: Option<usize>,
    /// The open `@` completion: fragment, highlight, and result. Cached
    /// (recomputing walks the filesystem). Fragment stored to prevent stale
    /// lists against newer fragments.
    completion: Option<(String, usize, paths::Completion)>,
    /// Which tool block `Ctrl+O` acts on (#155). Index into [`Self::transcript`],
    /// only [`Entry::Tool`]: messages have nothing to open. `None` is common.
    /// Unused in reading; appearing uninvited would move highlights while user types.
    cursor: Option<usize>,
    /// The turn, minus the derived state. See [`Phase`].
    phase: Phase,
    /// Stop asked but not sent. Like [`Self::should_quit`]: model records
    /// intent, caller does it (caller holds transport). Commands dispatch
    /// here; this type has no transport.
    cancel_requested: bool,
    /// Confirmations a turn is waiting on, oldest first (#103). Queue not slot:
    /// second `ask` waits behind first, doesn't overwrite. Dialog shows `1 of 2`.
    /// `Turn::Blocked` ← non-empty.
    prompts: VecDeque<Pending>,
    /// Whether the modal for the front of [`Self::prompts`] is on screen.
    /// Separate from "pending?": `Esc` closes without answering. Turn stays
    /// `Blocked` with question queued.
    dialog_open: bool,
    /// Transport died (gateway or core stopped, #104). Never clears.
    disconnected: bool,
    /// `warning`/`error` toast for a few ticks (#104), newest wins. Full text
    /// already in transcript; unread expirations lose nothing.
    toast: Option<Toast>,
    /// Session id being driven. Owned, mutable (#105). Switching replaces it
    /// in [`Self::load_session`].
    session: String,
    /// Transport is spawned-gateway. Set once at startup. Matters in
    /// [`Self::request_quit`]: stdio turn lost on quit.
    stdio: bool,
    /// The session switcher (#105), when open.
    sessions: Option<Sessions>,
    /// Set when `Ctrl+S`/`/sessions` asked but not sent yet. Like
    /// [`Self::cancel_requested`]: model records intent, caller performs it.
    sessions_requested: bool,
    /// Set when `/new` asked but not sent to core yet. Caller sends
    /// `session/create` and calls [`Self::load_session`].
    new_session_requested: bool,
    /// The help overlay (#105), when open.
    help: Option<Help>,
    /// The sidebar-in-a-dialog (#105), when open. Reachable at widths where
    /// the permanent sidebar pane hides.
    sidebar_dialog: bool,
    /// The quit confirm (#105), when open.
    quit: Option<QuitConfirm>,
}

/// A toast: text, kind, and expiry tick. Expiry in `tick` units (not
/// wall-clock), same as spinner advance. Testable without sleeps.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Toast {
    text: String,
    kind: ToastKind,
    expires_at: usize,
}

/// Toast display duration in ticks (~few seconds at 50ms/tick).
const TOAST_TICKS: usize = 60;

/// Toast type: `warning` or `error` (#104); others are in transcript.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToastKind {
    /// `warning()`.
    Warning,
    /// `removed()`.
    Error,
}

/// Session entry from `session/list`: id and preview. Shown in switcher
/// (#105), populated fresh per open.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionEntry {
    /// Session id.
    pub id: String,
    /// First 80 characters of the session's first user message, as
    /// `session/list` sends it.
    pub preview: String,
}

/// Session message from `session/get`. Reduced to transcript-rebuild needs
/// (#105 point 3). No `seq` (client doesn't address log positions);
/// `session/fork` out of scope.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionMessage {
    /// `system`, `user`, `assistant` or `tool`, exactly as the wire sends it.
    pub role: String,
    /// The message text.
    pub content: String,
    /// Present only on a tool result, tying it to the call it answers.
    pub tool_call_id: Option<String>,
}

/// The session switcher's model (#105). Populated once per open from
/// `session/list`, filtered by typed text.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
struct Sessions {
    entries: Vec<SessionEntry>,
    query: String,
    selected: usize,
}

/// Help overlay model (#105): scroll position only. Content is
/// [`crate::blocks::help_dialog`] (pure from [`crate::keymap::BINDINGS`]).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct Help {
    scroll: usize,
}

/// Quit confirm model (#105). Four independent facts, no invalid combos.
/// Enum per field would create four one-wider types replacing four flags.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(clippy::struct_excessive_bools)]
struct QuitConfirm {
    /// `false` is cancel, the default; `true` selects quit.
    quit_selected: bool,
    /// Whether a turn was running when opened. Needs the wording to say
    /// what happens to it and triggers a second confirmation.
    mid_turn: bool,
    /// Whether quitting genuinely loses the running turn: stdio's gateway
    /// dies with this client; `--addr`'s does not. Stored regardless of
    /// `mid_turn` rather than nested; nothing reads it without `mid_turn`.
    turn_lost: bool,
    /// Set once quit is selected and confirmed once while `mid_turn`. The
    /// "second, explicit confirmation" #105 asks for. A second `Enter` with
    /// this set actually quits.
    escalate: bool,
}

/// A confirmation a running turn is waiting on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Prompt {
    /// The session this question was asked on — **not necessarily the client's
    /// current one** (client may drive more than one). What `turn/answer`
    /// must be sent with. `tui.rs` used to send its own session instead, the
    /// bug #103 opens on.
    pub session: String,
    /// What is being asked.
    pub question: String,
    /// The answers core recognises. Empty means free text, the protocol's
    /// convention.
    pub options: Vec<String>,
    /// What core assumes if nobody answers — a denial, for the permission gate.
    pub default: String,
}

/// A queued prompt plus what changes before it's answered: which option is
/// highlighted. Kept beside the prompt rather than a second `Option` on
/// [`App`], so two queued questions won't be confused by *which* selection
/// field is set. One slot per prompt, traveling with it when a second `ask`
/// appends behind it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Pending {
    prompt: Prompt,
    /// Index into `prompt.options`. Starts at `default`'s index (searched,
    /// not assumed first). `None` when `default` is absent from `options`:
    /// core contradicting itself; demand explicit choice rather than guess.
    /// Also `None` for free-text prompts (`options` empty), where nothing
    /// selects.
    selected: Option<usize>,
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
    /// Starting at the newest is why this is `Alt+Up`: the transcript grows
    /// downwards, the wanted call usually just happened, walking up mirrors
    /// scrolling back.
    ///
    /// Returns whether it moved.
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

    /// Move cursor to next tool block or off the end. Past the newest it
    /// clears; `Alt+Down` is how user deselects — the same key that selected.
    pub fn cursor_down(&mut self) -> bool {
        let Some(at) = self.cursor else { return false };
        self.cursor = self.stops().iter().find(|&&i| i > at).copied();
        true
    }

    /// Open or close the block under the cursor. Returns whether anything
    /// happened. Nothing under cursor is a no-op, not a panic: index is
    /// re-checked, not trusted.
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

    /// The text being written. Accessor prevents desynchronising caret
    /// from text.
    #[must_use]
    pub fn input(&self) -> &str {
        self.composer.text()
    }

    /// Insert a typed character at the caret.
    pub fn push_char(&mut self, c: char) {
        self.composer.push(c);
        self.maybe_open_menu();
        if c == '@' {
            // Opened here only, so deleting to older `@` doesn't reopen
            // a dismissed list.
            self.completion = Some((String::new(), 0, paths::Completion::default()));
        }
    }

    /// Delete the grapheme before the caret.
    pub fn backspace(&mut self) {
        self.composer.backspace();
        self.maybe_open_menu();
    }

    /// Take trimmed, non-empty submission: record as [`Who::You`], clear
    /// input, return message. Whitespace-only yields `None`.
    pub fn take_submission(&mut self) -> Option<String> {
        let message = self.composer.take()?;
        self.history.push(message.clone());
        self.recall = None;
        self.record(Who::You, message.clone());
        Some(message)
    }

    /// The fragment the `/` menu is filtering on, when it is open.
    ///
    /// A command is `/` then non-whitespace. On space or newline, it stops
    /// being one and the menu closes.
    #[must_use]
    pub fn menu_query(&self) -> Option<&str> {
        self.menu?;
        let text = self.composer.text();
        (text.starts_with('/') && !text.contains(char::is_whitespace)).then_some(text)
    }

    /// The menu entries. Empty = empty state, not closed.
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

    /// Move the highlight, wrapping. Short list convenient; wrapping is
    /// disorienting in history.
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

    /// Run the highlighted command, or explain why it cannot run.
    ///
    /// Returns whether anything was accepted. `false` on empty means `Enter`
    /// still submits.
    pub fn menu_accept(&mut self) -> bool {
        let Some(command) = self.menu_entries().get(self.menu_selected()).copied() else {
            return false;
        };
        self.menu = None;
        self.composer.set("");
        match command.availability {
            Availability::Ready => match command.name {
                "/newline" => self.composer.push('\n'),
                // Ask before leaving rather than quitting outright. Same
                // confirming path `Ctrl+D` and `Ctrl+C` (idle, empty) use (#105).
                "/quit" => self.request_quit(),
                // Sets the ask; caller sends it. Commands dispatch here; this
                // type has no transport.
                "/cancel" => {
                    if !self.cancel() {
                        self.record(Who::Status, "no turn is running".to_string());
                    }
                }
                // #105: the other asks a transport must serve, plus the
                // one that needs none.
                "/new" => self.request_new_session(),
                "/sessions" => self.request_sessions(),
                "/help" => self.toggle_help(),
                // Unreachable while table and match agree. Status line rather
                // than panic if they diverge; a status beats a crash.
                other => self.record(Who::Status, format!("{other} is not wired up")),
            },
            Availability::Pending(reason) => {
                self.record(Who::Status, format!("{} — {reason}", command.name));
            }
        }
        true
    }

    /// A tool is about to run; open a block for it.
    pub fn record_tool_invoked(&mut self, id: String, name: String, arguments: Option<String>) {
        self.transcript.push(Entry::Tool(ToolBlock {
            id,
            name,
            arguments,
            status: ToolStatus::Running,
            expanded: false,
        }));
    }

    /// A tool returned; close the block with the matching id.
    ///
    /// Unmatched result becomes a muted line, not silence. Client and core
    /// disagree about what's open; user should see something arrived. Silent
    /// drop looks like it never came.
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
                // A failure opens itself. Before #162, this asked
                // `blocks::reads_as_failure(&content)`, matching core's wording.
                // Now the core reports it and we read the fact.
                block.expanded = failed;
                block.status = ToolStatus::Done { content, failed };
            }
            None => self.record_status(format!("tool result for an unknown call `{id}`")),
        }
    }

    /// The turn ended; nothing still open is still running.
    ///
    /// Called on `done` **and** on error. A failed turn leaves the same
    /// blocks open as a finished one. A spinner that never stops reads as a
    /// hung client, not as a turn that moved on.
    pub fn interrupt_open_tools(&mut self) {
        for entry in &mut self.transcript {
            if let Entry::Tool(block) = entry {
                if block.status == ToolStatus::Running {
                    block.status = ToolStatus::Interrupted;
                }
            }
        }
    }

    /// Whether the `@` completion is open.
    #[must_use]
    pub const fn completion_open(&self) -> bool {
        self.completion.is_some()
    }

    /// The paths on offer. Empty = empty state, not closed.
    #[must_use]
    pub fn completion_entries(&self) -> &[String] {
        self.completion
            .as_ref()
            .map_or(&[], |(_, _, found)| found.entries.as_slice())
    }

    /// Whether the list stopped at the cap. Shown, never silent (#145).
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

    /// Recompute list if fragment moved; close if none remains. `root` is an
    /// argument not `std::env::current_dir()` so tests state the tree.
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
    /// **No file is read.** The path is text in a message; the core decides
    /// what to do. A client that opened it would grow policy.
    ///
    /// Returns whether anything was accepted.
    pub fn completion_accept(&mut self) -> bool {
        let Some(path) = self
            .completion_entries()
            .get(self.completion_selected())
            .cloned()
        else {
            return false;
        };
        // The `@` goes too: #101 asks for accepted text to be "plain path".
        // Keeping it leaves a hint with no contract. Client inserts text, reads
        // nothing; message should say what user means, not carry unagreed
        // convention. `'@'` is one byte, hence `+ 1`.
        let typed = self
            .completion
            .as_ref()
            .map_or(0, |(fragment, _, _)| fragment.len() + 1);
        self.composer.replace_before(typed, &path);
        self.completion = None;
        true
    }

    /// Whether `↑`/`↓` walks history or moves caret. Two unmodified states:
    /// fresh empty composer, **and** one still holding exactly what recall
    /// put there. Without the second, `↑` twice recalls once, then moves caret.
    fn composer_unmodified(&self) -> bool {
        self.composer.is_empty()
            || self
                .recall
                .as_ref()
                .is_some_and(|(_, text)| text == self.composer.text())
    }

    /// `↑`: the previous message, or the caret up if history isn't this keypress.
    pub fn history_up(&mut self, width: usize) {
        if !(self.composer.on_first_row(width) && self.composer_unmodified()) {
            self.composer.up(width);
            return;
        }
        // A cleared composer starts the walk over, not resuming from recall's
        // position. Otherwise clearing and pressing `↑` lands on the entry
        // *before* the one just discarded, unlike emptying anywhere else.
        if self.composer.is_empty() {
            self.recall = None;
        }
        let next = match self.recall {
            // At the oldest: stop rather than wrap. Wrapping hands the newest
            // message at the moment they asked for the oldest.
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
            // Past the newest is the line the user was writing before
            // recalling; it's empty because they submitted the last one.
            self.composer.set("");
            self.recall = None;
        }
    }

    /// Where the turn is. `Blocked` wins over what's stored: a question's
    /// fact, not the stream. Turn underneath confirmation still streams,
    /// becoming visible when answered.
    #[must_use]
    pub fn turn(&self) -> Turn {
        if !self.prompts.is_empty() {
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

    /// Ask for the running turn to stop. Returns whether this is the
    /// **first** ask. Caller uses this to decide whether to act. A second
    /// `Ctrl+C` while `Cancelling` returns `false`: cancelling isn't
    /// idempotent at transport; hammering the key must not queue messages.
    /// A pending confirmation is dropped. A question about an ending turn is
    /// one nobody will answer.
    pub fn cancel(&mut self) -> bool {
        if !matches!(self.phase, Phase::Streaming) {
            return false;
        }
        self.phase = Phase::Cancelling;
        self.cancel_requested = true;
        self.prompts.clear();
        self.dialog_open = false;
        // Tool block left `Running` is a spinner that never stops, reading
        // as hung rather than stopped turn.
        self.interrupt_open_tools();
        true
    }

    /// Take pending cancel ask, sent **once** (not idempotent).
    /// [`Self::cancel`] sets it and refuses twice.
    pub const fn take_cancel_request(&mut self) -> bool {
        let asked = self.cancel_requested;
        self.cancel_requested = false;
        asked
    }

    /// Stream ended, answer incomplete. Transcript unchanged (by design).
    /// Keep what arrived before cancel; stops don't lose progress.
    pub fn end_turn(&mut self) {
        self.phase = Phase::Idle;
        self.prompts.clear();
        self.dialog_open = false;
        self.interrupt_open_tools();
    }

    /// Record core's answer.
    pub fn record_answer(&mut self, text: impl Into<String>) {
        self.record(Who::Klod, text);
    }

    /// Append delta to in-progress Klod message, or start one.
    pub fn apply_delta(&mut self, text: &str) {
        match self.transcript.last_mut() {
            Some(Entry::Message {
                who: Who::Klod,
                text: existing,
            }) => existing.push_str(text),
            _ => self.record(Who::Klod, text.to_string()),
        }
    }

    /// Finish turn: replace partial streamed entry or record fresh answer.
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

    /// Queue a confirmation, opening dialog if none pending. **Queue not
    /// stack** (#103): second ask appends. Dialog opens only first time (user
    /// may close intentionally). Selection starts at `default`'s index
    /// (searched, not assumed first). If `default` missing, no selection.
    pub fn ask(&mut self, prompt: Prompt) {
        let selected = prompt.options.iter().position(|o| o == &prompt.default);
        let opening = self.prompts.is_empty();
        self.prompts.push_back(Pending { prompt, selected });
        if opening {
            self.dialog_open = true;
        }
    }

    /// Whether the modal for the front prompt is on screen.
    #[must_use]
    pub const fn dialog_open(&self) -> bool {
        self.dialog_open
    }

    /// Reopen dialog if prompt pending (Esc close's other half). `Enter`
    /// while closed reopens rather than answering blind.
    pub fn open_dialog(&mut self) {
        if !self.prompts.is_empty() {
            self.dialog_open = true;
        }
    }

    /// Close dialog **without answering** (turn stays `Blocked`).
    /// Closing ≠ choosing.
    pub const fn close_dialog(&mut self) {
        self.dialog_open = false;
    }

    /// Front prompt if waiting (accessor to keep queue+dialog state private).
    #[must_use]
    pub fn pending_prompt(&self) -> Option<&Prompt> {
        self.prompts.front().map(|p| &p.prompt)
    }

    /// Which option is highlighted in the front prompt, if any.
    #[must_use]
    pub fn prompt_selected(&self) -> Option<usize> {
        self.prompts.front().and_then(|p| p.selected)
    }

    /// How many prompts are waiting, including the one on screen — what makes
    /// the dialog's `1 of 2` true.
    #[must_use]
    pub fn prompt_queue_len(&self) -> usize {
        self.prompts.len()
    }

    /// Move selection, wrapping like `/` menu. No-op if no options or none pending.
    pub fn prompt_move(&mut self, delta: isize) {
        let Some(front) = self.prompts.front_mut() else {
            return;
        };
        let len = front.prompt.options.len();
        if len == 0 {
            return;
        }
        let current = front.selected.unwrap_or(0);
        let len_i = isize::try_from(len).unwrap_or(1);
        let next = (isize::try_from(current).unwrap_or(0) + delta).rem_euclid(len_i);
        front.selected = Some(usize::try_from(next).unwrap_or(0));
    }

    /// Jump to digit-key option (as displayed). Out of range is no-op not clamp.
    pub fn prompt_jump(&mut self, one_based: usize) {
        let Some(front) = self.prompts.front_mut() else {
            return;
        };
        if one_based >= 1 && one_based <= front.prompt.options.len() {
            front.selected = Some(one_based - 1);
        }
    }

    /// Take front answer if ready, returning `(session, answer)`.
    ///
    /// Session from notification (not `self`): multiple clients may run (#103).
    /// Answer is byte-identical to option or composer text (no normalization).
    /// Returns `None` without dequeuing if no selection (no guessing).
    ///
    /// # Panics
    ///
    /// Never: the queue is proven non-empty above.
    pub fn take_answer(&mut self) -> Option<(String, String)> {
        let front = self.prompts.front()?;
        let answer = if front.prompt.options.is_empty() {
            let typed = self.composer.text();
            if typed.is_empty() {
                front.prompt.default.clone()
            } else {
                typed.to_string()
            }
        } else {
            front.selected.map(|i| front.prompt.options[i].clone())?
        };
        let Pending { prompt, .. } = self.prompts.pop_front().expect("front just checked above");
        // `take` clears the composer and its caret together: an answer left
        // in the buffer would send as the next message.
        self.composer.take();
        // The next queued prompt, if any, shows immediately, not behind a
        // closed dialog. "Answering the first shows the second" (#103's
        // Acceptance) means visibly.
        self.dialog_open = !self.prompts.is_empty();
        self.record(Who::Status, format!("{} → {answer}", prompt.question));
        Some((prompt.session, answer))
    }

    /// Record a client/transport error.
    pub fn record_error(&mut self, text: impl Into<String>) {
        self.record(Who::Error, text);
    }

    /// Record a status note.
    pub fn record_status(&mut self, text: impl Into<String>) {
        self.record(Who::Status, text);
    }

    /// Transport dead, stays so (#104). Like [`Self::end_turn`]: in-flight
    /// can't finish; drop unopened prompts, like [`Self::cancel`] for user
    /// stops. Idempotent: caller may notice same dead transport multiple times.
    pub fn disconnect(&mut self, reason: impl Into<String>) {
        if !self.disconnected {
            self.disconnected = true;
            self.record_error(reason);
        }
        // Unconditional: nothing can still be running once transport is gone,
        // whichever telling of it this is.
        self.phase = Phase::Idle;
        self.prompts.clear();
        self.dialog_open = false;
        self.interrupt_open_tools();
    }

    /// Whether the transport is known to be gone.
    #[must_use]
    pub const fn disconnected(&self) -> bool {
        self.disconnected
    }

    /// Status bar connection+turn word (#104): `ready`, `streaming`, `blocked`,
    /// `cancelling`, or `disconnected`. `disconnected` wins (turn already
    /// `Idle`; #104 prevents false `ready`).
    #[must_use]
    pub fn connection_state(&self) -> &'static str {
        if self.disconnected {
            return "disconnected";
        }
        match self.turn() {
            Turn::Idle => "ready",
            Turn::Streaming => "streaming",
            Turn::Blocked => "blocked",
            Turn::Cancelling => "cancelling",
        }
    }

    /// Toast `warning` (#104): text in transcript + toast, newest wins.
    pub fn toast_warning(&mut self, text: impl Into<String>, tick: usize) {
        let text = text.into();
        self.record_status(format!("⚠ {text}"));
        self.toast = Some(Toast {
            text,
            kind: ToastKind::Warning,
            expires_at: tick + TOAST_TICKS,
        });
    }

    /// Toast `error` (#104): like warning, but via [`Self::record_error`].
    pub fn toast_error(&mut self, text: impl Into<String>, tick: usize) {
        let text = text.into();
        self.record_error(text.clone());
        self.toast = Some(Toast {
            text,
            kind: ToastKind::Error,
            expires_at: tick + TOAST_TICKS,
        });
    }

    /// The toast on screen, if one has not expired, as `(text, kind)`.
    #[must_use]
    pub fn toast(&self) -> Option<(&str, ToastKind)> {
        self.toast.as_ref().map(|t| (t.text.as_str(), t.kind))
    }

    /// Expire the toast once `tick` passes the point it was raised.
    ///
    /// Driven by [`crate::tui`]'s poll tick, not a clock (#104). Spinner
    /// already advances on every timeout; reusing it lets toast expiry be
    /// tested without `sleep`.
    pub fn tick(&mut self, tick: usize) {
        if self.toast.as_ref().is_some_and(|t| tick >= t.expires_at) {
            self.toast = None;
        }
    }

    /// Request quit unconditionally. Kept for `Esc`, whose behaviour this
    /// slice leaves alone. #105 names three other triggers for the confirming
    /// version, [`Self::request_quit`].
    pub const fn quit(&mut self) {
        self.should_quit = true;
    }

    /// The session this client is driving.
    #[must_use]
    pub fn session(&self) -> &str {
        &self.session
    }

    /// Set the session id — at startup, and again on every switch or `/new`
    /// (#105's design point 1: id stops being fixed for the run).
    pub fn set_session(&mut self, id: impl Into<String>) {
        self.session = id.into();
    }

    /// Record which kind of transport this run has, once, at startup. See the
    /// field's own docs for why [`Self::request_quit`] needs it.
    pub const fn set_stdio(&mut self, stdio: bool) {
        self.stdio = stdio;
    }

    /// Whether anything has been sent this run — what quit confirm (#105)
    /// skips entirely for the lack of.
    const fn has_sent_anything(&self) -> bool {
        !self.history.is_empty()
    }

    // ---- session switcher (#105) ----

    /// `Ctrl+S`/`/sessions`: ask for fresh `session/list`. Never cached;
    /// see [`Self::open_sessions`]. No-op while ask dialog open (owns
    /// every key; second dialog unreachable).
    pub const fn request_sessions(&mut self) {
        if !self.dialog_open {
            self.sessions_requested = true;
        }
    }

    /// Take the pending `session/list` ask, if there is one. Drains; same
    /// contract as [`Self::take_cancel_request`].
    pub const fn take_sessions_request(&mut self) -> bool {
        let asked = self.sessions_requested;
        self.sessions_requested = false;
        asked
    }

    /// `session/list` answered: show the switcher with what it returned.
    pub fn open_sessions(&mut self, entries: Vec<SessionEntry>) {
        self.sessions = Some(Sessions {
            entries,
            query: String::new(),
            selected: 0,
        });
    }

    /// Whether the switcher is on screen.
    #[must_use]
    pub const fn sessions_open(&self) -> bool {
        self.sessions.is_some()
    }

    /// `Esc`: close the switcher. Nothing was asked of the core, so there is
    /// nothing to undo.
    pub fn sessions_close(&mut self) {
        self.sessions = None;
    }

    /// What has been typed to filter the list, since the switcher opened.
    #[must_use]
    pub fn sessions_query(&self) -> &str {
        self.sessions.as_ref().map_or("", |s| s.query.as_str())
    }

    /// Type one character into the filter.
    pub fn sessions_push_char(&mut self, c: char) {
        if let Some(sessions) = self.sessions.as_mut() {
            sessions.query.push(c);
            sessions.selected = 0;
        }
    }

    /// Delete the last character of the filter.
    pub fn sessions_backspace(&mut self) {
        if let Some(sessions) = self.sessions.as_mut() {
            sessions.query.pop();
            sessions.selected = 0;
        }
    }

    /// Entries matching typed filter on id or preview, case-insensitive.
    /// Empty = empty state, not closed; same as [`Self::menu_entries`].
    #[must_use]
    pub fn sessions_filtered(&self) -> Vec<&SessionEntry> {
        let Some(sessions) = self.sessions.as_ref() else {
            return Vec::new();
        };
        if sessions.query.is_empty() {
            return sessions.entries.iter().collect();
        }
        let needle = sessions.query.to_lowercase();
        sessions
            .entries
            .iter()
            .filter(|e| {
                e.id.to_lowercase().contains(&needle) || e.preview.to_lowercase().contains(&needle)
            })
            .collect()
    }

    /// Which entry is highlighted, clamped to what the filter shows.
    #[must_use]
    pub fn sessions_selected(&self) -> usize {
        let len = self.sessions_filtered().len();
        self.sessions
            .as_ref()
            .map_or(0, |s| s.selected)
            .min(len.saturating_sub(1))
    }

    /// Move the highlight, wrapping like the `/` menu.
    pub fn sessions_move(&mut self, delta: isize) {
        let len = self.sessions_filtered().len();
        if len == 0 {
            return;
        }
        let current = self.sessions_selected();
        let len_i = isize::try_from(len).unwrap_or(1);
        let next = (isize::try_from(current).unwrap_or(0) + delta).rem_euclid(len_i);
        if let Some(sessions) = self.sessions.as_mut() {
            sessions.selected = usize::try_from(next).unwrap_or(0);
        }
    }

    /// `Enter`: the session to switch to, or a refusal recorded with no id
    /// when a turn is running (#105 point 4). Mid-turn refused with reason,
    /// not silent. Switcher stays open; caller closes it when
    /// [`Self::load_session`] runs.
    pub fn sessions_confirm(&mut self) -> Option<String> {
        let id = self
            .sessions_filtered()
            .get(self.sessions_selected())
            .map(|e| e.id.clone())?;
        if self.turn() == Turn::Idle {
            Some(id)
        } else {
            self.record_status(
                "cannot switch sessions while a turn is running — cancel it first \
                 (Ctrl+C), or wait for it to finish"
                    .to_string(),
            );
            None
        }
    }

    /// `session/get` answered (or `/new`'s `session/create` with empty
    /// transcript): replace this client's view with what core holds for `id`
    /// (reload, not stash, #105 point 3). Switcher closes.
    pub fn load_session(&mut self, id: String, messages: Vec<SessionMessage>) {
        self.session = id;
        self.transcript = messages
            .into_iter()
            .filter_map(session_message_entry)
            .collect();
        self.cursor = None;
        self.sessions = None;
    }

    /// `/new`: ask the loop for a fresh `session/create`.
    pub const fn request_new_session(&mut self) {
        if !self.dialog_open {
            self.new_session_requested = true;
        }
    }

    /// Take the pending `session/create` ask, if there is one. Drains; same
    /// contract as [`Self::take_cancel_request`].
    pub const fn take_new_session_request(&mut self) -> bool {
        let asked = self.new_session_requested;
        self.new_session_requested = false;
        asked
    }

    // ---- help overlay (#105) ----

    /// Whether the overlay is on screen.
    #[must_use]
    pub const fn help_open(&self) -> bool {
        self.help.is_some()
    }

    /// `?` on empty composer, or `/help`: open or close. No-op while ask
    /// dialog open, for the same reason as [`Self::request_sessions`].
    pub fn toggle_help(&mut self) {
        if self.dialog_open {
            return;
        }
        self.help = if self.help.is_some() {
            None
        } else {
            Some(Help::default())
        };
    }

    /// `Esc`/`?`: close it.
    pub const fn close_help(&mut self) {
        self.help = None;
    }

    /// How far the overlay has scrolled. Renderer clamps it against what it
    /// drew. This model has no width or height to know the bound.
    #[must_use]
    pub fn help_scroll(&self) -> usize {
        self.help.as_ref().map_or(0, |h| h.scroll)
    }

    /// Scroll by `delta` rows, negative moving up. Saturates at zero; past
    /// the top is a no-op, not a jump to the bottom.
    pub const fn help_scroll_by(&mut self, delta: isize) {
        if let Some(help) = self.help.as_mut() {
            help.scroll = help.scroll.saturating_add_signed(delta);
        }
    }

    // ---- sidebar-in-a-dialog (#105) ----

    /// Whether the sidebar's dialog is on screen.
    #[must_use]
    pub const fn sidebar_dialog_open(&self) -> bool {
        self.sidebar_dialog
    }

    /// `Ctrl+B`, at a width where sidebar has no permanent pane. No-op while
    /// ask dialog open, for the same reason as [`Self::request_sessions`].
    /// Caller (`tui.rs`) refuses when sidebar is visible; only it knows
    /// the last frame's layout.
    pub const fn toggle_sidebar_dialog(&mut self) {
        if self.dialog_open {
            return;
        }
        self.sidebar_dialog = !self.sidebar_dialog;
    }

    /// `Esc`/`Ctrl+B`: close it.
    pub const fn close_sidebar_dialog(&mut self) {
        self.sidebar_dialog = false;
    }

    // ---- quit confirm (#105) ----

    /// `Ctrl+D`/`C` on empty composer while `Idle`, or `/quit`: ask before
    /// leaving unless nothing happened (empty session is friction). Cancel is
    /// the initial selection.
    pub fn request_quit(&mut self) {
        if !self.has_sent_anything() {
            self.should_quit = true;
            return;
        }
        if self.dialog_open {
            // The ask modal owns every key; a quit confirm under it would be
            // unreachable, same reason as `request_sessions` refuses.
            return;
        }
        let mid_turn = self.turn() != Turn::Idle;
        self.quit = Some(QuitConfirm {
            quit_selected: false,
            mid_turn,
            turn_lost: mid_turn && self.stdio,
            escalate: false,
        });
    }

    /// Whether the quit confirm is on screen.
    #[must_use]
    pub const fn quit_confirm_open(&self) -> bool {
        self.quit.is_some()
    }

    /// `Esc`: close without quitting.
    pub const fn quit_confirm_close(&mut self) {
        self.quit = None;
    }

    /// What the dialog renders: `(mid_turn, turn_lost, escalate,
    /// quit_selected)`.
    #[must_use]
    pub fn quit_confirm(&self) -> Option<(bool, bool, bool, bool)> {
        self.quit
            .map(|q| (q.mid_turn, q.turn_lost, q.escalate, q.quit_selected))
    }

    /// `↑`/`↓`/`Tab`: there are only two options, so this toggles rather than
    /// moving an index.
    pub const fn quit_confirm_move(&mut self) {
        if let Some(q) = self.quit.as_mut() {
            q.quit_selected = !q.quit_selected;
        }
    }

    /// `Enter`. Cancel closes outright. Quit needs a second, explicit
    /// confirmation when a turn is running (#105's design point 5). The first
    /// `Enter` on `quit` sets [`QuitConfirm::escalate`] and re-asks; the
    /// second actually quits. With no turn, one `Enter` is enough.
    pub const fn quit_confirm_accept(&mut self) {
        let Some(q) = self.quit.as_mut() else {
            return;
        };
        if !q.quit_selected {
            self.quit = None;
            return;
        }
        if q.mid_turn && !q.escalate {
            q.escalate = true;
            return;
        }
        self.should_quit = true;
        self.quit = None;
    }

    fn record(&mut self, who: Who, text: impl Into<String>) {
        self.transcript.push(Entry::Message {
            who,
            text: text.into(),
        });
    }
}

/// One `session/get` message, as a transcript [`Entry`], or `None` for
/// `system` (skipped: the model's own instructions, not user text) (#105
/// design point 3).
fn session_message_entry(message: SessionMessage) -> Option<Entry> {
    match message.role.as_str() {
        "user" => Some(Entry::Message {
            who: Who::You,
            text: message.content,
        }),
        "assistant" => Some(Entry::Message {
            who: Who::Klod,
            text: message.content,
        }),
        "tool" => {
            // `session/get` carries no tool name or `failed` flag; only
            // `session/message`'s live notifications do (#106 added `seq`, not
            // either). The call id is what's known, standing in for the name.
            // Results are assumed to have succeeded absent any way to tell,
            // the same default a core older than #162 already rendered.
            let id = message.tool_call_id.unwrap_or_default();
            Some(Entry::Tool(ToolBlock {
                name: id.clone(),
                id,
                arguments: None,
                status: ToolStatus::Done {
                    content: message.content,
                    failed: false,
                },
                expanded: false,
            }))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::{App, Entry, Prompt, ToastKind, ToolStatus, Turn, Who};

    /// Acceptance: a dead transport moves the status word to `disconnected`
    /// and stops the spinner. `connection_state` only reports the stored turn,
    /// which `disconnect` forces back to `Idle`. `spinner_frame` returns
    /// `None` for `Turn::Idle` and nothing else here; one assertion covers
    /// both halves of #104's acceptance line.
    #[test]
    fn a_dead_transport_reports_disconnected_and_stops_the_turn() {
        let mut app = App::default();
        app.begin_turn();
        app.apply_delta("half an answer");
        assert_eq!(app.turn(), Turn::Streaming);

        app.disconnect("jan-klod-gateway exited");
        assert_eq!(app.connection_state(), "disconnected");
        assert_eq!(
            app.turn(),
            Turn::Idle,
            "a dead transport cannot still be streaming"
        );
        assert!(
            format!("{:?}", app.transcript).contains("jan-klod-gateway exited"),
            "the reason was not recorded"
        );

        // A prompt nobody can answer must not stay `Blocked` forever.
        app.begin_turn();
        app.ask(Prompt {
            session: "s1".into(),
            question: "proceed?".into(),
            options: vec!["y".into()],
            default: "y".into(),
        });
        app.disconnect("again"); // idempotent: no second narration
        assert_eq!(app.turn(), Turn::Idle);
        assert_eq!(
            format!("{:?}", app.transcript).matches("again").count(),
            0,
            "a second disconnect narrated something new"
        );
    }

    #[test]
    fn connection_state_reports_ready_streaming_blocked_and_cancelling() {
        let mut app = App::default();
        assert_eq!(app.connection_state(), "ready");
        app.begin_turn();
        assert_eq!(app.connection_state(), "streaming");
        app.ask(Prompt {
            session: "s1".into(),
            question: "q".into(),
            options: vec!["y".into()],
            default: "y".into(),
        });
        assert_eq!(app.connection_state(), "blocked");
        app.take_answer();
        app.cancel();
        assert_eq!(app.connection_state(), "cancelling");
    }

    /// Acceptance: a `warning` produces both a toast and a transcript line.
    /// The transcript line outlives the toast; a toast that expires unread
    /// loses nothing.
    #[test]
    fn a_warning_toasts_and_writes_the_transcript_and_the_toast_expires_first() {
        let mut app = App::default();
        app.toast_warning("provider fallback to gpt-4o-mini", 0);

        let (text, kind) = app.toast().expect("a toast was raised");
        assert_eq!(text, "provider fallback to gpt-4o-mini");
        assert_eq!(kind, ToastKind::Warning);
        assert!(
            format!("{:?}", app.transcript).contains("provider fallback to gpt-4o-mini"),
            "the full text did not land in the transcript"
        );

        // The toast expires; the transcript line does not move.
        app.tick(1_000);
        assert!(app.toast().is_none(), "the toast should have expired");
        assert!(
            format!("{:?}", app.transcript).contains("provider fallback to gpt-4o-mini"),
            "the transcript line vanished with the toast; nothing should be lost \
             when a toast expires unread"
        );
    }

    #[test]
    fn a_second_toast_replaces_the_first_rather_than_queuing() {
        let mut app = App::default();
        app.toast_warning("first", 0);
        app.toast_error("second", 0);
        let (text, kind) = app.toast().expect("a toast is showing");
        assert_eq!(text, "second");
        assert_eq!(kind, ToastKind::Error);
    }

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
                    "a user cancelling a turn wanted it to stop, not to lose it"
                );
            }
            other => panic!("the partial answer is gone: {other:?}"),
        }
        assert_eq!(
            tools(&app)[0].status,
            ToolStatus::Interrupted,
            "a block left running after cancel is a spinner that never stops"
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
            "cancelling isn't idempotent at the transport; hammering the key \
             must not queue a second message"
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
            session: "s1".into(),
            question: "run `rm -rf /`?".into(),
            options: vec!["yes".into(), "no".into()],
            default: "no".into(),
        });
        assert_eq!(app.turn(), Turn::Blocked);

        assert_eq!(
            app.take_answer(),
            Some(("s1".to_string(), "no".to_string())),
            "the default"
        );
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
            session: "s1".into(),
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
    /// field; they cannot disagree.
    #[test]
    fn blocked_is_derived_from_the_prompt_and_not_stored_beside_it() {
        let mut app = App::default();
        app.begin_turn();
        for _ in 0..2 {
            app.ask(Prompt {
                session: "s1".into(),
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

    /// Acceptance: the initial selection is `default`'s own index, for an
    /// arbitrary option order — including one where `default` is not first.
    #[test]
    fn the_initial_selection_is_defaults_index_wherever_it_sits() {
        let mut app = App::default();
        app.ask(Prompt {
            session: "s1".into(),
            question: "which?".into(),
            options: vec!["always".into(), "yes".into(), "no".into()],
            default: "no".into(),
        });
        assert_eq!(
            app.prompt_selected(),
            Some(2),
            "the default is the third option; selection must find it, not \
             assume position 0"
        );

        // A core that contradicts itself (`default` not among `options`)
        // must not have this guess at a selection.
        let mut confused = App::default();
        confused.ask(Prompt {
            session: "s1".into(),
            question: "which?".into(),
            options: vec!["yes".into(), "no".into()],
            default: "always".into(),
        });
        assert_eq!(
            confused.prompt_selected(),
            None,
            "a default absent from options must not be guessed at"
        );
    }

    /// Acceptance: the answer sent is byte-identical to the chosen entry,
    /// never normalised, even if it would look "cleaner".
    #[test]
    fn the_answer_is_byte_identical_to_the_chosen_option() {
        let mut app = App::default();
        app.ask(Prompt {
            session: "s1".into(),
            question: "which?".into(),
            options: vec![" Yes ".into(), "NO".into()],
            default: "NO".into(),
        });
        app.prompt_jump(1);
        let (_, answer) = app.take_answer().expect("a selection was made");
        assert_eq!(
            answer, " Yes ",
            "the answer must not be trimmed or lowercased"
        );
    }

    /// Acceptance: the session sent is the one the notification carried, not
    /// whatever the client happens to be driving elsewhere.
    #[test]
    fn the_answer_carries_the_prompts_own_session() {
        let mut app = App::default();
        app.ask(Prompt {
            session: "the-notifications-session".into(),
            question: "which?".into(),
            options: vec!["yes".into(), "no".into()],
            default: "no".into(),
        });
        let (session, _) = app.take_answer().expect("the default was selected");
        assert_eq!(session, "the-notifications-session");
    }

    /// Acceptance: two overlapping `ask`s queue rather than stack. Answering
    /// the first leaves the second exactly as it arrived.
    #[test]
    fn a_second_ask_queues_behind_the_first_and_neither_disturbs_the_other() {
        let mut app = App::default();
        app.ask(Prompt {
            session: "first".into(),
            question: "first?".into(),
            options: vec!["yes".into(), "no".into()],
            default: "no".into(),
        });
        app.ask(Prompt {
            session: "second".into(),
            question: "second?".into(),
            options: vec!["always".into(), "yes".into(), "no".into()],
            default: "no".into(),
        });
        assert_eq!(
            app.prompt_queue_len(),
            2,
            "a second ask must queue, not overwrite"
        );
        assert_eq!(
            app.pending_prompt().map(|p| p.question.as_str()),
            Some("first?"),
            "the front is still the first"
        );

        let (session, answer) = app.take_answer().expect("the first has a default");
        assert_eq!((session.as_str(), answer.as_str()), ("first", "no"));

        assert_eq!(app.prompt_queue_len(), 1, "one prompt remains");
        assert_eq!(
            app.pending_prompt().map(|p| p.question.as_str()),
            Some("second?"),
            "the second shows, unchanged"
        );
        assert_eq!(
            app.prompt_selected(),
            Some(2),
            "the second kept its own selection; `no` is index 2 there"
        );
        assert!(
            app.dialog_open(),
            "the second shows, not left behind a closed dialog"
        );
    }

    /// `Esc` closes the dialog without answering; the prompt survives so it
    /// can be reopened.
    #[test]
    fn esc_closes_without_answering_and_the_prompt_survives() {
        let mut app = App::default();
        app.ask(Prompt {
            session: "s1".into(),
            question: "which?".into(),
            options: vec!["yes".into(), "no".into()],
            default: "no".into(),
        });
        assert!(app.dialog_open(), "ask opens the dialog");

        app.close_dialog();
        assert!(!app.dialog_open());
        assert_eq!(
            app.turn(),
            Turn::Blocked,
            "closing the dialog must not answer"
        );
        assert!(
            app.pending_prompt().is_some(),
            "the prompt must survive unanswered close"
        );

        app.open_dialog();
        assert!(app.dialog_open(), "it can be reopened");
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

        assert_eq!(app.cursor(), None, "at rest has no selection");
        assert!(app.cursor_up(), "the first press selects something");
        assert_eq!(
            app.cursor(),
            Some(3),
            "it's the newest call, not the oldest"
        );
        assert!(app.cursor_up());
        assert_eq!(
            app.cursor(),
            Some(1),
            "the message between them is not a stop"
        );
        assert!(!app.cursor_up(), "the oldest block holds, not wrapping");
        assert_eq!(app.cursor(), Some(1));

        assert!(app.cursor_down());
        assert_eq!(app.cursor(), Some(3));
        assert!(app.cursor_down());
        assert_eq!(app.cursor(), None, "past the newest deselects");
        assert!(!app.cursor_down(), "nothing below that");
    }

    /// Acceptance: a toggle with nothing under the cursor is a no-op.
    #[test]
    fn toggling_opens_the_selected_block_and_does_nothing_with_no_selection() {
        // The block is entry **zero** deliberately: a toggle that treated no
        // selection as "the first one" would pass when index 0 is a message,
        // opening the wrong block in the client.
        let mut app = App::default();
        app.record_tool_invoked("c1".into(), "read".into(), None);
        app.record(Who::You, "hello");
        assert!(!app.toggle_expanded(), "empty selection toggled nothing");
        assert!(!tools(&app)[0].expanded, "nothing opened");

        app.cursor_up();
        assert!(app.toggle_expanded());
        assert!(tools(&app)[0].expanded, "it opened");
        assert!(app.toggle_expanded());
        assert!(!tools(&app)[0].expanded, "same key closed it");

        // A cursor that outlived its entry costs a keystroke, not the client.
        app.transcript.clear();
        assert!(!app.toggle_expanded(), "stale index doesn't panic");
    }

    /// Acceptance: **a failure opens itself.** A tool that failed is the
    /// block whose contents a user certainly wants. Hiding the reason behind
    /// a keystroke they must know about is wrong. A successful call is the
    /// opposite: twelve reads = twelve screens of JSON nobody asked for.
    #[test]
    fn a_failed_call_arrives_expanded_and_a_successful_one_does_not() {
        let mut app = App::default();
        app.record_tool_invoked("c1".into(), "fs.read".into(), Some("{}".into()));
        app.record_tool_result("c1", "contents".into(), false);
        app.record_tool_invoked("c2".into(), "fs.read".into(), Some("{}".into()));
        app.record_tool_result("c2", "tool `fs.read` error: NotFound".into(), true);

        let blocks = tools(&app);
        assert!(
            !blocks[0].expanded,
            "a result nobody needs to read didn't open"
        );
        assert!(
            blocks[1].expanded,
            "the reason a call failed is one keystroke away; the user doesn't \
             know which keystroke"
        );
    }

    /// The second: an unmatched result is visible, not dropped.
    #[test]
    fn a_result_for_an_unknown_call_is_a_muted_line_and_not_a_panic() {
        let mut app = App::default();
        app.record_tool_result("nobody", "orphan".into(), false);

        assert!(tools(&app).is_empty(), "no block was invented");
        match app.transcript.last() {
            Some(Entry::Message { who, text }) => {
                assert_eq!(*who, Who::Status);
                assert!(
                    text.contains("nobody"),
                    "the line doesn't say which call: {text:?}"
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
            "a finished block isn't retroactively interrupted"
        );
    }

    use super::{SessionEntry, SessionMessage};

    fn entries(ids: &[&str]) -> Vec<SessionEntry> {
        ids.iter()
            .map(|id| SessionEntry {
                id: (*id).to_string(),
                preview: format!("preview of {id}"),
            })
            .collect()
    }

    /// Acceptance: switching mid-turn is refused with a reason; the turn
    /// is unaffected.
    #[test]
    fn switching_mid_turn_is_refused_and_the_turn_is_unaffected() {
        let mut app = App::default();
        app.set_session("cli");
        app.open_sessions(entries(&["cli", "other"]));
        app.sessions_move(1); // "other"
        app.begin_turn();

        assert_eq!(
            app.sessions_confirm(),
            None,
            "a running turn refuses the switch"
        );
        assert_eq!(app.turn(), Turn::Streaming, "unaffected");
        assert_eq!(app.session(), "cli", "session didn't change");
        assert!(
            app.sessions_open(),
            "switcher stays open; not asked to close"
        );
        assert!(
            format!("{:?}", app.transcript).contains("running"),
            "refusal explains why"
        );

        // Idle, the same choice is accepted.
        app.end_turn();
        assert_eq!(app.sessions_confirm(), Some("other".to_string()));
    }

    /// The switcher filters both id and preview, case-insensitively. The
    /// current session is inspectable so the caller can mark it.
    #[test]
    fn the_switcher_filters_by_id_or_preview() {
        let mut app = App::default();
        app.open_sessions(vec![
            SessionEntry {
                id: "alpha".to_string(),
                preview: "hello world".to_string(),
            },
            SessionEntry {
                id: "beta".to_string(),
                preview: "nothing else".to_string(),
            },
        ]);
        app.sessions_push_char('W');
        app.sessions_push_char('o');
        let ids: Vec<&str> = app
            .sessions_filtered()
            .iter()
            .map(|e| e.id.as_str())
            .collect();
        assert_eq!(ids, vec!["alpha"], "matched on the preview, not id");

        app.sessions_backspace();
        app.sessions_backspace();
        assert_eq!(app.sessions_filtered().len(), 2, "empty filter shows all");
    }

    /// #105's design point 3: roles map as documented; `system` is skipped,
    /// not an empty block.
    #[test]
    fn load_session_maps_roles_and_skips_system() {
        let mut app = App::default();
        app.load_session(
            "s2".to_string(),
            vec![
                SessionMessage {
                    role: "system".to_string(),
                    content: "you are an agent".to_string(),
                    tool_call_id: None,
                },
                SessionMessage {
                    role: "user".to_string(),
                    content: "hello".to_string(),
                    tool_call_id: None,
                },
                SessionMessage {
                    role: "tool".to_string(),
                    content: "file contents".to_string(),
                    tool_call_id: Some("call-1".to_string()),
                },
                SessionMessage {
                    role: "assistant".to_string(),
                    content: "done".to_string(),
                    tool_call_id: None,
                },
            ],
        );

        assert_eq!(app.session(), "s2");
        assert_eq!(
            app.transcript.len(),
            3,
            "system message must not become an empty block: {:?}",
            app.transcript
        );
        assert_eq!(
            app.transcript[0],
            Entry::Message {
                who: Who::You,
                text: "hello".to_string()
            }
        );
        match &app.transcript[1] {
            Entry::Tool(block) => {
                assert_eq!(block.id, "call-1");
                assert_eq!(
                    block.status,
                    ToolStatus::Done {
                        content: "file contents".to_string(),
                        failed: false
                    }
                );
            }
            other @ Entry::Message { .. } => panic!("expected a tool block, got {other:?}"),
        }
        assert_eq!(
            app.transcript[2],
            Entry::Message {
                who: Who::Klod,
                text: "done".to_string()
            }
        );
    }

    /// The sidebar dialog toggles like the help overlay; declines under the
    /// ask dialog for the same reason.
    #[test]
    fn the_sidebar_dialog_toggles_and_declines_under_the_ask_dialog() {
        let mut app = App::default();
        app.toggle_sidebar_dialog();
        assert!(app.sidebar_dialog_open());
        app.toggle_sidebar_dialog();
        assert!(!app.sidebar_dialog_open());

        app.ask(Prompt {
            session: "s1".to_string(),
            question: "q".to_string(),
            options: vec!["y".to_string()],
            default: "y".to_string(),
        });
        app.toggle_sidebar_dialog();
        assert!(
            !app.sidebar_dialog_open(),
            "the ask dialog owns every key; a second dialog under it is unreachable."
        );
    }

    /// Acceptance: the quit confirm is skipped when nothing has happened,
    /// shown otherwise with cancel selected.
    #[test]
    fn the_quit_confirm_is_skipped_on_an_empty_session_and_shown_otherwise() {
        let mut app = App::default();
        app.request_quit();
        assert!(
            app.should_quit,
            "nothing happened; confirming has no purpose"
        );

        let mut used = App::default();
        used.take_submission(); // whitespace-only; exercises the path
        used.push_char('h');
        used.take_submission();
        used.should_quit = false;
        used.request_quit();
        assert!(!used.should_quit, "a session with history isn't skipped");
        assert!(used.quit_confirm_open());
        let (mid_turn, _, _, quit_selected) = used.quit_confirm().expect("open");
        assert!(!mid_turn);
        assert!(!quit_selected, "cancel is selected by default");
    }

    /// Acceptance: quitting mid-turn requires a second, explicit confirmation.
    #[test]
    fn quitting_mid_turn_requires_a_second_confirmation() {
        let mut app = App::default();
        app.push_char('h');
        app.take_submission();
        app.begin_turn();
        app.request_quit();
        assert!(app.quit_confirm_open());

        app.quit_confirm_move(); // select "quit"
        app.quit_confirm_accept();
        assert!(
            !app.should_quit,
            "the first confirmation mid-turn doesn't quit outright"
        );
        assert!(
            app.quit_confirm_open(),
            "the dialog stays open asking to confirm again"
        );

        app.quit_confirm_accept();
        assert!(app.should_quit, "the second confirmation quits");
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
