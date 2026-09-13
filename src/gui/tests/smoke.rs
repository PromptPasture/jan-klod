//! Automatable half of slice 17b-2's acceptance: window starts, loads the page,
//! and the token arrives in `sessionStorage` under the key the web client reads.
//!
//! **What this doesn't prove:** Nobody looks at a screen. The webview fetching
//! and running script is evidence of rendering, not a sighting — #142 separates
//! them. This file covers the automation; manual rendering is in the changelog.
//!
//! No HTTP dependency: the server is forty lines of `std::net` (less than
//! explaining a new package in a tree measured at +256 by #141).
//!
//! ## Skips, and stopping them being a lie
//!
//! A webview needs a display server. On Linux without `DISPLAY`/`WAYLAND_DISPLAY`,
//! these skip. Set `JK_REQUIRE_GUI=1` to fail instead (same as `JK_REQUIRE_GUESTS`).

use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

/// Timeout for webview to start and fetch two URLs (generous for cold WebKit on CI).
const TIMEOUT: Duration = Duration::from_secs(30);

/// Token key read by `src/web/src/api.ts`, restated to catch drift (not imported).
const TOKEN_KEY: &str = "jan-klod-token";

/// Whether a window can be opened at all here.
fn has_a_display() -> bool {
    if cfg!(target_os = "macos") || cfg!(target_os = "windows") {
        return true;
    }
    std::env::var_os("DISPLAY").is_some() || std::env::var_os("WAYLAND_DISPLAY").is_some()
}

/// Skip unless environment insists otherwise. Returns `false` if body shouldn't run.
fn runnable() -> bool {
    if has_a_display() {
        return true;
    }
    assert!(
        std::env::var_os("JK_REQUIRE_GUI").is_none(),
        "JK_REQUIRE_GUI=1 is set but there is no DISPLAY or WAYLAND_DISPLAY — \
         the webview cannot open a window, so this suite would have proved \
         nothing"
    );
    eprintln!("smoke: no display; skipping (set JK_REQUIRE_GUI=1 to make this a failure)");
    false
}

/// One-page HTTP server on an ephemeral port.
///
/// `/` serves a page whose script reports `sessionStorage`; other paths get `200`.
/// Request lines are sent down the channel for assertions.
fn serve() -> (u16, Receiver<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("an ephemeral port");
    let port = listener.local_addr().expect("a bound address").port();
    let (tx, rx) = mpsc::channel();

    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(stream) = stream else { break };
            // A webview opens and drops connections of its own accord
            // (favicons, keep-alives), so one failed exchange is not the end of
            // the server — the result is deliberately discarded.
            let _ = handle(stream, &tx);
        }
    });

    (port, rx)
}

fn handle(mut stream: TcpStream, tx: &mpsc::Sender<String>) -> std::io::Result<()> {
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut request_line = String::new();
    reader.read_line(&mut request_line)?;
    if request_line.is_empty() {
        return Ok(());
    }
    // Drain headers so client doesn't block on a full buffer (until blank line or EOF).
    let mut line = String::new();
    loop {
        line.clear();
        if reader.read_line(&mut line)? == 0 || line.trim().is_empty() {
            break;
        }
    }

    let path = request_line.split_whitespace().nth(1).unwrap_or("/");
    let _ = tx.send(request_line.trim().to_string());

    let body = if path == "/" {
        format!(
            "<!doctype html><meta charset=utf-8><title>smoke</title>\
             <body><p>smoke</p><script>\
             var t = null; try {{ t = window.sessionStorage.getItem({TOKEN_KEY:?}); }} catch (e) {{}}\
             fetch('/seen?token=' + encodeURIComponent(t === null ? '<null>' : t));\
             </script>"
        )
    } else {
        String::new()
    };

    write!(
        stream,
        "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\n\
         Content-Length: {}\r\nConnection: close\r\n\r\n{}",
        body.len(),
        body
    )?;
    stream.flush()
}

/// Wait for a request line matching `want`; return it.
fn wait_for(rx: &Receiver<String>, want: impl Fn(&str) -> bool, what: &str) -> String {
    let deadline = Instant::now() + TIMEOUT;
    let mut seen = Vec::new();
    while Instant::now() < deadline {
        match rx.recv_timeout(Duration::from_millis(250)) {
            Ok(line) => {
                if want(&line) {
                    return line;
                }
                seen.push(line);
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
    panic!("timed out waiting for {what}; the window asked for: {seen:?}");
}

/// Child process, killed when test ends.
struct Window(std::process::Child);

impl Drop for Window {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn open(port: u16, token: Option<&str>) -> Window {
    let mut command = std::process::Command::new(env!("CARGO_BIN_EXE_jan-klod-gui"));
    command.args(["--url", &format!("http://127.0.0.1:{port}/")]);
    match token {
        Some(token) => command.env("JAN_KLOD_TOKEN", token),
        // Remove rather than leave alone: developer may have one exported.
        None => command.env_remove("JAN_KLOD_TOKEN"),
    };
    Window(command.spawn().expect("gui binary built by cargo"))
}

/// Token from launcher reaches the page without typing.
#[test]
fn the_window_loads_the_page_and_the_token_is_already_there() {
    if !runnable() {
        return;
    }
    let (port, rx) = serve();
    let _window = open(port, Some("smoke-token-42"));

    wait_for(&rx, |l| l.starts_with("GET / "), "the page itself");
    let seen = wait_for(&rx, |l| l.contains("/seen?token="), "the page's report");
    assert!(
        seen.contains("token=smoke-token-42"),
        "the page did not find the seeded token: {seen}"
    );
}

/// With no token, the page must see nothing (not empty string), or it sends
/// `Bearer ` and the gateway rejects it with no explanation.
#[test]
fn with_no_token_the_page_finds_nothing_rather_than_an_empty_string() {
    if !runnable() {
        return;
    }
    let (port, rx) = serve();
    let _window = open(port, None);

    wait_for(&rx, |l| l.starts_with("GET / "), "the page itself");
    let seen = wait_for(&rx, |l| l.contains("/seen?token="), "the page's report");
    assert!(
        seen.contains("token=%3Cnull%3E"),
        "storage should hold no key at all, got: {seen}"
    );
}
