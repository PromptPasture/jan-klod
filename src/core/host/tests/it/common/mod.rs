// Each test binary compiles this module independently; not every binary needs every item.
#![allow(dead_code)]

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::thread;

use jan_klod_core::http::WireResponse;
use jan_klod_core::route::HttpFn;

pub mod minisig;

/// Removes the test temp directory on drop — even if the test panics.
pub struct TempDir(pub PathBuf);
impl Drop for TempDir {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).ok();
    }
}

/// Resolves the repo root from the crate manifest directory.
pub fn repo_root() -> PathBuf {
    [env!("CARGO_MANIFEST_DIR"), "..", "..", ".."]
        .iter()
        .collect()
}

/// Set this to turn "guest not staged, skip the test" into a hard failure.
/// `make gate` exports it; see [`guests_staged`].
const REQUIRE: &str = "JK_REQUIRE_GUESTS";

/// Whether every named guest is staged in `ext/` — and therefore whether the
/// caller may run.
///
/// A skipped test reports as passing, so an unstaged `ext/` can hide a broken
/// assertion indefinitely (it has happened). Skipping is allowed only when
/// nobody has asked for the real thing: with `JK_REQUIRE_GUESTS` set (`make
/// gate`, CI), a missing guest panics with what to run instead of skipping.
pub fn guests_staged(guests: &[&str]) -> bool {
    let ext_dir = repo_root().join("ext");
    let absent: Vec<&str> = guests
        .iter()
        .copied()
        .filter(|g| !ext_dir.join(g).exists())
        .collect();
    if absent.is_empty() {
        return true;
    }
    assert!(
        std::env::var(REQUIRE).is_err(),
        "{REQUIRE} is set, so this test must not be skipped, but {absent:?} \
         are not staged in {} — run `make ext`",
        ext_dir.display()
    );
    eprintln!("skipping: {absent:?} not staged — run `make ext`");
    false
}

/// Whether `program` answers a version query. Tries both spellings: `TinyGo`
/// only answers the bare `version` subcommand, not `--version`, so checking
/// only one can misreport an installed compiler as absent.
fn runnable(program: &str) -> bool {
    ["--version", "version"].iter().any(|flag| {
        std::process::Command::new(program)
            .arg(flag)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|status| status.success())
    })
}

/// Whether an optional developer toolchain is present, with **no** requirement
/// that it be. Distinct from [`tool_available`]: this is for a genuinely
/// optional toolchain (e.g. `tinygo`/`wkg`, which CI doesn't carry) where a hard
/// failure would just teach people to unset `JK_REQUIRE_GUESTS`. Skip is
/// announced, not silent.
pub fn optional_tool(program: &str) -> bool {
    if runnable(program) {
        return true;
    }
    eprintln!("skipping: optional toolchain `{program}` is not installed");
    false
}

/// Whether an external program a test needs is on `PATH`. Same policy as
/// [`guests_staged`] and for the same reason: a test that quietly vanishes when
/// a prerequisite is missing proves nothing while looking green.
pub fn tool_available(program: &str) -> bool {
    if runnable(program) {
        return true;
    }
    assert!(
        std::env::var(REQUIRE).is_err(),
        "{REQUIRE} is set, so this test must not be skipped, but `{program}` is not \
         on PATH"
    );
    eprintln!("skipping: `{program}` is not available");
    false
}

/// A canned chat-completions reply, so the routed provider completes offline.
pub fn canned_http(content: &'static str) -> HttpFn {
    Box::new(move |_m, _u, _h, _b, _t| {
        let body = serde_json::json!({
            "choices": [{
                "message": { "role": "assistant", "content": content },
                "finish_reason": "stop"
            }]
        });
        Ok(WireResponse {
            status: 200,
            headers: vec![],
            body: serde_json::to_vec(&body).unwrap(),
        })
    })
}

/// A loopback server whose request count can be asserted to be **zero**
/// without a race.
///
/// `egress_boundary.rs`'s own `Sentinel` cannot be used for that. It polls a
/// non-blocking `accept()` with 20ms sleeps, so a connection that *was* made
/// may not be counted yet when the assertion reads the counter — which turns
/// "the server saw nothing" into a false pass, the exact class of defect
/// [#83](https://github.com/PromptPasture/jan-klod/issues/83) records against
/// it. Asserting a negative on a counter that can lag is asserting nothing.
///
/// Here rather than in one test module because two now need it, and a sentinel
/// whose entire purpose is not lying is the last thing to keep two copies of.
///
/// This one closes the race rather than shortening it: [`Self::only_hit`]
/// connects once itself and waits for *that* connection to be counted, so the
/// queue is known to be drained past anything the code under test might have
/// done. If the fetch had connected, the count would be two.
pub struct Countable {
    /// The loopback port it listens on.
    pub port: u16,
    hits: Arc<AtomicU32>,
}

impl Countable {
    pub fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("binds loopback");
        let port = listener.local_addr().expect("has an address").port();
        let hits = Arc::new(AtomicU32::new(0));
        let counted = Arc::clone(&hits);
        // Blocking accept, and the thread is left to die with the test: there
        // is no stop flag to race against, and a leaked thread blocked on
        // accept costs a test binary nothing.
        thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut socket) = stream else { continue };
                counted.fetch_add(1, Ordering::SeqCst);
                // Read until the headers end rather than once into a fixed
                // buffer: a short read followed by a reply and a close is what
                // sends an RST back, which #78 and #83 both record.
                let mut seen = Vec::new();
                let mut byte = [0_u8; 1];
                while socket.read(&mut byte).unwrap_or(0) == 1 {
                    seen.push(byte[0]);
                    if seen.ends_with(b"\r\n\r\n") {
                        break;
                    }
                }
                let _ = socket.write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Length: 6\r\nConnection: close\r\n\r\nsecret",
                );
            }
        });
        Self { port, hits }
    }

    /// Assert this server was reached exactly once, by us.
    ///
    /// The self-connection is the point: it proves the counter is live *and*
    /// flushes the accept queue, so a count of one means the code under test
    /// never connected. A bare `assert_eq!(hits, 0)` could pass simply because
    /// the accepting thread had not got there yet.
    pub fn only_hit(&self) {
        let before = self.hits.load(Ordering::SeqCst);
        let mut probe =
            TcpStream::connect(("127.0.0.1", self.port)).expect("the sentinel is listening");
        probe
            .write_all(b"GET /probe HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n")
            .expect("writes the probe");
        let mut sink = Vec::new();
        let _ = probe.read_to_end(&mut sink);
        assert_eq!(
            self.hits.load(Ordering::SeqCst),
            before + 1,
            "the counter is live — if this fails the assertion below proves nothing"
        );
        assert_eq!(
            self.hits.load(Ordering::SeqCst),
            1,
            "the only connection this server ever saw is the one this test made — \
             anything more means the code under test reached it"
        );
    }
}
