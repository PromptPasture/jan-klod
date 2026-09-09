//! A model on localhost, with no API key, over a real socket.
//!
//! "Runs on any model" is one of the four claims, and self-hosting means a local
//! one: Ollama, LM Studio, llama.cpp. Every other provider test injects a canned
//! `HttpFn`, which is right for testing the loop and wrong for testing this — it
//! skips the real HTTP client, the egress policy, and the question of whether a
//! provider works at all without credentials. Three things had to hold for a local
//! model and none of them was covered:
//!
//! 1. **A provider with no `api-key` must boot and complete.** It does —
//!    `provider-openai` adds `Authorization` only when a key is present — but
//!    nothing said so, and `config.yaml` demands `${OPENAI_API_KEY}` two lines
//!    above, which is a reasonable thing to assume is mandatory.
//! 2. **The egress policy must permit the endpoint.** `127.0.0.1:11434` is
//!    loopback, which the policy refuses by default; it is allowed because a
//!    provider's `base-url` is lifted out of config. That is the whole reason the
//!    rule is per-origin rather than a flag.
//! 3. **`type:` must name a component that exists.** It did not. The shipped
//!    `ollama` block said `type: ollama` → `provider-ollama.wasm`, which has never
//!    existed, so enabling the most common self-hosted setup produced a missing
//!    component and no provider.
//!
//! So this stands up an OpenAI-shaped endpoint on loopback, points a keyless
//! provider at it the way the shipped config now does, and drives a turn through
//! the real client.
//!
//! Skips (passes as a no-op) when the guests are not staged in `ext/`.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::thread;

use jan_klod_core::conductor::RunResult;
use jan_klod_core::Runtime;

use crate::common;

/// A minimal OpenAI-compatible endpoint: one completion, then it keeps serving.
struct FakeOllama {
    port: u16,
    requests: Arc<AtomicU32>,
    saw_authorization: Arc<AtomicU32>,
    stop: Arc<AtomicU32>,
}

impl FakeOllama {
    fn start(answer: &'static str) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("binds loopback");
        let port = listener.local_addr().expect("has an address").port();
        listener.set_nonblocking(true).expect("nonblocking");
        let requests = Arc::new(AtomicU32::new(0));
        let saw_authorization = Arc::new(AtomicU32::new(0));
        let stop = Arc::new(AtomicU32::new(0));
        let (counted, authed, stopped) = (
            Arc::clone(&requests),
            Arc::clone(&saw_authorization),
            Arc::clone(&stop),
        );
        thread::spawn(move || {
            while stopped.load(Ordering::Relaxed) == 0 {
                match listener.accept() {
                    Ok((mut socket, _)) => {
                        counted.fetch_add(1, Ordering::Relaxed);
                        let mut buf = [0_u8; 4096];
                        let read = socket.read(&mut buf).unwrap_or(0);
                        let request = String::from_utf8_lossy(&buf[..read]).to_lowercase();
                        if request.contains("authorization:") {
                            authed.fetch_add(1, Ordering::Relaxed);
                        }
                        let body = serde_json::json!({
                            "choices": [{
                                "message": { "role": "assistant", "content": answer },
                                "finish_reason": "stop"
                            }]
                        })
                        .to_string();
                        let response = format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\
                             Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                            body.len()
                        );
                        let _ = socket.write_all(response.as_bytes());
                    }
                    Err(_) => thread::sleep(std::time::Duration::from_millis(10)),
                }
            }
        });
        Self {
            port,
            requests,
            saw_authorization,
            stop,
        }
    }
}

impl Drop for FakeOllama {
    fn drop(&mut self) {
        self.stop.store(1, Ordering::Relaxed);
        let _ = TcpStream::connect(("127.0.0.1", self.port));
    }
}

#[test]
fn a_keyless_local_model_completes_a_turn_over_a_real_socket() {
    if !common::guests_staged(&["provider-openai.wasm"]) {
        return;
    }
    let dir = std::env::temp_dir().join(format!("jk-local-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let _guard = common::TempDir(dir.clone());

    let ollama = FakeOllama::start("the local model answered");
    let config = dir.join("config.yaml");
    // Exactly the shape of the shipped `provider.ollama` block: a `type: openai`
    // component, a loopback `base-url`, and no `api-key` at all.
    std::fs::write(
        &config,
        format!(
            "
extensions:
  provider:
    ollama:
      enabled: true
      type: openai
      base-url: http://127.0.0.1:{}/v1
      model: qwen2.5:7b
",
            ollama.port
        ),
    )
    .unwrap();

    let runtime = Runtime::boot(&config, common::repo_root().join("ext")).expect("runtime boots");
    // The real client, bounded by the real policy — not a canned `HttpFn`. The
    // endpoint is loopback, so this only reaches it because the policy lifts a
    // provider's `base-url` out of config.
    let policy = runtime.egress_policy();
    let factory = move || -> jan_klod_core::route::HttpFn {
        let policy = policy.clone();
        Box::new(move |method, url, headers, body, timeout| {
            jan_klod_core::http::fetch_within(&policy, method, url, headers, body, timeout)
        })
    };
    let mut agent = runtime
        .build_agent(&factory)
        .expect("agent boots without an api-key");

    let result = agent.run("local", "hello");
    match result {
        RunResult::Answered { text, .. } => {
            assert!(
                text.contains("the local model answered"),
                "the answer came from the local endpoint: {text}"
            );
        }
        other @ RunResult::Failed(_) => panic!("a local model must complete a turn, got {other:?}"),
    }

    assert!(
        ollama.requests.load(Ordering::Relaxed) >= 1,
        "the request actually crossed a socket"
    );
    // No key configured means no header invented: a local endpoint that rejects
    // unexpected credentials must not receive any.
    assert_eq!(
        ollama.saw_authorization.load(Ordering::Relaxed),
        0,
        "no Authorization header is sent when no api-key is configured"
    );
}

/// A failure names the endpoint and what to check.
///
/// The message used to be the Debug of a generated binding:
/// `provider error: ProviderError { code: 5, name: "transient", message: "Any
/// other transient error." }`. Three problems in one line — a wasm-binding
/// internal reached the user, "transient" invited retrying something that could
/// never succeed, and nothing named the endpoint the reader had to go and look at.
///
/// The endpoint here is a port with nothing listening, which is the most common
/// first-run failure for a local model: the server is not started, or the address
/// has a typo.
#[test]
fn an_unreachable_provider_says_so_and_names_the_endpoint() {
    if !common::guests_staged(&["provider-openai.wasm"]) {
        return;
    }
    let dir = std::env::temp_dir().join(format!("jk-unreach-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let _guard = common::TempDir(dir.clone());

    // Bind and immediately drop, so the port is real and closed.
    let port = {
        let listener = TcpListener::bind("127.0.0.1:0").expect("binds");
        listener.local_addr().expect("addr").port()
    };
    let config = dir.join("config.yaml");
    std::fs::write(
        &config,
        format!(
            "
extensions:
  provider:
    local:
      enabled: true
      type: openai
      base-url: http://127.0.0.1:{port}/v1
      model: local
"
        ),
    )
    .unwrap();

    let runtime = Runtime::boot(&config, common::repo_root().join("ext")).expect("boots");
    let policy = runtime.egress_policy();
    let factory = move || -> jan_klod_core::route::HttpFn {
        let policy = policy.clone();
        Box::new(move |method, url, headers, body, timeout| {
            jan_klod_core::http::fetch_within(&policy, method, url, headers, body, timeout)
        })
    };
    let mut agent = runtime.build_agent(&factory).expect("agent boots");

    let RunResult::Failed(message) = agent.run("s", "hello") else {
        panic!("an unreachable provider cannot answer");
    };
    assert!(
        message.contains(&format!("127.0.0.1:{port}")),
        "the failure names the endpoint to look at: {message}"
    );
    assert!(
        message.contains("could not be reached"),
        "and says it was unreachable rather than transient: {message}"
    );
    assert!(
        !message.contains("ProviderError {"),
        "no generated-binding Debug reaches the user: {message}"
    );
    // The egress hint matters: a typo'd base-url and a denied origin fail here
    // identically, and the reader needs to know the second is possible.
    assert!(
        message.contains("egress"),
        "the message mentions egress: {message}"
    );
}
