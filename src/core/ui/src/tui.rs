//! Terminal-UI shell over the [`app`](jan_klod::app) model and a
//! [`Transport`]. This is thin, terminal-bound glue (not unit-tested); all
//! state logic lives in the tested `App` model, which has never known what the
//! transport is and still does not.
//!
//! The transport arrives as an `Arc<dyn Transport>` rather than as an address
//! because a turn and an answer run on different threads and, over stdio,
//! share one pipe to one child process. An address could be cloned per thread;
//! a pipe cannot.

use std::sync::mpsc;
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use jan_klod::app::{App, Prompt, Turn};
use jan_klod::blocks;
use jan_klod::commands::Availability;
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

/// Run the TUI against `transport` with the given `session`, restoring the
/// terminal on exit.
///
/// `force_ascii` is `--ascii`. The theme is resolved once here, from the
/// process environment, rather than per frame: the terminal's capabilities do
/// not change while it is running, and re-reading them every draw would make
/// the rendering depend on something a test cannot hold still.
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
    let mut view = Viewport::default();
    // What the last frame drew. The scroll keys need the transcript's line count
    // and the pane's height, and only `render` knows either — it is the thing
    // that wraps the text to the width the terminal currently has.
    let mut pane = Pane::default();
    // Resolved once: the completion root is where the client was started, and a
    // list that moved because something called `chdir` would be worse than one
    // that is simply wrong about a directory that no longer exists.
    let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
    app.record_status(format!(
        "connected to {} (session `{session}`); Esc to quit",
        transport.describe()
    ));

    // Channel carrying stream events from a background turn thread.
    let mut rx: Option<mpsc::Receiver<Result<StreamEvent, String>>> = None;
    // How many poll periods have elapsed, which is what drives the spinner.
    // `wrapping_add` because this only ever feeds a modulo — an overflow after
    // ~29 000 years of streaming should not be a panic.
    let mut tick: usize = 0;

    while !app.should_quit {
        // Drain all pending stream events before redrawing.
        if let Some(receiver) = &rx {
            loop {
                match apply_event(&mut app, receiver) {
                    Step::Applied => {}
                    Step::Drained => break,
                    Step::Ended => {
                        rx = None;
                        // One place where the turn goes back to `Idle`,
                        // whichever way it ended — answered, failed, or the
                        // sender dropped. `finish_turn` has already set it on
                        // the answered path; this costs nothing there and is
                        // the only thing that sets it on the other two.
                        app.end_turn();
                        break;
                    }
                }
            }
        }

        // One place sends a cancel, whoever asked for it — `Ctrl+C` or the `/`
        // menu. `App::cancel` raises the ask and refuses to raise it twice, so
        // this cannot send two to a core that is already stopping, and the menu
        // gets to act without `App` ever learning what a transport is.
        if app.take_cancel_request() {
            match transport.cancel(session) {
                Ok(()) => app.record_status("cancelling — the answer so far is kept"),
                Err(err) => app.record_status(format!("cancel could not be sent: {err}")),
            }
        }

        terminal.draw(|frame| render(frame, &app, theme, &mut view, &mut pane, tick))?;

        // Short poll so we redraw incrementally during streaming.
        if !event::poll(Duration::from_millis(POLL_MS))? {
            // A poll that timed out is the spinner's heartbeat (#160). The
            // frame advances here rather than where a delta arrives, so a turn
            // that is thinking rather than emitting still looks alive.
            tick = tick.wrapping_add(1);
            continue;
        }
        let Event::Key(key) = event::read()? else {
            continue;
        };
        if key.kind != KeyEventKind::Press {
            continue;
        }
        // Editing and scrolling first: they are a pure mapping onto the
        // composer and the viewport, and separating them keeps this loop about
        // the one thing it alone can do — drive a turn.
        if edit_or_scroll(&key, &mut app, &mut view, pane, theme) {
            // The completion list is a function of the buffer, so it is
            // refreshed after whatever just changed it rather than in each arm
            // that might have.
            app.refresh_completion(&cwd);
            continue;
        }
        match key.code {
            KeyCode::Esc => app.quit(),
            // `Ctrl+C` stops the running turn. *How* is the transport's
            // business — a `turn/cancel` over stdio, a stream teardown over
            // REST — and this does not know which it got.
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                request_cancel(&mut app);
            }
            KeyCode::Enter => match enter_means(&app) {
                // A pending confirmation is answered even though a turn is
                // running — that turn is precisely what is blocked waiting for
                // it, which is why `Blocked` comes first in `enter_means`.
                Enter::Answer => {
                    if let Some(answer) = app.take_answer() {
                        let transport = Arc::clone(transport);
                        let session = session.to_string();
                        // Off-thread: sending the answer can block on the core
                        // — over REST it is a whole HTTP round trip — and the
                        // UI must keep drawing the stream meanwhile.
                        thread::spawn(move || {
                            let _ = transport.answer(&session, &answer);
                        });
                    }
                }
                Enter::Steer => steer(transport.as_ref(), session, &mut app),
                Enter::Send if rx.is_none() => {
                    if let Some(message) = app.take_submission() {
                        app.begin_turn();
                        let transport = Arc::clone(transport);
                        let session = session.to_string();
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

/// What `Enter` does right now (#159).
///
/// `Enter` has four claims on it and the state picks between them. Extracted
/// from the event loop rather than left as a chain of match guards for one
/// reason: the loop needs a terminal, so a guard chain is a table nothing can
/// assert. This is a pure function of the model, and
/// `enter::the_state_decides_what_enter_means` reads it as the table it is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Enter {
    /// Answer the pending confirmation. **Does not steer**: a question about a
    /// turn is not a message to it.
    Answer,
    /// Steer the turn already running — `turn/follow-up`.
    Steer,
    /// Start a turn — `session/message`.
    Send,
    /// Nothing, because the turn is stopping and steering a turn that is being
    /// cancelled is not a thing to offer.
    Nothing,
}

/// Which meaning the state gives `Enter`.
///
/// Order matters and is the table from #159. `Blocked` is checked first because
/// `App::turn()` derives it from `pending_prompt`, and a turn that is blocked is
/// also streaming underneath — so testing for `Streaming` first would steer
/// instead of answering.
///
/// Read from `App::turn()` rather than from the event loop's `rx`. Both answer
/// "is a turn running" and they are reconciled at `Step::Ended`, but they are
/// two fields with one opinion and the model's accessor is the one #158 made
/// authoritative; a steering arm that tested the other would work until the
/// first turn that ended early.
///
/// Note the `/` menu and the `@` completion take `Enter` *before* this is ever
/// reached (19d), and both fall through on an empty list so a list showing
/// nothing cannot make the composer unsendable. This layers after that order
/// rather than joining it.
const fn enter_means(app: &App) -> Enter {
    match app.turn() {
        Turn::Blocked => Enter::Answer,
        Turn::Streaming => Enter::Steer,
        Turn::Idle => Enter::Send,
        Turn::Cancelling => Enter::Nothing,
    }
}

/// Send the composer's text as a follow-up, and show a refusal rather than
/// swallowing it (#159).
///
/// **Synchronous**, unlike `answer`, and the difference is deliberate.
/// `Rest::follow_up` is a refusal with no I/O at all, and `Stdio::follow_up` is
/// one write to a pipe whose *reader* is another thread — neither can block the
/// draw loop meaningfully. A spawned thread would instead throw away the error,
/// which over REST is the entire answer: 13b recorded that the REST surface has
/// no route reaching a turn in flight, so a keystroke that vanished there would
/// teach a user that steering is broken rather than that this connection cannot
/// carry it.
///
/// The message is taken from the composer either way, so a refused follow-up
/// still clears what was typed and records it — the user said it, and a
/// transcript that showed the refusal without the thing refused would be
/// missing half the exchange.
fn steer(transport: &dyn Transport, session: &str, app: &mut App) {
    let Some(message) = app.take_submission() else {
        return;
    };
    if let Err(refusal) = transport.follow_up(session, &message) {
        app.record_status(refusal);
    }
}

/// `Ctrl+C`: ask the running turn to stop, and say what that actually does.
///
/// Returns whether this was the ask that acted, which is `false` both when
/// nothing is running and when a cancel has already been asked for — the
/// second `Ctrl+C` of a pair must not produce a second line saying the same
/// thing, for the same reason it must not send a second message.
///
/// **The partial answer stays.** `App::cancel` does not erase what has already
/// arrived, and it must not: cancelling finalizes with what is in hand, and
/// deleting it would misreport what the core actually did before it stopped.
///
/// This used to only *say* a cancel had been asked for, because the client had
/// no way to send one ([#157]). It can now, and does — over stdio a real
/// `turn/cancel`, over REST the stream teardown the conductor reads as
/// `Flow::Stop`. One method, two honest implementations, and this does not know
/// which it got.
///
/// It does not send anything itself. `App::cancel` raises the ask and the event
/// loop drains it, so `Ctrl+C` and the `/cancel` command go out by one path —
/// the menu is dispatched inside `App`, which holds no transport, and giving it
/// one to make this key simpler would have been the wrong trade.
///
/// [#157]: https://github.com/PromptPasture/jan-klod/issues/157
fn request_cancel(app: &mut App) -> bool {
    app.cancel()
}

/// What the last frame drew, so the scroll keys have dimensions to work with.
#[derive(Debug, Default, Clone, Copy)]
struct Pane {
    /// Rendered transcript lines.
    total: usize,
    /// Rows the transcript pane can show.
    height: usize,
    /// Cells the composer's text may use, for the vertical motion and history
    /// arms — both are written in terms of *visual* rows, so neither can be
    /// answered without knowing where the text wraps.
    composer_width: usize,
    /// Cells the *transcript* has, which is a different number: the composer
    /// gives up two more to its caret glyph. `span_of` has to be asked in the
    /// same width the frame was drawn in or it measures a layout nobody saw.
    transcript_width: usize,
}

/// Keys that only move a caret or a viewport.
///
/// Returns whether the key was consumed. Split out of the event loop because it
/// is a mapping and nothing else — the loop's own arms spawn threads and own the
/// turn, and mixing the two made one function that did both badly.
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
    // `@` completion first: the two lists are mutually exclusive — one needs a
    // slash at column 0, the other an `@` anywhere — but ordering them makes
    // that a property of the code rather than of the data.
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
    // The `/` menu takes the keys it needs while it is open, and only those.
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
    // The transcript cursor, and the one key it exists for (#155). `Alt+Up`
    // and `Alt+Down` rather than the bare arrows because those are the
    // composer's — a client where moving the caret also moved a selection
    // somewhere else on screen would be one where neither is predictable.
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
        // Scrolling. `Ctrl+U`/`Ctrl+D` reach here only when the composer is
        // empty: #150 consumes them when there is content and deliberately
        // does not when there is none, so this arm needs no composer test
        // and the two slices stay independent.
        KeyCode::PageUp => *view = view.page_up(pane.height),
        KeyCode::PageDown => *view = view.page_down(pane.total, pane.height),
        KeyCode::Char('u') if ctrl && app.composer.is_empty() => *view = view.half_up(pane.height),
        KeyCode::Char('d') if ctrl && app.composer.is_empty() => {
            *view = view.half_down(pane.total, pane.height);
        }
        // Editing. Readline where the terminal delivers it; the composer
        // owns what each one means, so this is a mapping and nothing more.
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
        // The other half of #148's arm: with content, this kills the line
        // and consumes the key; empty, it fell through to the scroll above.
        KeyCode::Char('u') if ctrl => app.composer.kill_line(),
        // Both spellings, because terminals disagree about which reaches the
        // application — and `/newline` (#152) is the fallback where neither
        // does. Leaving a user with no way to type a newline is not an
        // option a client gets to choose.
        KeyCode::Enter if shift || alt => app.composer.push('\n'),
        // History, or the caret — `App` decides which, because the condition is
        // two facts about its own state and a key arm should ask one question.
        KeyCode::Up => app.history_up(pane.composer_width),
        KeyCode::Down => app.history_down(pane.composer_width),
        KeyCode::Backspace => app.backspace(),
        KeyCode::Char(c) if !ctrl => app.push_char(c),
        _ => return false,
    }
    true
}

/// Scroll so the selected block is on screen, if there is one.
///
/// Called after every key that can move the cursor *or* change the height of
/// the block under it — opening a block that then runs off the bottom of the
/// pane is the same defect as selecting one that was never on it.
fn reveal_cursor(app: &App, view: &mut Viewport, pane: Pane, theme: Theme) {
    let Some(at) = app.cursor() else { return };
    // Measured against the pane the **last** frame used. The alternative is
    // laying the transcript out twice per keystroke, and a one-frame-stale
    // width can only matter on the frame a resize lands — where the next
    // `reflow` corrects it anyway.
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
fn apply_event(app: &mut App, receiver: &mpsc::Receiver<Result<StreamEvent, String>>) -> Step {
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
        Ok(Ok(StreamEvent::Warning(msg))) => app.record_status(format!("⚠ {msg}")),
        Ok(Ok(StreamEvent::Prompt {
            question,
            options,
            default,
        })) => app.ask(Prompt {
            question,
            options,
            default,
        }),
        Ok(Ok(StreamEvent::Error(err)) | Err(err)) => {
            // A failed turn leaves the same blocks open as a finished one.
            app.interrupt_open_tools();
            app.record_error(err);
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
    app.pending_prompt.as_ref().map_or_else(
        || {
            match app.turn() {
                Turn::Streaming => "steer this turn",
                Turn::Cancelling => "cancelling",
                // `Blocked` is unreachable here — `pending_prompt` is what
                // makes a turn `Blocked`, and this arm is the `None` branch.
                Turn::Idle | Turn::Blocked => "message",
            }
            .to_string()
        },
        |prompt| format!("answer [{}]", prompt.options.join("/")),
    )
}

/// Which keys act right now — the third of #160's three signals.
///
/// Written from the state rather than from a fixed string, because the whole
/// point of the trio is that `Enter` does not always mean the same thing and a
/// hint that said "Enter to send" during a turn would be the lie the signals
/// exist to prevent.
fn status_hint(app: &App) -> String {
    if let Some(prompt) = app.pending_prompt.as_ref() {
        return format!("Enter for `{}` · Esc to quit", prompt.default);
    }
    match app.turn() {
        Turn::Streaming => "Enter to steer · Ctrl+C to cancel".to_string(),
        Turn::Cancelling => "stopping · Ctrl+C already sent".to_string(),
        Turn::Idle | Turn::Blocked => "Enter to send · Esc to quit".to_string(),
    }
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

fn render(
    frame: &mut Frame,
    app: &App,
    theme: Theme,
    view: &mut Viewport,
    pane: &mut Pane,
    tick: usize,
) {
    // The composer grows with its content and then scrolls, so the split is
    // computed from the buffer rather than fixed at three rows. `- 2` for the
    // block border, and again for the caret glyph and the space after it.
    let caret = format!("{} ", theme.glyph(caret_glyph(app.turn())));
    let caret_cells = jan_klod::wrap::width(&caret);
    let composer_width = usize::from(frame.area().width).saturating_sub(2 + caret_cells);

    let (composer_rows, (caret_row, caret_col)) = app.composer.visible(composer_width);
    let composer_height = u16::try_from(composer_rows.len().max(1)).unwrap_or(1);

    let [transcript_area, input_area] =
        Layout::vertical([Constraint::Min(1), Constraint::Length(composer_height + 2)])
            .areas(frame.area());

    // The transcript is rendered by `blocks::transcript` (#147), which is a
    // pure function of the entries plus a width — so the layout is tested
    // without a terminal and this function only places the result. The gutter,
    // the role label and the wrapping all live there; what is left here is
    // where it goes on screen, which is #148's concern next.
    //
    // `- 2` for the block's own border, which is not part of the pane the text
    // may use. Getting that wrong is how a wrap that passes its own test still
    // overflows on screen.
    // `- 2` for the block's own border, which is not part of the pane the text
    // may use. Getting that wrong is how a wrap that passes its own test still
    // overflows on screen.
    let inner_width = usize::from(transcript_area.width).saturating_sub(2);
    let inner_height = usize::from(transcript_area.height).saturating_sub(2);
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
    };

    // The detach marker: a glyph and a count, never a tint. Under `Mode::Mono`
    // every role resolves to `Color::Reset`, so a marker that were only a colour
    // would vanish exactly when the user most needs to know the view is stale.
    let mut block = Block::bordered().title("jan-klod");
    // The streaming indicator, top right. It advances on the caller's poll tick
    // (#160), so it keeps moving through a long wait in which no delta arrives.
    if let Some(frame_glyph) = spinner_frame(theme, app.turn(), tick) {
        block = block.title(
            Line::from(Span::styled(
                format!(" {frame_glyph} "),
                Style::default().fg(theme.focus()),
            ))
            .right_aligned(),
        );
    }
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

    let offset = u16::try_from(view.offset()).unwrap_or(u16::MAX);
    let transcript = Paragraph::new(lines).block(block).scroll((offset, 0));
    frame.render_widget(transcript, transcript_area);

    // Two of #160's three signals: what this field is (the title) and which
    // keys act (the hint, along the bottom edge). The third is the caret above.
    // Three separate marks rather than one sentence, because the cost of
    // missing the change is sending a message to a turn that is still running.
    let title = composer_label(app);
    let hint = status_hint(app);
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
    let input = Paragraph::new(composer).block(
        Block::bordered().title(title).title_bottom(
            Line::from(Span::styled(
                format!(" {hint} "),
                Style::default().fg(theme.muted()),
            ))
            .right_aligned(),
        ),
    );
    frame.render_widget(input, input_area);
    // +1 for the border, and the caret column is already in display cells.
    frame.set_cursor_position((
        input_area.x + 1 + u16::try_from(caret_cells + caret_col).unwrap_or(0),
        input_area.y + 1 + u16::try_from(caret_row).unwrap_or(0),
    ));
    render_menu(frame, app, theme, input_area);
    render_completion(frame, app, theme, input_area);
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
            status_hint(&idle),
            status_hint(&busy),
            "the hint names the same keys in both states"
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
        assert_ne!(status_hint(&idle), status_hint(&busy));
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
        app.pending_prompt = Some(Prompt {
            question: "write src/main.rs?".to_string(),
            options: vec!["yes".to_string(), "no".to_string(), "always".to_string()],
            default: "no".to_string(),
        });
        assert_eq!(app.turn(), Turn::Blocked);
        assert!(composer_label(&app).contains("yes/no/always"));
        assert!(
            status_hint(&app).contains("`no`"),
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
    use super::{enter_means, steer, Enter};
    use jan_klod::app::{App, Prompt, Turn};
    use jan_klod::transport::{Rest, Transport};
    use jan_klod::StreamEvent;
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
            question: "write src/main.rs?".to_string(),
            options: vec!["yes".to_string(), "no".to_string()],
            default: "no".to_string(),
        }
    }

    /// #159's table, as a table. `Enter` has four meanings and the state is
    /// what picks one.
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
        blocked.pending_prompt = Some(prompt());
        assert_eq!(blocked.turn(), Turn::Blocked);
        assert_eq!(
            enter_means(&blocked),
            Enter::Answer,
            "a pending confirmation must take Enter ahead of steering"
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
        app.pending_prompt = Some(prompt());
        typed(&mut app, "yes");

        assert_eq!(enter_means(&app), Enter::Answer);

        let recorder = Recorder::default();
        let answer = app.take_answer().expect("the composer holds the answer");
        recorder.answer("s1", &answer).expect("recorded");

        assert_eq!(
            recorder.sent.lock().expect("lock").clone(),
            vec!["turn/answer s1 yes".to_string()]
        );
        assert!(
            app.pending_prompt.is_none(),
            "answering left the prompt pending, so the next Enter answers it again"
        );
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
