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

use jan_klod::app::{App, Prompt};
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

    while !app.should_quit {
        // Drain all pending stream events before redrawing.
        if let Some(receiver) = &rx {
            loop {
                match receiver.try_recv() {
                    Ok(Ok(StreamEvent::Delta(text))) => app.apply_delta(&text),
                    Ok(Ok(StreamEvent::Done(answer))) => {
                        app.finish_turn(answer);
                        rx = None;
                        break;
                    }
                    Ok(Ok(StreamEvent::Tool { name, .. })) => {
                        app.record_status(format!("· {name}"));
                    }
                    // Deliberately not rendered. The status line already names
                    // the running tool, and marking it finished would need the
                    // call id on the invocation to pair them — `Tool` carries
                    // none. #100 (tool blocks) is where a paired view belongs.
                    // What matters here is that it is no longer an error.
                    Ok(Ok(StreamEvent::ToolResult { .. })) => {}
                    Ok(Ok(StreamEvent::Warning(msg))) => {
                        app.record_status(format!("⚠ {msg}"));
                    }
                    Ok(Ok(StreamEvent::Prompt {
                        question,
                        options,
                        default,
                    })) => {
                        app.ask(Prompt {
                            question,
                            options,
                            default,
                        });
                    }
                    Ok(Ok(StreamEvent::Error(err)) | Err(err)) => {
                        app.record_error(err);
                        rx = None;
                        break;
                    }
                    Err(mpsc::TryRecvError::Empty) => break,
                    Err(mpsc::TryRecvError::Disconnected) => {
                        rx = None;
                        break;
                    }
                }
            }
        }

        terminal.draw(|frame| render(frame, &app, theme, &mut view, &mut pane))?;

        // Short poll so we redraw incrementally during streaming.
        if !event::poll(Duration::from_millis(POLL_MS))? {
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
        if edit_or_scroll(&key, &mut app, &mut view, pane) {
            // The completion list is a function of the buffer, so it is
            // refreshed after whatever just changed it rather than in each arm
            // that might have.
            app.refresh_completion(&cwd);
            continue;
        }
        match key.code {
            KeyCode::Esc => app.quit(),
            // A pending confirmation is answered even though a turn is running —
            // that turn is precisely what is blocked waiting for it.
            KeyCode::Enter if app.pending_prompt.is_some() => {
                if let Some(answer) = app.take_answer() {
                    let transport = Arc::clone(transport);
                    let session = session.to_string();
                    // Off-thread: sending the answer can block on the core, and
                    // the UI must keep drawing the stream meanwhile.
                    thread::spawn(move || {
                        let _ = transport.answer(&session, &answer);
                    });
                }
            }
            KeyCode::Enter if rx.is_none() => {
                if let Some(message) = app.take_submission() {
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
            _ => {}
        }
    }
    Ok(())
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

fn render(frame: &mut Frame, app: &App, theme: Theme, view: &mut Viewport, pane: &mut Pane) {
    // The composer grows with its content and then scrolls, so the split is
    // computed from the buffer rather than fixed at three rows. `- 2` for the
    // block border, and again for the caret glyph and the space after it.
    let caret = format!("{} ", theme.glyph(Glyph::Caret));
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
    let lines = blocks::transcript(&app.transcript, inner_width, theme);

    // Re-clamp against what this frame actually holds, *then* record it. While
    // attached this follows a streaming delta to the new bottom; while detached
    // it holds position, which is the whole point of #148.
    *view = view.reflow(lines.len(), inner_height);
    *pane = Pane {
        total: lines.len(),
        height: inner_height,
        composer_width,
    };

    // The detach marker: a glyph and a count, never a tint. Under `Mode::Mono`
    // every role resolves to `Color::Reset`, so a marker that were only a colour
    // would vanish exactly when the user most needs to know the view is stale.
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

    let offset = u16::try_from(view.offset()).unwrap_or(u16::MAX);
    let transcript = Paragraph::new(lines).block(block).scroll((offset, 0));
    frame.render_widget(transcript, transcript_area);

    let title = app.pending_prompt.as_ref().map_or_else(
        || "message — Enter to send, Esc to quit".to_string(),
        |prompt| {
            format!(
                "answer [{}] — Enter for `{}`",
                prompt.options.join("/"),
                prompt.default
            )
        },
    );
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
    use super::{edit_or_scroll, Pane};
    use jan_klod::app::App;
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
            },
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
            },
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
                    other => panic!("{other} is Ready and untested — add it here"),
                },
                jan_klod::commands::Availability::Pending(reason) => {
                    let last = app.transcript.last().expect("a status line");
                    assert!(
                        last.text.contains(reason),
                        "{} said {:?} rather than its reason",
                        command.name,
                        last.text
                    );
                }
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
}
