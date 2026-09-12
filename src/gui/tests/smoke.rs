//! The automatable half of slice 17b-2's acceptance: the window process starts,
//! loads the page it was pointed at, and the token it was given arrives in the
//! page's `sessionStorage` under the key the web client reads.
//!
//! **What this does not prove.** Nobody here looks at a screen. The webview
//! fetching the page and running its script is strong evidence that a window
//! rendered, but it is evidence, not a sighting — #142 says to keep the two
//! apart and report which one was actually done, so this file is named for the
//! half it covers and the other half stays a manual step in the changelog.
//!
//! No HTTP dependency: the server is forty lines of `std::net`, which is less
//! than the cost of explaining a new package in a tree this repository already
//! measured at +256 (#141). It serves exactly one page and records what it was
//! asked for.
//!
//! ## Skips, and how to stop them being a lie
//!
//! A webview needs a display server. On Linux without `DISPLAY`/`WAYLAND_DISPLAY`
//! there is nothing to open a window on, so these skip. Set `JK_REQUIRE_GUI=1`
//! to turn that skip into a failure — the same lever `JK_REQUIRE_GUESTS` is for
//! the guest suite, and for the same reason: a check that silently no-ops looks
//! exactly like one that passes.

use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

/// How long to wait for a webview to start and fetch two URLs. Generous: a cold
/// `WebKit` process on a loaded CI runner is much slower than on a laptop.
const TIMEOUT: Duration = Duration::from_secs(30);

/// The key `src/web/src/api.ts` reads the token from. Restated rather than
/// shared, deliberately — this test is the thing that would catch the two
/// drifting, so importing the constant from the code under test would defeat it.
const TOKEN_KEY: &str = "jan-klod-token";

/// Whether a window can be opened at all here.
fn has_a_display() -> bool {
    if cfg!(target_os = "macos") || cfg!(target_os = "windows") {
        return true;
    }
    std::env::var_os("DISPLAY").is_some() || std::env::var_os("WAYLAND_DISPLAY").is_some()
}

/// Skip unless the environment insists otherwise. Returns `false` when the test
/// body should not run.
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

/// A one-page HTTP server on an ephemeral port.
///
/// `/` serves a page whose script reports back what it found in
/// `sessionStorage`; every other path is answered with an empty `200`. Each
/// request line it handles is sent down the channel, which is what the
/// assertions read.
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
    // Drain the headers so the client is not left writing into a full buffer.
    // Stops at the blank line that ends them, or at EOF.
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

/// Wait for a request line matching `want`, returning it.
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

/// The child, killed when the test ends however it ends.
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
        // Removed rather than left alone: the developer running this may well
        // have one exported, and the no-token case has to mean no token.
        None => command.env_remove("JAN_KLOD_TOKEN"),
    };
    Window(command.spawn().expect("the gui binary is built by cargo"))
}

/// The token the launcher was given reaches the page without anyone typing it.
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

/// With no token there is nothing to seed, and the page must see *nothing*
/// rather than an empty string — which it would send as `Bearer ` and the
/// gateway would reject, with the user left looking at a window that cannot
/// explain itself.
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
