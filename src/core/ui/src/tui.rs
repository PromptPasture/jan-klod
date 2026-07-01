//! Terminal-UI shell over the [`app`](jan_klod_ui::app) model and
//! [`send_turn`](jan_klod_ui::send_turn). This is thin, terminal-bound glue (not
//! unit-tested); all state logic lives in the tested `App` model.

use jan_klod_ui::app::{App, Who};
use jan_klod_ui::send_turn;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind};
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, List, ListItem, Paragraph};
use ratatui::{DefaultTerminal, Frame};

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

    while !app.should_quit {
        terminal.draw(|frame| render(frame, &app))?;

        let Event::Key(key) = event::read()? else { continue };
        if key.kind != KeyEventKind::Press {
            continue;
        }
        match key.code {
            KeyCode::Esc => app.quit(),
            KeyCode::Backspace => app.backspace(),
            KeyCode::Char(c) => app.push_char(c),
            KeyCode::Enter => {
                if let Some(message) = app.take_submission() {
                    // Show the user's line before the (blocking) turn.
                    terminal.draw(|frame| render(frame, &app))?;
                    match send_turn(addr, session, &message) {
                        Ok(answer) => app.record_answer(answer),
                        Err(err) => app.record_error(err),
                    }
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
