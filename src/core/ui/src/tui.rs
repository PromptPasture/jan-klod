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
use jan_klod::theme::{Glyph, Theme};
use jan_klod::transport::Transport;
use jan_klod::viewport::Viewport;
use jan_klod::StreamEvent;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::layout::{Constraint, Layout};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph};
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
                    Ok(Ok(StreamEvent::Tool(name))) => {
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
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Esc => app.quit(),
            KeyCode::Backspace => app.backspace(),
            // Scrolling. `Ctrl+U`/`Ctrl+D` reach here only when the composer is
            // empty: #150 consumes them when there is content and deliberately
            // does not when there is none, so this arm needs no composer test
            // and the two slices stay independent.
            KeyCode::PageUp => view = view.page_up(pane.height),
            KeyCode::PageDown => view = view.page_down(pane.total, pane.height),
            KeyCode::Char('u') if ctrl && app.input.is_empty() => view = view.half_up(pane.height),
            KeyCode::Char('d') if ctrl && app.input.is_empty() => {
                view = view.half_down(pane.total, pane.height);
            }
            KeyCode::Char(c) => app.push_char(c),
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
}

fn render(frame: &mut Frame, app: &App, theme: Theme, view: &mut Viewport, pane: &mut Pane) {
    let [transcript_area, input_area] =
        Layout::vertical([Constraint::Min(1), Constraint::Length(3)]).areas(frame.area());

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
    let input = Paragraph::new(app.input.as_str()).block(Block::bordered().title(title));
    frame.render_widget(input, input_area);
}
