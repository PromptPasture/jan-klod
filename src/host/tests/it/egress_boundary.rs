//! Network-granted components cannot reach their own machine. `tool-fetch`'s SSRF
//! guard runs in-sandbox, catching confused models not malicious omissions.
//! Tests assert on real HTTP server request counts, not errors, distinguishing a
//! real boundary from guest-side courtesy.
//!
//! Tests drive `fetch_within` (host backend) directly: honest guests self-censor
//! before boundary exercised, so testing through one vacuates assertions.
//!
//! Verifies: policy from `config.yaml` refuses unconfigured locals, permits
//! operator-named ones at the function every backend calls; and
//! [`every_guest_facing_backend_goes_through_the_policy`] ensures no backend
//! reverts to the unbounded client.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use jan_klod_core::Runtime;

use crate::common;

/// Long enough to bound failure, not healthy requests, so it doesn't pace the
/// test. Mirrors `local_model.rs` `FakeOllama` constant.
const REQUEST_DEADLINE: Duration = Duration::from_secs(10);

/// Loopback server counting requests, answering all — so successful guest
/// escapes see plausible replies, not connection errors.
struct Sentinel {
    port: u16,
    hits: Arc<AtomicU32>,
    stop: Arc<AtomicU32>,
    /// I/O failures. Discarding them let it answer before reading, the defect
    /// in #78 and #83. Tests trusting `hits()` must assert this is empty
    /// first.
    faults: Arc<Mutex<Vec<String>>>,
}

impl Sentinel {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("binds loopback");
        let port = listener.local_addr().expect("has an address").port();
        // Listener stays blocking: BSD `accept()` inherits `O_NONBLOCK` (#78).
        // Non-blocking listeners made sockets non-blocking on macOS, blocking on
        // Linux. Drop unblocks accept by connecting, no poll-sleep races.
        let hits = Arc::new(AtomicU32::new(0));
        let stop = Arc::new(AtomicU32::new(0));
        let faults = Arc::new(Mutex::new(Vec::new()));
        let (counted, stopped, faulted) =
            (Arc::clone(&hits), Arc::clone(&stop), Arc::clone(&faults));
        let fault = move |what: &str, e: &dyn std::fmt::Debug| {
            if let Ok(mut log) = faulted.lock() {
                log.push(format!("{what}: {e:?}"));
            }
        };
        thread::spawn(move || {
            loop {
                match listener.accept() {
                    Ok((mut socket, _)) => {
                        // Drop's connection carries no request, leave before
                        // counting.
                        if stopped.load(Ordering::Relaxed) != 0 {
                            break;
                        }
                        counted.fetch_add(1, Ordering::Relaxed);
                        // Read whole request under deadline before answering: one
                        // `read` may return partial/nothing, answering early
                        // leaves rest unread. Close sends RST not FIN, RST
                        // discards written response — client sees failure for
                        // already-counted hit.
                        if let Err(e) = socket.set_read_timeout(Some(REQUEST_DEADLINE)) {
                            fault("set_read_timeout", &e);
                        }
                        if let Err(e) = common::read_request(&mut socket) {
                            fault("read_request", &e);
                            continue;
                        }
                        if let Err(e) = socket.write_all(
                            b"HTTP/1.1 200 OK\r\nContent-Length: 6\r\nConnection: close\r\n\r\nsecret",
                        ) {
                            fault("write_all", &e);
                        }
                    }
                    // Listener that can't accept serves nothing — break rather
                    // than spin. Tests read `faults` before trusting sentinel.
                    Err(e) => {
                        fault("accept", &e);
                        break;
                    }
                }
            }
        });
        Self {
            port,
            hits,
            stop,
            faults,
        }
    }

    fn hits(&self) -> u32 {
        self.hits.load(Ordering::Relaxed)
    }

    /// Assert sentinel's I/O succeeded so `hits()` reflects read requests, not
    /// blind answers. Call before trusting `hits()` — same as `FakeOllama`: a
    /// silent read discard let endpoints answer unread requests.
    fn assert_no_faults(&self) {
        let faults = self.faults.lock().expect("not poisoned").clone();
        assert!(
            faults.is_empty(),
            "the sentinel could not complete its own I/O, so `hits()` cannot be \
             trusted: {faults:?}"
        );
    }
}

impl Drop for Sentinel {
    fn drop(&mut self) {
        self.stop.store(1, Ordering::Relaxed);
        // Unblock the accept loop.
        let _ = TcpStream::connect(("127.0.0.1", self.port));
    }
}

fn config_at(dir: &std::path::Path, extra: &str) -> std::path::PathBuf {
    let path = dir.join("config.yaml");
    std::fs::write(
        &path,
        format!(
            "
extensions:
  provider:
    openai:
      enabled: true
      base-url: http://mock/v1
      model: mock-1
      api-key: test
  tool:
    fetch:
      enabled: true
      network: true
{extra}
"
        ),
    )
    .unwrap();
    path
}

/// Policy derived from `config`, then one request through host's real HTTP
/// backend — the exact function served to guest's `host-http`.
fn request_through_the_host(config: &std::path::Path, url: &str) -> Result<(), ()> {
    let runtime = Runtime::boot(config, common::repo_root().join("ext")).expect("boots");
    let policy = runtime.egress_policy();
    jan_klod_core::http::fetch_within(&policy, "GET", url, &[], None, 2_000)
        .map(|_| ())
        .map_err(|_| ())
}

#[test]
fn a_live_local_service_is_not_reachable() {
    let dir = std::env::temp_dir().join(format!("jk-egress-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let _guard = common::TempDir(dir.clone());

    let sentinel = Sentinel::start();
    let config = config_at(&dir, "");
    let url = format!("http://127.0.0.1:{}/secrets", sentinel.port);
    assert!(
        request_through_the_host(&config, &url).is_err(),
        "the request is refused"
    );

    sentinel.assert_no_faults();
    // Boundary vs. courtesy: server never saw any request.
    assert_eq!(
        sentinel.hits(),
        0,
        "a request reached a service on the host — this is the gateway's own port, \
         the cloud metadata endpoint, and the LAN, all of which were reachable by \
         any component granted the network"
    );
}

/// Rule is per-origin, not a global switch: self-hosted models on loopback
/// need reachability.
#[test]
fn an_endpoint_named_in_config_is_reachable() {
    let dir = std::env::temp_dir().join(format!("jk-egress-ok-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let _guard = common::TempDir(dir.clone());

    let sentinel = Sentinel::start();
    // How operators name local Ollama or MCP servers.
    let allow = format!(
        "\nnetwork:\n  allow:\n    - http://127.0.0.1:{}\n",
        sentinel.port
    );
    let config = config_at(&dir, &allow);
    let url = format!("http://127.0.0.1:{}/models", sentinel.port);

    assert!(
        request_through_the_host(&config, &url).is_ok(),
        "the named origin answers"
    );
    // Before trusting `hits()`: unread-request answers (#83 shape) can pass
    // count while assertion failed for socket reasons.
    sentinel.assert_no_faults();
    assert_eq!(sentinel.hits(), 1, "and the request actually arrived");
}

/// Provider `base-url` is an allowlisted endpoint — the common case for local
/// model config; no need to name it twice.
#[test]
fn a_providers_base_url_is_allowed_without_naming_it_again() {
    let dir = std::env::temp_dir().join(format!("jk-egress-base-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let _guard = common::TempDir(dir.clone());

    let sentinel = Sentinel::start();
    let path = dir.join("config.yaml");
    std::fs::write(
        &path,
        format!(
            "
extensions:
  provider:
    ollama:
      enabled: true
      type: openai
      base-url: http://127.0.0.1:{}/v1
      model: local
      api-key: none
",
            sentinel.port
        ),
    )
    .unwrap();

    let url = format!("http://127.0.0.1:{}/v1/chat/completions", sentinel.port);
    assert!(
        request_through_the_host(&path, &url).is_ok(),
        "the configured model answers"
    );
    sentinel.assert_no_faults();
    assert_eq!(sentinel.hits(), 1);
}

/// Granting one origin must not open the machine.
#[test]
fn a_grant_covers_one_origin_and_not_its_neighbours() {
    let dir = std::env::temp_dir().join(format!("jk-egress-nb-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let _guard = common::TempDir(dir.clone());

    let allowed = Sentinel::start();
    let neighbour = Sentinel::start();
    let allow = format!(
        "\nnetwork:\n  allow:\n    - http://127.0.0.1:{}\n",
        allowed.port
    );
    let config = config_at(&dir, &allow);

    let _ = request_through_the_host(&config, &format!("http://127.0.0.1:{}/", neighbour.port));
    neighbour.assert_no_faults();
    assert_eq!(
        neighbour.hits(),
        0,
        "granting the model's port must not grant the database beside it"
    );
}

/// Every host-http serving point must consult policy. One backend calling
/// unbounded client reopens hole undetected by above tests. Scans for unbounded
/// `http::fetch` instead.
#[test]
fn every_guest_facing_backend_goes_through_the_policy() {
    // Scanned, not hard-coded — literal list misses new backends added later.
    let core = common::repo_root().join("src/core/src");
    let mut backends = Vec::new();
    for entry in std::fs::read_dir(&core)
        .expect("core sources are readable")
        .flatten()
    {
        let path = entry.path();
        if path.extension().is_none_or(|e| e != "rs") {
            continue;
        }
        let text = std::fs::read_to_string(&path).expect("a core source is readable");
        // Host-side implementations of guest's `host-http` import, any binding
        // module name.
        if text.contains("_http::Host for") {
            backends.push((path, text));
        }
    }
    assert!(
        backends.len() >= 4,
        "only {} host-http backends found — the scan is broken, and a broken scan \
         here reports success while the boundary is unguarded",
        backends.len()
    );

    for (path, text) in &backends {
        let name = path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        for (number, line) in text.lines().enumerate() {
            // Code only — skip comments would flag ban prose.
            let trimmed = line.trim_start();
            if trimmed.starts_with("//") {
                continue;
            }
            // `contains` not `starts_with`: call sites `let result = crate::http::fetch(`.
            // Trailing `(` excludes `fetch_within(`.
            assert!(
                !line.contains("crate::http::fetch(")
                    && !line.contains("jan_klod_core::http::fetch("),
                "{name}:{} calls the unbounded client. A guest's egress must go \
                 through `fetch_within`, or it can reach this gateway's own port, \
                 the cloud metadata service, and the LAN.",
                number + 1
            );
        }
    }

    // Other entry: callers *handing* guests the unbounded client. Providers and
    // tools get HttpFn from builders, so binary passing `Box::new(http::fetch)`
    // reopens hole without touching scanned files.
    //
    // `expect` not silent `continue`: hand-named files; rename moving one out
    // leaves check green with nothing scanned (happened with `ui/` → `tui/`).
    // Missing file = broken test, not absent binary.
    for binary in ["host/src/main.rs", "tui/src/main.rs"] {
        let path = common::repo_root().join("src").join(binary);
        let text = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("{} is readable: {e}", path.display()));
        for (number, line) in text.lines().enumerate() {
            let trimmed = line.trim_start();
            if trimmed.starts_with("//") {
                continue;
            }
            assert!(
                !line.contains("Box::new(jan_klod_core::http::fetch)")
                    && !line.contains("Box::new(crate::http::fetch)"),
                "{binary}:{} hands a guest the unbounded client as its `host-http`. \
                 Wrap it in the runtime's egress policy with `fetch_within`.",
                number + 1
            );
        }
    }
}

// ---- Redirects (#107) ----

/// Server answering all requests with `302` to `target`, counts hits.
pub fn redirector_to(target: String) -> (u16, Arc<AtomicU32>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("binds loopback");
    let port = listener.local_addr().expect("has an address").port();
    let hits = Arc::new(AtomicU32::new(0));
    let counted = Arc::clone(&hits);
    thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut socket) = stream else { continue };
            counted.fetch_add(1, Ordering::SeqCst);
            let mut seen = Vec::new();
            let mut byte = [0_u8; 1];
            while socket.read(&mut byte).unwrap_or(0) == 1 {
                seen.push(byte[0]);
                if seen.ends_with(b"\r\n\r\n") {
                    break;
                }
            }
            let _ = socket.write_all(
                format!(
                    "HTTP/1.1 302 Found\r\nLocation: {target}\r\nContent-Length: 0\r\n\
                     Connection: close\r\n\r\n"
                )
                .as_bytes(),
            );
        }
    });
    (port, hits)
}

/// Permitted origins cannot redirect to refused ones. The redirector is
/// explicitly allowed so policy says yes to caller's URL. Beyond that, the
/// redirect's doing — the policy never saw it.
#[test]
fn a_permitted_origin_cannot_redirect_to_a_refused_one() {
    let forbidden = common::Countable::start();
    let target = format!("http://127.0.0.1:{}/secrets", forbidden.port);
    let (redirect_port, redirect_hits) = redirector_to(target);
    let entry = format!("http://127.0.0.1:{redirect_port}/start");

    // Redirector operator-named; sentinel is not.
    let policy = jan_klod_core::egress::EgressPolicy::public_only()
        .allowing(&format!("http://127.0.0.1:{redirect_port}"));

    let outcome = jan_klod_core::http::fetch_within(&policy, "GET", &entry, &[], None, 2_000);

    // Control: request reached permitted origin, so hits == 0 can't mean nothing
    // happened.
    assert_eq!(
        redirect_hits.load(Ordering::SeqCst),
        1,
        "the permitted origin was reached — otherwise this test proves nothing"
    );

    // Caller gets *policy* refusal, not 302. Per-hop loop changed this: #107's
    // first box declined-then-surfaced; now hop checked and refused (safe,
    // legible).
    assert_eq!(
        outcome.err(),
        Some(jan_klod_core::http::WireError::ConnectionFailed),
        "the refused hop is reported as the policy refusing it"
    );

    // The assertion this test exists for.
    forbidden.only_hit();
}

/// Server redirecting once, then serving — records headers and method of all
/// requests to assert what survived hops.
struct Hops {
    port: u16,
    seen: Arc<std::sync::Mutex<Vec<String>>>,
}

impl Hops {
    /// First request gets `status` and `Location`, rest get `200 landed`.
    fn start(status: u16, location: String) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("binds loopback");
        let port = listener.local_addr().expect("has an address").port();
        let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
        let recorded = Arc::clone(&seen);
        thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut socket) = stream else { continue };
                let mut request = Vec::new();
                let mut byte = [0_u8; 1];
                while socket.read(&mut byte).unwrap_or(0) == 1 {
                    request.push(byte[0]);
                    if request.ends_with(b"\r\n\r\n") {
                        break;
                    }
                }
                let text = String::from_utf8_lossy(&request).into_owned();
                let first = recorded.lock().expect("not poisoned").is_empty();
                recorded.lock().expect("not poisoned").push(text);
                let reply = if first {
                    format!(
                        "HTTP/1.1 {status} Moved\r\nLocation: {location}\r\n\
                         Content-Length: 0\r\nConnection: close\r\n\r\n"
                    )
                } else {
                    "HTTP/1.1 200 OK\r\nContent-Length: 6\r\nConnection: close\r\n\r\nlanded"
                        .to_owned()
                };
                let _ = socket.write_all(reply.as_bytes());
            }
        });
        Self { port, seen }
    }

    fn requests(&self) -> Vec<String> {
        self.seen.lock().expect("not poisoned").clone()
    }
}

/// Redirects to permitted destinations still follow — fix for #107 isn't
/// "break all redirects".
#[test]
fn a_redirect_to_a_permitted_destination_is_followed() {
    // Same origin: one grant covers both hops.
    let hops = Hops::start(302, "/landed".to_owned());
    let policy = jan_klod_core::egress::EgressPolicy::public_only()
        .allowing(&format!("http://127.0.0.1:{}", hops.port));
    let entry = format!("http://127.0.0.1:{}/start", hops.port);

    let response = jan_klod_core::http::fetch_within(&policy, "GET", &entry, &[], None, 2_000)
        .expect("a permitted redirect is followed");
    assert_eq!(
        response.status, 200,
        "the second hop's response is returned"
    );
    assert_eq!(response.body, b"landed");

    let requests = hops.requests();
    assert_eq!(requests.len(), 2, "two hops were made: {requests:?}");
    assert!(
        requests[1].starts_with("GET /landed"),
        "relative Location resolves against first URL: {:?}",
        requests[1]
    );
}

/// `Authorization` doesn't survive redirects. ureq stripped it
/// (`RedirectAuthHeaders::Never`); hand-following means deliberate stripping,
/// else API keys leak to redirect targets — credential leak introduced by
/// policy-bypass fix.
#[test]
fn credentials_do_not_survive_a_redirect() {
    let hops = Hops::start(302, "/landed".to_owned());
    let policy = jan_klod_core::egress::EgressPolicy::public_only()
        .allowing(&format!("http://127.0.0.1:{}", hops.port));
    let entry = format!("http://127.0.0.1:{}/start", hops.port);

    let headers = vec![
        ("Authorization".to_owned(), "Bearer sk-secret".to_owned()),
        ("X-Kept".to_owned(), "yes".to_owned()),
    ];
    jan_klod_core::http::fetch_within(&policy, "GET", &entry, &headers, None, 2_000)
        .expect("follows");

    let requests = hops.requests();
    assert_eq!(requests.len(), 2, "{requests:?}");
    assert!(
        requests[0].contains("sk-secret"),
        "first hop carried it, else test proves nothing: {:?}",
        requests[0]
    );
    assert!(
        !requests[1].contains("sk-secret"),
        "second did not: {:?}",
        requests[1]
    );
    assert!(
        requests[1].to_ascii_lowercase().contains("x-kept"),
        "ordinary headers travel (lowercased): {:?}",
        requests[1]
    );
}

/// `302` turns POST→GET, drops body; `307` preserves method and body. Silent
/// when wrong: request succeeds with unexpected method/body.
#[test]
fn a_302_becomes_a_get_and_a_307_keeps_the_post() {
    for (status, expected) in [(302_u16, "GET"), (307, "POST")] {
        let hops = Hops::start(status, "/landed".to_owned());
        let policy = jan_klod_core::egress::EgressPolicy::public_only()
            .allowing(&format!("http://127.0.0.1:{}", hops.port));
        let entry = format!("http://127.0.0.1:{}/start", hops.port);

        jan_klod_core::http::fetch_within(&policy, "POST", &entry, &[], Some(b"payload"), 2_000)
            .expect("follows");

        let requests = hops.requests();
        assert_eq!(requests.len(), 2, "{status}: {requests:?}");
        assert!(
            requests[1].starts_with(&format!("{expected} /landed")),
            "{status} arrives as {expected}: {:?}",
            requests[1]
        );
    }
}
