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
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use jan_klod_core::conductor::RunResult;
use jan_klod_core::Runtime;

use crate::common;

/// Long enough that a healthy request never reaches it, so it bounds only the
/// failure case rather than pacing the test.
const REQUEST_DEADLINE: Duration = Duration::from_secs(10);

/// A minimal OpenAI-compatible endpoint: one completion, then it keeps serving.
struct FakeOllama {
    port: u16,
    requests: Arc<AtomicU32>,
    saw_authorization: Arc<AtomicU32>,
    stop: Arc<AtomicU32>,
    /// I/O the endpoint could not complete. Discarding these is what let
    /// [`FakeOllama`] answer a request it had not read, so a test that trusts
    /// what the endpoint saw has to assert this is empty first.
    faults: Arc<Mutex<Vec<String>>>,
}

/// Read one whole HTTP request: the headers, then exactly `Content-Length` bytes
/// of body. One `read` is not enough — either part can arrive in a later
/// segment, and every byte still unread when the socket closes turns the reply
/// into an RST that destroys the response already written.
fn read_request(socket: &mut TcpStream) -> std::io::Result<String> {
    let mut raw: Vec<u8> = Vec::new();
    let mut chunk = [0_u8; 1024];
    loop {
        let read = socket.read(&mut chunk)?;
        if read == 0 {
            break;
        }
        raw.extend_from_slice(&chunk[..read]);
        let Some(head_end) = raw.windows(4).position(|w| w == b"\r\n\r\n") else {
            continue;
        };
        let head = String::from_utf8_lossy(&raw[..head_end]).to_lowercase();
        let body_len: usize = head
            .lines()
            .find_map(|line| line.strip_prefix("content-length:"))
            .and_then(|value| value.trim().parse().ok())
            .unwrap_or(0);
        if raw.len() >= head_end + 4 + body_len {
            break;
        }
    }
    Ok(String::from_utf8_lossy(&raw).into_owned())
}

impl FakeOllama {
    fn start(answer: &'static str) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("binds loopback");
        let port = listener.local_addr().expect("has an address").port();
        listener.set_nonblocking(true).expect("nonblocking");
        let requests = Arc::new(AtomicU32::new(0));
        let saw_authorization = Arc::new(AtomicU32::new(0));
        let stop = Arc::new(AtomicU32::new(0));
        let faults = Arc::new(Mutex::new(Vec::new()));
        let (counted, authed, stopped, faulted) = (
            Arc::clone(&requests),
            Arc::clone(&saw_authorization),
            Arc::clone(&stop),
            Arc::clone(&faults),
        );
        let fault = move |what: &str, e: &dyn std::fmt::Debug| {
            if let Ok(mut log) = faulted.lock() {
                log.push(format!("{what}: {e:?}"));
            }
        };
        thread::spawn(move || {
            while stopped.load(Ordering::Relaxed) == 0 {
                match listener.accept() {
                    Ok((mut socket, _)) => {
                        counted.fetch_add(1, Ordering::Relaxed);
                        // BSD `accept()` hands back a socket that inherited the
                        // listener's O_NONBLOCK — verified on this host — so
                        // this is non-blocking on macOS and blocking on Linux.
                        // Make it blocking with a deadline, and read the request
                        // to completion: a single non-blocking read can return
                        // part of it or none of it, and answering early leaves
                        // the rest unread, so the close sends RST instead of FIN
                        // and the RST discards the response already written. The
                        // client sees "could not be reached" for a turn the
                        // endpoint answered correctly.
                        if let Err(e) = socket.set_nonblocking(false) {
                            fault("set_nonblocking", &e);
                        }
                        if let Err(e) = socket.set_read_timeout(Some(REQUEST_DEADLINE)) {
                            fault("set_read_timeout", &e);
                        }
                        let request = match read_request(&mut socket) {
                            Ok(request) => request.to_lowercase(),
                            Err(e) => {
                                fault("read_request", &e);
                                continue;
                            }
                        };
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
                        if let Err(e) = socket.write_all(response.as_bytes()) {
                            fault("write_all", &e);
                        }
                    }
                    Err(_) => thread::sleep(Duration::from_millis(10)),
                }
            }
        });
        Self {
            port,
            requests,
            saw_authorization,
            stop,
            faults,
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
    // Assert this *before* what the endpoint saw: the header check below reads a
    // counter derived from the request text, so an unreported read failure used
    // to satisfy it with an empty string rather than with evidence.
    let faults = ollama
        .faults
        .lock()
        .expect("the fault log is not poisoned")
        .clone();
    assert!(
        faults.is_empty(),
        "the fake endpoint could not complete its own I/O, so it saw less than the \
         request it answered: {faults:?}"
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
