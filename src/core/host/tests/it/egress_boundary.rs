//! A component granted the network cannot reach the machine it runs on.
//! `tool-fetch`'s SSRF guard runs *inside the sandbox*, so it protects against a
//! confused model, not a malicious component that simply omits the check —
//! `host-http` otherwise hands out an unrestricted client. These tests stand up
//! a real HTTP server on loopback and assert on *its* request count, not a
//! returned error, since only that distinguishes a real boundary from a
//! guest-side courtesy.
//!
//! They drive `fetch_within` (the host backend) directly rather than a staged
//! guest: an honest guest self-censors before this boundary is ever exercised,
//! so testing through one made every assertion here vacuous.
//!
//! Verified: the policy derived from a real `config.yaml` refuses an
//! unconfigured local service and permits the operator-named one, at the
//! function every guest-facing backend calls; and
//! [`every_guest_facing_backend_goes_through_the_policy`] checks no backend has
//! quietly gone back to the unbounded client.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::thread;

use jan_klod_core::Runtime;

use crate::common;

/// A loopback server that counts requests and answers every one, so a guest that
/// got through would see a plausible reply rather than a connection error.
struct Sentinel {
    port: u16,
    hits: Arc<AtomicU32>,
    stop: Arc<AtomicU32>,
}

impl Sentinel {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("binds loopback");
        let port = listener.local_addr().expect("has an address").port();
        listener.set_nonblocking(true).expect("nonblocking");
        let hits = Arc::new(AtomicU32::new(0));
        let stop = Arc::new(AtomicU32::new(0));
        let counted = Arc::clone(&hits);
        let stopped = Arc::clone(&stop);
        thread::spawn(move || {
            while stopped.load(Ordering::Relaxed) == 0 {
                match listener.accept() {
                    Ok((mut socket, _)) => {
                        counted.fetch_add(1, Ordering::Relaxed);
                        let mut buf = [0_u8; 1024];
                        let _ = socket.read(&mut buf);
                        let _ = socket.write_all(
                            b"HTTP/1.1 200 OK\r\nContent-Length: 6\r\nConnection: close\r\n\r\nsecret",
                        );
                    }
                    Err(_) => thread::sleep(std::time::Duration::from_millis(20)),
                }
            }
        });
        Self { port, hits, stop }
    }

    fn hits(&self) -> u32 {
        self.hits.load(Ordering::Relaxed)
    }
}

impl Drop for Sentinel {
    fn drop(&mut self) {
        self.stop.store(1, Ordering::Relaxed);
        // Unblock the accept loop so the thread notices.
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

/// The policy the runtime derives from `config`, then one request through the
/// host's real HTTP backend — the exact function served to a guest's `host-http`.
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

    // The assertion that distinguishes a boundary from a courtesy: the server
    // itself never saw anything.
    assert_eq!(
        sentinel.hits(),
        0,
        "a request reached a service on the host — this is the gateway's own port, \
         the cloud metadata endpoint, and the LAN, all of which were reachable by \
         any component granted the network"
    );
}

/// The rule is per-origin, not a global switch: a self-hosted model lives on
/// loopback too, and a blanket refusal would make it unreachable.
#[test]
fn an_endpoint_named_in_config_is_reachable() {
    let dir = std::env::temp_dir().join(format!("jk-egress-ok-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let _guard = common::TempDir(dir.clone());

    let sentinel = Sentinel::start();
    // Written the way an operator names a local Ollama or MCP server.
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
    assert_eq!(sentinel.hits(), 1, "and the request actually arrived");
}

/// A provider's `base-url` is a named endpoint too — the common case, since that
/// is where a local model is configured, and nobody should have to write it twice.
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
    assert_eq!(sentinel.hits(), 1);
}

/// Naming one local origin must not open the machine.
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
    assert_eq!(
        neighbour.hits(),
        0,
        "granting the model's port must not grant the database beside it"
    );
}

/// Every place the host serves `host-http` to a guest must consult the policy.
/// One backend still calling the unbounded client would reopen the whole hole
/// while the tests above stay green, since they only exercise other backends.
/// This scans the sources for the unbounded `http::fetch` instead.
#[test]
fn every_guest_facing_backend_goes_through_the_policy() {
    // Discovered by scanning, not a hard-coded file list — a literal list can't
    // notice a new backend added after it was written.
    let core = common::repo_root().join("src/core/core/src");
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
        // Any host-side implementation of a guest's `host-http` import, whatever
        // the binding module happens to be called.
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
            // Code only — matching comments would flag prose describing the ban.
            let trimmed = line.trim_start();
            if trimmed.starts_with("//") {
                continue;
            }
            // `contains`, not `starts_with`: call sites read `let result =
            // crate::http::fetch(`. Trailing `(` keeps `fetch_within(` from matching.
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

    // The other way in: a caller that *hands* a guest the unbounded client.
    // Providers and tools receive an `HttpFn` from whoever builds them, so a
    // binary passing `Box::new(http::fetch)` reopens the hole without touching
    // any file scanned above.
    for binary in ["host/src/main.rs", "ui/src/main.rs"] {
        let path = common::repo_root().join("src/core").join(binary);
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
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

/// A loopback server whose request count can be asserted to be **zero**
/// without a race.
///
/// [`Sentinel`] above cannot be used for that. It polls a non-blocking
/// `accept()` with 20ms sleeps, so a connection that *was* made may not be
/// counted yet when the assertion reads the counter — which turns "the server
/// saw nothing" into a false pass, the exact class of defect
/// [#83](https://github.com/PromptPasture/jan-klod/issues/83) records against
/// it. Asserting a negative on a counter that can lag is asserting nothing.
///
/// This one closes the race rather than shortening it: [`Self::only_hit`]
/// connects once itself and waits for *that* connection to be counted, so the
/// queue is known to be drained past anything the code under test might have
/// done. If the fetch had connected, the count would be two.
struct Countable {
    port: u16,
    hits: Arc<AtomicU32>,
}

impl Countable {
    fn start() -> Self {
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
    fn only_hit(&self) {
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
            "the only connection this server ever saw is the one this test made; \
             anything more means the redirect was followed"
        );
    }
}

/// A server that answers every request with `302` to `target`, and counts.
fn redirector_to(target: String) -> (u16, Arc<AtomicU32>) {
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

/// A permitted origin cannot redirect the host to one the policy refuses.
///
/// The shape that makes this a real test: the *redirector* is explicitly
/// allowed, so the policy says yes to the URL the caller passed. Everything
/// after that is the redirect's doing, which is what the policy never saw.
#[test]
fn a_permitted_origin_cannot_redirect_to_a_refused_one() {
    let forbidden = Countable::start();
    let target = format!("http://127.0.0.1:{}/secrets", forbidden.port);
    let (redirect_port, redirect_hits) = redirector_to(target);
    let entry = format!("http://127.0.0.1:{redirect_port}/start");

    // The redirector is named by the operator; the sentinel is not.
    let policy = jan_klod_core::egress::EgressPolicy::public_only()
        .allowing(&format!("http://127.0.0.1:{redirect_port}"));

    let outcome = jan_klod_core::http::fetch_within(&policy, "GET", &entry, &[], None, 2_000);

    // The control: the request really did reach the permitted origin, so a
    // clean `hits == 0` below cannot be explained by nothing having happened.
    assert_eq!(
        redirect_hits.load(Ordering::SeqCst),
        1,
        "the permitted origin was reached — otherwise this test proves nothing"
    );

    // The caller gets the *policy's* refusal, not a puzzling 302. That is what
    // the per-hop loop changed: #107's first box merely declined to follow and
    // surfaced the redirect; now the hop is checked and refused, which is both
    // safe and legible.
    assert_eq!(
        outcome.err(),
        Some(jan_klod_core::http::WireError::ConnectionFailed),
        "the refused hop is reported as the policy refusing it"
    );

    // And the assertion this test exists for.
    forbidden.only_hit();
}

/// A server that redirects once, then serves — and records the headers and
/// method of every request, so what survived a hop can be asserted.
struct Hops {
    port: u16,
    seen: Arc<std::sync::Mutex<Vec<String>>>,
}

impl Hops {
    /// `first` is answered with `status` and a `Location` of `location`;
    /// everything after is answered `200 landed`.
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

/// A redirect to a destination the policy permits is still followed — the fix
/// for #107 must not be "break every redirect".
#[test]
fn a_redirect_to_a_permitted_destination_is_followed() {
    // Same origin, so one grant covers both hops.
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
        "the relative Location resolved against the first URL: {:?}",
        requests[1]
    );
}

/// `Authorization` does not survive a redirect.
///
/// ureq stripped it for us (`RedirectAuthHeaders::Never`); following redirects
/// by hand means doing that deliberately, and forgetting would send a
/// provider's API key to wherever the redirect pointed — a credential leak
/// introduced *by* the fix for a policy bypass.
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
        "the first hop did carry it, or this test proves nothing: {:?}",
        requests[0]
    );
    assert!(
        !requests[1].contains("sk-secret"),
        "and the second did not: {:?}",
        requests[1]
    );
    assert!(
        requests[1].to_ascii_lowercase().contains("x-kept"),
        "while an ordinary header still travels — names arrive lowercased: {:?}",
        requests[1]
    );
}

/// A `302` turns a POST into a GET and drops the body; a `307` does not.
///
/// Getting this wrong is silent: the request still succeeds, just with a method
/// or a body the caller did not send.
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
            "{status} should arrive as {expected}: {:?}",
            requests[1]
        );
    }
}
