//! `jan-klod-ui` — a REPL client for a running core.
//!
//! Usage: `jan-klod-ui [addr] [session]`
//!   addr     `host:port` of a running `jan-klod serve` (default: 127.0.0.1:8787)
//!   session  session id, shared across the conversation (default: cli)
//!
//! Type a message and press enter to drive a turn; empty input, `quit`, or EOF
//! exits. A `ratatui` TUI is a later step over this same transport.

use std::io::{self, Write};
use std::process::ExitCode;

use jan_klod_ui::send_turn;

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let addr = args.next().unwrap_or_else(|| "127.0.0.1:8787".to_string());
    let session = args.next().unwrap_or_else(|| "cli".to_string());

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
        match send_turn(&addr, &session, message) {
            Ok(answer) => println!("klod › {answer}"),
            Err(err) => eprintln!("error: {err}"),
        }
    }
    ExitCode::SUCCESS
}
