//! Terminal glue over [`app`](jk_tui::app) and [`Transport`]; not unit-tested.
//! `Transport` is `Arc<dyn Transport>` because turn and answer threads share one stdio pipe.

use std::sync::mpsc;
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use jk_tui::app::{App, Prompt, SessionEntry, SessionMessage, ToastKind, Turn};
use jk_tui::blocks;
use jk_tui::commands::Availability;
use jk_tui::keymap::{self, Context};
use jk_tui::layout;
use jk_tui::sidebar::{self, SessionInfo};
use jk_tui::theme::{Glyph, Theme};
use jk_tui::transport::Transport;
use jk_tui::viewport::Viewport;
use jk_tui::StreamEvent;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::layout::Rect;
use ratatui::layout::{Constraint, Layout};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, Paragraph};
use ratatui::{DefaultTerminal, Frame};

const POLL_MS: u64 = 50;

/// Run the TUI against `transport`, restoring the terminal on exit. The theme
/// is resolved once, not per frame, so rendering does not depend on re-read state.
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
    app.set_session(session.to_string());
    take_connect_facts(&mut app, transport);
    let mut view = Viewport::default();
    let mut pane = Pane::default();
    let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
    let described = transport.describe();
    let meta = Meta {
        described: &described,
        cwd: &cwd,
    };
    app.record_status(format!(
        "connected to {described} (session `{session}`); Esc to quit"
    ));

    let mut rx: Option<mpsc::Receiver<Result<StreamEvent, String>>> = None;
    let mut tick: usize = 0;

    while !app.should_quit {
        if let Some(receiver) = &rx {
            loop {
                match apply_event(&mut app, receiver, tick) {
                    Step::Applied => {}
                    Step::Drained => break,
                    Step::Ended => {
                        rx = None;
                        app.end_turn();
                        break;
                    }
                }
            }
        }

        drain_transport_requests(&mut app, transport);

        terminal.draw(|frame| render(frame, &app, theme, &mut view, &mut pane, tick, &meta))?;

        if !event::poll(Duration::from_millis(POLL_MS))? {
            tick = tick.wrapping_add(1);
            app.tick(tick);
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
        if edit_or_scroll(&key, &mut app, &mut view, pane, theme) {
            app.refresh_completion(&cwd);
            continue;
        }
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Esc => app.quit(),
            KeyCode::Char('d') if ctrl => app.request_quit(),
            KeyCode::Char('c') if ctrl => {
                if app.turn() == Turn::Idle && app.input().is_empty() {
                    app.request_quit();
                } else {
                    request_cancel(&mut app);
                }
            }
            KeyCode::Char('s') if ctrl => app.request_sessions(),
            KeyCode::Enter if app.sessions_open() => switch_session(&mut app, transport),
            KeyCode::Enter => match enter_means(&app) {
                Enter::Answer => {
                    if let Some((prompt_session, answer)) = app.take_answer() {
                        let transport = Arc::clone(transport);
                        thread::spawn(move || {
                            let _ = transport.answer(&prompt_session, &answer);
                        });
                    }
                }
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

/// Connect-time facts, read once: the transport kind (it sets the quit
/// confirm's wording) and the contributed commands.
fn take_connect_facts(app: &mut App, transport: &Arc<dyn Transport>) {
    app.set_stdio(transport.is_stdio());
    app.set_contributions(&transport.contributions());
}

/// Executes the asks the model raised since the last frame. `session/get` is
/// not here: the switcher answers it from a keystroke.
fn drain_transport_requests(app: &mut App, transport: &Arc<dyn Transport>) {
    if app.take_cancel_request() {
        match transport.cancel(app.session()) {
            Ok(()) => app.record_status("cancelling — the answer so far is kept"),
            Err(err) => app.record_status(format!("cancel could not be sent: {err}")),
        }
    }

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

    if let Some((extension, name)) = app.take_invoke_request() {
        match transport.invoke_contribution(&extension, &name) {
            Ok(text) if text.is_empty() => app.record_status(format!("{name} ran")),
            Ok(text) => app.record_status(text),
            Err(err) => app.record_error(format!("{name}: {err}")),
        }
        app.set_contributions(&transport.contributions());
    }

    if app.take_new_session_request() {
        match transport.create_session() {
            Ok(id) => app.load_session(id, Vec::new()),
            Err(err) => app.record_error(format!("session/create: {err}")),
        }
    }
}

/// Switcher's `Enter`. `session_get` is synchronous because nothing reads the
/// pipe during a switch, which only starts when `Idle`.
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

/// What `Enter` does, picked from the turn state. A pure function, so the table is testable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Enter {
    /// Answer the pending confirmation (not a steer).
    Answer,
    /// Pending prompt whose dialog is closed: reopen it, never answer blind.
    ReopenPrompt,
    /// Steer the turn already running — `turn/follow-up`.
    Steer,
    /// Start a turn — `session/message`.
    Send,
    /// Turn is cancelling: nothing to do.
    Nothing,
}

/// Maps turn state to `Enter`'s meaning. `Blocked` is checked first because a
/// blocked turn is still streaming underneath. Reads `App::turn()`, not the loop's `rx`.
fn enter_means(app: &App) -> Enter {
    match app.turn() {
        Turn::Blocked if app.dialog_open() => Enter::Answer,
        Turn::Blocked => Enter::ReopenPrompt,
        Turn::Streaming => Enter::Steer,
        Turn::Idle => Enter::Send,
        Turn::Cancelling => Enter::Nothing,
    }
}

/// Sends a follow-up to the running turn and records any refusal, so it is not
/// silently dropped. Synchronous on purpose: a thread would lose the refusal,
/// which over REST is the only feedback.
fn steer(transport: &dyn Transport, session: &str, app: &mut App) {
    let Some(message) = app.take_submission() else {
        return;
    };
    if let Err(refusal) = transport.follow_up(session, &message) {
        app.record_status(refusal);
    }
}

/// Asks the running turn to stop. Returns false if nothing runs or it was
/// already asked. The partial answer is kept. Sending happens in the loop's
/// drain, so the menu and Ctrl+C share one path.
fn request_cancel(app: &mut App) -> bool {
    app.cancel()
}

/// Last frame's dimensions for scroll keys.
#[derive(Debug, Default, Clone, Copy)]
struct Pane {
    total: usize,
    height: usize,
    /// Composer width in cells, for visual-row motion.
    composer_width: usize,
    /// Transcript width in cells; differs from the composer, whose caret takes two more.
    transcript_width: usize,
    /// True when there is no permanent sidebar pane, so `Ctrl+B` opens it as a dialog.
    sidebar_hidden: bool,
}

/// Keys moving the caret or viewport. Returns true if consumed. Kept out of the
/// loop, which owns threads and the turn.
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
    if dialog_priority_keys(key, app, pane) {
        return true;
    }
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
            KeyCode::Enter | KeyCode::Tab if app.menu_accept() => return true,
            _ => {}
        }
    }
    match key.code {
        KeyCode::Char('o') if ctrl => {
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
        KeyCode::PageUp => *view = view.page_up(pane.height),
        KeyCode::PageDown => *view = view.page_down(pane.total, pane.height),
        KeyCode::Char('u') if ctrl && app.composer.is_empty() => *view = view.half_up(pane.height),
        KeyCode::Char('d') if ctrl && app.composer.is_empty() => {
            *view = view.half_down(pane.total, pane.height);
        }
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
        KeyCode::Char('u') if ctrl => app.composer.kill_line(),
        KeyCode::Enter if shift || alt => app.composer.push('\n'),
        KeyCode::Up => app.history_up(pane.composer_width),
        KeyCode::Down => app.history_down(pane.composer_width),
        KeyCode::Backspace => app.backspace(),
        KeyCode::Char(c) if !ctrl => app.push_char(c),
        _ => return false,
    }
    true
}

/// Dialog keys, checked before the composer and menu. Only one dialog is open
/// at a time. `?` and `Ctrl+B` open dialogs, so they are checked last.
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
    if key.code == KeyCode::Char('?')
        && !ctrl
        && !key.modifiers.contains(KeyModifiers::ALT)
        && app.composer.is_empty()
    {
        app.toggle_help();
        return true;
    }
    if key.code == KeyCode::Char('b') && ctrl && pane.sidebar_hidden {
        app.toggle_sidebar_dialog();
        return true;
    }
    false
}

/// Ask-dialog keys while open. Returns true if consumed. `Esc` closes without
/// answering, so the prompt stays reopenable. A list prompt consumes every other
/// key, so nothing reaches the composer. `Enter` is never consumed here.
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

/// Quit confirm owns all keys while open: there is no free text to type.
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

/// Switcher keys while open: `Esc` closes, arrows highlight, typing filters.
/// `Enter` is not consumed; the loop handles it because it needs the transport.
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

/// Help overlay keys while open: `Esc` or `?` closes; the arrows and PageUp/PageDown scroll.
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

/// Sidebar dialog keys while open: `Esc` or `Ctrl+B` closes it.
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

/// Scrolls so the selected block is visible after a cursor move or a height change.
fn reveal_cursor(app: &App, view: &mut Viewport, pane: Pane, theme: Theme) {
    let Some(at) = app.cursor() else { return };
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

/// Takes one streamed event and applies it. Separate from the loop, which only
/// spawns threads and owns the channel.
fn apply_event(
    app: &mut App,
    receiver: &mpsc::Receiver<Result<StreamEvent, String>>,
    tick: usize,
) -> Step {
    match receiver.try_recv() {
        Ok(Ok(StreamEvent::Delta(text))) => app.apply_delta(&text),
        Ok(Ok(StreamEvent::Done(answer))) => {
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
        Ok(Ok(StreamEvent::Error(err))) => {
            app.interrupt_open_tools();
            app.record_error(err);
            return Step::Ended;
        }
        Ok(Ok(StreamEvent::Contributions(sets))) => app.set_contributions(&sets),
        Ok(Err(err)) => {
            app.disconnect(err);
            return Step::Ended;
        }
        Err(mpsc::TryRecvError::Empty) => return Step::Drained,
        Err(mpsc::TryRecvError::Disconnected) => return Step::Ended,
    }
    Step::Applied
}

/// Composer caret glyph. A glyph rather than a tint, because `Mode::Mono` drops all colour.
const fn caret_glyph(turn: Turn) -> Glyph {
    match turn {
        Turn::Streaming => Glyph::CaretSteering,
        Turn::Idle | Turn::Cancelling | Turn::Blocked => Glyph::Caret,
    }
}

/// Composer border title: what the field is right now. Kept separate from
/// [`status_hint`], which says which keys act.
fn composer_label(app: &App) -> String {
    app.pending_prompt().map_or_else(
        || {
            match app.turn() {
                Turn::Streaming => "steer this turn",
                Turn::Cancelling => "cancelling",
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

/// The keybinding [`Context`] for the current state.
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
        Turn::Idle | Turn::Blocked => Context::Idle,
    }
}

/// Keybinding hint for the current state, produced from [`keymap::BINDINGS`] so
/// the status bar cannot drift from the table. `narrow` keeps only the first
/// binding per context (60–79 columns). A pending prompt's default is appended
/// here: it is data from the core, not a binding.
fn status_hint(app: &App, narrow: bool) -> String {
    status_hint_from(keymap::BINDINGS, app, narrow)
}

/// [`status_hint`] against an arbitrary table. The table is a parameter so tests
/// can prove the status bar reads it rather than a hand-written copy.
fn status_hint_from(table: &[keymap::Binding], app: &App, narrow: bool) -> String {
    let context = status_context(app);
    let base = if narrow {
        keymap::hint_narrow(table, context)
    } else {
        keymap::hint(table, context)
    };
    if context == Context::DialogOpen {
        if let Some(prompt) = app.pending_prompt() {
            return format!("{base} (default: `{}`)", prompt.default);
        }
    }
    base
}

/// The spinner frame for `tick`, or nothing when idle. Driven by the poll tick,
/// not by deltas, which can be sparse.
fn spinner_frame(theme: Theme, turn: Turn, tick: usize) -> Option<&'static str> {
    match turn {
        Turn::Idle => None,
        Turn::Streaming | Turn::Cancelling | Turn::Blocked => {
            let frames = theme.spinner();
            frames.get(tick % frames.len()).copied()
        }
    }
}

/// Run-fixed facts for rendering: how the client reached the core, and where it
/// started. The session id is not here because it can change; read it from `App`.
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

    let caret = format!("{} ", theme.glyph(caret_glyph(app.turn())));
    let caret_cells = jk_tui::wrap::width(&caret);
    let composer_width = usize::from(content.width).saturating_sub(2 + caret_cells);

    let (composer_rows, (caret_row, caret_col)) = app.composer.visible(composer_width);
    let composer_height = u16::try_from(composer_rows.len().max(1)).unwrap_or(1);

    let [transcript_area, input_area] =
        Layout::vertical([Constraint::Min(1), Constraint::Length(composer_height + 2)])
            .areas(content);

    let border = !regions.single_column;
    let reserved = if border { 2 } else { 0 };
    let inner_width = usize::from(transcript_area.width).saturating_sub(reserved);
    let inner_height = usize::from(transcript_area.height).saturating_sub(reserved);
    let lines = blocks::transcript(&app.transcript, app.cursor(), inner_width, theme);

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

    let title = composer_label(app);
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
    frame.set_cursor_position((
        input_area.x + 1 + u16::try_from(caret_cells + caret_col).unwrap_or(0),
        input_area.y + 1 + u16::try_from(caret_row).unwrap_or(0),
    ));
    render_menu(frame, app, theme, input_area);
    render_completion(frame, app, theme, input_area);
    render_prompt_dialog(frame, app, theme);
    render_sessions_dialog(frame, app, theme);
    render_help(frame, app, theme);
    render_sidebar_dialog(frame, app, meta, theme);
    render_quit_confirm(frame, app, theme);
}

/// The header row: wordmark, session id, how the client connected, and the
/// spinner on the right. `collapsed` (60–79 columns) drops the connection.
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

/// The status bar: keybinding hints on the left, and a toast or the connection state on the right.
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

/// The sidebar, drawn from [`sidebar::view`]. This function only places it.
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

/// The sidebar as a dialog, for widths where there is no permanent sidebar pane.
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

/// Drawn instead of the normal frame when the terminal is too small to read.
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

/// Width of every modal: about two thirds of the frame, clamped to a usable range.
/// Dialogs wrap their lines to this same width.
fn modal_width(area_width: u16) -> u16 {
    ((area_width * 2) / 3).clamp(24, area_width.saturating_sub(4).max(24))
}

/// The one modal primitive: centred, on `raised()` with a `focus()` border.
/// Every dialog goes through it, because a drawing mistake in the ask dialog
/// is a security issue. `scroll` is in rows; only help uses it.
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

/// The ask dialog: content from [`blocks::prompt_dialog`], framed by [`render_modal`].
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

/// The session switcher: content from [`blocks::sessions_dialog`], framed by [`render_modal`].
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

/// The help overlay: content from [`blocks::help_dialog`], framed by [`render_modal`].
/// Scrollable, since its content can exceed the modal's height.
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

/// The quit confirm: content from [`blocks::quit_dialog`], framed by [`render_modal`].
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

/// Draws `rows` in a bordered overlay just above the composer.
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
    if app.menu_query().is_none() {
        return;
    }
    {
        let entries = app.menu_entries();
        let rows: Vec<Line<'static>> = if entries.is_empty() {
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
    use jk_tui::app::{App, Entry as JanKlodEntry, Turn};
    use jk_tui::theme::{Depth, GlyphSet, Mode, Theme};
    use jk_tui::viewport::Viewport;
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

        let mut app = App::default();
        "ab".chars().for_each(|c| app.push_char(c));
        assert!(!key(&mut app, KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(app.input(), "ab", "Enter must not have edited the buffer");
    }

    /// `Ctrl+U` kills a line with content and scrolls when the composer is empty.
    #[test]
    fn ctrl_u_kills_a_line_with_content_and_is_left_alone_when_empty() {
        let mut app = App::default();
        "hello".chars().for_each(|c| app.push_char(c));
        assert!(key(&mut app, KeyCode::Char('u'), KeyModifiers::CONTROL));
        assert_eq!(app.input(), "", "the line was killed");

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
        assert!(!key(&mut app, KeyCode::Char('z'), KeyModifiers::CONTROL));
        assert_eq!(app.input(), "");
    }

    /// Arrow keys walk history only when the caret cannot move.
    #[test]
    fn arrows_walk_history_only_when_the_caret_cannot_move() {
        let mut app = App::default();
        for message in ["first", "second"] {
            message.chars().for_each(|c| app.push_char(c));
            app.take_submission();
        }

        assert!(key(&mut app, KeyCode::Up, KeyModifiers::NONE));
        assert_eq!(app.input(), "second");
        assert!(key(&mut app, KeyCode::Up, KeyModifiers::NONE));
        assert_eq!(app.input(), "first", "a second press keeps walking back");

        assert!(key(&mut app, KeyCode::Up, KeyModifiers::NONE));
        assert_eq!(app.input(), "first", "the oldest is a wall, not a loop");

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

        app.push_char('!');
        assert_eq!(app.input(), "original!");
        key(&mut app, KeyCode::Up, KeyModifiers::NONE);
        assert_eq!(
            app.input(),
            "original!",
            "a modified composer moves the caret instead of recalling"
        );

        app.composer.set("");
        key(&mut app, KeyCode::Up, KeyModifiers::NONE);
        assert_eq!(app.input(), "original", "history holds what was sent");
    }

    /// The menu opens only for a slash at column zero of an empty composer.
    #[test]
    fn the_menu_opens_only_at_column_zero_of_an_empty_composer() {
        let mut app = App::default();
        app.push_char('/');
        assert!(
            app.menu_query().is_some(),
            "a slash on an empty line opens it"
        );

        let mut mid = App::default();
        "ls /tmp".chars().for_each(|c| mid.push_char(c));
        assert!(
            mid.menu_query().is_none(),
            "a slash inside a message is a slash"
        );

        let mut spaced = App::default();
        "/new ".chars().for_each(|c| spaced.push_char(c));
        assert!(spaced.menu_query().is_none());

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

        assert!(!key(&mut app, KeyCode::Enter, KeyModifiers::NONE));
    }

    #[test]
    fn every_command_in_the_table_does_what_it_claims() {
        for command in jk_tui::commands::COMMANDS {
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
                jk_tui::commands::Availability::Ready => match command.name {
                    "/newline" => assert_eq!(app.input(), "\n"),
                    "/quit" => assert!(app.should_quit),
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
                jk_tui::commands::Availability::Pending(reason) => match app.transcript.last() {
                    Some(jk_tui::app::Entry::Message { text, .. }) => assert!(
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
            jk_tui::commands::COMMANDS.len() - 1,
            "a six-item list wraps, unlike history"
        );

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
        assert_eq!(app.completion_entries().len(), 0);
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

    /// `Ctrl+O` toggles the block under the cursor, and is a no-op with none.
    #[test]
    fn ctrl_o_toggles_the_selected_block_and_never_types_an_o() {
        let mut app = App::default();
        app.record_tool_invoked("c1".into(), "read".into(), None);

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

    /// Bare arrows stay with the composer; the transcript cursor needs Alt.
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

    /// `Ctrl+C` raises one cancel ask and keeps the partial answer.
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
    use jk_tui::app::{App, Prompt, Turn};
    use jk_tui::theme::{Depth, GlyphSet, Mode, Theme};

    fn streaming() -> App {
        let mut app = App::default();
        app.begin_turn();
        app
    }

    /// The caret, label and hint all differ between `Idle` and `Streaming`, so
    /// one missed signal cannot hide the change.
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

    /// The status bar is produced from the keymap table: a doctored table must change it.
    #[test]
    fn the_status_bar_renders_from_the_keymap_table_and_not_a_duplicate() {
        use jk_tui::keymap::{Binding, Context};

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

    /// Under `Mode::Mono` the three signals still differ, since colour is gone.
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
        assert_ne!(composer_label(&idle), composer_label(&busy));
        assert_ne!(status_hint(&idle, false), status_hint(&busy, false));
    }

    /// The spinner advances on a poll tick with no delta received.
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

        assert!(
            spinner_frame(theme, App::default().turn(), 3).is_none(),
            "an idle client spins, which says a turn is running when none is"
        );
    }

    /// A pending confirmation names the answer in words, not by reusing the steering caret.
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
    use jk_tui::app::{App, Prompt, Turn};
    use jk_tui::transport::{Rest, Transport};
    use jk_tui::viewport::Viewport;
    use jk_tui::StreamEvent;
    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use std::sync::Mutex;

    /// Records what the client sent; replies are not modelled.
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
        fn session_list(&self) -> Result<Vec<jk_tui::transport::SessionSummary>, String> {
            self.sent
                .lock()
                .expect("lock")
                .push("session/list".to_string());
            Ok(vec![jk_tui::transport::SessionSummary {
                id: "s1".to_string(),
                preview: "hi".to_string(),
            }])
        }
        fn session_get(&self, session: &str) -> Result<jk_protocol::SessionGetResult, String> {
            self.sent
                .lock()
                .expect("lock")
                .push(format!("session/get {session}"));
            Ok(jk_protocol::SessionGetResult {
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

    /// The `Enter` table: the turn state picks its meaning.
    #[test]
    fn the_state_decides_what_enter_means() {
        let idle = App::default();
        assert_eq!(idle.turn(), Turn::Idle);
        assert_eq!(enter_means(&idle), Enter::Send);

        let mut busy = App::default();
        busy.begin_turn();
        assert_eq!(busy.turn(), Turn::Streaming);
        assert_eq!(enter_means(&busy), Enter::Steer);

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

    /// Steering while streaming sends `turn/follow-up`, not a new message.
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

    /// `Blocked` answers the prompt and does not steer.
    #[test]
    fn a_blocked_turn_answers_and_does_not_steer() {
        let mut app = App::default();
        app.begin_turn();
        app.ask(prompt());
        app.prompt_jump(1);

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

    /// The answer goes to the prompt's own session, not the client's current one.
    #[test]
    fn the_answer_is_sent_with_the_prompts_own_session_not_the_clients() {
        let mut app = App::default();
        app.begin_turn();
        app.ask(Prompt {
            session: "the-turns-session".to_string(),
            ..prompt()
        });
        app.prompt_jump(2);

        let recorder = Recorder::default();
        let (session, answer) = app.take_answer().expect("the default was selected");
        recorder.answer(&session, &answer).expect("recorded");

        assert_eq!(
            recorder.sent.lock().expect("lock").clone(),
            vec!["turn/answer the-turns-session no".to_string()],
            "the client's own session, `s1` in this loop, must not appear here"
        );
    }

    /// `Esc` closes the dialog without sending, and the prompt survives.
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
            jk_tui::theme::Theme::new(
                jk_tui::theme::Mode::Dark,
                jk_tui::theme::Depth::TrueColor,
                jk_tui::theme::GlyphSet::Unicode,
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
        assert!(recorder.sent.lock().expect("lock").is_empty());
    }

    /// `/cancel` and `Ctrl+C` raise the same ask, which the loop drains once.
    #[test]
    fn a_menu_cancel_raises_the_same_ask_ctrl_c_does() {
        for opened_by_menu in [false, true] {
            let mut app = App::default();
            app.begin_turn();

            if opened_by_menu {
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

    /// Over REST, steering is refused with a reason shown in the transcript.
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
    //! Dialog wiring between `App` intents and `Transport`.

    use jk_protocol::{SessionGetResult, TranscriptMessage};
    use jk_tui::app::{App, Entry, SessionEntry, SessionMessage};
    use jk_tui::theme::{Depth, GlyphSet, Mode, Theme};
    use jk_tui::transport::{SessionSummary, Transport};
    use jk_tui::viewport::Viewport;
    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use std::sync::Mutex;

    /// `?` on an empty composer opens help, tested through `edit_or_scroll`.
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

    /// `Ctrl+B` opens the sidebar dialog only when there is no sidebar pane.
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

    /// Fake transport that counts `session/list` and `session/get` calls.
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
            _on_event: &mut dyn FnMut(jk_tui::StreamEvent),
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

    /// Opening the switcher lists once; selecting loads once and rebuilds the
    /// transcript, resolving a `tool` message onto its call.
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

        app.sessions_move(1);
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
                who: jk_tui::app::Who::You,
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
                who: jk_tui::app::Who::Klod,
                text: "done".to_string()
            }
        );
    }

    /// Only one function draws a dialog frame. Checked by grep, since a second
    /// copy would compile just as well.
    #[test]
    fn every_dialog_shares_one_frame_implementation() {
        let source = include_str!("tui.rs");
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
