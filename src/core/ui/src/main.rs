//! `jan-klod-ui` — a client for a running core.
//!
//! Usage:
//!   `jan-klod-ui [addr] [session]`       — line REPL
//!   `jan-klod-ui tui [addr] [session]`   — full-screen terminal UI (`ratatui`)
//!
//!   addr     `host:port` of a running `jan-klod serve` (default: 127.0.0.1:8787)
//!   session  session id, shared across the conversation (default: cli)
//!
//! In the REPL, type a message and press enter to drive a turn; empty input,
//! `quit`, or EOF exits.

mod tui;

use std::io::{self, Write};
use std::process::ExitCode;

use jan_klod_ui::{stream_turn, StreamEvent};

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1).peekable();
    let use_tui = matches!(args.peek().map(String::as_str), Some("tui" | "--tui"));
    if use_tui {
        args.next();
    }
    let addr = args.next().unwrap_or_else(|| "127.0.0.1:8787".to_string());
    let session = args.next().unwrap_or_else(|| "cli".to_string());

    if use_tui {
        return match tui::run(&addr, &session) {
            Ok(()) => ExitCode::SUCCESS,
            Err(err) => {
                eprintln!("jan-klod-ui: {err}");
                ExitCode::FAILURE
            }
        };
    }

    println!("jan-klod-ui → {addr} (session `{session}`); type a message, `quit` to exit.");

    loop {
        print!("you › ");
        if io::stdout().flush().is_err() {
            return ExitCode::FAILURE;
        }
        // Read one line per turn — no persistent stdin lock held across the loop.
        let mut line = String::new();
        match io::stdin().read_line(&mut line) {
            Ok(0) => break, // EOF
            Ok(_) => {}
            Err(err) => {
                eprintln!("input error: {err}");
                return ExitCode::FAILURE;
            }
        }
        let message = line.trim();
        if message.is_empty() || message == "quit" || message == "exit" {
            break;
        }
        // Stream the turn: print deltas live; notices to stderr. `done` is the
        // authoritative answer — printed only if nothing was streamed (e.g. a
        // finalize-only rewrite).
        print!("klod › ");
        let _ = io::stdout().flush();
        let mut streamed = String::new();
        let mut final_answer = String::new();
        let outcome = stream_turn(&addr, &session, message, &mut |event| match event {
            StreamEvent::Delta(text) => {
                print!("{text}");
                let _ = io::stdout().flush();
                streamed.push_str(&text);
            }
            StreamEvent::Done(text) => final_answer = text,
            StreamEvent::Tool(name) => eprint!("\n  ⚙ {name}… "),
            StreamEvent::Warning(msg) => eprint!("\n  ⚠ {msg}"),
            StreamEvent::Error(msg) => eprint!("\n  error: {msg}"),
        });
        if streamed.is_empty() && !final_answer.is_empty() {
            print!("{final_answer}");
        }
        println!();
        if let Err(err) = outcome {
            eprintln!("error: {err}");
        }
    }
    ExitCode::SUCCESS
}
