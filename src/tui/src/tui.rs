//! TUI shell over [`app`](jan_klod::app) and [`Transport`]. Thin,
//! terminal-bound glue (not unit-tested); state logic in tested `App`, which
//! has never known its transport.
//!
//! Transport is `Arc<dyn Transport>` not `Address` because turn and answer
//! threads share one pipe over stdio. Address could be cloned; pipe can't.

use std::sync::mpsc;
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use jan_klod::app::{App, Prompt, SessionEntry, SessionMessage, ToastKind, Turn};
use jan_klod::blocks;
use jan_klod::commands::Availability;
use jan_klod::keymap::{self, Context};
use jan_klod::layout;
use jan_klod::sidebar::{self, SessionInfo};
use jan_klod::theme::{Glyph, Theme};
use jan_klod::transport::Transport;
use jan_klod::viewport::Viewport;
use jan_klod::StreamEvent;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::layout::Rect;
use ratatui::layout::{Constraint, Layout};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, Paragraph};
use ratatui::{DefaultTerminal, Frame};

const POLL_MS: u64 = 50;

/// Run TUI against `transport` with `session`, restore terminal on exit.
/// `force_ascii` is `--ascii`. Theme resolved once from environment, not per
/// frame (terminal capabilities unchanged; re-reading would make rendering
/// depend on untestable state).
///
/// # Errors
/// Propagates a terminal I/O error from the draw/event loop.
pub fn run(
    transport: &Arc<dyn Transport>,
    session: &str,
    force_ascii: bool,
) -> std::io::Result<()> {
    let theme = Theme::from_process_env(force_ascii);
    let mut terminal = ratatui::init();
    let result = event_loop(&mut terminal, transport, session, theme);
    ratatui::restore();
    result
}

fn event_loop(
    terminal: &mut DefaultTerminal,
    transport: &Arc<dyn Transport>,
    session: &str,
    theme: Theme,
) -> std::io::Result<()> {
    let mut app = App::default();
    // Session id owned by App, mutable from here (#105): switch or `/new`
    // replaces it via `App::load_session`, not held by loop.
    app.set_session(session.to_string());
    take_connect_facts(&mut app, transport);
    let mut view = Viewport::default();
    // Last frame's state. Scroll keys need transcript line count and pane
    // height; only `render` knows these (it wraps text to terminal width).
    let mut pane = Pane::default();
    // Completion root: startup directory. Moved list (if chdir called) worse
    // than wrong about a missing directory.
    let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
    // Header and sidebar's SESSION show `cwd` and `described` for run life;
    // neither polls (true at connect time, unchanged). Session id not here
    // (#105): can change, read from `app.session()`.
    let described = transport.describe();
    let meta = Meta {
        described: &described,
        cwd: &cwd,
    };
    app.record_status(format!(
        "connected to {described} (session `{session}`); Esc to quit"
    ));

    // Stream events from background turn thread.
    let mut rx: Option<mpsc::Receiver<Result<StreamEvent, String>>> = None;
    // Poll count drives spinner. `wrapping_add`: overflow after ~29k years
    // shouldn't panic.
    let mut tick: usize = 0;

    while !app.should_quit {
        // Drain all pending stream events before redrawing.
        if let Some(receiver) = &rx {
            loop {
                match apply_event(&mut app, receiver, tick) {
                    Step::Applied => {}
                    Step::Drained => break,
                    Step::Ended => {
                        rx = None;
                        // Turn back to Idle (any end: answered, failed,
                        // sender dropped). `finish_turn` sets on answered path;
                        // only place it's set on other two.
                        app.end_turn();
                        break;
                    }
                }
            }
        }

        // Asks since last frame (cancel, session/list, session/create)
        // sent by the one transport-holder.
        drain_transport_requests(&mut app, transport);

        terminal.draw(|frame| render(frame, &app, theme, &mut view, &mut pane, tick, &meta))?;

        // Short poll for incremental redraws during streaming.
        if !event::poll(Duration::from_millis(POLL_MS))? {
            // Poll timeout drives spinner heartbeat (#160). Frame advances
            // here not on delta arrival, so thinking turn looks alive.
            tick = tick.wrapping_add(1);
            // Toast heartbeat (#104): same tick as spinner, no clock/sleep
            // needed to test expiry.
            app.tick(tick);
            // Gateway can exit silently. `Transport::alive` catches it;
            // mid-turn errors caught in `apply_event`.
            if !app.disconnected() && !transport.alive() {
                app.disconnect(format!("{} exited", transport.describe()));
            }
            continue;
        }
        let Event::Key(key) = event::read()? else {
            continue;
        };
        if key.kind != KeyEventKind::Press {
            continue;
        }
        // Editing and scrolling first: pure mapping to composer/viewport.
        // Separating keeps loop focused on driving turns.
        if edit_or_scroll(&key, &mut app, &mut view, pane, theme) {
            // Completion is buffer function, refreshed after change not in
            // each arm.
            app.refresh_completion(&cwd);
            continue;
        }
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Esc => app.quit(),
            // `Ctrl+D`, or `Ctrl+C` on an empty composer while `Idle` (#105):
            // ask before leaving rather than cancelling nothing. Any other
            // `Ctrl+C` still stops the running turn — *how* is the
            // transport's business, a `turn/cancel` over stdio, a stream
            // teardown over REST — and this does not know which it got.
            KeyCode::Char('d') if ctrl => app.request_quit(),
            KeyCode::Char('c') if ctrl => {
                if app.turn() == Turn::Idle && app.input().is_empty() {
                    app.request_quit();
                } else {
                    request_cancel(&mut app);
                }
            }
            // `Ctrl+S`: ask for a fresh `session/list`, drained above.
            KeyCode::Char('s') if ctrl => app.request_sessions(),
            // The switcher's `Enter` needs `transport.session_get`, which
            // `App` cannot hold — handled here rather than through
            // `enter_means`, which is the running turn's state machine and
            // knows nothing about the switcher.
            KeyCode::Enter if app.sessions_open() => switch_session(&mut app, transport),
            KeyCode::Enter => match enter_means(&app) {
                // A pending confirmation is answered even though a turn is
                // running — that turn is precisely what is blocked waiting for
                // it, which is why `Blocked` comes first in `enter_means`.
                Enter::Answer => {
                    if let Some((prompt_session, answer)) = app.take_answer() {
                        let transport = Arc::clone(transport);
                        // The **notification's own session**, not this loop's
                        // `session` — a client may be driving more than one,
                        // and that is precisely why the notification carries
                        // it (#103). Answering with the wrong one is the bug
                        // this slice exists to fix.
                        thread::spawn(move || {
                            let _ = transport.answer(&prompt_session, &answer);
                        });
                    }
                }
                // A prompt is pending but its dialog was closed (`Esc`): show
                // it again rather than answer blind. The user must see the
                // options before `Enter` can mean anything.
                Enter::ReopenPrompt => app.open_dialog(),
                Enter::Steer => {
                    let session = app.session().to_string();
                    steer(transport.as_ref(), &session, &mut app);
                }
                Enter::Send if rx.is_none() => {
                    if let Some(message) = app.take_submission() {
                        app.begin_turn();
                        let transport = Arc::clone(transport);
                        let session = app.session().to_string();
                        let (tx, new_rx) = mpsc::channel();
                        thread::spawn(move || {
                            let result = transport.stream_turn(&session, &message, &mut |event| {
                                let _ = tx.send(Ok(event));
                            });
                            if let Err(err) = result {
                                let _ = tx.send(Err(err));
                            }
                        });
                        rx = Some(new_rx);
                    }
                }
                Enter::Send | Enter::Nothing => {}
            },
            _ => {}
        }
    }
    Ok(())
}

/// What the model learns once, at connect, and never polls for again.
///
/// The transport kind drives the quit confirm's wording (#105) — over stdio
/// the spawned gateway dies with the client, over `--addr` it does not. The
/// contributions were reported during the handshake and held by the transport
/// until there was a model to give them to; a connection that does not carry
/// them yields none, which renders as no extra menu entries and nothing else.
fn take_connect_facts(app: &mut App, transport: &Arc<dyn Transport>) {
    app.set_stdio(transport.is_stdio());
    app.set_contributions(&transport.contributions());
}

/// Asks raised since last frame, sent to transport (#105). One function
/// (all three same shape): model raises intent, loop executes/reports back.
/// Switcher's `session/get` not here: answered from keystroke, not drained.
fn drain_transport_requests(app: &mut App, transport: &Arc<dyn Transport>) {
    // `Ctrl+C` or menu. `App::cancel` refuses twice, so no double-send. Menu
    // acts without App knowing transport.
    if app.take_cancel_request() {
        match transport.cancel(app.session()) {
            Ok(()) => app.record_status("cancelling — the answer so far is kept"),
            Err(err) => app.record_status(format!("cancel could not be sent: {err}")),
        }
    }

    // `Ctrl+S`/`/sessions` safe here: can't reach while turn streams. App
    // refuses during ask dialog; switcher's Enter (only mid-turn option)
    // refused by `App::sessions_confirm`.
    if app.take_sessions_request() {
        match transport.session_list() {
            Ok(sessions) => app.open_sessions(
                sessions
                    .into_iter()
                    .map(|s| SessionEntry {
                        id: s.id,
                        preview: s.preview,
                    })
                    .collect(),
            ),
            Err(err) => app.record_error(format!("session/list: {err}")),
        }
    }

    // A contributed command the user chose. Same shape as the three above:
    // the model recorded intent, and this is where a transport exists to act
    // on it. The answer goes to the status line — a contribution reports in
    // words, and the transcript is for the turn.
    if let Some((extension, name)) = app.take_invoke_request() {
        match transport.invoke_contribution(&extension, &name) {
            Ok(text) if text.is_empty() => app.record_status(format!("{name} ran")),
            Ok(text) => app.record_status(text),
            Err(err) => app.record_error(format!("{name}: {err}")),
        }
        // An invocation may have changed what is contributed; the transport
        // caught the new set while reading the answer.
        app.set_contributions(&transport.contributions());
    }

    // `/new`: `session/create` needs no id, can't race turn's reader.
    if app.take_new_session_request() {
        match transport.create_session() {
            Ok(id) => app.load_session(id, Vec::new()),
            Err(err) => app.record_error(format!("session/create: {err}")),
        }
    }
}

/// Switcher's `Enter`: `App::sessions_confirm` allows switch (refusing
/// mid-turn). Once approved, `transport.session_get` is the one synchronous
/// call (as its docs require — nothing reads pipe during switch, which only
/// starts on `Idle`).
fn switch_session(app: &mut App, transport: &Arc<dyn Transport>) {
    let Some(id) = app.sessions_confirm() else {
        return;
    };
    match transport.session_get(&id) {
        Ok(result) => {
            let messages = result
                .messages
                .into_iter()
                .map(|m| SessionMessage {
                    role: m.role,
                    content: m.content,
                    tool_call_id: m.tool_call_id,
                })
                .collect();
            app.load_session(result.id, messages);
        }
        Err(err) => app.record_error(format!("session/get: {err}")),
    }
}

/// What `Enter` does now (#159). Four meanings, state picks. Extracted from
/// loop (guard chain untestable); pure model function. Read in
/// `enter::the_state_decides_what_enter_means` as the table it is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Enter {
    /// Answer pending confirmation. **Not steer**: question ≠ message.
    Answer,
    /// A prompt is pending but its dialog is closed: show it again rather than
    /// answer (#103). The user must see the options before `Enter` can send
    /// anything — a closed dialog answering blind would be exactly the "press
    /// Enter twice" path to an unseen choice the design rules out.
    ReopenPrompt,
    /// Steer the turn already running — `turn/follow-up`.
    Steer,
    /// Start a turn — `session/message`.
    Send,
    /// Nothing, because the turn is stopping and steering a turn that is being
    /// cancelled is not a thing to offer.
    Nothing,
}

/// What meaning state gives `Enter`. Order matters (#159 table). `Blocked`
/// checked first because turn is streaming underneath — steer not answer if
/// reversed. `Blocked` (added #103): dialog open → answer; closed → reopen
/// (blind answer is forbidden). Read from `App::turn()` not loop's `rx` (both
/// answer "running?"; model accessor authoritative per #158; early-exit
/// steers wrong otherwise). `/` menu and `@` completion take `Enter` before
/// here (19d), fallthrough on empty so empty list doesn't block send.
fn enter_means(app: &App) -> Enter {
    match app.turn() {
        Turn::Blocked if app.dialog_open() => Enter::Answer,
        Turn::Blocked => Enter::ReopenPrompt,
        Turn::Streaming => Enter::Steer,
        Turn::Idle => Enter::Send,
        Turn::Cancelling => Enter::Nothing,
    }
}

/// Steer turn (send follow-up), show refusal not silent drop (#159).
/// **Synchronous** (unlike `answer`): deliberate. `Rest::follow_up` is refusal
/// with no I/O; `Stdio::follow_up` writes to pipe with reader on other thread
/// — neither blocks draw loop. Thread would drop error, which over REST is the
/// whole answer: no in-flight route, keystroke vanishes, user thinks steering
/// broken not connection incapable. Message always taken, refusal recorded; if
/// refused and not shown, transcript missing half exchange.
fn steer(transport: &dyn Transport, session: &str, app: &mut App) {
    let Some(message) = app.take_submission() else {
        return;
    };
    if let Err(refusal) = transport.follow_up(session, &message) {
        app.record_status(refusal);
    }
}

/// `Ctrl+C`: ask running turn to stop. Returns false if nothing runs or
/// already asked (no double-narration, no double-send). **Partial answer
/// stays**: cancel finalizes with what arrived; delete would misreport what
/// core did. Used to only narrate (#157 — client couldn't send); now does send
/// (stdio: `turn/cancel`, REST: stream teardown → `Flow::Stop`). Doesn't send
/// itself; App raises ask, loop drains, so menu and key share path (App holds
/// no transport). One method, two implementations.
fn request_cancel(app: &mut App) -> bool {
    app.cancel()
}

/// Last frame's dimensions for scroll keys.
#[derive(Debug, Default, Clone, Copy)]
struct Pane {
    /// Rendered transcript lines.
    total: usize,
    /// Transcript pane rows.
    height: usize,
    /// Composer text cells (vertical motion/history — visual rows).
    composer_width: usize,
    /// Transcript cells (different: composer caret takes 2 more).
    /// `span_of` measured against actual frame width.
    transcript_width: usize,
    /// Sidebar hidden (#105 "sidebar-in-a-dialog")? `Ctrl+B` dialog only
    /// opens when true (pointless at width with permanent pane).
    sidebar_hidden: bool,
}

/// Keys moving caret or viewport. Returns true if consumed. Split from loop
/// (pure mapping, loop spawns threads/owns turn; mixing does both badly).
fn edit_or_scroll(
    key: &ratatui::crossterm::event::KeyEvent,
    app: &mut App,
    view: &mut Viewport,
    pane: Pane,
    theme: Theme,
) -> bool {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let alt = key.modifiers.contains(KeyModifiers::ALT);
    let shift = key.modifiers.contains(KeyModifiers::SHIFT);
    // Dialogs and their shortcuts first (#103, #105): own all keys when open.
    if dialog_priority_keys(key, app, pane) {
        return true;
    }
    // `@` completion first: mutually exclusive lists (slash col 0 vs `@`
    // anywhere); ordering makes it code property not data.
    if app.completion_open() {
        match key.code {
            KeyCode::Esc => {
                app.completion_dismiss();
                return true;
            }
            KeyCode::Up => {
                app.completion_move(-1);
                return true;
            }
            KeyCode::Down => {
                app.completion_move(1);
                return true;
            }
            KeyCode::Enter | KeyCode::Tab if app.completion_accept() => return true,
            _ => {}
        }
    }
    // Menu takes its keys when open, only those.
    if app.menu_query().is_some() {
        match key.code {
            KeyCode::Esc => {
                app.menu_dismiss();
                return true;
            }
            KeyCode::Up => {
                app.menu_move(-1);
                return true;
            }
            KeyCode::Down => {
                app.menu_move(1);
                return true;
            }
            // `Enter` on the empty state falls through to submit rather than
            // being swallowed: a menu showing nothing must not make the
            // composer unsendable.
            KeyCode::Enter | KeyCode::Tab if app.menu_accept() => return true,
            _ => {}
        }
    }
    // Transcript cursor key (#155). `Alt+Up`/`Down` not bare arrows (those
    // are composer's). Bare + cursor move would make neither predictable.
    match key.code {
        KeyCode::Char('o') if ctrl => {
            // A no-op when nothing is selected, and it still consumes the key:
            // falling through would reach the `Char(c)` arm and type an `o`
            // into the message, which is the worst of both answers.
            app.toggle_expanded();
            reveal_cursor(app, view, pane, theme);
            return true;
        }
        KeyCode::Up if alt => {
            app.cursor_up();
            reveal_cursor(app, view, pane, theme);
            return true;
        }
        KeyCode::Down if alt => {
            app.cursor_down();
            reveal_cursor(app, view, pane, theme);
            return true;
        }
        _ => {}
    }
    match key.code {
        // Scrolling. `Ctrl+U`/`D` reach here only when composer empty (#150
        // consumes with content, deliberately not without); no test needed.
        KeyCode::PageUp => *view = view.page_up(pane.height),
        KeyCode::PageDown => *view = view.page_down(pane.total, pane.height),
        KeyCode::Char('u') if ctrl && app.composer.is_empty() => *view = view.half_up(pane.height),
        KeyCode::Char('d') if ctrl && app.composer.is_empty() => {
            *view = view.half_down(pane.total, pane.height);
        }
        // Editing: readline where terminal delivers. Composer owns meanings;
        // pure mapping.
        KeyCode::Left if alt => app.composer.word_left(),
        KeyCode::Right if alt => app.composer.word_right(),
        KeyCode::Left => app.composer.left(),
        KeyCode::Right => app.composer.right(),
        KeyCode::Home => app.composer.home(),
        KeyCode::End => app.composer.end(),
        KeyCode::Char('a') if ctrl => app.composer.home(),
        KeyCode::Char('e') if ctrl => app.composer.end(),
        KeyCode::Char('w') if ctrl => app.composer.delete_word_back(),
        KeyCode::Char('k') if ctrl => app.composer.kill_to_end(),
        // Other half #148: with content kills line; empty falls through.
        KeyCode::Char('u') if ctrl => app.composer.kill_line(),
        // Both spellings, because terminals disagree about which reaches the
        // application — and `/newline` (#152) is the fallback where neither
        // does. Leaving a user with no way to type a newline is not an
        // option a client gets to choose.
        KeyCode::Enter if shift || alt => app.composer.push('\n'),
        // History or caret: `App` decides (condition is two state facts).
        KeyCode::Up => app.history_up(pane.composer_width),
        KeyCode::Down => app.history_down(pane.composer_width),
        KeyCode::Backspace => app.backspace(),
        KeyCode::Char(c) if !ctrl => app.push_char(c),
        _ => return false,
    }
    true
}

/// Dialogs [`edit_or_scroll`] checks before composer/menu, plus their open
/// shortcuts (#103, #105). Ask first (owns all keys; digit can't go to message).
/// Quit, switcher, help, sidebar follow (App refuses >1 open). `?` and `Ctrl+B`
/// open from nothing, checked last (so `?` while filtering switcher reaches
/// its `Char` arm).
fn dialog_priority_keys(
    key: &ratatui::crossterm::event::KeyEvent,
    app: &mut App,
    pane: Pane,
) -> bool {
    if dialog_keys(key, app) {
        return true;
    }
    if quit_confirm_keys(key, app) {
        return true;
    }
    if sessions_keys(key, app) {
        return true;
    }
    if help_keys(key, app) {
        return true;
    }
    if sidebar_dialog_keys(key, app) {
        return true;
    }
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    // `?` on empty composer opens help; elsewhere it's punctuation.
    if key.code == KeyCode::Char('?')
        && !ctrl
        && !key.modifiers.contains(KeyModifiers::ALT)
        && app.composer.is_empty()
    {
        app.toggle_help();
        return true;
    }
    // `Ctrl+B`: sidebar as dialog, only where 19g layout has no permanent
    // pane (pane.sidebar_hidden answers it). No `layout::regions` copy needed.
    // At width with permanent sidebar, deliberately pointless (redundant).
    if key.code == KeyCode::Char('b') && ctrl && pane.sidebar_hidden {
        app.toggle_sidebar_dialog();
        return true;
    }
    false
}

/// Ask dialog keys while open (#103). Returns consumed. `Esc` closes without
/// answering (prompt queued, reopenable). Digit jumps to option index (1-based);
/// `↑`/`↓` move; nothing answers (no pre-approved key). **List prompt: no
/// free typing** — unmapped keys no-op, can't reach composer. Free-text:
/// only `Esc` handled, rest falls through (answer is what's typed). `Enter`
/// never consumed here → loop's `enter_means`/`Answer`.
fn dialog_keys(key: &ratatui::crossterm::event::KeyEvent, app: &mut App) -> bool {
    if !app.dialog_open() {
        return false;
    }
    let Some(prompt) = app.pending_prompt() else {
        return false;
    };
    let list_mode = !prompt.options.is_empty();
    match key.code {
        KeyCode::Esc => {
            app.close_dialog();
            return true;
        }
        KeyCode::Up if list_mode => {
            app.prompt_move(-1);
            return true;
        }
        KeyCode::Down if list_mode => {
            app.prompt_move(1);
            return true;
        }
        KeyCode::Char(c) if list_mode && c.is_ascii_digit() && c != '0' => {
            if let Some(digit) = c.to_digit(10) {
                app.prompt_jump(usize::try_from(digit).unwrap_or(0));
            }
            return true;
        }
        KeyCode::Enter => return false,
        _ => {}
    }
    list_mode
}

/// Keys the quit confirm owns while it is open (#105). Consumes everything —
/// there is no free text to type here, only cancel or quit.
const fn quit_confirm_keys(key: &ratatui::crossterm::event::KeyEvent, app: &mut App) -> bool {
    if !app.quit_confirm_open() {
        return false;
    }
    match key.code {
        KeyCode::Esc => app.quit_confirm_close(),
        KeyCode::Up | KeyCode::Down | KeyCode::Left | KeyCode::Right | KeyCode::Tab => {
            app.quit_confirm_move();
        }
        KeyCode::Enter => app.quit_confirm_accept(),
        _ => {}
    }
    true
}

/// Switcher keys while open (#105). `Esc` closes; `↑`/`↓` highlight; type to
/// filter. **`Enter` not consumed** → loop's match (calls `Transport::session_get`,
/// unreachable here; no transport; same reason `dialog_keys` leaves `Enter`).
fn sessions_keys(key: &ratatui::crossterm::event::KeyEvent, app: &mut App) -> bool {
    if !app.sessions_open() {
        return false;
    }
    match key.code {
        KeyCode::Esc => {
            app.sessions_close();
            true
        }
        KeyCode::Up => {
            app.sessions_move(-1);
            true
        }
        KeyCode::Down => {
            app.sessions_move(1);
            true
        }
        KeyCode::Backspace => {
            app.sessions_backspace();
            true
        }
        KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
            app.sessions_push_char(c);
            true
        }
        KeyCode::Enter => false,
        _ => true,
    }
}

/// Keys the help overlay owns while it is open (#105). `Esc`/`?` closes it;
/// `↑`/`↓`/`PageUp`/`PageDown` scroll it.
const fn help_keys(key: &ratatui::crossterm::event::KeyEvent, app: &mut App) -> bool {
    if !app.help_open() {
        return false;
    }
    match key.code {
        KeyCode::Esc | KeyCode::Char('?') => app.close_help(),
        KeyCode::Up => app.help_scroll_by(-1),
        KeyCode::Down => app.help_scroll_by(1),
        KeyCode::PageUp => app.help_scroll_by(-10),
        KeyCode::PageDown => app.help_scroll_by(10),
        _ => {}
    }
    true
}

/// Keys the sidebar's dialog owns while it is open (#105). `Esc`/`Ctrl+B`
/// closes it; it has nothing else to do with a key, since it is a read-only
/// projection the same way the permanent sidebar pane is.
const fn sidebar_dialog_keys(key: &ratatui::crossterm::event::KeyEvent, app: &mut App) -> bool {
    if !app.sidebar_dialog_open() {
        return false;
    }
    match key.code {
        KeyCode::Esc => app.close_sidebar_dialog(),
        KeyCode::Char('b') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            app.close_sidebar_dialog();
        }
        _ => {}
    }
    true
}

/// Scroll so selected block is visible. Called after key moving cursor or
/// changing block height (opening block off-screen same defect as selecting
/// off-screen).
fn reveal_cursor(app: &App, view: &mut Viewport, pane: Pane, theme: Theme) {
    let Some(at) = app.cursor() else { return };
    // Measured against last frame's pane (alternative: double layout per key).
    // One-frame-stale width only matters on resize (next `reflow` fixes).
    let (start, rows) = blocks::span_of(&app.transcript, at, pane.transcript_width, theme);
    *view = view.reveal(start, rows, pane.total, pane.height);
}

/// What one streamed event did to the turn.
enum Step {
    /// Applied; there may be more waiting.
    Applied,
    /// Nothing left right now.
    Drained,
    /// The turn is over, cleanly or otherwise.
    Ended,
}

/// Take one streamed event and apply it.
///
/// Its own function because the event loop was past clippy's line limit — and
/// the split is a real one rather than a concession: this half is a fold over
/// `App`, while the loop's other job is spawning threads and owning the
/// channel.
fn apply_event(
    app: &mut App,
    receiver: &mpsc::Receiver<Result<StreamEvent, String>>,
    tick: usize,
) -> Step {
    match receiver.try_recv() {
        Ok(Ok(StreamEvent::Delta(text))) => app.apply_delta(&text),
        Ok(Ok(StreamEvent::Done(answer))) => {
            // Before the answer: a block still open when the turn ends is
            // interrupted, not still running.
            app.interrupt_open_tools();
            app.finish_turn(answer);
            return Step::Ended;
        }
        Ok(Ok(StreamEvent::Tool {
            id,
            name,
            arguments,
        })) => app.record_tool_invoked(id, name, arguments),
        Ok(Ok(StreamEvent::ToolResult {
            id,
            content,
            failed,
        })) => app.record_tool_result(&id, content, failed),
        // A toast (#104): the full text still lands in the transcript, via
        // `toast_warning`, so a toast that expires unread has lost nothing.
        Ok(Ok(StreamEvent::Warning(msg))) => app.toast_warning(msg, tick),
        Ok(Ok(StreamEvent::Prompt {
            session,
            question,
            options,
            default,
        })) => app.ask(Prompt {
            session,
            question,
            options,
            default,
        }),
        // A turn-level failure the core itself reported. Distinct from the
        // arm below: this is the core saying a turn went wrong, not the
        // connection to it dying — the transcript gets the error and the turn
        // ends, but the transport is not declared dead over an ordinary
        // failure.
        Ok(Ok(StreamEvent::Error(err))) => {
            app.interrupt_open_tools();
            app.record_error(err);
            return Step::Ended;
        }
        // Not a turn event: an extension reported that what it offers has
        // changed, mid-turn. Taking it now keeps the menu honest without
        // touching the turn.
        Ok(Ok(StreamEvent::Contributions(sets))) => app.set_contributions(&sets),
        // The transport itself failed — `Transport::stream_turn` returned
        // `Err` rather than a frame the core sent. That is a dead connection,
        // not a turn outcome (#104): say so in the status bar and stop
        // pretending the spinner still means something.
        Ok(Err(err)) => {
            app.disconnect(err);
            return Step::Ended;
        }
        Err(mpsc::TryRecvError::Empty) => return Step::Drained,
        Err(mpsc::TryRecvError::Disconnected) => return Step::Ended,
    }
    Step::Applied
}

/// Which caret the composer wears — the first of #160's three signals.
///
/// `Enter` means something different while a turn is running, and this is the
/// mark that says so. A `Glyph` rather than a tint, because under `Mode::Mono`
/// every role is `Color::Reset` and a tinted caret would be no signal at all.
const fn caret_glyph(turn: Turn) -> Glyph {
    match turn {
        Turn::Streaming => Glyph::CaretSteering,
        // `Cancelling` keeps the idle caret on purpose: the turn is stopping,
        // so steering it is not a thing to advertise. `Blocked` likewise — what
        // `Enter` does there is answer the confirmation, which the title says
        // in words rather than by reusing the steering mark for a third meaning.
        Turn::Idle | Turn::Cancelling | Turn::Blocked => Glyph::Caret,
    }
}

/// What the composer is *for* right now — the second of #160's three signals.
///
/// The border title, which is the closest thing a `ratatui` input has to a
/// placeholder. Separate from [`status_hint`] deliberately: this says what the
/// field is, that says which keys act, and #160 asks for both because one
/// signal is one thing to miss.
fn composer_label(app: &App) -> String {
    app.pending_prompt().map_or_else(
        || {
            match app.turn() {
                Turn::Streaming => "steer this turn",
                Turn::Cancelling => "cancelling",
                // `Blocked` is unreachable here — `pending_prompt()` is what
                // makes a turn `Blocked`, and this arm is the `None` branch.
                Turn::Idle | Turn::Blocked => "message",
            }
            .to_string()
        },
        |prompt| {
            if prompt.options.is_empty() {
                "answer".to_string()
            } else {
                format!("answer [{}]", prompt.options.join("/"))
            }
        },
    )
}

/// Which [`Context`] the current state reads keybindings in — the mapping
/// [`status_hint`] (and 19h's help overlay) reads the table through.
fn status_context(app: &App) -> Context {
    if app.pending_prompt().is_some() {
        return if app.dialog_open() {
            Context::DialogOpen
        } else {
            Context::DialogClosed
        };
    }
    match app.turn() {
        Turn::Streaming => Context::Streaming,
        Turn::Cancelling => Context::Cancelling,
        // `Blocked` is unreachable here — `pending_prompt()` above is what
        // makes a turn `Blocked`, and this arm is the `None` branch of it.
        Turn::Idle | Turn::Blocked => Context::Idle,
    }
}

/// Which keys act right now — the third of #160's three signals, and #104's
/// Acceptance line 3: every word of this comes from [`keymap::BINDINGS`]
/// rather than a hand-written string, so the table and the status bar cannot
/// drift apart — see `keymap::the_hint_changes_when_the_table_entry_changes`
/// for what proves it.
///
/// `narrow` is #104's 60–79 width band ("fewer status hints"): the first
/// binding for the context rather than all of them, still read from the same
/// table.
///
/// The one thing that is not in the table: a pending prompt's `default`,
/// which is data the core sent this turn, not a keybinding — it is appended
/// after the table-driven hint rather than folded into it.
fn status_hint(app: &App, narrow: bool) -> String {
    status_hint_from(keymap::BINDINGS, app, narrow)
}

/// [`status_hint`] against an arbitrary table.
///
/// The table is a **parameter** rather than closed over, and that is #104's
/// Acceptance line 3 rather than a style choice: the hint must be *produced
/// from* the keymap, so that a hand-written duplicate would fail. Testing
/// `keymap::hint` with a doctored table proves only that `keymap::hint` reads
/// its argument — it says nothing about whether this function calls it, which
/// is precisely where a duplicate would live. Threading the table to the seam
/// the status bar actually uses is what closes that.
///
/// Found by probe: replacing this body with a hand-written `match` on
/// `App::turn` that still varied by state passed every keymap test, and failed
/// only one unrelated assertion about the prompt default.
fn status_hint_from(table: &[keymap::Binding], app: &App, narrow: bool) -> String {
    let context = status_context(app);
    let base = if narrow {
        keymap::hint_narrow(table, context)
    } else {
        keymap::hint(table, context)
    };
    if context == Context::DialogOpen {
        if let Some(prompt) = app.pending_prompt() {
            // The default is *data from the notification*, not a binding, so it
            // is appended here rather than living in the keymap — what silence
            // means differs per prompt and the table is per build.
            return format!("{base} (default: `{}`)", prompt.default);
        }
    }
    base
}

/// The spinner frame for `tick`, or nothing when no turn is running.
///
/// **Advanced by the caller's poll tick, not by an arriving delta** (#160).
/// `text-delta` is documented as one per completion rather than one per token,
/// so an animation driven by arrivals would sit frozen through exactly the long
/// wait it exists to reassure someone about — a spinner that stops looks like a
/// client that has hung.
fn spinner_frame(theme: Theme, turn: Turn, tick: usize) -> Option<&'static str> {
    match turn {
        Turn::Idle => None,
        Turn::Streaming | Turn::Cancelling | Turn::Blocked => {
            let frames = theme.spinner();
            frames.get(tick % frames.len()).copied()
        }
    }
}

/// What the frame needs to know that is fixed for the whole run: how it
/// reached the core, and where it started. Resolved once in [`event_loop`]
/// and handed in rather than re-read, because neither changes while the
/// client runs — unlike the session id (#105), which is `app.session()` now
/// precisely because it can.
struct Meta<'a> {
    described: &'a str,
    cwd: &'a std::path::Path,
}

fn render(
    frame: &mut Frame,
    app: &App,
    theme: Theme,
    view: &mut Viewport,
    pane: &mut Pane,
    tick: usize,
    meta: &Meta<'_>,
) {
    let area = frame.area();
    let regions = layout::regions(area.width, area.height);

    // Below five rows for the transcript and three for the composer, there is
    // nothing this frame can show that is not itself misleading (#104) — a
    // border squeezed to nothing reads as a rendering bug, not as "make the
    // window bigger".
    if regions.too_small {
        render_too_small(frame, area, theme);
        return;
    }

    render_header(
        frame,
        regions.header,
        app,
        meta,
        theme,
        regions.collapsed_header,
        tick,
    );
    render_status_bar(frame, regions.status, app, theme, regions.collapsed_header);
    if let Some(sidebar_area) = regions.sidebar {
        render_sidebar(frame, sidebar_area, app, meta, theme);
    }

    let content = regions.content;

    // The composer grows with its content and then scrolls, so the split is
    // computed from the buffer rather than fixed at three rows. `- 2` for the
    // block border, and again for the caret glyph and the space after it.
    let caret = format!("{} ", theme.glyph(caret_glyph(app.turn())));
    let caret_cells = jan_klod::wrap::width(&caret);
    let composer_width = usize::from(content.width).saturating_sub(2 + caret_cells);

    let (composer_rows, (caret_row, caret_col)) = app.composer.visible(composer_width);
    let composer_height = u16::try_from(composer_rows.len().max(1)).unwrap_or(1);

    let [transcript_area, input_area] =
        Layout::vertical([Constraint::Min(1), Constraint::Length(composer_height + 2)])
            .areas(content);

    // The transcript is rendered by `blocks::transcript` (#147), which is a
    // pure function of the entries plus a width — so the layout is tested
    // without a terminal and this function only places the result. The gutter,
    // the role label and the wrapping all live there; what is left here is
    // where it goes on screen, which is #148's concern next.
    //
    // Below 60 columns (#104) the transcript loses its border: `single_column`
    // spends those two columns and two rows on the pane instead, and there is
    // no border left to hold the spinner or the "more below" marker.
    let border = !regions.single_column;
    let reserved = if border { 2 } else { 0 };
    let inner_width = usize::from(transcript_area.width).saturating_sub(reserved);
    let inner_height = usize::from(transcript_area.height).saturating_sub(reserved);
    let lines = blocks::transcript(&app.transcript, app.cursor(), inner_width, theme);

    // Re-clamp against what this frame actually holds, *then* record it. While
    // attached this follows a streaming delta to the new bottom; while detached
    // it holds position, which is the whole point of #148.
    *view = view.reflow(lines.len(), inner_height);
    *pane = Pane {
        total: lines.len(),
        height: inner_height,
        composer_width,
        transcript_width: inner_width,
        sidebar_hidden: regions.sidebar.is_none(),
    };

    let offset = u16::try_from(view.offset()).unwrap_or(u16::MAX);
    let transcript = if border {
        // The detach marker: a glyph and a count, never a tint. Under
        // `Mode::Mono` every role resolves to `Color::Reset`, so a marker that
        // were only a colour would vanish exactly when the user most needs to
        // know the view is stale.
        let mut block = Block::bordered().title("jan-klod");
        let below = view.below(lines.len(), inner_height);
        if below > 0 {
            block = block.title_bottom(
                Line::from(Span::styled(
                    format!(" {} {below} ", theme.glyph(Glyph::MoreBelow)),
                    Style::default().fg(theme.warning()),
                ))
                .right_aligned(),
            );
        }
        Paragraph::new(lines).block(block).scroll((offset, 0))
    } else {
        Paragraph::new(lines).scroll((offset, 0))
    };
    frame.render_widget(transcript, transcript_area);

    // Two of #160's three signals: what this field is (the title) and which
    // keys act (the status bar, below the composer rather than on its own
    // border since #104 gave the hint a permanent home). The third is the
    // caret above. Three separate marks rather than one sentence, because the
    // cost of missing the change is sending a message to a turn that is still
    // running.
    let title = composer_label(app);
    // The caret glyph is the prompt marker, and it is a `Glyph` so it degrades;
    // the *cursor* is the terminal's own, placed below, because a drawn block
    // does not blink and does not move with the user's own expectations.
    let composer: Vec<Line<'static>> = composer_rows
        .into_iter()
        .enumerate()
        .map(|(i, row)| {
            let marker = if i == 0 {
                caret.clone()
            } else {
                " ".repeat(caret_cells)
            };
            Line::from(vec![
                Span::styled(marker, Style::default().fg(theme.focus())),
                Span::styled(row, Style::default().fg(theme.body())),
            ])
        })
        .collect();
    let input = Paragraph::new(composer).block(Block::bordered().title(title));
    frame.render_widget(input, input_area);
    // +1 for the border, and the caret column is already in display cells.
    frame.set_cursor_position((
        input_area.x + 1 + u16::try_from(caret_cells + caret_col).unwrap_or(0),
        input_area.y + 1 + u16::try_from(caret_row).unwrap_or(0),
    ));
    render_menu(frame, app, theme, input_area);
    render_completion(frame, app, theme, input_area);
    // Last, so it draws over everything above — the transcript keeps streaming
    // behind it and the header keeps timing, exactly as #103 asks. At most one
    // of the four is ever open (`App` enforces that — see `edit_or_scroll`'s
    // ordering comment), so which is drawn last among these four does not
    // matter; all four are tried because each is a no-op when it is not.
    render_prompt_dialog(frame, app, theme);
    render_sessions_dialog(frame, app, theme);
    render_help(frame, app, theme);
    render_sidebar_dialog(frame, app, meta, theme);
    render_quit_confirm(frame, app, theme);
}

/// The header, one row (#104): a short wordmark, the session id, how the
/// client reached the core, and — on the right — the turn indicator 19e gave
/// the transcript border, moved here now that there is a header to hold it.
///
/// `collapsed` is #104's 60–79 width band: session id and turn state only,
/// dropping how the client connected.
fn render_header(
    frame: &mut Frame,
    area: Option<Rect>,
    app: &App,
    meta: &Meta<'_>,
    theme: Theme,
    collapsed: bool,
    tick: usize,
) {
    let Some(area) = area else { return };
    let left = if collapsed {
        format!(
            " jan-klod · {} · {} ",
            app.session(),
            app.connection_state()
        )
    } else {
        format!(" jan-klod · {} · {} ", app.session(), meta.described)
    };
    let mut block = Block::default().title(Span::styled(left, Style::default().fg(theme.body())));
    if let Some(frame_glyph) = spinner_frame(theme, app.turn(), tick) {
        block = block.title(
            Line::from(Span::styled(
                format!(" {frame_glyph} "),
                Style::default().fg(theme.focus()),
            ))
            .right_aligned(),
        );
    }
    frame.render_widget(
        Paragraph::new(Vec::<Line<'static>>::new()).block(block),
        area,
    );
}

/// The status bar, one row (#104). Left: keybinding hints for the current
/// state, from [`status_hint`] — which is the keymap table, never a literal.
/// Right: a toast if one is showing, in `warning()`/`removed()` with its
/// glyph; otherwise the connection+turn word
/// (`ready`/`streaming`/`blocked`/`cancelling`/`disconnected`).
fn render_status_bar(frame: &mut Frame, area: Rect, app: &App, theme: Theme, collapsed: bool) {
    let hint = status_hint(app, collapsed);
    let mut block = Block::default().title(Span::styled(
        format!(" {hint} "),
        Style::default().fg(theme.muted()),
    ));

    let right = app.toast().map_or_else(
        || {
            Span::styled(
                format!(" {} ", app.connection_state()),
                Style::default().fg(theme.muted()),
            )
        },
        |(text, kind)| {
            let (glyph, colour) = match kind {
                ToastKind::Warning => (theme.glyph(Glyph::Warning), theme.warning()),
                ToastKind::Error => (theme.glyph(Glyph::ToolFailed), theme.removed()),
            };
            Span::styled(format!(" {glyph} {text} "), Style::default().fg(colour))
        },
    );
    block = block.title(Line::from(right).right_aligned());
    frame.render_widget(
        Paragraph::new(Vec::<Line<'static>>::new()).block(block),
        area,
    );
}

/// The sidebar (#104): SESSION, THIS TURN and CHANGED, drawn from
/// [`sidebar::view`] — a projection of `app` and nothing else. This function's
/// own job is placement, the same split every other pane here uses.
fn render_sidebar(frame: &mut Frame, area: Rect, app: &App, meta: &Meta<'_>, theme: Theme) {
    let info = SessionInfo {
        id: app.session(),
        via: meta.described,
        cwd: meta.cwd,
    };
    let inner_width = usize::from(area.width).saturating_sub(2);
    let lines = sidebar::view(app, info, inner_width, theme);
    frame.render_widget(
        Paragraph::new(lines)
            .block(Block::bordered().border_style(Style::default().fg(theme.border_idle()))),
        area,
    );
}

/// The sidebar as a dialog (#105's "sidebar-in-a-dialog"): the same
/// [`sidebar::view`] projection [`render_sidebar`] draws into its permanent
/// pane, framed by [`render_modal`] instead — reachable at a width where 19g's
/// layout leaves the sidebar with no pane of its own.
fn render_sidebar_dialog(frame: &mut Frame, app: &App, meta: &Meta<'_>, theme: Theme) {
    if !app.sidebar_dialog_open() {
        return;
    }
    let info = SessionInfo {
        id: app.session(),
        via: meta.described,
        cwd: meta.cwd,
    };
    let inner_width = usize::from(modal_width(frame.area().width)).saturating_sub(4);
    let lines = sidebar::view(app, info, inner_width, theme);
    render_modal(frame, " sidebar ", lines, theme, 0);
}

/// #104's Acceptance: a render below the floor a normal frame needs draws
/// this instead of a transcript and a composer squeezed past readability.
fn render_too_small(frame: &mut Frame, area: Rect, theme: Theme) {
    let lines = vec![Line::from(Span::styled(
        "terminal too small",
        Style::default().fg(theme.warning()),
    ))];
    frame.render_widget(
        Paragraph::new(lines)
            .block(Block::bordered())
            .alignment(ratatui::layout::Alignment::Center),
        area,
    );
}

/// The width of any modal drawn by [`render_modal`] — roughly two thirds of
/// the frame, capped so a huge terminal does not stretch it edge to edge, and
/// floored so a tiny one still gets something usable rather than a sliver.
/// Shared by every dialog's content function and [`render_modal`] itself, so
/// the width a dialog wraps its lines to is the width it is actually drawn
/// at.
fn modal_width(area_width: u16) -> u16 {
    ((area_width * 2) / 3).clamp(24, area_width.saturating_sub(4).max(24))
}

/// The one modal-dialog primitive (#105): centred, on `raised()` with a
/// `focus()` border — the one place in this client where a rendering mistake
/// has a security consequence (the ask dialog, #103), so every dialog gets
/// this surface rather than drawing its own. The ask dialog, the session
/// switcher, the help overlay and the quit confirm all go through this one
/// function; `tui::every_dialog_shares_one_frame_implementation` is what
/// checks a fifth cannot draw its own and drift from the rest, the way 19a
/// checks for stray `Color` literals.
///
/// Placement and colour live here; each dialog's content is a pure function
/// of the model in `blocks` — the same split `transcript`/`render` already
/// draws, and the reason the acceptance line "asserted on the rendered cells,
/// not by inspection" has something to test without a terminal.
///
/// `scroll` is rows, not cells — only the help overlay uses a nonzero value,
/// since it alone can exceed the modal's height.
fn render_modal(
    frame: &mut Frame,
    title: &str,
    lines: Vec<Line<'static>>,
    theme: Theme,
    scroll: u16,
) {
    let area = frame.area();
    let width = modal_width(area.width);
    let height = u16::try_from(lines.len() + 2)
        .unwrap_or(u16::MAX)
        .min(area.height.saturating_sub(2));
    let modal_area = Rect {
        x: area.x + (area.width.saturating_sub(width)) / 2,
        y: area.y + (area.height.saturating_sub(height)) / 2,
        width,
        height,
    };

    frame.render_widget(Clear, modal_area);
    let surface = Style::default().bg(theme.raised()).fg(theme.body());
    let block = Block::bordered()
        .title(title.to_string())
        .border_style(Style::default().fg(theme.focus()).bg(theme.raised()))
        .style(surface);
    frame.render_widget(
        Paragraph::new(lines)
            .block(block)
            .style(surface)
            .scroll((scroll, 0)),
        modal_area,
    );
}

/// The ask dialog (#103): content from [`blocks::prompt_dialog`], framed by
/// [`render_modal`].
fn render_prompt_dialog(frame: &mut Frame, app: &App, theme: Theme) {
    if !app.dialog_open() {
        return;
    }
    let Some(prompt) = app.pending_prompt() else {
        return;
    };
    let inner_width = usize::from(modal_width(frame.area().width)).saturating_sub(4);
    let lines = blocks::prompt_dialog(
        prompt,
        app.prompt_selected(),
        app.prompt_queue_len(),
        app.input(),
        inner_width,
        theme,
    );
    render_modal(frame, " confirm ", lines, theme, 0);
}

/// The session switcher (#105): content from [`blocks::sessions_dialog`],
/// framed by [`render_modal`].
fn render_sessions_dialog(frame: &mut Frame, app: &App, theme: Theme) {
    if !app.sessions_open() {
        return;
    }
    let inner_width = usize::from(modal_width(frame.area().width)).saturating_sub(4);
    let entries = app.sessions_filtered();
    let lines = blocks::sessions_dialog(
        app.sessions_query(),
        &entries,
        app.sessions_selected(),
        app.session(),
        inner_width,
        theme,
    );
    render_modal(frame, " sessions ", lines, theme, 0);
}

/// The help overlay (#105): content from [`blocks::help_dialog`], framed by
/// [`render_modal`] — scrollable, since it is the one dialog whose content can
/// exceed the modal's height (#105's Scope).
fn render_help(frame: &mut Frame, app: &App, theme: Theme) {
    if !app.help_open() {
        return;
    }
    let area = frame.area();
    let inner_width = usize::from(modal_width(area.width)).saturating_sub(4);
    let lines = blocks::help_dialog(inner_width, theme);
    let modal_height = u16::try_from(lines.len() + 2)
        .unwrap_or(u16::MAX)
        .min(area.height.saturating_sub(2));
    let visible = usize::from(modal_height.saturating_sub(2));
    let max_scroll = lines.len().saturating_sub(visible);
    let offset = app.help_scroll().min(max_scroll);
    render_modal(
        frame,
        " help ",
        lines,
        theme,
        u16::try_from(offset).unwrap_or(0),
    );
}

/// The quit confirm (#105): content from [`blocks::quit_dialog`], framed by
/// [`render_modal`].
fn render_quit_confirm(frame: &mut Frame, app: &App, theme: Theme) {
    let Some((mid_turn, turn_lost, escalate, quit_selected)) = app.quit_confirm() else {
        return;
    };
    let inner_width = usize::from(modal_width(frame.area().width)).saturating_sub(4);
    let lines = blocks::quit_dialog(
        app.session(),
        mid_turn,
        turn_lost,
        escalate,
        quit_selected,
        inner_width,
        theme,
    );
    render_modal(frame, " quit? ", lines, theme, 0);
}

/// Draw the `/` menu over the transcript, just above the composer.
///
/// Its own function because `render` was past clippy's line limit, and because
/// this is a self-contained overlay: it reads the menu and draws it, and knows
/// nothing about the transcript it floats over.
/// Draw `rows` in a bordered overlay just above the composer.
fn render_overlay(
    frame: &mut Frame,
    title: &'static str,
    rows: Vec<Line<'static>>,
    input_area: Rect,
) {
    let height = u16::try_from(rows.len()).unwrap_or(1) + 2;
    let area = Rect {
        x: input_area.x,
        y: input_area.y.saturating_sub(height),
        width: input_area.width,
        height: height.min(input_area.y),
    };
    if area.height > 2 {
        frame.render_widget(Clear, area);
        frame.render_widget(
            Paragraph::new(rows).block(Block::bordered().title(title)),
            area,
        );
    }
}

/// The `@` completion list.
fn render_completion(frame: &mut Frame, app: &App, theme: Theme, input_area: Rect) {
    if !app.completion_open() {
        return;
    }
    let entries = app.completion_entries();
    let mut rows: Vec<Line<'static>> = if entries.is_empty() {
        vec![Line::from(Span::styled(
            " no path matches".to_string(),
            Style::default().fg(theme.muted()),
        ))]
    } else {
        entries
            .iter()
            .enumerate()
            .map(|(i, path)| {
                let selected = i == app.completion_selected();
                let style = Style::default().fg(if selected {
                    theme.focus()
                } else {
                    theme.body()
                });
                Line::from(vec![
                    Span::styled(
                        if selected {
                            format!("{} ", theme.glyph(Glyph::Collapsed))
                        } else {
                            "  ".to_string()
                        },
                        style,
                    ),
                    Span::styled(path.clone(), style),
                ])
            })
            .collect()
    };
    if app.completion_truncated() {
        // A glyph and words, not a colour: under `Mode::Mono` a tinted marker is
        // nothing, and a list that stops without saying so is #145 again.
        rows.push(Line::from(Span::styled(
            format!(
                " {} more, keep typing to narrow",
                theme.glyph(Glyph::MoreBelow)
            ),
            Style::default().fg(theme.warning()),
        )));
    }
    render_overlay(frame, "paths", rows, input_area);
}

fn render_menu(frame: &mut Frame, app: &App, theme: Theme, input_area: Rect) {
    // The menu floats over the transcript, immediately above the composer, so
    // the line being typed stays where the user is looking.
    if app.menu_query().is_none() {
        return;
    }
    {
        let entries = app.menu_entries();
        let rows: Vec<Line<'static>> = if entries.is_empty() {
            // An empty state rather than a closed menu: a list that vanishes as
            // you type reads as a dropped keystroke.
            vec![Line::from(Span::styled(
                " no command matches".to_string(),
                Style::default().fg(theme.muted()),
            ))]
        } else {
            entries
                .iter()
                .enumerate()
                .map(|(i, command)| {
                    let selected = i == app.menu_selected();
                    let name = Style::default().fg(if selected {
                        theme.focus()
                    } else {
                        theme.body()
                    });
                    let mut spans = vec![
                        // The highlight is a glyph as well as a colour, because
                        // under `Mode::Mono` the colour is nothing.
                        Span::styled(
                            if selected {
                                format!("{} ", theme.glyph(Glyph::Collapsed))
                            } else {
                                "  ".to_string()
                            },
                            name,
                        ),
                        Span::styled(command.name.to_string(), name),
                        Span::styled(
                            format!("  {}", command.summary),
                            Style::default().fg(theme.muted()),
                        ),
                    ];
                    if let Availability::Pending(_) = command.availability {
                        spans.push(Span::styled(
                            "  (not yet)".to_string(),
                            Style::default().fg(theme.warning()),
                        ));
                    }
                    Line::from(spans)
                })
                .collect()
        };
        render_overlay(frame, "commands", rows, input_area);
    }
}

#[cfg(test)]
mod tests {
    use super::{edit_or_scroll, request_cancel, Pane};
    use jan_klod::app::{App, Entry as JanKlodEntry, Turn};
    use jan_klod::theme::{Depth, GlyphSet, Mode, Theme};
    use jan_klod::viewport::Viewport;
    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    fn press(code: KeyCode, modifiers: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, modifiers)
    }

    /// Drive one key and report whether the mapping consumed it.
    fn key(app: &mut App, code: KeyCode, modifiers: KeyModifiers) -> bool {
        let mut view = Viewport::default();
        edit_or_scroll(
            &press(code, modifiers),
            app,
            &mut view,
            Pane {
                total: 100,
                height: 10,
                composer_width: 40,
                transcript_width: 40,
                sidebar_hidden: false,
            },
            Theme::new(Mode::Dark, Depth::TrueColor, GlyphSet::Unicode),
        )
    }

    #[test]
    fn both_newline_spellings_insert_one_and_enter_does_not() {
        for modifier in [KeyModifiers::SHIFT, KeyModifiers::ALT] {
            let mut app = App::default();
            "ab".chars().for_each(|c| app.push_char(c));
            assert!(
                key(&mut app, KeyCode::Enter, modifier),
                "{modifier:?} unhandled"
            );
            assert_eq!(app.input(), "ab\n");
        }

        // Plain `Enter` must fall through: submitting is the event loop's job,
        // and consuming it here would make the composer impossible to send.
        let mut app = App::default();
        "ab".chars().for_each(|c| app.push_char(c));
        assert!(!key(&mut app, KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(app.input(), "ab", "Enter must not have edited the buffer");
    }

    /// The two halves of `Ctrl+U`, which #148 and #150 each own one of.
    #[test]
    fn ctrl_u_kills_a_line_with_content_and_is_left_alone_when_empty() {
        let mut app = App::default();
        "hello".chars().for_each(|c| app.push_char(c));
        assert!(key(&mut app, KeyCode::Char('u'), KeyModifiers::CONTROL));
        assert_eq!(app.input(), "", "the line was killed");

        // Empty: the composer does not consume it, so the transcript's
        // half-page scroll gets it. Consuming it here would make scrolling
        // impossible from an empty composer, which is the only state you can
        // scroll from.
        let mut empty = App::default();
        let mut view = Viewport::default().reflow(100, 10);
        let consumed = edit_or_scroll(
            &press(KeyCode::Char('u'), KeyModifiers::CONTROL),
            &mut empty,
            &mut view,
            Pane {
                total: 100,
                height: 10,
                composer_width: 40,
                transcript_width: 40,
                sidebar_hidden: false,
            },
            Theme::new(Mode::Dark, Depth::TrueColor, GlyphSet::Unicode),
        );
        assert!(consumed, "the scroll arm handles it");
        assert!(
            !view.attached(),
            "and it scrolled rather than killing a line"
        );
    }

    #[test]
    fn a_control_chord_is_never_typed_into_the_buffer() {
        let mut app = App::default();
        // `Ctrl+Z` has no binding. It must not arrive as a literal `z`, which is
        // what the catch-all did before the guard.
        assert!(!key(&mut app, KeyCode::Char('z'), KeyModifiers::CONTROL));
        assert_eq!(app.input(), "");
    }

    /// #151's Acceptance, driven through the mapping rather than the model.
    #[test]
    fn arrows_walk_history_only_when_the_caret_cannot_move() {
        let mut app = App::default();
        for message in ["first", "second"] {
            message.chars().for_each(|c| app.push_char(c));
            app.take_submission();
        }

        // Empty composer, caret on the only row: history.
        assert!(key(&mut app, KeyCode::Up, KeyModifiers::NONE));
        assert_eq!(app.input(), "second");
        assert!(key(&mut app, KeyCode::Up, KeyModifiers::NONE));
        assert_eq!(app.input(), "first", "a second press keeps walking back");

        // Past the oldest: stop rather than wrap to the newest.
        assert!(key(&mut app, KeyCode::Up, KeyModifiers::NONE));
        assert_eq!(app.input(), "first", "the oldest is a wall, not a loop");

        // Back down, and past the newest is the empty line again.
        key(&mut app, KeyCode::Down, KeyModifiers::NONE);
        assert_eq!(app.input(), "second");
        key(&mut app, KeyCode::Down, KeyModifiers::NONE);
        assert_eq!(app.input(), "", "past the newest is where you started");
    }

    #[test]
    fn editing_a_recalled_entry_moves_the_caret_and_leaves_history_alone() {
        let mut app = App::default();
        "original".chars().for_each(|c| app.push_char(c));
        app.take_submission();

        key(&mut app, KeyCode::Up, KeyModifiers::NONE);
        assert_eq!(app.input(), "original");

        // Typing makes it modified, so the arrows stop recalling.
        app.push_char('!');
        assert_eq!(app.input(), "original!");
        key(&mut app, KeyCode::Up, KeyModifiers::NONE);
        assert_eq!(
            app.input(),
            "original!",
            "a modified composer moves the caret instead of recalling"
        );

        // And the stored entry was never touched.
        app.composer.set("");
        key(&mut app, KeyCode::Up, KeyModifiers::NONE);
        assert_eq!(app.input(), "original", "history holds what was sent");
    }

    /// #152's Acceptance, driven through the mapping.
    #[test]
    fn the_menu_opens_only_at_column_zero_of_an_empty_composer() {
        let mut app = App::default();
        app.push_char('/');
        assert!(
            app.menu_query().is_some(),
            "a slash on an empty line opens it"
        );

        // Mid-word: not a command, so no menu.
        let mut mid = App::default();
        "ls /tmp".chars().for_each(|c| mid.push_char(c));
        assert!(
            mid.menu_query().is_none(),
            "a slash inside a message is a slash"
        );

        // A space ends it, because a command is the whole buffer.
        let mut spaced = App::default();
        "/new ".chars().for_each(|c| spaced.push_char(c));
        assert!(spaced.menu_query().is_none());

        // Deleting the slash closes it.
        let mut deleted = App::default();
        deleted.push_char('/');
        deleted.backspace();
        assert!(deleted.menu_query().is_none());
    }

    #[test]
    fn a_fragment_matching_nothing_shows_an_empty_state_rather_than_closing() {
        let mut app = App::default();
        "/zzz".chars().for_each(|c| app.push_char(c));
        assert!(
            app.menu_query().is_some(),
            "the menu stays open — vanishing reads as a dropped keystroke"
        );
        assert!(app.menu_entries().is_empty(), "and shows nothing");

        // And `Enter` on the empty state still submits rather than being eaten.
        assert!(!key(&mut app, KeyCode::Enter, KeyModifiers::NONE));
    }

    #[test]
    fn every_command_in_the_table_does_what_it_claims() {
        for command in jan_klod::commands::COMMANDS {
            let mut app = App::default();
            command.name.chars().for_each(|c| app.push_char(c));
            assert_eq!(
                app.menu_entries().first().map(|c| c.name),
                Some(command.name),
                "{} is a prefix of another command and did not sort first",
                command.name
            );
            assert!(key(&mut app, KeyCode::Enter, KeyModifiers::NONE));
            assert!(
                !app.input().starts_with('/'),
                "{} left its own name in the composer",
                command.name
            );

            match command.availability {
                jan_klod::commands::Availability::Ready => match command.name {
                    "/newline" => assert_eq!(app.input(), "\n"),
                    "/quit" => assert!(app.should_quit),
                    // No turn is running in this loop's fresh `App`, so
                    // `/cancel` has nothing to stop and says so rather than
                    // raising an ask nobody will send. The case where it *does*
                    // act is `a_menu_cancel_raises_the_same_ask_ctrl_c_does`.
                    "/cancel" => {
                        assert!(
                            !app.take_cancel_request(),
                            "/cancel queued a cancel with no turn running"
                        );
                        assert!(
                            format!("{:?}", app.transcript).contains("no turn is running"),
                            "/cancel did nothing and said nothing: {:?}",
                            app.transcript
                        );
                    }
                    // #105: each of these raises the ask its own drain acts
                    // on, or (help) needs no drain at all.
                    "/new" => assert!(
                        app.take_new_session_request(),
                        "/new did not ask for a fresh session/create"
                    ),
                    "/sessions" => assert!(
                        app.take_sessions_request(),
                        "/sessions did not ask for a fresh session/list"
                    ),
                    "/help" => assert!(app.help_open(), "/help did not open the overlay"),
                    other => panic!("{other} is Ready and untested — add it here"),
                },
                jan_klod::commands::Availability::Pending(reason) => match app.transcript.last() {
                    Some(jan_klod::app::Entry::Message { text, .. }) => assert!(
                        text.contains(reason),
                        "{} said {:?} rather than its reason",
                        command.name,
                        text
                    ),
                    other => panic!("{} recorded {other:?}", command.name),
                },
            }
        }
    }

    #[test]
    fn arrows_move_the_highlight_while_the_menu_is_open() {
        let mut app = App::default();
        app.push_char('/');
        assert_eq!(app.menu_selected(), 0);
        key(&mut app, KeyCode::Down, KeyModifiers::NONE);
        assert_eq!(app.menu_selected(), 1, "and not history");
        key(&mut app, KeyCode::Up, KeyModifiers::NONE);
        key(&mut app, KeyCode::Up, KeyModifiers::NONE);
        assert_eq!(
            app.menu_selected(),
            jan_klod::commands::COMMANDS.len() - 1,
            "a six-item list wraps, unlike history"
        );

        // Esc closes and leaves the text alone.
        assert!(key(&mut app, KeyCode::Esc, KeyModifiers::NONE));
        assert!(app.menu_query().is_none());
        assert_eq!(app.input(), "/", "dismissing is not deleting");
        assert!(
            !app.should_quit,
            "Esc closed the menu rather than the client"
        );
    }

    /// A temp tree with one ignored path, for the completion tests.
    fn completion_tree() -> std::path::PathBuf {
        let root = std::env::temp_dir().join(format!(
            "jk-complete-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        std::fs::create_dir_all(root.join("src")).expect("tree");
        std::fs::write(root.join(".gitignore"), "ignored.txt\n").expect("gitignore");
        std::fs::write(root.join("src/main.rs"), "").expect("file");
        std::fs::write(root.join("ignored.txt"), "").expect("file");
        root
    }

    /// Type `text`, refreshing the completion against `root` as a real run does.
    fn type_into(app: &mut App, root: &std::path::Path, text: &str) {
        for c in text.chars() {
            app.push_char(c);
            app.refresh_completion(root);
        }
    }

    #[test]
    fn at_completion_offers_paths_and_accepting_reads_nothing() {
        let root = completion_tree();
        let mut app = App::default();
        type_into(&mut app, &root, "look at @main");

        assert!(app.completion_open(), "`@` mid-message opens the list");
        assert_eq!(app.completion_entries(), ["src/main.rs".to_string()]);
        assert!(
            !app.completion_entries()
                .iter()
                .any(|e| e.contains("ignored")),
            "an ignored path was offered"
        );

        assert!(key(&mut app, KeyCode::Tab, KeyModifiers::NONE));
        assert_eq!(
            app.input(),
            "look at src/main.rs",
            "the fragment was replaced by the path, mid-message"
        );
        assert!(!app.completion_open(), "accepting closes the list");
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn a_space_ends_the_reference_and_esc_leaves_the_text() {
        let root = completion_tree();

        let mut spaced = App::default();
        type_into(&mut spaced, &root, "@src ");
        assert!(!spaced.completion_open(), "a space ended the reference");

        let mut dismissed = App::default();
        type_into(&mut dismissed, &root, "@src");
        assert!(dismissed.completion_open());
        assert!(key(&mut dismissed, KeyCode::Esc, KeyModifiers::NONE));
        assert!(!dismissed.completion_open());
        assert_eq!(dismissed.input(), "@src", "dismissing is not deleting");
        assert!(
            !dismissed.should_quit,
            "Esc closed the list, not the client"
        );
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn a_fragment_matching_no_path_keeps_the_list_open_and_enter_still_submits() {
        let root = completion_tree();
        let mut app = App::default();
        type_into(&mut app, &root, "@zzzz");
        assert!(app.completion_open(), "the list stays open on no match");
        assert!(app.completion_entries().is_empty());
        assert!(
            !key(&mut app, KeyCode::Enter, KeyModifiers::NONE),
            "Enter falls through to submit rather than being swallowed"
        );
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn word_motion_and_deletion_reach_the_composer() {
        let mut app = App::default();
        "alpha beta".chars().for_each(|c| app.push_char(c));
        assert!(key(&mut app, KeyCode::Char('w'), KeyModifiers::CONTROL));
        assert_eq!(app.input(), "alpha ");
        assert!(key(&mut app, KeyCode::Char('a'), KeyModifiers::CONTROL));
        assert_eq!(app.composer.caret(), 0, "Ctrl+A went to the line start");
    }

    /// Acceptance: `Ctrl+O` toggles the block under the cursor, and toggling
    /// with nothing under it is a no-op rather than a panic.
    #[test]
    fn ctrl_o_toggles_the_selected_block_and_never_types_an_o() {
        let mut app = App::default();
        app.record_tool_invoked("c1".into(), "read".into(), None);

        // Nothing selected: consumed, nothing opened, and — the part that
        // would actually be noticed — no `o` in the message.
        assert!(key(&mut app, KeyCode::Char('o'), KeyModifiers::CONTROL));
        assert_eq!(app.input(), "", "the chord was typed into the composer");
        let JanKlodEntry::Tool(block) = &app.transcript[0] else {
            panic!("not a tool block")
        };
        assert!(!block.expanded, "a block with no cursor on it opened");

        assert!(key(&mut app, KeyCode::Up, KeyModifiers::ALT));
        assert_eq!(app.cursor(), Some(0));
        assert!(key(&mut app, KeyCode::Char('o'), KeyModifiers::CONTROL));
        let JanKlodEntry::Tool(block) = &app.transcript[0] else {
            panic!("not a tool block")
        };
        assert!(block.expanded, "`Ctrl+O` did not open the selected block");

        assert!(key(&mut app, KeyCode::Down, KeyModifiers::ALT));
        assert_eq!(app.cursor(), None, "`Alt+Down` puts the selection away");
    }

    /// The cursor keys are `Alt`-qualified so the composer keeps the bare
    /// arrows — history and the caret are what a user reaches for far more
    /// often than a selection.
    #[test]
    fn the_bare_arrows_still_belong_to_the_composer() {
        let mut app = App::default();
        app.record_tool_invoked("c1".into(), "read".into(), None);
        key(&mut app, KeyCode::Up, KeyModifiers::NONE);
        key(&mut app, KeyCode::Down, KeyModifiers::NONE);
        assert_eq!(
            app.cursor(),
            None,
            "a bare arrow moved the transcript cursor"
        );
    }

    /// `Ctrl+C` now raises a real cancel, where it used to only apologise.
    ///
    /// This test asserted the status line contained `#157` — the issue number
    /// for "the client cannot send `turn/cancel`". #157 is built, so that
    /// assertion would be pinning an apology for a limitation that no longer
    /// exists. What is asserted instead is the behaviour the apology stood in
    /// for: the ask is raised exactly once, and the partial answer survives it.
    ///
    /// The *sending* is the event loop's, not this function's, so it is not
    /// asserted here — `enter::a_cancel_is_asked_for_once_however_it_was_asked`
    /// covers the drain, which is the part that could send twice.
    #[test]
    fn ctrl_c_asks_once_and_keeps_the_partial_answer() {
        let mut app = App::default();

        assert!(!request_cancel(&mut app), "there is no turn to stop");
        assert!(
            app.transcript.is_empty(),
            "a key with nothing to cancel must not narrate"
        );
        assert!(
            !app.take_cancel_request(),
            "a cancel was queued for a turn that was not running"
        );

        app.begin_turn();
        app.apply_delta("half an answer");
        assert!(request_cancel(&mut app), "the first ask acts");
        assert_eq!(app.turn(), Turn::Cancelling);

        // The partial answer stays: cancelling finalizes with what is in hand,
        // and erasing it would misreport what the core did before it stopped.
        assert!(
            format!("{:?}", app.transcript).contains("half an answer"),
            "cancelling erased the answer that had already arrived"
        );

        assert!(!request_cancel(&mut app), "the second ask does not act");
        assert!(
            app.take_cancel_request(),
            "the ask was raised and then lost"
        );
        assert!(
            !app.take_cancel_request(),
            "draining twice would send a second cancel to a core already stopping"
        );
    }
}

#[cfg(test)]
mod signals {
    use super::{caret_glyph, composer_label, spinner_frame, status_hint};
    use jan_klod::app::{App, Prompt, Turn};
    use jan_klod::theme::{Depth, GlyphSet, Mode, Theme};

    fn streaming() -> App {
        let mut app = App::default();
        app.begin_turn();
        app
    }

    /// #160's first Acceptance line: **all three** differ between `Idle` and
    /// `Streaming`.
    ///
    /// All three, not any one, because this is the only warning a user gets
    /// that `Enter` has stopped starting a turn and started steering one. A
    /// single signal is a single thing to miss, and the consequence of missing
    /// it is a message sent into a turn that is still running.
    #[test]
    fn the_caret_the_label_and_the_hint_all_change_when_a_turn_starts() {
        let idle = App::default();
        let busy = streaming();
        assert_eq!(idle.turn(), Turn::Idle);
        assert_eq!(busy.turn(), Turn::Streaming);

        assert_ne!(
            caret_glyph(idle.turn()),
            caret_glyph(busy.turn()),
            "the caret is the same in both states"
        );
        assert_ne!(
            composer_label(&idle),
            composer_label(&busy),
            "the composer claims to be the same field in both states"
        );
        assert_ne!(
            status_hint(&idle, false),
            status_hint(&busy, false),
            "the hint names the same keys in both states"
        );
    }

    /// #104 Acceptance 3: the status bar is **produced from** the keymap
    /// table, so a hand-written duplicate fails.
    ///
    /// Hands `status_hint_from` a table whose wording exists nowhere in the
    /// source and asserts the rendered hint carries it. A duplicate that
    /// reimplemented the match on `App::turn` would return the real wording and
    /// fail this outright.
    ///
    /// This test exists because a probe showed the keymap's own tests did not
    /// catch it: they prove `keymap::hint` reads its argument, which says
    /// nothing about whether the status bar calls it.
    #[test]
    fn the_status_bar_renders_from_the_keymap_table_and_not_a_duplicate() {
        use jan_klod::keymap::{Binding, Context};

        const DOCTORED: &[Binding] = &[
            Binding {
                keys: "Enter",
                action: "ZZQQ-idle",
                context: Context::Idle,
            },
            Binding {
                keys: "Enter",
                action: "ZZQQ-steer",
                context: Context::Streaming,
            },
        ];

        let idle = App::default();
        let hint = super::status_hint_from(DOCTORED, &idle, false);
        assert!(
            hint.contains("ZZQQ-idle"),
            "the idle hint ignored the table it was given: {hint:?}"
        );

        let busy = streaming();
        let hint = super::status_hint_from(DOCTORED, &busy, false);
        assert!(
            hint.contains("ZZQQ-steer"),
            "the streaming hint ignored the table it was given: {hint:?}"
        );
    }

    /// #160's third Acceptance line. `Mode::Mono` resolves every role to
    /// `Color::Reset`, so any of the three that were only a tint would be
    /// nothing at all — which is exactly the failure the trio exists to avoid.
    #[test]
    fn under_monochrome_the_three_still_differ() {
        let idle = App::default();
        let busy = streaming();
        for set in [GlyphSet::Unicode, GlyphSet::Ascii] {
            let theme = Theme::new(Mode::Mono, Depth::TrueColor, set);
            assert_ne!(
                theme.glyph(caret_glyph(idle.turn())),
                theme.glyph(caret_glyph(busy.turn())),
                "{set:?}: the two carets render alike with colour gone"
            );
        }
        // The other two are text, so they carry in mono by construction — but
        // asserting it keeps the trio honest if either ever becomes a style.
        assert_ne!(composer_label(&idle), composer_label(&busy));
        assert_ne!(status_hint(&idle, false), status_hint(&busy, false));
    }

    /// #160's second Acceptance line: the frame advances on a **poll tick**,
    /// with no delta received.
    ///
    /// `text-delta` is one per completion rather than one per token, so a
    /// spinner tied to arrivals would freeze through the long wait it exists to
    /// reassure someone about. Driving it from `tick` is what makes "the model
    /// is thinking" look different from "the client has hung", and this asserts
    /// the difference without a terminal or a turn.
    #[test]
    fn the_spinner_advances_on_a_tick_with_no_delta() {
        let theme = Theme::new(Mode::Dark, Depth::TrueColor, GlyphSet::Unicode);
        let busy = streaming();

        let first = spinner_frame(theme, busy.turn(), 0).expect("a running turn spins");
        let second = spinner_frame(theme, busy.turn(), 1).expect("a running turn spins");
        assert_ne!(
            first, second,
            "the frame did not move between two poll ticks, so a thinking \
             model is indistinguishable from a hung client"
        );

        // And it stops when there is nothing to wait for.
        assert!(
            spinner_frame(theme, App::default().turn(), 3).is_none(),
            "an idle client spins, which says a turn is running when none is"
        );
    }

    /// A pending confirmation is the third meaning of `Enter`, and it says so
    /// in words rather than by reusing the steering caret (#159's table).
    #[test]
    fn a_pending_prompt_names_the_answer_enter_will_send() {
        let mut app = streaming();
        app.ask(Prompt {
            session: "s1".to_string(),
            question: "write src/main.rs?".to_string(),
            options: vec!["yes".to_string(), "no".to_string(), "always".to_string()],
            default: "no".to_string(),
        });
        assert_eq!(app.turn(), Turn::Blocked);
        assert!(composer_label(&app).contains("yes/no/always"));
        assert!(
            status_hint(&app, false).contains("`no`"),
            "the hint must name the default, because that is what silence sends"
        );
        assert_eq!(
            caret_glyph(app.turn()),
            caret_glyph(Turn::Idle),
            "blocked reuses the steering caret, giving one mark two meanings"
        );
    }
}

#[cfg(test)]
mod enter {
    use super::{edit_or_scroll, enter_means, steer, Enter, Pane};
    use jan_klod::app::{App, Prompt, Turn};
    use jan_klod::transport::{Rest, Transport};
    use jan_klod::viewport::Viewport;
    use jan_klod::StreamEvent;
    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use std::sync::Mutex;

    /// Records what the client asked of a transport, and nothing else.
    ///
    /// The point of #159's first Acceptance line is *which command was sent*,
    /// not what came back — a test that asserted a reply would be testing the
    /// fake. `tests/roundtrip.rs` stands up a canned server where the wire
    /// matters; here it does not.
    #[derive(Default)]
    pub struct Recorder {
        pub sent: Mutex<Vec<String>>,
    }

    impl Transport for Recorder {
        fn stream_turn(
            &self,
            session: &str,
            message: &str,
            _on_event: &mut dyn FnMut(StreamEvent),
        ) -> Result<(), String> {
            self.sent
                .lock()
                .expect("lock")
                .push(format!("session/message {session} {message}"));
            Ok(())
        }
        fn answer(&self, session: &str, answer: &str) -> Result<(), String> {
            self.sent
                .lock()
                .expect("lock")
                .push(format!("turn/answer {session} {answer}"));
            Ok(())
        }
        fn follow_up(&self, session: &str, message: &str) -> Result<(), String> {
            self.sent
                .lock()
                .expect("lock")
                .push(format!("turn/follow-up {session} {message}"));
            Ok(())
        }
        fn create_session(&self) -> Result<String, String> {
            self.sent
                .lock()
                .expect("lock")
                .push("session/create".to_string());
            Ok("new".to_string())
        }
        fn session_list(&self) -> Result<Vec<jan_klod::transport::SessionSummary>, String> {
            self.sent
                .lock()
                .expect("lock")
                .push("session/list".to_string());
            Ok(vec![jan_klod::transport::SessionSummary {
                id: "s1".to_string(),
                preview: "hi".to_string(),
            }])
        }
        fn session_get(
            &self,
            session: &str,
        ) -> Result<jan_klod_protocol::SessionGetResult, String> {
            self.sent
                .lock()
                .expect("lock")
                .push(format!("session/get {session}"));
            Ok(jan_klod_protocol::SessionGetResult {
                id: session.to_string(),
                messages: Vec::new(),
            })
        }
        fn cancel(&self, session: &str) -> Result<(), String> {
            self.sent
                .lock()
                .expect("lock")
                .push(format!("turn/cancel {session}"));
            Ok(())
        }
        fn describe(&self) -> String {
            "a recorder".to_string()
        }
    }

    fn typed(app: &mut App, text: &str) {
        app.composer.insert(text);
    }

    fn prompt() -> Prompt {
        Prompt {
            session: "s1".to_string(),
            question: "write src/main.rs?".to_string(),
            options: vec!["yes".to_string(), "no".to_string()],
            default: "no".to_string(),
        }
    }

    /// #159's table, as a table. `Enter` has four meanings and the state is
    /// what picks one — five since #103 split `Blocked` on whether the dialog
    /// is open.
    #[test]
    fn the_state_decides_what_enter_means() {
        let idle = App::default();
        assert_eq!(idle.turn(), Turn::Idle);
        assert_eq!(enter_means(&idle), Enter::Send);

        let mut busy = App::default();
        busy.begin_turn();
        assert_eq!(busy.turn(), Turn::Streaming);
        assert_eq!(enter_means(&busy), Enter::Steer);

        // Blocked is checked before Streaming, and has to be: a blocked turn is
        // still streaming underneath, so the other order would steer instead of
        // answering — and the answer is what the turn is waiting for.
        let mut blocked = App::default();
        blocked.begin_turn();
        blocked.ask(prompt());
        assert_eq!(blocked.turn(), Turn::Blocked);
        assert!(blocked.dialog_open(), "`ask` opens the dialog");
        assert_eq!(
            enter_means(&blocked),
            Enter::Answer,
            "a pending confirmation must take Enter ahead of steering"
        );

        // #103: with the dialog closed, `Enter` must not answer blind.
        blocked.close_dialog();
        assert_eq!(blocked.turn(), Turn::Blocked, "still waiting");
        assert_eq!(
            enter_means(&blocked),
            Enter::ReopenPrompt,
            "a closed dialog must show the options again rather than answer"
        );

        let mut stopping = App::default();
        stopping.begin_turn();
        assert!(stopping.cancel());
        assert_eq!(stopping.turn(), Turn::Cancelling);
        assert_eq!(
            enter_means(&stopping),
            Enter::Nothing,
            "steering a turn that is being cancelled is not a thing to offer"
        );
    }

    /// #159 Acceptance 1: `Streaming` sends `turn/follow-up`, `Idle` does not.
    #[test]
    fn steering_sends_a_follow_up_and_not_a_new_message() {
        let recorder = Recorder::default();
        let mut app = App::default();
        app.begin_turn();
        typed(&mut app, "actually use serde");

        steer(&recorder, "s1", &mut app);

        let sent = recorder.sent.lock().expect("lock").clone();
        assert_eq!(
            sent,
            vec!["turn/follow-up s1 actually use serde".to_string()]
        );
        assert!(
            !sent.iter().any(|s| s.starts_with("session/message")),
            "steering started a second turn instead of steering the first"
        );
    }

    /// #159 Acceptance 2: `Blocked` answers the prompt and **does not steer**.
    ///
    /// Mostly an assertion about behaviour that already existed — nothing
    /// pinned the ordering before, which is exactly how it would have been
    /// reversed by someone tidying the match arms.
    #[test]
    fn a_blocked_turn_answers_and_does_not_steer() {
        let mut app = App::default();
        app.begin_turn();
        app.ask(prompt());
        app.prompt_jump(1); // "yes" is the first option, as displayed

        assert_eq!(enter_means(&app), Enter::Answer);

        let recorder = Recorder::default();
        let (session, answer) = app.take_answer().expect("a selection was made");
        recorder.answer(&session, &answer).expect("recorded");

        assert_eq!(
            recorder.sent.lock().expect("lock").clone(),
            vec!["turn/answer s1 yes".to_string()]
        );
        assert!(
            app.pending_prompt().is_none(),
            "answering left the prompt pending, so the next Enter answers it again"
        );
    }

    /// #103 Acceptance: the session sent is the notification's own, not the
    /// client's current session — the bug #103 opens on.
    #[test]
    fn the_answer_is_sent_with_the_prompts_own_session_not_the_clients() {
        let mut app = App::default();
        app.begin_turn();
        app.ask(Prompt {
            session: "the-turns-session".to_string(),
            ..prompt()
        });
        app.prompt_jump(2); // "no", the default

        let recorder = Recorder::default();
        let (session, answer) = app.take_answer().expect("the default was selected");
        recorder.answer(&session, &answer).expect("recorded");

        assert_eq!(
            recorder.sent.lock().expect("lock").clone(),
            vec!["turn/answer the-turns-session no".to_string()],
            "the client's own session, `s1` in this loop, must not appear here"
        );
    }

    /// #103 Acceptance: `Esc` closes the dialog without sending anything, and
    /// the prompt survives so it can be reopened.
    #[test]
    fn esc_closes_the_dialog_without_sending_turn_answer() {
        let mut app = App::default();
        app.begin_turn();
        app.ask(prompt());
        assert!(app.dialog_open());

        assert!(edit_or_scroll(
            &KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
            &mut app,
            &mut Viewport::default(),
            Pane::default(),
            jan_klod::theme::Theme::new(
                jan_klod::theme::Mode::Dark,
                jan_klod::theme::Depth::TrueColor,
                jan_klod::theme::GlyphSet::Unicode,
            ),
        ));
        assert!(!app.dialog_open(), "Esc must close the dialog");
        assert_eq!(
            app.turn(),
            Turn::Blocked,
            "the prompt survives — it can still be reopened"
        );
        assert_eq!(
            enter_means(&app),
            Enter::ReopenPrompt,
            "Enter must reopen rather than answer while the dialog is closed"
        );

        let recorder = Recorder::default();
        // Confirm nothing was ever sent — the dialog closing must not be
        // reachable from any transport call in this test.
        assert!(recorder.sent.lock().expect("lock").is_empty());
    }

    /// `/cancel` and `Ctrl+C` raise the **same** ask, and it is sent once.
    ///
    /// The two enter by different doors — one through `App::run_command` inside
    /// `edit_or_scroll`, the other through the key match in the event loop —
    /// and neither can send anything itself, because `App` holds no transport.
    /// They meet at the flag the loop drains, which is what makes "a second
    /// `Ctrl+C` does not send a second cancel" true of the menu as well without
    /// either path knowing about the other.
    #[test]
    fn a_menu_cancel_raises_the_same_ask_ctrl_c_does() {
        for opened_by_menu in [false, true] {
            let mut app = App::default();
            app.begin_turn();

            if opened_by_menu {
                // Through `push_char`, which is what opens the menu — a
                // direct composer insert would leave it closed and
                // `menu_accept` with nothing highlighted.
                "/cancel".chars().for_each(|c| app.push_char(c));
                assert!(app.menu_accept(), "the menu did not take the command");
            } else {
                assert!(app.cancel(), "Ctrl+C did not act");
            }

            assert_eq!(app.turn(), Turn::Cancelling, "menu={opened_by_menu}");
            assert!(
                app.take_cancel_request(),
                "menu={opened_by_menu}: nothing was queued for the loop to send"
            );
            assert!(
                !app.take_cancel_request(),
                "menu={opened_by_menu}: the ask survived its own drain"
            );
        }
    }

    /// #159 Acceptance 3: over REST, steering is refused **with a reason in the
    /// transcript** rather than silently.
    ///
    /// 13b recorded that the REST surface has no route reaching a turn in
    /// flight. That is a property of the connection, not a missing feature, and
    /// the difference between the two is the sentence a user gets to read.
    #[test]
    fn rest_refuses_to_steer_and_the_user_can_read_why() {
        let rest = Rest::new("127.0.0.1:1".to_string());
        let mut app = App::default();
        app.begin_turn();
        typed(&mut app, "actually use serde");

        steer(&rest, "s1", &mut app);

        let transcript = format!("{:?}", app.transcript);
        assert!(
            transcript.contains("REST"),
            "the refusal is not in the transcript, so the keystroke vanished: \
             {transcript}"
        );
        assert!(
            transcript.contains("stdio"),
            "the refusal does not say what to do instead: {transcript}"
        );
        assert!(
            transcript.contains("actually use serde"),
            "the refused message is not shown, so the transcript has the \
             refusal without the thing refused"
        );
    }
}

#[cfg(test)]
mod dialogs {
    //! #105's three dialogs: the wiring between `App`'s intents and
    //! `Transport`, and the one-frame-implementation invariant.

    use jan_klod::app::{App, Entry, SessionEntry, SessionMessage};
    use jan_klod::theme::{Depth, GlyphSet, Mode, Theme};
    use jan_klod::transport::{SessionSummary, Transport};
    use jan_klod::viewport::Viewport;
    use jan_klod_protocol::{SessionGetResult, TranscriptMessage};
    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use std::sync::Mutex;

    /// `?` on an empty composer opens the help overlay, through
    /// `edit_or_scroll` rather than the model directly — so a probe that
    /// guarded the shortcut wrongly (behind the wrong modifier, say) fails
    /// here rather than only in `App`'s own tests.
    #[test]
    fn a_bare_question_mark_on_an_empty_composer_opens_help() {
        let mut app = App::default();
        let mut view = Viewport::default();
        let theme = Theme::new(Mode::Dark, Depth::TrueColor, GlyphSet::Unicode);
        assert!(super::edit_or_scroll(
            &KeyEvent::new(KeyCode::Char('?'), KeyModifiers::NONE),
            &mut app,
            &mut view,
            super::Pane::default(),
            theme,
        ));
        assert!(app.help_open());
        assert_eq!(app.input(), "", "the `?` must not have been typed");

        // Typed mid-sentence, it is punctuation.
        let mut typing = App::default();
        "is this on".chars().for_each(|c| typing.push_char(c));
        super::edit_or_scroll(
            &KeyEvent::new(KeyCode::Char('?'), KeyModifiers::NONE),
            &mut typing,
            &mut view,
            super::Pane::default(),
            theme,
        );
        assert!(!typing.help_open());
        assert_eq!(typing.input(), "is this on?");
    }

    /// `Ctrl+B` is pointless at a width with a permanent sidebar pane, and
    /// only opens the dialog where `pane.sidebar_hidden` says the last frame
    /// had none (#105's "sidebar-in-a-dialog").
    #[test]
    fn ctrl_b_only_opens_the_sidebar_dialog_where_the_pane_is_hidden() {
        let mut wide = App::default();
        let mut view = Viewport::default();
        let theme = Theme::new(Mode::Dark, Depth::TrueColor, GlyphSet::Unicode);
        let wide_pane = super::Pane {
            total: 100,
            height: 10,
            composer_width: 40,
            transcript_width: 40,
            sidebar_hidden: false,
        };
        super::edit_or_scroll(
            &KeyEvent::new(KeyCode::Char('b'), KeyModifiers::CONTROL),
            &mut wide,
            &mut view,
            wide_pane,
            theme,
        );
        assert!(
            !wide.sidebar_dialog_open(),
            "a permanent sidebar pane makes the key pointless"
        );

        let mut narrow = App::default();
        let narrow_pane = super::Pane {
            sidebar_hidden: true,
            ..wide_pane
        };
        super::edit_or_scroll(
            &KeyEvent::new(KeyCode::Char('b'), KeyModifiers::CONTROL),
            &mut narrow,
            &mut view,
            narrow_pane,
            theme,
        );
        assert!(narrow.sidebar_dialog_open());
    }

    /// A fake with a real `session/list`/`session/get`, counted — the two
    /// calls the switcher's Acceptance line names.
    #[derive(Default)]
    struct Fake {
        list_calls: Mutex<usize>,
        get_calls: Mutex<usize>,
    }

    impl Transport for Fake {
        fn create_session(&self) -> Result<String, String> {
            Ok("new".to_string())
        }
        fn session_list(&self) -> Result<Vec<SessionSummary>, String> {
            *self.list_calls.lock().expect("lock") += 1;
            Ok(vec![
                SessionSummary {
                    id: "cli".to_string(),
                    preview: "hi".to_string(),
                },
                SessionSummary {
                    id: "other".to_string(),
                    preview: "bye".to_string(),
                },
            ])
        }
        fn session_get(&self, session: &str) -> Result<SessionGetResult, String> {
            *self.get_calls.lock().expect("lock") += 1;
            Ok(SessionGetResult {
                id: session.to_string(),
                messages: vec![
                    TranscriptMessage {
                        seq: 1,
                        role: "user".to_string(),
                        content: "hello".to_string(),
                        tool_call_id: None,
                    },
                    TranscriptMessage {
                        seq: 2,
                        role: "tool".to_string(),
                        content: "file contents".to_string(),
                        tool_call_id: Some("call-1".to_string()),
                    },
                    TranscriptMessage {
                        seq: 3,
                        role: "assistant".to_string(),
                        content: "done".to_string(),
                        tool_call_id: None,
                    },
                ],
            })
        }
        fn cancel(&self, _session: &str) -> Result<(), String> {
            Ok(())
        }
        fn stream_turn(
            &self,
            _session: &str,
            _message: &str,
            _on_event: &mut dyn FnMut(jan_klod::StreamEvent),
        ) -> Result<(), String> {
            Ok(())
        }
        fn answer(&self, _session: &str, _answer: &str) -> Result<(), String> {
            Ok(())
        }
        fn follow_up(&self, _session: &str, _message: &str) -> Result<(), String> {
            Ok(())
        }
        fn describe(&self) -> String {
            "fake".to_string()
        }
    }

    /// Acceptance: opening the switcher issues exactly one `session/list`;
    /// selecting an entry issues exactly one `session/get`, and the rebuilt
    /// transcript matches what it returned — including a `tool` message
    /// resolved onto its call via `tool-call-id`.
    #[test]
    fn the_switcher_issues_one_list_and_one_get_and_rebuilds_the_transcript() {
        let fake = Fake::default();
        let mut app = App::default();
        app.set_session("cli".to_string());

        app.request_sessions();
        assert!(app.take_sessions_request());
        let sessions = fake.session_list().expect("the fake answers");
        app.open_sessions(
            sessions
                .into_iter()
                .map(|s| SessionEntry {
                    id: s.id,
                    preview: s.preview,
                })
                .collect(),
        );
        assert_eq!(*fake.list_calls.lock().expect("lock"), 1, "opened twice");
        assert!(app.sessions_open());

        app.sessions_move(1); // "other"
        let id = app
            .sessions_confirm()
            .expect("idle, so the switch is accepted");
        let result = fake.session_get(&id).expect("the fake answers");
        let messages: Vec<SessionMessage> = result
            .messages
            .into_iter()
            .map(|m| SessionMessage {
                role: m.role,
                content: m.content,
                tool_call_id: m.tool_call_id,
            })
            .collect();
        app.load_session(result.id, messages);

        assert_eq!(*fake.get_calls.lock().expect("lock"), 1, "fetched twice");
        assert_eq!(app.session(), "other");
        assert!(
            !app.sessions_open(),
            "loading a session closes the switcher"
        );
        assert_eq!(app.transcript.len(), 3);
        assert_eq!(
            app.transcript[0],
            Entry::Message {
                who: jan_klod::app::Who::You,
                text: "hello".to_string()
            }
        );
        match &app.transcript[1] {
            Entry::Tool(block) => assert_eq!(
                block.id, "call-1",
                "the tool message must resolve onto its call by tool-call-id"
            ),
            other @ Entry::Message { .. } => panic!("expected a tool block, got {other:?}"),
        }
        assert_eq!(
            app.transcript[2],
            Entry::Message {
                who: jan_klod::app::Who::Klod,
                text: "done".to_string()
            }
        );
    }

    /// #105's Acceptance: one dialog frame implementation, checked the way
    /// 19a checks for stray `Color` literals — a grep, not a type-level proof,
    /// because the thing being guarded against is a *second copy* of code
    /// that would otherwise compile just as well as the first.
    #[test]
    fn every_dialog_shares_one_frame_implementation() {
        let source = include_str!("tui.rs");
        // The needle is split across two literals so this test's own source
        // does not match itself — `include_str!` pulls in this very function,
        // and a whole literal here would count its own assertion as a second
        // definition.
        let definition = format!("fn {}render_modal(", "");
        assert_eq!(
            source.matches(&definition).count(),
            1,
            "more than one function draws a dialog's frame"
        );
        let call_sites = source.matches("render_modal(frame,").count();
        assert!(
            call_sites >= 4,
            "expected at least 4 dialogs (ask, sessions, help, quit) to call \
             the one modal primitive, found {call_sites}"
        );
    }
}
