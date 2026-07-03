//! Terminal-UI shell over the [`app`](jan_klod_ui::app) model and
//! [`stream_turn`](jan_klod_ui::stream_turn). This is thin, terminal-bound glue (not
//! unit-tested); all state logic lives in the tested `App` model.

use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use jan_klod_ui::app::{App, Who};
use jan_klod_ui::{stream_turn, StreamEvent};
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind};
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, List, ListItem, Paragraph};
use ratatui::{DefaultTerminal, Frame};

const POLL_MS: u64 = 50;

/// Run the TUI against the core at `addr` with the given `session`, restoring the
/// terminal on exit.
///
/// # Errors
/// Propagates a terminal I/O error from the draw/event loop.
pub fn run(addr: &str, session: &str) -> std::io::Result<()> {
    let mut terminal = ratatui::init();
    let result = event_loop(&mut terminal, addr, session);
    ratatui::restore();
    result
}

fn event_loop(terminal: &mut DefaultTerminal, addr: &str, session: &str) -> std::io::Result<()> {
    let mut app = App::default();
    app.record_status(format!("connected to {addr} (session `{session}`); Esc to quit"));

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
                    Ok(Ok(StreamEvent::Warning(msg))) => {
                        app.record_status(format!("⚠ {msg}"));
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

        terminal.draw(|frame| render(frame, &app))?;

        // Short poll so we redraw incrementally during streaming.
        if !event::poll(Duration::from_millis(POLL_MS))? {
            continue;
        }
        let Event::Key(key) = event::read()? else { continue };
        if key.kind != KeyEventKind::Press {
            continue;
        }
        match key.code {
            KeyCode::Esc => app.quit(),
            KeyCode::Backspace => app.backspace(),
            KeyCode::Char(c) => app.push_char(c),
            KeyCode::Enter if rx.is_none() => {
                if let Some(message) = app.take_submission() {
                        let addr = addr.to_string();
                        let session = session.to_string();
                        let (tx, new_rx) = mpsc::channel();
                        thread::spawn(move || {
                            let result = stream_turn(&addr, &session, &message, &mut |event| {
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

fn render(frame: &mut Frame, app: &App) {
    let [transcript_area, input_area] =
        Layout::vertical([Constraint::Min(1), Constraint::Length(3)]).areas(frame.area());

    let items: Vec<ListItem> = app
        .transcript
        .iter()
        .map(|entry| {
            let (label, color) = match entry.who {
                Who::You => ("you", Color::Cyan),
                Who::Klod => ("klod", Color::Green),
                Who::Error => ("err", Color::Red),
                Who::Status => ("··", Color::DarkGray),
            };
            ListItem::new(Line::from(vec![
                Span::styled(format!("{label} › "), Style::default().fg(color)),
                Span::raw(entry.text.as_str()),
            ]))
        })
        .collect();
    let transcript = List::new(items).block(Block::bordered().title("jan-klod"));
    frame.render_widget(transcript, transcript_area);

    let input = Paragraph::new(app.input.as_str())
        .block(Block::bordered().title("message — Enter to send, Esc to quit"));
    frame.render_widget(input, input_area);
}
