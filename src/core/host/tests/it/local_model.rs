//! A model on localhost, with no API key, over a real socket — the self-hosted
//! case (Ollama, LM Studio, llama.cpp). Other provider tests inject a canned
//! `HttpFn`, which skips exactly what matters here: that a keyless provider
//! boots and completes, that the egress policy permits a provider's own
//! `base-url` even on loopback, and that `type:` names a component that
//! actually exists. This stands up an OpenAI-shaped endpoint on loopback and
//! drives a turn through the real client to cover all three.
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
    // Shape of the shipped `provider.ollama` block: `type: openai`, loopback
    // `base-url`, no `api-key`.
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
    // The real client, bounded by the real policy — not a canned `HttpFn`. Reaches
    // this loopback endpoint only because the policy lifts a provider's `base-url`
    // out of config.
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

/// A failure names the endpoint and what to check, rather than surfacing a
/// generated binding's Debug output (`ProviderError { code: 5, ... }`) that
/// names nothing and wrongly implies retrying could help.
///
/// The endpoint here is a port with nothing listening — the most common
/// first-run failure for a local model.
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
    // A typo'd base-url and a denied origin fail identically, so the message
    // must hint at egress as a possible cause.
    assert!(
        message.contains("egress"),
        "the message mentions egress: {message}"
    );
}
