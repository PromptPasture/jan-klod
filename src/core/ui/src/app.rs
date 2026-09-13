//! The TUI's pure state model — no terminal, no I/O, so it is unit-tested.
//!
//! The `tui` render/event glue (in the binary) is a thin shell over this: key
//! events mutate the [`App`], each turn's answer is recorded, and the view is a
//! pure function of this state.

use std::collections::VecDeque;

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
///
/// More than three bools, and each one is a fact this model already has to
/// track separately — `should_quit`, `dialog_open`, `cancel_requested` and
/// `disconnected` answer four different questions with no shared state
/// machine between them, so folding them into one enum would invent a joint
/// state nothing here has ever needed to distinguish.
#[derive(Debug, Default)]
#[allow(clippy::struct_excessive_bools)]
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
    /// Confirmations a running turn is waiting on, oldest first (#103).
    ///
    /// A queue rather than one slot: a second `ask` arriving while the first is
    /// still open must not overwrite it or stack a second modal — it waits, and
    /// the dialog says `1 of 2`. `Turn::Blocked` is derived from this being
    /// non-empty, the same way it used to be derived from `Option::is_some`.
    prompts: VecDeque<Pending>,
    /// Whether the modal for the front of [`Self::prompts`] is on screen.
    ///
    /// Separate from "is a prompt pending", on purpose: `Esc` closes the dialog
    /// without answering, and the turn stays `Blocked` with the question still
    /// queued. A single `Option` for both facts would make "closed but still
    /// waiting" — the exact state `Esc` has to produce — inexpressible.
    dialog_open: bool,
    /// Set once the transport is known to have died — the spawned gateway
    /// exited, or the `--addr` core stopped answering (#104). `false` is the
    /// default and stays the common case; there is no reconnection in this
    /// slice, so once set it never clears.
    disconnected: bool,
    /// A `warning`/`error` shown in the status bar for a few ticks, newest
    /// wins (#104). The full text has already landed in the transcript by the
    /// time this is set, via [`Self::toast_warning`]/[`Self::toast_error`] —
    /// so a toast that expires unread has lost nothing.
    toast: Option<Toast>,
    /// The session this client is driving. Owned and mutable (#105) rather
    /// than a `&str` held for the event loop's lifetime — switching sessions
    /// replaces this, in [`Self::load_session`], instead of restarting the
    /// process with a different argument.
    session: String,
    /// Whether the transport is the spawned-gateway kind. Set once, at
    /// startup — see [`Self::set_stdio`] — and read only by
    /// [`Self::request_quit`], which is the one place the distinction
    /// matters: over stdio a turn still running when this client quits is
    /// genuinely lost, because the gateway is this process's child.
    stdio: bool,
    /// The session switcher (#105), when open.
    sessions: Option<Sessions>,
    /// Set when something asked for a fresh `session/list` — `Ctrl+S` or
    /// `/sessions` — and nothing has sent that ask to the core yet. The same
    /// shape as [`Self::cancel_requested`]: this model records the intent,
    /// the caller (which holds a transport) performs it and then calls
    /// [`Self::open_sessions`] with what came back.
    sessions_requested: bool,
    /// Set when `/new` asked for a fresh session and nothing has sent
    /// `session/create` yet. The caller performs it and then calls
    /// [`Self::load_session`] with the new, empty session.
    new_session_requested: bool,
    /// The help overlay (#105), when open.
    help: Option<Help>,
    /// The sidebar-in-a-dialog (#105's "sidebar-in-a-dialog"), when open —
    /// reachable through the same modal primitive at widths where 19g hides
    /// the permanent sidebar pane.
    sidebar_dialog: bool,
    /// The quit confirm (#105), when open.
    quit: Option<QuitConfirm>,
}

/// A toast: the text, which colour it reads in, and the tick it expires on.
///
/// Expiry is measured in the same `tick` [`crate::tui`] advances the spinner
/// with, not wall-clock time — see the module docs on why: a poll timeout is
/// what this client has instead of a clock, and driving both off it is what
/// makes a toast's expiry testable without sleeping.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Toast {
    text: String,
    kind: ToastKind,
    expires_at: usize,
}

/// How long a toast is shown, in poll ticks. `tui::POLL_MS` is 50ms, so this
/// is a few seconds — long enough to read, short enough that it is gone
/// before someone asks whether it ever will be.
const TOAST_TICKS: usize = 60;

/// Which of the two toast colours a notification reads in (#104's Scope: only
/// `warning` and `error` toast — everything else already has its own place in
/// the transcript).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToastKind {
    /// `warning()`.
    Warning,
    /// `removed()`.
    Error,
}

/// One session `session/list` reports: an id and a preview of its first user
/// message. What the switcher (#105) shows, populated fresh at every open —
/// see [`App::open_sessions`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionEntry {
    /// Session id.
    pub id: String,
    /// First 80 characters of the session's first user message, as
    /// `session/list` sends it.
    pub preview: String,
}

/// One message of a session fetched via `session/get`.
///
/// Already reduced to what rebuilding the transcript needs (#105's design
/// point 3). The wire's `seq` is not kept here: nothing on this client
/// addresses a log position, which is also why `session/fork` stays out of
/// this slice's scope.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionMessage {
    /// `system`, `user`, `assistant` or `tool`, exactly as the wire sends it.
    pub role: String,
    /// The message text.
    pub content: String,
    /// Present only on a tool result, tying it to the call it answers.
    pub tool_call_id: Option<String>,
}

/// The session switcher's model (#105): populated once per open from
/// `session/list`, filtered by what has been typed since.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
struct Sessions {
    entries: Vec<SessionEntry>,
    query: String,
    selected: usize,
}

/// The help overlay's model (#105): just how far it has been scrolled — the
/// content itself is [`crate::blocks::help_dialog`], a pure function of
/// [`crate::keymap::BINDINGS`], so there is nothing else to track here.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct Help {
    scroll: usize,
}

/// The quit confirm's model (#105).
///
/// Four independent facts, each answering a different question (which option
/// is highlighted, was a turn running, would it be lost, has quit been
/// confirmed once already) with no invalid combination between them — an enum
/// per axis would be four one-variant-wider types replacing four flags, not a
/// simplification.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(clippy::struct_excessive_bools)]
struct QuitConfirm {
    /// `false` is cancel, the default; `true` selects quit.
    quit_selected: bool,
    /// Whether a turn was running when this was opened — the case that needs
    /// the wording to say what happens to it, and the second confirmation.
    mid_turn: bool,
    /// Whether quitting genuinely loses the running turn: stdio's spawned
    /// gateway dies with this client; `--addr`'s does not. Meaningless unless
    /// `mid_turn`, but stored either way rather than as a nested `Option` —
    /// there is nothing that reads it without also reading `mid_turn`.
    turn_lost: bool,
    /// Set once quit has been selected and confirmed once while `mid_turn` —
    /// the "second, explicit confirmation" #105 asks for. A second `Enter` on
    /// `quit` with this set is what actually quits.
    escalate: bool,
}

/// A confirmation a running turn is waiting on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Prompt {
    /// The session this question was asked on — **not necessarily the client's
    /// current one** (a client may be driving more than one), and it is what
    /// `turn/answer` must be sent with. `tui.rs` used to answer with its own
    /// session instead of this one, which is the bug #103 opens on.
    pub session: String,
    /// What is being asked.
    pub question: String,
    /// The answers core recognises. Empty means free text, the protocol's own
    /// convention.
    pub options: Vec<String>,
    /// What core assumes if nobody answers — a denial, for the permission gate.
    pub default: String,
}

/// A queued prompt plus the one thing about it that changes before it is
/// answered: which option is highlighted.
///
/// Kept beside the prompt rather than as a second `Option` on [`App`], so two
/// queued questions cannot be told apart by *which* selection field happens to
/// be set — there is one slot per prompt, and it travels with the prompt when a
/// second `ask` is appended behind it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Pending {
    prompt: Prompt,
    /// An index into `prompt.options`. Starts on `default`'s own index — found
    /// by searching, not assumed to be first — and `None` when `default` is not
    /// among `options` at all: that is core contradicting itself, and the right
    /// answer is to demand an explicit choice rather than guess one. Also `None`
    /// for a free-text prompt (`options` empty), where there is nothing to
    /// select.
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
                // Ask before leaving rather than quitting outright — the same
                // confirming path `Ctrl+D` and `Ctrl+C` (idle, empty) use
                // (#105).
                "/quit" => self.request_quit(),
                // Sets the ask; the caller sends it, because commands are
                // dispatched here and this type holds no transport.
                "/cancel" => {
                    if !self.cancel() {
                        self.record(Who::Status, "no turn is running".to_string());
                    }
                }
                // #105: the other two asks a transport has to serve, and the
                // one that needs none at all.
                "/new" => self.request_new_session(),
                "/sessions" => self.request_sessions(),
                "/help" => self.toggle_help(),
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
        self.prompts.clear();
        self.dialog_open = false;
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
        self.prompts.clear();
        self.dialog_open = false;
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

    /// A turn is waiting on a confirmation: queue it, opening the dialog if
    /// nothing was already waiting.
    ///
    /// **A queue, not a stack** (#103): a second `ask` arriving while the first
    /// is still open is appended behind it rather than replacing it, and the
    /// open one's selection is untouched — nothing here can overwrite
    /// [`Self::prompts`]' front. The dialog opens automatically only the first
    /// time, because a second arrival must not reopen (or otherwise disturb)
    /// one the user may have deliberately closed.
    ///
    /// The initial selection is `default`'s own index in `options`, found by
    /// searching rather than assumed to be first. When `default` is not among
    /// `options` at all — core contradicting itself — nothing is selected, and
    /// answering requires an explicit choice rather than a guess.
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

    /// Reopen the dialog for the prompt still pending, if there is one.
    ///
    /// `Esc` closes the dialog without answering; this is the other half —
    /// `Enter` while a prompt is pending but the dialog is closed must show the
    /// options again rather than answer blind, so this is what a keystroke that
    /// is *not* a visible choice does instead of silently approving anything.
    pub fn open_dialog(&mut self) {
        if !self.prompts.is_empty() {
            self.dialog_open = true;
        }
    }

    /// `Esc`: close the dialog **without answering**. The prompt stays queued
    /// and the turn stays `Blocked` — closing a modal must never read as a
    /// choice, only as declining to look at it right now.
    pub const fn close_dialog(&mut self) {
        self.dialog_open = false;
    }

    /// The prompt at the front of the queue, if one is waiting.
    ///
    /// An accessor rather than the field itself, so the queue and the "is a
    /// dialog open" flag stay this module's to keep consistent.
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

    /// Move the front prompt's selection, wrapping like the `/` menu's.
    /// A no-op on a free-text prompt (`options` empty) or with nothing pending.
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

    /// Jump the front prompt's selection to `one_based`'s option — a digit key,
    /// as displayed. Out of range is a no-op rather than a clamp: a digit that
    /// names nothing must not silently select something else.
    pub fn prompt_jump(&mut self, one_based: usize) {
        let Some(front) = self.prompts.front_mut() else {
            return;
        };
        if one_based >= 1 && one_based <= front.prompt.options.len() {
            front.selected = Some(one_based - 1);
        }
    }

    /// Take the front prompt's answer, if one is ready to send.
    ///
    /// Returns `(session, answer)` — the **session the notification carried**,
    /// never `self`'s own, because a client may be driving more than one and
    /// the core told this client precisely which turn asked (#103's first
    /// design point; `tui.rs` used to answer with its own session instead).
    ///
    /// `answer` is byte-identical to the chosen entry in `options` — never
    /// lowercased, trimmed or otherwise normalised — except for a free-text
    /// prompt (`options` empty), where it is the composer's text, or `default`
    /// if that was left empty.
    ///
    /// Returns `None`, dequeuing nothing, when a list prompt has no selection —
    /// `default` absent from `options` left nothing chosen, and nothing here
    /// may guess one on the user's behalf. Dequeuing only happens once an
    /// answer actually exists, so a prompt that cannot yet be answered is not
    /// silently dropped.
    ///
    /// # Panics
    ///
    /// Never in practice: the `pop_front` below cannot fail because
    /// `front()` above already proved the queue non-empty, and nothing between
    /// the two can shrink it.
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
        // `take` clears the composer and its caret together, which is the point
        // of the type: an answer left behind in the buffer would be sent as the
        // next message.
        self.composer.take();
        // The next queued prompt, if any, is shown immediately rather than left
        // behind a closed dialog — "answering the first shows the second"
        // (#103's Acceptance) means visibly, not just in the model.
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

    /// The transport is gone — say so, permanently (#104).
    ///
    /// The same shape as [`Self::end_turn`], and for the same reason: whatever
    /// was in flight cannot finish, so a spinner left running would be a
    /// client that still looks alive. A prompt nobody can answer is dropped
    /// rather than left `Blocked` forever, exactly as [`Self::cancel`] already
    /// does for a turn the user stopped.
    ///
    /// Idempotent: a second call narrates nothing new, because the caller may
    /// notice the same dead transport more than once (a poll tick and a failed
    /// write, say) and only the first telling is news.
    pub fn disconnect(&mut self, reason: impl Into<String>) {
        if !self.disconnected {
            self.disconnected = true;
            self.record_error(reason);
        }
        // Unconditional, even on a repeat call: nothing can still be running
        // once the transport is gone, whichever telling of it this is.
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

    /// The connection+turn word the status bar's right side shows (#104):
    /// `ready`, `streaming`, `blocked`, `cancelling`, or `disconnected`.
    ///
    /// `disconnected` wins over everything else — [`Self::disconnect`] has
    /// already forced the turn back to `Idle`, so without this a dead
    /// transport would read as merely `ready`, which is the one thing #104
    /// exists to stop a client from claiming.
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

    /// A `warning` notification (#104): the full text lands in the transcript
    /// immediately, as it always did, and a toast is raised beside it —
    /// newest wins, so a second warning before the first expires replaces it
    /// rather than queuing behind it.
    pub fn toast_warning(&mut self, text: impl Into<String>, tick: usize) {
        let text = text.into();
        self.record_status(format!("⚠ {text}"));
        self.toast = Some(Toast {
            text,
            kind: ToastKind::Warning,
            expires_at: tick + TOAST_TICKS,
        });
    }

    /// A toasted error (#104): the same relationship to the transcript as
    /// [`Self::toast_warning`], through [`Self::record_error`] instead of a
    /// status line — an error is a failure, not a note.
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

    /// Expire the toast once `tick` has passed the point it was raised for.
    ///
    /// Driven by [`crate::tui`]'s own poll tick rather than a clock (#104): the
    /// spinner already advances on every poll timeout, and reusing it is what
    /// lets a toast's expiry be asserted in a test with no `sleep` in it.
    pub fn tick(&mut self, tick: usize) {
        if self.toast.as_ref().is_some_and(|t| tick >= t.expires_at) {
            self.toast = None;
        }
    }

    /// Request quit — unconditionally and without a confirmation. Kept for
    /// `Esc`, whose behaviour this slice leaves alone (#105's Scope names
    /// three other triggers for the confirming version, [`Self::request_quit`]).
    pub const fn quit(&mut self) {
        self.should_quit = true;
    }

    /// The session this client is driving.
    #[must_use]
    pub fn session(&self) -> &str {
        &self.session
    }

    /// Set the session id — at startup, and again on every switch or `/new`
    /// (#105's design point 1: the id stops being fixed for the run).
    pub fn set_session(&mut self, id: impl Into<String>) {
        self.session = id.into();
    }

    /// Record which kind of transport this run has, once, at startup — see
    /// the field's own docs for why [`Self::request_quit`] needs it.
    pub const fn set_stdio(&mut self, stdio: bool) {
        self.stdio = stdio;
    }

    /// Whether anything has been sent this run — what the quit confirm
    /// (#105) is skipped entirely for the lack of.
    const fn has_sent_anything(&self) -> bool {
        !self.history.is_empty()
    }

    // ---- session switcher (#105) ----

    /// `Ctrl+S`/`/sessions`: ask the loop for a fresh `session/list`. The
    /// switcher is never populated from a cache — see [`Self::open_sessions`].
    /// A no-op while the ask dialog is open: that modal owns every key, and a
    /// second dialog opening under it would be unreachable anyway.
    pub const fn request_sessions(&mut self) {
        if !self.dialog_open {
            self.sessions_requested = true;
        }
    }

    /// Take the pending `session/list` ask, if there is one. Drains, the same
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

    /// The entries matching the typed filter, on id or preview, case
    /// insensitively — empty means the empty state, not a closed list, the
    /// same convention [`Self::menu_entries`] uses.
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

    /// Move the highlight, wrapping like the `/` menu's.
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

    /// `Enter`: the session to switch to, or a refusal recorded in the
    /// transcript and no id, when a turn is running (#105's design point 4 —
    /// switching mid-turn is refused with a stated reason, not a silent
    /// abandon). The switcher stays open either way; the caller closes it
    /// once [`Self::load_session`] actually runs.
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

    /// `session/get` answered (or `/new`'s `session/create` did, with an
    /// empty transcript): replace this client's view with what the core holds
    /// for `id` — a reload, never a stash (#105's design point 3). The
    /// switcher closes, whether or not it was the one that asked.
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

    /// Take the pending `session/create` ask, if there is one. Drains, the
    /// same contract as [`Self::take_cancel_request`].
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

    /// `?` on an empty composer, or `/help`: open or close it. A no-op while
    /// the ask dialog is open, for the same reason [`Self::request_sessions`]
    /// is.
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

    /// How far the overlay has scrolled. The renderer clamps this against
    /// what it actually drew — this model has no width or height to know the
    /// bound itself.
    #[must_use]
    pub fn help_scroll(&self) -> usize {
        self.help.as_ref().map_or(0, |h| h.scroll)
    }

    /// Scroll by `delta` rows, negative moving up. Saturates at zero rather
    /// than wrapping — scrolling past the top is a no-op, not a jump to the
    /// bottom.
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

    /// `Ctrl+B`, at a width where the sidebar has no permanent pane. A no-op
    /// while the ask dialog is open, for the same reason
    /// [`Self::request_sessions`] is — and the caller (`tui.rs`) is what
    /// refuses this at a width where the sidebar is already visible, since
    /// only it knows the last frame's layout.
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

    /// `Ctrl+D`, `Ctrl+C` on an empty composer while `Idle`, or `/quit`: ask
    /// before leaving, unless nothing has happened this run — confirming an
    /// empty session is friction with no purpose. Cancel is the initial
    /// selection either way.
    pub fn request_quit(&mut self) {
        if !self.has_sent_anything() {
            self.should_quit = true;
            return;
        }
        if self.dialog_open {
            // The ask modal owns every key; a quit confirm under it would be
            // unreachable, the same reason `request_sessions` refuses too.
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

    /// What the dialog needs to render: `(mid_turn, turn_lost, escalate,
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
    /// confirmation when a turn is running (#105's design point 5) — the
    /// first `Enter` on `quit` there only sets [`QuitConfirm::escalate`] and
    /// re-asks; the second actually quits. With no turn running, one `Enter`
    /// on `quit` is enough.
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

/// One `session/get` message, as a transcript [`Entry`] — or `None` for
/// `system`, which is skipped: the model's own instructions are not something
/// a user typed or said (#105's design point 3).
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
            // `session/get` carries no tool name and no `failed` flag — only
            // `session/message`'s live notifications do (#106 added `seq`,
            // not either of those). The call id is what is actually known, so
            // it stands in for the name; a result is assumed to have
            // succeeded absent any way to tell otherwise, which is the same
            // default a core older than #162 already rendered.
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
    /// and — because `connection_state` only ever reports the stored turn,
    /// which `disconnect` forces back to `Idle` — the spinner stops with it.
    /// `spinner_frame` returns `None` for `Turn::Idle` and nothing else here,
    /// so this one assertion covers both halves of #104's acceptance line.
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

        // A prompt nobody can answer must not be left `Blocked` forever.
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

    /// Acceptance: a `warning` produces both a toast and a transcript line,
    /// and the transcript line outlives the toast — so a toast that expires
    /// unread has lost nothing.
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
            "the transcript line vanished with the toast — nothing should be \
             lost when a toast expires unread"
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
    /// field, so they cannot disagree.
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
            "the default is the third option, and the selection must find it \
             there rather than assume position 0"
        );

        // A core that contradicts itself — `default` is not among `options` —
        // must not have this guess at a selection either.
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

    /// Acceptance: the answer sent is byte-identical to the chosen entry —
    /// never normalised, even when it would look "cleaner" normalised.
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

    /// Acceptance: two overlapping `ask`s queue rather than stack, and
    /// answering the first leaves the second exactly as it arrived.
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
            "a second ask must queue, not overwrite the first"
        );
        assert_eq!(
            app.pending_prompt().map(|p| p.question.as_str()),
            Some("first?"),
            "the front is still the first question"
        );

        let (session, answer) = app.take_answer().expect("the first has a default");
        assert_eq!((session.as_str(), answer.as_str()), ("first", "no"));

        assert_eq!(app.prompt_queue_len(), 1, "one prompt is left");
        assert_eq!(
            app.pending_prompt().map(|p| p.question.as_str()),
            Some("second?"),
            "the second is shown, unchanged"
        );
        assert_eq!(
            app.prompt_selected(),
            Some(2),
            "the second kept its own selection — `no` is index 2 there"
        );
        assert!(
            app.dialog_open(),
            "the second prompt is shown, not left behind a closed dialog"
        );
    }

    /// `Esc` closes the dialog without answering, and the prompt survives so it
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
            "closing the dialog must not answer the question"
        );
        assert!(
            app.pending_prompt().is_some(),
            "the prompt must survive being closed unanswered"
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

    use super::{SessionEntry, SessionMessage};

    fn entries(ids: &[&str]) -> Vec<SessionEntry> {
        ids.iter()
            .map(|id| SessionEntry {
                id: (*id).to_string(),
                preview: format!("preview of {id}"),
            })
            .collect()
    }

    /// Acceptance: switching mid-turn is refused, with a reason, and the turn
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
            "a running turn must refuse the switch"
        );
        assert_eq!(app.turn(), Turn::Streaming, "the turn is unaffected");
        assert_eq!(app.session(), "cli", "the session did not change");
        assert!(
            app.sessions_open(),
            "the switcher stays open — it was not asked to close"
        );
        assert!(
            format!("{:?}", app.transcript).contains("running"),
            "the refusal must say why"
        );

        // Idle, the same choice is accepted.
        app.end_turn();
        assert_eq!(app.sessions_confirm(), Some("other".to_string()));
    }

    /// The switcher filters both id and preview, case-insensitively, and the
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
        assert_eq!(ids, vec!["alpha"], "matched on the preview, not the id");

        app.sessions_backspace();
        app.sessions_backspace();
        assert_eq!(
            app.sessions_filtered().len(),
            2,
            "an empty filter shows all"
        );
    }

    /// #105's design point 3: the roles map as documented, and `system` is
    /// skipped rather than becoming an empty block.
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
            "the system message must not become an empty block: {:?}",
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

    /// The sidebar dialog toggles like the help overlay, and for the same
    /// reason declines while the ask dialog is open.
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
            "the ask dialog owns every key; a second dialog under it is unreachable"
        );
    }

    /// Acceptance: the quit confirm is skipped when nothing has happened, and
    /// shown otherwise with cancel selected.
    #[test]
    fn the_quit_confirm_is_skipped_on_an_empty_session_and_shown_otherwise() {
        let mut app = App::default();
        app.request_quit();
        assert!(
            app.should_quit,
            "nothing happened yet — confirming has no purpose"
        );

        let mut used = App::default();
        used.take_submission(); // whitespace-only; still exercises the path
        used.push_char('h');
        used.take_submission();
        used.should_quit = false;
        used.request_quit();
        assert!(!used.should_quit, "a session with history is not skipped");
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
            "the first confirmation mid-turn must not quit outright"
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
