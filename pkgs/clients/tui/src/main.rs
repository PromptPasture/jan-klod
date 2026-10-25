//! `jan-klod` — TUI client for the jan-klod gateway.

mod tui;

use std::io::{self, IsTerminal, Write};
use std::net::TcpStream;
use std::process::ExitCode;
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use jk_tui::transport::{Logs, Rest, Stdio as StdioTransport, Transport};
use jk_tui::{gateway_bin, StreamEvent};

const DEFAULT_ADDR: &str = "127.0.0.1:8787";

const USAGE: &str = "\
Usage: jan-klod [tui|repl] [--addr <host:port>] [--ascii] [session]

  session        session id, shared across the conversation (default: cli)
  tui            full-screen terminal UI; the default when stdin and stdout are terminals
  repl           line REPL; the default otherwise
  --addr <addr>  drive a gateway running there; without it, one is spawned
  --ascii        draw with ASCII glyphs whatever the locale says
  -V, --version  print the version
  -h, --help     print this help
";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if let Some(text) = info(&args) {
        print!("{text}");
        return ExitCode::SUCCESS;
    }
    let terminal = io::stdin().is_terminal() && io::stdout().is_terminal();
    let (mode, rest) = split_mode(&args, terminal);
    let parsed = match parse_args(&rest) {
        Ok(parsed) => parsed,
        Err(message) => {
            eprintln!("{message}");
            return ExitCode::FAILURE;
        }
    };

    let use_tui = mode == Mode::Tui;
    let session = parsed.session.unwrap_or_else(|| "cli".to_string());

    let logs = if use_tui {
        Logs::Discard
    } else {
        Logs::Inherit
    };
    let (transport, _gateway): (Arc<dyn Transport>, Option<Child>) = match parsed.addr {
        Some(addr) => {
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
        return match tui::run(&transport, &session, parsed.ascii) {
            Ok(()) => ExitCode::SUCCESS,
            Err(err) => {
                eprintln!("jan-klod: {err}");
                ExitCode::FAILURE
            }
        };
    }

    repl(&transport, &session)
}

#[derive(Debug)]
struct Args {
    addr: Option<String>,
    session: Option<String>,
    ascii: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    /// The line REPL: the default without a terminal, since it needs no terminal capability.
    Repl,
    /// The full-screen UI: the default when stdin and stdout are both terminals.
    Tui,
}

/// Only the first argument selects the mode; without one, `terminal` decides.
fn split_mode(args: &[String], terminal: bool) -> (Mode, Vec<String>) {
    match args.split_first() {
        Some((first, rest)) if first == "tui" || first == "--tui" => (Mode::Tui, rest.to_vec()),
        Some((first, rest)) if first == "repl" || first == "--repl" => (Mode::Repl, rest.to_vec()),
        _ if terminal => (Mode::Tui, args.to_vec()),
        _ => (Mode::Repl, args.to_vec()),
    }
}

fn info(args: &[String]) -> Option<String> {
    match args.first().map(String::as_str) {
        Some("--version" | "-V") => Some(format!("jan-klod {}\n", env!("CARGO_PKG_VERSION"))),
        Some("--help" | "-h") => Some(USAGE.to_string()),
        _ => None,
    }
}

fn parse_args(args: &[String]) -> Result<Args, String> {
    let mut addr = None;
    let mut session = None;
    let mut ascii = false;
    let mut rest = args.iter();
    while let Some(arg) = rest.next() {
        if arg == "--addr" {
            addr = Some(rest.next().map_or_else(
                || DEFAULT_ADDR.to_string(),
                std::string::ToString::to_string,
            ));
        } else if let Some(value) = arg.strip_prefix("--addr=") {
            addr = Some(value.to_string());
        } else if arg == "--ascii" {
            ascii = true;
        } else if arg.starts_with('-') {
            return Err(format!("jan-klod: unknown option `{arg}`"));
        } else if session.is_some() {
            return Err(format!(
                "jan-klod: `{arg}` is a second session id. Usage: jan-klod [tui|repl] \
                 [--addr <host:port>] [--ascii] [session]"
            ));
        } else if looks_like_an_address(arg) {
            return Err(format!(
                "jan-klod: `{arg}` looks like an address, not a session id. Use \
                 `--addr {arg}` to drive a gateway that is already running."
            ));
        } else {
            session = Some(arg.clone());
        }
    }
    Ok(Args {
        addr,
        session,
        ascii,
    })
}

/// Whether a positional arg looks like `host:port`: the port tail must be digits.
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
            StreamEvent::Tool { name, .. } => eprint!("\n  ⚙ {name}… "),
            StreamEvent::ToolResult { .. } => eprint!("✓"),
            StreamEvent::Warning(msg) => eprint!("\n  ⚠ {msg}"),
            StreamEvent::Error(msg) => eprint!("\n  error: {msg}"),
            StreamEvent::Contributions(_) => {}
            StreamEvent::Prompt {
                session: prompt_session,
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
                if let Err(err) = transport.answer(&prompt_session, &answer) {
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
    use super::{info, parse_args, split_mode, Mode};

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| (*s).to_string()).collect()
    }

    #[test]
    fn no_arguments_means_stdio_and_the_default_session() {
        let (mode, rest) = split_mode(&args(&[]), false);
        assert_eq!(mode, Mode::Repl);
        let parsed = parse_args(&rest).expect("parses");
        assert_eq!(parsed.addr, None, "no address means spawn one");
        assert_eq!(parsed.session, None);
    }

    #[test]
    fn a_session_id_is_the_only_positional() {
        let (_, rest) = split_mode(&args(&["my-session"]), true);
        let parsed = parse_args(&rest).expect("parses");
        assert_eq!(parsed.addr, None);
        assert_eq!(parsed.session.as_deref(), Some("my-session"));
    }

    #[test]
    fn the_mode_word_is_stripped_in_both_spellings() {
        for (spelling, want) in [
            ("tui", Mode::Tui),
            ("--tui", Mode::Tui),
            ("repl", Mode::Repl),
            ("--repl", Mode::Repl),
        ] {
            let (mode, rest) = split_mode(&args(&[spelling, "s1"]), true);
            assert_eq!(mode, want, "{spelling}");
            assert_eq!(
                parse_args(&rest).expect("parses").session.as_deref(),
                Some("s1")
            );
        }
    }

    #[test]
    fn a_terminal_opens_the_tui_without_a_mode_word() {
        let (mode, rest) = split_mode(&args(&["s1"]), true);
        assert_eq!(mode, Mode::Tui);
        assert_eq!(rest, args(&["s1"]));
    }

    #[test]
    fn without_a_terminal_the_default_is_the_repl() {
        let (mode, rest) = split_mode(&args(&["s1"]), false);
        assert_eq!(mode, Mode::Repl, "the TUI cannot draw on a pipe");
        assert_eq!(rest, args(&["s1"]));
    }

    #[test]
    fn the_repl_can_be_chosen_from_a_terminal() {
        let (mode, rest) = split_mode(&args(&["repl", "s1"]), true);
        assert_eq!(mode, Mode::Repl);
        assert_eq!(rest, args(&["s1"]));
    }

    #[test]
    fn a_mode_word_that_is_not_first_is_not_a_mode() {
        let (mode, rest) = split_mode(&args(&["s1", "--tui"]), false);
        assert_eq!(mode, Mode::Repl);
        let err = parse_args(&rest).expect_err("--tui is not an option");
        assert!(err.contains("--tui"), "{err}");
    }

    #[test]
    fn version_and_help_are_answered_before_any_parsing() {
        let version = info(&args(&["--version"])).expect("--version is answered");
        assert_eq!(version, format!("jan-klod {}\n", env!("CARGO_PKG_VERSION")));
        let help = info(&args(&["--help"])).expect("--help is answered");
        assert!(help.starts_with("Usage: jan-klod"), "{help}");
        assert_eq!(info(&args(&["-V"])), Some(version), "-V is --version");
        assert_eq!(info(&args(&["-h"])), Some(help), "-h is --help");
        assert!(info(&args(&["my-session"])).is_none());
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
