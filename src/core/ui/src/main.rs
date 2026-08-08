//! `jan-klod` — TUI client for the jan-klod gateway.
//!
//! Usage:
//!   `jan-klod [addr] [session]`       — line REPL
//!   `jan-klod tui [addr] [session]`   — full-screen terminal UI (`ratatui`)
//!
//!   addr     `host:port` of the gateway (default: 127.0.0.1:8787)
//!   session  session id, shared across the conversation (default: cli)
//!
//! If the gateway is not already running at `addr`, jan-klod will attempt to
//! start `jan-klod-gateway serve config.yaml ext <addr>` automatically,
//! looking for the binary next to its own executable first, then in PATH.
//!
//! In the REPL, type a message and press enter to drive a turn; empty input,
//! `quit`, or EOF exits.

mod tui;

use std::io::{self, Write};
use std::net::TcpStream;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};
use std::process::ExitCode;

use jan_klod::{answer_prompt, stream_turn, StreamEvent};

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1).peekable();
    let use_tui = matches!(args.peek().map(String::as_str), Some("tui" | "--tui"));
    if use_tui {
        args.next();
    }
    let addr = args.next().unwrap_or_else(|| "127.0.0.1:8787".to_string());
    let session = args.next().unwrap_or_else(|| "cli".to_string());

    // Ensure the gateway is up; spawn it if not.
    let _gateway = ensure_gateway(&addr);

    if use_tui {
        return match tui::run(&addr, &session) {
            Ok(()) => ExitCode::SUCCESS,
            Err(err) => {
                eprintln!("jan-klod: {err}");
                ExitCode::FAILURE
            }
        };
    }

    println!("jan-klod → {addr} (session `{session}`); type a message, `quit` to exit.");

    loop {
        print!("you › ");
        if io::stdout().flush().is_err() {
            return ExitCode::FAILURE;
        }
        let mut line = String::new();
        match io::stdin().read_line(&mut line) {
            Ok(0) => break,
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
            // The turn is blocked until this is answered, so ask right here on
            // the same stdin the REPL already owns.
            StreamEvent::Prompt { question, options, default } => {
                eprintln!("\n  ? {question}");
                eprint!("  [{}] (Enter = {default}): ", options.join("/"));
                let _ = io::stderr().flush();
                let mut typed = String::new();
                let answer = match io::stdin().read_line(&mut typed) {
                    Ok(_) if !typed.trim().is_empty() => typed.trim().to_string(),
                    _ => default,
                };
                if let Err(err) = answer_prompt(&addr, &session, &answer) {
                    eprintln!("  error sending answer: {err}");
                }
            }
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

/// Check if the gateway is reachable; if not, spawn it and wait until it
/// responds to a TCP connection (up to 10 s). Returns the child handle so the
/// caller keeps it alive for the duration of the process.
fn ensure_gateway(addr: &str) -> Option<Child> {
    if is_up(addr) {
        return None;
    }
    let bin = gateway_bin();
    eprintln!("jan-klod: gateway not found at {addr}, starting {} …", bin.display());
    let child = Command::new(&bin)
        .args(["serve", "config.yaml", "ext", addr])
        .stdin(Stdio::null())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .spawn();
    match child {
        Err(err) => {
            eprintln!("jan-klod: could not start gateway ({}): {err}", bin.display());
            eprintln!("jan-klod: start it manually: jan-klod-gateway serve config.yaml ext {addr}");
            None
        }
        Ok(child) => {
            wait_for_gateway(addr, Duration::from_secs(10));
            Some(child)
        }
    }
}

/// Resolve the `jan-klod-gateway` binary: sibling of the current exe first,
/// then fall back to PATH.
fn gateway_bin() -> PathBuf {
    if let Ok(exe) = std::env::current_exe() {
        let sibling = exe.with_file_name("jan-klod-gateway");
        if sibling.exists() {
            return sibling;
        }
    }
    PathBuf::from("jan-klod-gateway")
}

/// Poll TCP connect until the gateway accepts connections or the deadline passes.
fn wait_for_gateway(addr: &str, timeout: Duration) {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if is_up(addr) {
            return;
        }
        thread::sleep(Duration::from_millis(200));
    }
    eprintln!("jan-klod: gateway did not become ready within {timeout:?}");
}

fn is_up(addr: &str) -> bool {
    TcpStream::connect_timeout(
        &addr.parse().unwrap_or_else(|_| "127.0.0.1:8787".parse().unwrap()),
        Duration::from_millis(300),
    )
    .is_ok()
}
