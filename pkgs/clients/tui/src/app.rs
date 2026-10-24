//! The TUI's pure state model, with no terminal or I/O.

use std::collections::VecDeque;

use crate::commands::Availability;
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
    Done {
        /// Tool's return value.
        content: String,
        /// Whether core reported the call failed, rather than inferring it from `content`.
        failed: bool,
    },
    /// The turn ended with this call still open.
    Interrupted,
}

/// A tool call and its result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolBlock {
    /// Call id; pairs the two halves.
    pub id: String,
    /// Tool name.
    pub name: String,
    /// JSON arguments.
    pub arguments: Option<String>,
    /// Running, done, or interrupted.
    pub status: ToolStatus,
    /// Whether full arguments and result show.
    pub expanded: bool,
}

/// One entry in the transcript.
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

/// Where the turn is; `Blocked` is derived from the prompt queue, not stored.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum Turn {
    /// Nothing running.
    #[default]
    Idle,
    /// A turn is running and text is arriving.
    Streaming,
    /// A cancel has been *asked for*, not confirmed.
    Cancelling,
    /// A confirmation is waiting.
    Blocked,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
enum Phase {
    #[default]
    Idle,
    Streaming,
    Cancelling,
}

/// The REPL/TUI state: the input buffer and the scrollback transcript.
#[derive(Debug, Default)]
#[allow(clippy::struct_excessive_bools)]
pub struct App {
    /// The message being written, with its caret.
    pub composer: Composer,
    /// The conversation so far, oldest first.
    pub transcript: Vec<Entry>,
    /// Set when the user asked to quit.
    pub should_quit: bool,
    /// Append-only: recall edits do not change it.
    history: Vec<String>,
    recall: Option<(usize, String)>,
    menu: Option<usize>,
    /// Cached because recomputing walks the filesystem; the fragment is kept to detect stale results.
    completion: Option<(String, usize, paths::Completion)>,
    /// Tool block `Ctrl+O` acts on: an index into [`Self::transcript`] that is always an [`Entry::Tool`].
    cursor: Option<usize>,
    phase: Phase,
    cancel_requested: bool,
    /// A queue, not a slot: a second `ask` waits behind the first.
    prompts: VecDeque<Pending>,
    dialog_open: bool,
    /// Never clears.
    disconnected: bool,
    toast: Option<Toast>,
    session: String,
    stdio: bool,
    sessions: Option<Sessions>,
    sessions_requested: bool,
    /// Already made inert; see [`Self::set_contributions`].
    contributed: Vec<crate::commands::Contributed>,
    /// The status items from the same notification, likewise inert.
    contributed_status: Vec<ContributedStatus>,
    invoke_requested: Option<(String, String)>,
    new_session_requested: bool,
    help: Option<Help>,
    sidebar_dialog: bool,
    quit: Option<QuitConfirm>,
}

/// Expiry is counted in `tick` units, not wall-clock time, so it is testable without sleeps.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Toast {
    text: String,
    kind: ToastKind,
    expires_at: usize,
}

const TOAST_TICKS: usize = 60;

/// Kind of toast.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToastKind {
    /// Raised by [`App::toast_warning`].
    Warning,
    /// Raised by [`App::toast_error`].
    Error,
}

/// A status item an extension contributed, as this client holds it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContributedStatus {
    /// The declaring extension's instance id, so a reader can tell who is speaking.
    pub extension: String,
    /// The short text to show.
    pub text: String,
    /// One line of detail, for a pane with room for it.
    pub detail: String,
}

/// A session row in the switcher, from `session/list`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionEntry {
    /// Session id.
    pub id: String,
    /// First 80 characters of the session's first user message, as `session/list` sends it.
    pub preview: String,
}

/// A message from `session/get`, reduced to what rebuilds the transcript.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionMessage {
    /// `system`, `user`, `assistant` or `tool`, exactly as the wire sends it.
    pub role: String,
    /// The message text.
    pub content: String,
    /// Present only on a tool result, tying it to the call it answers.
    pub tool_call_id: Option<String>,
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
struct Sessions {
    entries: Vec<SessionEntry>,
    query: String,
    selected: usize,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct Help {
    scroll: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(clippy::struct_excessive_bools)]
struct QuitConfirm {
    quit_selected: bool,
    mid_turn: bool,
    /// Whether quitting loses the running turn: a stdio gateway dies with this client, a `--addr` gateway does not.
    turn_lost: bool,
    escalate: bool,
}

/// A confirmation a running turn is waiting on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Prompt {
    /// The session this question was asked on, which is not necessarily the client's current one.
    pub session: String,
    /// What is being asked.
    pub question: String,
    /// The answers core accepts.
    pub options: Vec<String>,
    /// What core assumes if nobody answers — a denial, for the permission gate.
    pub default: String,
}

/// Kept per prompt rather than as one `Option` on [`App`], so queued questions cannot share a selection.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Pending {
    prompt: Prompt,
    selected: Option<usize>,
}

impl App {
    /// Which entry the cursor is on, if the user has picked one.
    #[must_use]
    pub const fn cursor(&self) -> Option<usize> {
        self.cursor
    }

    fn stops(&self) -> Vec<usize> {
        self.transcript
            .iter()
            .enumerate()
            .filter_map(|(i, entry)| matches!(entry, Entry::Tool(_)).then_some(i))
            .collect()
    }

    /// Move the cursor to the previous tool block, or to the newest one.
    pub fn cursor_up(&mut self) -> bool {
        let stops = self.stops();
        let moved = self.cursor.map_or_else(
            || stops.last().copied(),
            |at| stops.iter().rev().find(|&&i| i < at).copied(),
        );
        if moved.is_some() {
            self.cursor = moved;
            return true;
        }
        false
    }

    /// Move cursor to the next tool block.
    pub fn cursor_down(&mut self) -> bool {
        let Some(at) = self.cursor else { return false };
        self.cursor = self.stops().iter().find(|&&i| i > at).copied();
        true
    }

    /// Open or close the block under the cursor.
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

    /// The text being written.
    #[must_use]
    pub fn input(&self) -> &str {
        self.composer.text()
    }

    /// Insert a typed character at the caret.
    pub fn push_char(&mut self, c: char) {
        self.composer.push(c);
        self.maybe_open_menu();
        if c == '@' {
            self.completion = Some((String::new(), 0, paths::Completion::default()));
        }
    }

    /// Delete the grapheme before the caret.
    pub fn backspace(&mut self) {
        self.composer.backspace();
        self.maybe_open_menu();
    }

    /// Takes the trimmed, non-empty input and records it as a [`Who::You`] entry.
    pub fn take_submission(&mut self) -> Option<String> {
        let message = self.composer.take()?;
        self.history.push(message.clone());
        self.recall = None;
        self.record(Who::You, message.clone());
        Some(message)
    }

    /// The fragment the `/` menu is filtering on, when it is open.
    #[must_use]
    pub fn menu_query(&self) -> Option<&str> {
        self.menu?;
        let text = self.composer.text();
        (text.starts_with('/') && !text.contains(char::is_whitespace)).then_some(text)
    }

    /// The menu entries — built-ins and what extensions contributed.
    #[must_use]
    pub fn menu_entries(&self) -> Vec<crate::commands::Entry<'_>> {
        self.menu_query()
            .map(|typed| crate::commands::matching_with(typed, &self.contributed))
            .unwrap_or_default()
    }

    /// Replace what extensions contribute, from a `surface/contributions` notification.
    pub fn set_contributions(&mut self, sets: &[jk_protocol::Contributions]) {
        self.contributed = sets
            .iter()
            .flat_map(|set| {
                let extension = crate::untrusted::inert(&set.extension);
                set.commands
                    .iter()
                    .map(move |command| crate::commands::Contributed {
                        extension: extension.clone(),
                        name: crate::untrusted::inert(&command.name),
                        summary: crate::untrusted::inert(&command.description),
                    })
            })
            .filter(|c| !c.name.is_empty())
            .collect();
        self.contributed_status = sets
            .iter()
            .flat_map(|set| {
                let extension = crate::untrusted::inert(&set.extension);
                set.status_items.iter().map(move |item| ContributedStatus {
                    extension: extension.clone(),
                    text: crate::untrusted::inert(&item.text),
                    detail: crate::untrusted::inert(&item.detail),
                })
            })
            .filter(|item| !item.text.is_empty())
            .collect();
    }

    /// What extensions contribute as commands, in the order the host reported.
    #[must_use]
    pub fn contributions(&self) -> &[crate::commands::Contributed] {
        &self.contributed
    }

    /// What extensions contribute as status, in the order the host reported.
    #[must_use]
    pub fn contributed_status(&self) -> &[ContributedStatus] {
        &self.contributed_status
    }

    /// Which entry is highlighted, clamped to what is on offer.
    #[must_use]
    pub fn menu_selected(&self) -> usize {
        let len = self.menu_entries().len();
        self.menu.unwrap_or(0).min(len.saturating_sub(1))
    }

    fn maybe_open_menu(&mut self) {
        if self.composer.text() == "/" {
            self.menu = Some(0);
        } else if self.menu_query().is_none() {
            self.menu = None;
        }
    }

    /// `Esc` with the menu open: close it and leave the text alone.
    pub const fn menu_dismiss(&mut self) {
        self.menu = None;
    }

    /// Move the highlight, wrapping at either end.
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
    pub fn menu_accept(&mut self) -> bool {
        let Some((name, availability, extension)) =
            self.menu_entries().get(self.menu_selected()).map(|entry| {
                (
                    entry.name.to_owned(),
                    entry.availability,
                    match entry.source {
                        crate::commands::Source::BuiltIn => None,
                        crate::commands::Source::Extension(id) => Some(id.to_owned()),
                    },
                )
            })
        else {
            return false;
        };
        self.menu = None;
        self.composer.set("");
        if let Some(extension) = extension {
            self.invoke_requested = Some((extension, name));
            return true;
        }
        match availability {
            Availability::Ready => match name.as_str() {
                "/newline" => self.composer.push('\n'),
                "/quit" => self.request_quit(),
                "/cancel" => {
                    if !self.cancel() {
                        self.record(Who::Status, "no turn is running".to_string());
                    }
                }
                "/new" => self.request_new_session(),
                "/sessions" => self.request_sessions(),
                "/help" => self.toggle_help(),
                other => self.record(Who::Status, format!("{other} is not wired up")),
            },
            Availability::Pending(reason) => {
                self.record(Who::Status, format!("{name} — {reason}"));
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
                block.expanded = failed;
                block.status = ToolStatus::Done { content, failed };
            }
            None => self.record_status(format!("tool result for an unknown call `{id}`")),
        }
    }

    /// The turn ended; nothing still open can still be running.
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

    /// The paths on offer.
    #[must_use]
    pub fn completion_entries(&self) -> &[String] {
        self.completion
            .as_ref()
            .map_or(&[], |(_, _, found)| found.entries.as_slice())
    }

    /// Whether the list stopped at the cap.
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

    /// Recompute the list if the fragment moved; close it if none remains.
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
    pub fn completion_accept(&mut self) -> bool {
        let Some(path) = self
            .completion_entries()
            .get(self.completion_selected())
            .cloned()
        else {
            return false;
        };
        let typed = self
            .completion
            .as_ref()
            .map_or(0, |(fragment, _, _)| fragment.len() + 1);
        self.composer.replace_before(typed, &path);
        self.completion = None;
        true
    }

    fn composer_unmodified(&self) -> bool {
        self.composer.is_empty()
            || self
                .recall
                .as_ref()
                .is_some_and(|(_, text)| text == self.composer.text())
    }

    /// `↑`: the previous message, or moves the caret up when history does not apply.
    pub fn history_up(&mut self, width: usize) {
        if !(self.composer.on_first_row(width) && self.composer_unmodified()) {
            self.composer.up(width);
            return;
        }
        if self.composer.is_empty() {
            self.recall = None;
        }
        let next = match self.recall {
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
            self.composer.set("");
            self.recall = None;
        }
    }

    /// Where the turn is.
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
    pub fn cancel(&mut self) -> bool {
        if !matches!(self.phase, Phase::Streaming) {
            return false;
        }
        self.phase = Phase::Cancelling;
        self.cancel_requested = true;
        self.prompts.clear();
        self.dialog_open = false;
        self.interrupt_open_tools();
        true
    }

    /// Take the pending cancel request.
    pub const fn take_cancel_request(&mut self) -> bool {
        let asked = self.cancel_requested;
        self.cancel_requested = false;
        asked
    }

    /// Stream ended.
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

    /// Queue a confirmation.
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

    /// Reopens the dialog if a prompt is pending.
    pub fn open_dialog(&mut self) {
        if !self.prompts.is_empty() {
            self.dialog_open = true;
        }
    }

    /// Close the dialog without answering; the turn stays `Blocked`.
    pub const fn close_dialog(&mut self) {
        self.dialog_open = false;
    }

    /// The front prompt, if one is waiting.
    #[must_use]
    pub fn pending_prompt(&self) -> Option<&Prompt> {
        self.prompts.front().map(|p| &p.prompt)
    }

    /// Which option is highlighted in the front prompt, if any.
    #[must_use]
    pub fn prompt_selected(&self) -> Option<usize> {
        self.prompts.front().and_then(|p| p.selected)
    }

    /// How many prompts are waiting, including the one on screen.
    #[must_use]
    pub fn prompt_queue_len(&self) -> usize {
        self.prompts.len()
    }

    /// Move selection, wrapping like `/` menu.
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

    /// Jump to digit-key option (as displayed).
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
    /// # Panics
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
        self.composer.take();
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

    /// Transport is gone and stays gone.
    pub fn disconnect(&mut self, reason: impl Into<String>) {
        if !self.disconnected {
            self.disconnected = true;
            self.record_error(reason);
        }
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

    /// Status word for the status bar; `disconnected` wins over the turn state.
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

    /// Show a `warning` toast; the text also goes to the transcript.
    pub fn toast_warning(&mut self, text: impl Into<String>, tick: usize) {
        let text = text.into();
        self.record_status(format!("⚠ {text}"));
        self.toast = Some(Toast {
            text,
            kind: ToastKind::Warning,
            expires_at: tick + TOAST_TICKS,
        });
    }

    /// Show an `error` toast; the text is also recorded as an error.
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

    /// Expire the toast once `tick` reaches its expiry.
    pub fn tick(&mut self, tick: usize) {
        if self.toast.as_ref().is_some_and(|t| tick >= t.expires_at) {
            self.toast = None;
        }
    }

    /// Quit unconditionally, without confirmation.
    pub const fn quit(&mut self) {
        self.should_quit = true;
    }

    /// The session this client is driving.
    #[must_use]
    pub fn session(&self) -> &str {
        &self.session
    }

    /// Set the session id at startup and on every switch or `/new`.
    pub fn set_session(&mut self, id: impl Into<String>) {
        self.session = id.into();
    }

    /// Record which kind of transport this run has, once, at startup.
    pub const fn set_stdio(&mut self, stdio: bool) {
        self.stdio = stdio;
    }

    const fn has_sent_anything(&self) -> bool {
        !self.history.is_empty()
    }

    /// `Ctrl+S`/`/sessions`: ask for a fresh `session/list`.
    pub const fn request_sessions(&mut self) {
        if !self.dialog_open {
            self.sessions_requested = true;
        }
    }

    /// Take the pending `session/list` ask, if there is one.
    pub const fn take_sessions_request(&mut self) -> bool {
        let asked = self.sessions_requested;
        self.sessions_requested = false;
        asked
    }

    /// Take the contributed command the user chose, if any.
    pub const fn take_invoke_request(&mut self) -> Option<(String, String)> {
        self.invoke_requested.take()
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

    /// `Esc`: close the switcher.
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

    /// `Enter`: the session to switch to.
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

    /// Replaces this client's view with session `id` as core holds it.
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

    /// Take the pending `session/create` ask, if there is one.
    pub const fn take_new_session_request(&mut self) -> bool {
        let asked = self.new_session_requested;
        self.new_session_requested = false;
        asked
    }

    /// Whether the overlay is on screen.
    #[must_use]
    pub const fn help_open(&self) -> bool {
        self.help.is_some()
    }

    /// `?` on empty composer, or `/help`: open or close.
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

    /// How far the overlay has scrolled.
    #[must_use]
    pub fn help_scroll(&self) -> usize {
        self.help.as_ref().map_or(0, |h| h.scroll)
    }

    /// Scroll by `delta` rows, negative moving up.
    pub const fn help_scroll_by(&mut self, delta: isize) {
        if let Some(help) = self.help.as_mut() {
            help.scroll = help.scroll.saturating_add_signed(delta);
        }
    }

    /// Whether the sidebar's dialog is on screen.
    #[must_use]
    pub const fn sidebar_dialog_open(&self) -> bool {
        self.sidebar_dialog
    }

    /// `Ctrl+B`, at a width where sidebar has no permanent pane.
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

    /// Asks before quitting, unless nothing has happened yet.
    pub fn request_quit(&mut self) {
        if !self.has_sent_anything() {
            self.should_quit = true;
            return;
        }
        if self.dialog_open {
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

    /// What the dialog renders: `(mid_turn, turn_lost, escalate, quit_selected)`.
    #[must_use]
    pub fn quit_confirm(&self) -> Option<(bool, bool, bool, bool)> {
        self.quit
            .map(|q| (q.mid_turn, q.turn_lost, q.escalate, q.quit_selected))
    }

    /// `↑`/`↓`/`Tab`: toggles between the two options.
    pub const fn quit_confirm_move(&mut self) {
        if let Some(q) = self.quit.as_mut() {
            q.quit_selected = !q.quit_selected;
        }
    }

    /// `Enter`: acts on the highlighted option, escalating once mid-turn before quitting.
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

    fn contributing(name: &str, summary: &str) -> Vec<jk_protocol::Contributions> {
        vec![jk_protocol::Contributions {
            extension: "interceptor.system".to_owned(),
            commands: vec![jk_protocol::SurfaceCommand {
                name: name.to_owned(),
                title: name.to_owned(),
                description: summary.to_owned(),
                arguments: vec![],
            }],
            status_items: vec![],
            forms: vec![],
        }]
    }

    fn reporting(text: &str, detail: &str) -> Vec<jk_protocol::Contributions> {
        vec![jk_protocol::Contributions {
            extension: "interceptor.system".to_owned(),
            commands: vec![],
            status_items: vec![jk_protocol::StatusItem {
                name: "prompt-source".to_owned(),
                text: text.to_owned(),
                detail: detail.to_owned(),
            }],
            forms: vec![],
        }]
    }

    #[test]
    fn a_contributed_status_item_is_stored_inert() {
        let mut app = App::default();
        app.set_contributions(&reporting("built\x1b[2Jin", "why\x07"));
        let item = app.contributed_status().first().expect("one item");
        assert_eq!(item.text, "built[2Jin");
        assert!(!item.detail.contains('\u{7}'), "{:?}", item.detail);
    }

    #[test]
    fn a_status_item_that_cleans_away_is_dropped() {
        let mut app = App::default();
        app.set_contributions(&reporting("\x1b\x1b", "gone"));
        assert_eq!(app.contributed_status(), []);
    }

    #[test]
    fn a_contributed_command_appears_in_the_menu() {
        let mut app = App::default();
        app.set_contributions(&contributing("prompt", "show the standing instructions"));
        app.push_char('/');
        assert!(
            app.menu_entries()
                .iter()
                .any(|entry| entry.name == "prompt"),
            "the menu offers what the extension contributed"
        );
    }

    #[test]
    fn a_hostile_label_is_inert_before_it_is_ever_stored() {
        let mut app = App::default();
        app.set_contributions(&contributing("pr\x1b[2Jompt", "clear\x1b]0;pwned\x07"));
        let stored = app.contributions().first().expect("one contribution");
        assert!(!stored.name.contains('\u{1b}'), "{:?}", stored.name);
        assert!(!stored.summary.contains('\u{1b}'), "{:?}", stored.summary);
    }

    #[test]
    fn a_contribution_whose_name_is_only_escapes_is_dropped() {
        let mut app = App::default();
        app.set_contributions(&contributing("\x1b\x1b", "invisible"));
        assert_eq!(app.contributions(), []);
    }

    #[test]
    fn choosing_a_contributed_command_asks_for_an_invocation() {
        let mut app = App::default();
        app.set_contributions(&contributing("prompt", "show it"));
        app.push_char('/');
        for c in "prompt".chars() {
            app.push_char(c);
        }
        assert!(app.menu_accept(), "the menu accepted the entry");
        assert_eq!(
            app.take_invoke_request(),
            Some(("interceptor.system".to_owned(), "prompt".to_owned())),
            "the extension and the name together identify it"
        );
        assert_eq!(app.take_invoke_request(), None, "draining is once only");
    }

    #[test]
    fn choosing_a_built_in_asks_for_no_invocation() {
        let mut app = App::default();
        app.set_contributions(&contributing("prompt", "show it"));
        app.push_char('/');
        for c in "help".chars() {
            app.push_char(c);
        }
        assert!(app.menu_accept());
        assert_eq!(app.take_invoke_request(), None);
    }

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

        app.begin_turn();
        app.ask(Prompt {
            session: "s1".into(),
            question: "proceed?".into(),
            options: vec!["y".into()],
            default: "y".into(),
        });
        app.disconnect("again");
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

    #[test]
    fn toggling_opens_the_selected_block_and_does_nothing_with_no_selection() {
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

        app.transcript.clear();
        assert!(!app.toggle_expanded(), "stale index doesn't panic");
    }

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

    #[test]
    fn switching_mid_turn_is_refused_and_the_turn_is_unaffected() {
        let mut app = App::default();
        app.set_session("cli");
        app.open_sessions(entries(&["cli", "other"]));
        app.sessions_move(1);
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

        app.end_turn();
        assert_eq!(app.sessions_confirm(), Some("other".to_string()));
    }

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

    #[test]
    fn the_quit_confirm_is_skipped_on_an_empty_session_and_shown_otherwise() {
        let mut app = App::default();
        app.request_quit();
        assert!(
            app.should_quit,
            "nothing happened; confirming has no purpose"
        );

        let mut used = App::default();
        used.take_submission();
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

    #[test]
    fn quitting_mid_turn_requires_a_second_confirmation() {
        let mut app = App::default();
        app.push_char('h');
        app.take_submission();
        app.begin_turn();
        app.request_quit();
        assert!(app.quit_confirm_open());

        app.quit_confirm_move();
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
