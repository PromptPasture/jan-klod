//! A component granted the network cannot reach the machine it runs on.
//!
//! `tool-fetch` carries an SSRF guard: it classifies the URL and refuses
//! loopback, private and link-local destinations. That guard runs **inside the
//! sandbox**. It protects a confused model from a URL the model chose; it says
//! nothing about the component, which is the party this runtime declines to
//! trust. A guest that simply omitted the check reached whatever it liked,
//! because `host-http` handed out an unrestricted client.
//!
//! So these tests stand up a real HTTP server on loopback and ask *the server*
//! whether anybody knocked. An assertion on a returned error would pass against a
//! guest-side check; an assertion on the server's own request count only passes
//! if the packet never left.
//!
//! **They drive `fetch_within` — the host backend — rather than a guest.** The
//! first version of this file drove the staged `tool-fetch` and all three tests
//! passed immediately, which was the tell: `tool-fetch` refuses loopback on its
//! own, so the sentinel saw zero requests whether or not a host boundary existed.
//! Every assertion was vacuous. No shipped guest will attempt an unconfigured
//! private URL, precisely because the honest ones self-censor — so a guest is the
//! wrong instrument for measuring what stops the dishonest one.
//!
//! What is verified instead: the policy the runtime derives from a real
//! `config.yaml` refuses a live local service and permits the one the operator
//! named, at the function every guest-facing backend calls; and
//! [`every_guest_facing_backend_goes_through_the_policy`] checks that no backend
//! has quietly gone back to the unbounded client.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::thread;

use jan_klod_core::Runtime;

mod common;

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

/// The reason the rule is per-origin rather than a switch: a self-hosted model
/// lives on loopback, and refusing it would make the private runtime unable to
/// reach the private model.
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
///
/// The boundary is only as good as its installation: one backend still calling
/// the unbounded client would reopen the whole hole while these tests stayed
/// green, because they exercise the *other* backends. This reads the sources and
/// insists the unbounded `http::fetch` appears in none of them.
#[test]
fn every_guest_facing_backend_goes_through_the_policy() {
    // Derived, not named. The first version listed three files — and there were
    // four: `route.rs`, the provider path, was missing, which is the busiest
    // egress route in the runtime. A check whose coverage is a literal cannot
    // notice a backend added after it was written, and this is the third time that
    // shape has bitten; the fix is to ask the source which files implement the
    // capability.
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
            // Code only. A doc comment explaining what must *not* be called would
            // otherwise trip this, and a checker that fires on prose is one
            // somebody silences — which costs more than the check is worth.
            let trimmed = line.trim_start();
            if trimmed.starts_with("//") {
                continue;
            }
            // `contains`, not `starts_with`: the call sites read
            // `let result = crate::http::fetch(`, so anchoring to the line start
            // matched nothing and this check passed against a backend I had
            // deliberately broken. The trailing `(` is what keeps
            // `http::fetch_within(` from matching.
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

    // The other way in: a caller that *hands* a guest the unbounded client. Providers
    // and tools receive an `HttpFn` from whoever builds them, so `route.rs` needs no
    // policy of its own — and that is exactly why a binary passing
    // `Box::new(http::fetch)` would reopen the hole without touching any file above.
    // `main.rs` did precisely that until 2026-08-11.
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
