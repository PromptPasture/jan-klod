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

use jan_klod::app::{App, Prompt, Who};
use jan_klod::theme::Theme;
use jan_klod::transport::Transport;
use jan_klod::StreamEvent;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind};
use ratatui::layout::{Constraint, Layout};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, List, ListItem, Paragraph};
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

        terminal.draw(|frame| render(frame, &app, theme))?;

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
        match key.code {
            KeyCode::Esc => app.quit(),
            KeyCode::Backspace => app.backspace(),
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

fn render(frame: &mut Frame, app: &App, theme: Theme) {
    let [transcript_area, input_area] =
        Layout::vertical([Constraint::Min(1), Constraint::Length(3)]).areas(frame.area());

    let items: Vec<ListItem> = app
        .transcript
        .iter()
        .map(|entry| {
            // Routed through the theme rather than named here (#128). Two of
            // these are exact — an error is `removed`, a status line is `muted`
            // — and two are the nearest role rather than the same pixels:
            // `you` was `Cyan` and takes `focus`, the blue-family accent, and
            // `klod` was `Green` and takes `body`, since the assistant's output
            // *is* the body text. #97 eventually wants the two speakers told
            // apart by gutter and indent rather than by tint at all, but that
            // is 19b's redesign, not this slice's routing.
            let (label, color) = match entry.who {
                Who::You => ("you", theme.focus()),
                Who::Klod => ("klod", theme.body()),
                Who::Error => ("err", theme.removed()),
                Who::Status => ("··", theme.muted()),
            };
            ListItem::new(Line::from(vec![
                Span::styled(format!("{label} › "), Style::default().fg(color)),
                Span::raw(entry.text.as_str()),
            ]))
        })
        .collect();
    let transcript = List::new(items).block(Block::bordered().title("jan-klod"));
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
