//! `jan-klod` — TUI client for the jan-klod gateway.
//!
//! Usage:
//!   `jan-klod [session]`                    — line REPL over stdio
//!   `jan-klod tui [session]`                — full-screen terminal UI (`ratatui`)
//!   `jan-klod --addr <host:port> [session]` — drive a gateway that is running
//!
//!   session  session id, shared across the conversation (default: cli)
//!   --addr   `host:port` of a running gateway; without it, one is spawned
//!
//! By default jan-klod starts its own `jan-klod-gateway rpc` and talks to it
//! over that process's stdin and stdout — no port, no token, nothing left
//! running when the client exits. It deliberately does *not* name
//! `config.yaml`/`ext`, so the gateway resolves them itself: the working
//! directory when it holds them, otherwise the installed copies.
//!
//! With `--addr`, it drives a gateway already listening there over REST + SSE
//! instead, starting one with `serve --bind <addr>` if nothing answers.
//!
//! ## Why `--addr` is a flag
//!
//! It used to be the first positional argument, with the session second. That
//! only worked while an address was required: now that the common case has no
//! address at all, `jan-klod my-session` would have read a session id as a
//! host:port and failed to connect to it. The gateway made the same move for
//! the same reason (`serve --bind`, rather than a path slot that might be an
//! address), and it tells the user so when it sees one in the wrong place.
//! This does too.
//!
//! In the REPL, type a message and press enter to drive a turn; empty input,
//! `quit`, or EOF exits.

mod tui;

use std::io::{self, Write};
use std::net::TcpStream;
use std::process::ExitCode;
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use jan_klod::transport::{Logs, Rest, Stdio as StdioTransport, Transport};
use jan_klod::{gateway_bin, StreamEvent};

/// The address `--addr` defaults to when it is given with no value, and the one
/// `serve` binds by default.
const DEFAULT_ADDR: &str = "127.0.0.1:8787";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (use_tui, rest) = split_mode(&args);
    let parsed = match parse_args(&rest) {
        Ok(parsed) => parsed,
        Err(message) => {
            eprintln!("{message}");
            return ExitCode::FAILURE;
        }
    };
    let session = parsed.session.unwrap_or_else(|| "cli".to_string());

    // The gateway's own log output shares this terminal. A line REPL can read
    // an interleaved log line; a full-screen TUI cannot, since it is drawing
    // over the same cells.
    let logs = if use_tui {
        Logs::Discard
    } else {
        Logs::Inherit
    };
    let (transport, _gateway): (Arc<dyn Transport>, Option<Child>) = match parsed.addr {
        Some(addr) => {
            // Keep the child alive for the process's lifetime, as before: this
            // path spawns a gateway that listens, and killing it at the end of
            // `main` is what stops it outliving the client.
            let spawned = ensure_gateway(&addr);
            (Arc::new(Rest::new(addr)), spawned)
        }
        None => match StdioTransport::spawn(logs) {
            Ok(transport) => (Arc::new(transport), None),
            Err(err) => {
                eprintln!("jan-klod: {err}");
                return ExitCode::FAILURE;
            }
        },
    };

    if use_tui {
        return match tui::run(&transport, &session) {
            Ok(()) => ExitCode::SUCCESS,
            Err(err) => {
                eprintln!("jan-klod: {err}");
                ExitCode::FAILURE
            }
        };
    }

    repl(&transport, &session)
}

/// What the arguments asked for.
#[derive(Debug)]
struct Args {
    /// `Some` when the user named a running gateway, which selects REST.
    addr: Option<String>,
    session: Option<String>,
}

/// Strip a leading `tui`/`--tui` mode word.
fn split_mode(args: &[String]) -> (bool, Vec<String>) {
    match args.split_first() {
        Some((first, rest)) if first == "tui" || first == "--tui" => (true, rest.to_vec()),
        _ => (false, args.to_vec()),
    }
}

/// Read `--addr <host:port>` and at most one positional session id.
fn parse_args(args: &[String]) -> Result<Args, String> {
    let mut addr = None;
    let mut session = None;
    let mut rest = args.iter();
    while let Some(arg) = rest.next() {
        if arg == "--addr" {
            addr = Some(rest.next().map_or_else(
                || DEFAULT_ADDR.to_string(),
                std::string::ToString::to_string,
            ));
        } else if let Some(value) = arg.strip_prefix("--addr=") {
            addr = Some(value.to_string());
        } else if arg.starts_with('-') {
            return Err(format!("jan-klod: unknown option `{arg}`"));
        } else if session.is_some() {
            return Err(format!(
                "jan-klod: `{arg}` is a second session id. Usage: jan-klod [tui] \
                 [--addr <host:port>] [session]"
            ));
        } else if looks_like_an_address(arg) {
            // The mistake the old positional form invited. Refused rather than
            // silently used as a session id, because a session named
            // `127.0.0.1:8787` is not what anyone meant.
            return Err(format!(
                "jan-klod: `{arg}` looks like an address, not a session id. Use \
                 `--addr {arg}` to drive a gateway that is already running."
            ));
        } else {
            session = Some(arg.clone());
        }
    }
    Ok(Args { addr, session })
}

/// Whether a positional argument looks like a `host:port` rather than a session
/// id. Deliberately narrow: the tail must be digits, so a session id with a
/// colon in it is left alone.
fn looks_like_an_address(arg: &str) -> bool {
    let Some((host, port)) = arg.rsplit_once(':') else {
        return false;
    };
    if port.is_empty() || !port.chars().all(|c| c.is_ascii_digit()) {
        return false;
    }
    host.is_empty()
        || host == "localhost"
        || host.ends_with(']')
        || host.chars().all(|c| c.is_ascii_digit() || c == '.')
}

/// The line REPL: one message per line, streamed back as it arrives.
fn repl(transport: &Arc<dyn Transport>, session: &str) -> ExitCode {
    println!(
        "jan-klod → {} (session `{session}`); type a message, `quit` to exit.",
        transport.describe()
    );

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
        let outcome = transport.stream_turn(session, message, &mut |event| match event {
            StreamEvent::Delta(text) => {
                print!("{text}");
                let _ = io::stdout().flush();
                streamed.push_str(&text);
            }
            StreamEvent::Done(text) => final_answer = text,
            StreamEvent::Tool(name) => eprint!("\n  ⚙ {name}… "),
            // Closes the line the invocation left open. The content is not
            // printed: it is the model's input, often long, and the answer it
            // produces arrives as `Done`.
            StreamEvent::ToolResult { .. } => eprint!("✓"),
            StreamEvent::Warning(msg) => eprint!("\n  ⚠ {msg}"),
            StreamEvent::Error(msg) => eprint!("\n  error: {msg}"),
            // The turn is blocked until this is answered, so ask right here on
            // the same stdin the REPL already owns.
            StreamEvent::Prompt {
                question,
                options,
                default,
            } => {
                eprintln!("\n  ? {question}");
                eprint!("  [{}] (Enter = {default}): ", options.join("/"));
                let _ = io::stderr().flush();
                let mut typed = String::new();
                let answer = match io::stdin().read_line(&mut typed) {
                    Ok(_) if !typed.trim().is_empty() => typed.trim().to_string(),
                    _ => default,
                };
                if let Err(err) = transport.answer(session, &answer) {
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

/// Check if a gateway is reachable at `addr`; if not, spawn one and wait until
/// it answers a TCP connection (up to 10 s). Returns the child handle so the
/// caller keeps it alive for the duration of the process.
///
/// Only the `--addr` path needs this. The stdio transport spawns its own
/// gateway and owns the pipes, so there is no port to poll.
fn ensure_gateway(addr: &str) -> Option<Child> {
    if is_up(addr) {
        return None;
    }
    let bin = gateway_bin();
    eprintln!(
        "jan-klod: gateway not found at {addr}, starting {} …",
        bin.display()
    );
    let child = Command::new(&bin)
        // `--bind` rather than positional paths, so config/ext stay unnamed and
        // the gateway resolves them itself instead of defaulting to the cwd.
        .args(["serve", "--bind", addr])
        .stdin(Stdio::null())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .spawn();
    match child {
        Err(err) => {
            eprintln!(
                "jan-klod: could not start gateway ({}): {err}",
                bin.display()
            );
            eprintln!("jan-klod: start it manually: jan-klod-gateway serve --bind {addr}");
            None
        }
        Ok(child) => {
            wait_for_gateway(addr, Duration::from_secs(10));
            Some(child)
        }
    }
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
        &addr
            .parse()
            .unwrap_or_else(|_| DEFAULT_ADDR.parse().expect("a valid default address")),
        Duration::from_millis(300),
    )
    .is_ok()
}

#[cfg(test)]
mod tests {
    use super::{parse_args, split_mode};

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| (*s).to_string()).collect()
    }

    #[test]
    fn no_arguments_means_stdio_and_the_default_session() {
        let (tui, rest) = split_mode(&args(&[]));
        assert!(!tui);
        let parsed = parse_args(&rest).expect("parses");
        assert_eq!(parsed.addr, None, "no address means spawn one");
        assert_eq!(parsed.session, None);
    }

    #[test]
    fn a_session_id_is_the_only_positional() {
        // The case the old form got wrong: this is a session, not a host:port.
        let (_, rest) = split_mode(&args(&["my-session"]));
        let parsed = parse_args(&rest).expect("parses");
        assert_eq!(parsed.addr, None);
        assert_eq!(parsed.session.as_deref(), Some("my-session"));
    }

    #[test]
    fn the_mode_word_is_stripped_in_both_spellings() {
        for spelling in ["tui", "--tui"] {
            let (tui, rest) = split_mode(&args(&[spelling, "s1"]));
            assert!(tui, "{spelling}");
            assert_eq!(
                parse_args(&rest).expect("parses").session.as_deref(),
                Some("s1")
            );
        }
    }

    #[test]
    fn addr_selects_rest_in_either_spelling_and_either_order() {
        for form in [
            vec!["--addr", "10.0.0.2:9000", "s1"],
            vec!["s1", "--addr", "10.0.0.2:9000"],
            vec!["--addr=10.0.0.2:9000", "s1"],
        ] {
            let parsed = parse_args(&args(&form)).expect("parses");
            assert_eq!(parsed.addr.as_deref(), Some("10.0.0.2:9000"), "{form:?}");
            assert_eq!(parsed.session.as_deref(), Some("s1"), "{form:?}");
        }
    }

    #[test]
    fn a_bare_addr_flag_takes_the_default_address() {
        let parsed = parse_args(&args(&["--addr"])).expect("parses");
        assert_eq!(parsed.addr.as_deref(), Some(super::DEFAULT_ADDR));
    }

    #[test]
    fn an_address_where_a_session_belongs_names_the_flag() {
        // Every spelling the old positional form accepted, now refused with the
        // fix in the message rather than treated as a session id.
        for typo in [
            "127.0.0.1:8787",
            "localhost:8787",
            "[::1]:8787",
            ":8787",
            "10.0.0.5:80",
        ] {
            let err = parse_args(&args(&[typo])).expect_err(typo);
            assert!(err.contains("--addr"), "{typo}: {err}");
        }
        // And a session id that merely contains a colon is left alone.
        for session in ["cli", "notes:2024", "a:b"] {
            assert_eq!(
                parse_args(&args(&[session]))
                    .expect(session)
                    .session
                    .as_deref(),
                Some(session)
            );
        }
    }

    #[test]
    fn a_second_positional_is_refused_rather_than_ignored() {
        let err = parse_args(&args(&["s1", "s2"])).expect_err("two sessions");
        assert!(err.contains("session id"), "{err}");
    }

    #[test]
    fn an_unknown_option_is_refused() {
        let err = parse_args(&args(&["--bind", "x"])).expect_err("not our flag");
        assert!(err.contains("--bind"), "{err}");
    }
}
