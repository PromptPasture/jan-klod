//! A mid-turn confirmation, answered over HTTP.
//!
//! The permission gate can stop a turn to ask the user something. Over the REST
//! surface that means the turn blocks *inside* the SSE response and the answer has
//! to arrive as a separate request while that response is still open — the case a
//! single-threaded server cannot serve by accident. This drives the whole path:
//!
//!   client A: POST /session/:id/message (SSE)  → receives `event: prompt`
//!   client B: POST /session/:id/answer         → the waiting turn takes it
//!   client A: the stream continues to `event: done`
//!
//! The `AgentSession` is `!Send`, so it stays on the main thread and the clients
//! run on spawned threads (as in `api_rest.rs`).
//!
//! Skips (passes as a no-op) when the guests are not staged in `ext/`.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::thread;

use jan_klod_core::http::WireResponse;
use jan_klod_core::route::HttpFn;
use jan_klod_core::serve::serve_once;
use jan_klod_core::Runtime;
use tiny_http::Server;

mod common;

/// A lost answer must fail fast rather than stall for the three-minute default
/// and then pass on the prompt's own denial. See `auth.rs` for the run this cost.
fn short_answer_timeout() {
    std::env::set_var("JK_ANSWER_TIMEOUT_SECS", "5");
}

const GUESTS: [&str; 4] = [
    "provider-openai.wasm",
    "interceptor-intent-router.wasm",
    "interceptor-tool-selector.wasm",
    "interceptor-permission.wasm",
];

/// First completion calls a dangerous tool (so the gate asks), then answers.
fn tool_then_answer_http() -> HttpFn {
    let calls = Arc::new(AtomicU32::new(0));
    Box::new(move |_m, _u, _h, _b, _t| {
        let n = calls.fetch_add(1, Ordering::Relaxed);
        let body = if n == 0 {
            serde_json::json!({
                "choices": [{
                    "message": {
                        "role": "assistant",
                        "tool_calls": [{
                            "id": "call-1",
                            "function": { "name": "bash", "arguments": "{}" }
                        }]
                    },
                    "finish_reason": "tool_calls"
                }]
            })
        } else {
            serde_json::json!({
                "choices": [{
                    "message": { "role": "assistant", "content": "all done" },
                    "finish_reason": "stop"
                }]
            })
        };
        Ok(WireResponse {
            status: 200,
            headers: vec![],
            body: serde_json::to_vec(&body).unwrap(),
        })
    })
}

fn write_config(dir: &std::path::Path) -> std::path::PathBuf {
    let config = dir.join("config.yaml");
    std::fs::write(
        &config,
        "
extensions:
  provider:
    openai:
      enabled: true
      base-url: http://mock/v1
      model: mock-1
      api-key: test
  interceptor:
    intent-router:
      enabled: true
    tool-selector:
      enabled: true
    permission:
      enabled: true
",
    )
    .unwrap();
    config
}

/// Send `answer` for `session` on its own connection and return the raw response.
fn post_answer(port: u16, session: &str, answer: &str) -> String {
    let body = serde_json::json!({ "answer": answer }).to_string();
    let request = format!(
        "POST /session/{session}/answer HTTP/1.1\r\nHost: localhost\r\n\
         Content-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        body.len(),
        body
    );
    let mut stream = TcpStream::connect(("127.0.0.1", port)).expect("connects");
    stream.write_all(request.as_bytes()).unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).unwrap();
    response
}

#[test]
fn a_confirmation_is_asked_over_sse_and_answered_on_a_second_connection() {
    short_answer_timeout();
    let ext_dir = common::repo_root().join("ext");
    if !common::guests_staged(&GUESTS) {
        return;
    }

    let dir = std::env::temp_dir().join(format!("jk-apiprompt-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let _guard = common::TempDir(dir.clone());
    let config = write_config(&dir);

    let runtime = Runtime::boot(&config, &ext_dir).expect("runtime boots");
    let factory = tool_then_answer_http;
    let mut agent = runtime.build_agent(&factory).expect("agent boots");

    let server = Server::http("127.0.0.1:0").expect("binds an ephemeral port");
    let port = server.server_addr().to_ip().expect("ip addr").port();

    // Client A: start the turn and read frames as they arrive. When the `prompt`
    // frame lands, client B answers on a second connection — while this stream is
    // still open and the turn is blocked.
    let turn = thread::spawn(move || {
        let mut stream = TcpStream::connect(("127.0.0.1", port)).expect("connects");
        let body = r#"{"message":"use bash to clean up, then report"}"#;
        let request = format!(
            "POST /session/p-1/message HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\n\
             Accept: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        );
        stream.write_all(request.as_bytes()).unwrap();

        let mut collected = String::new();
        let mut answered = false;
        let reader = BufReader::new(stream);
        for line in reader.lines() {
            let Ok(line) = line else { break };
            collected.push_str(&line);
            collected.push('\n');
            // The prompt frame is the cue: the turn is now parked waiting for us.
            if line.starts_with("event: prompt") && !answered {
                answered = true;
                let ack = post_answer(port, "p-1", "yes");
                assert!(
                    ack.contains("\"accepted\":true"),
                    "answer acknowledged: {ack}"
                );
            }
        }
        (collected, answered)
    });

    // The session-owning thread serves the message request; the waiting driver
    // serves the answer request from inside it, so one `serve_once` covers both.
    serve_once(&server, &mut agent).expect("serves the turn");

    let (stream_text, answered) = turn.join().expect("turn client thread");
    assert!(answered, "the turn asked for a confirmation: {stream_text}");
    assert!(
        stream_text.contains("event: prompt"),
        "a prompt frame was sent: {stream_text}"
    );
    assert!(
        stream_text.contains("\"question\""),
        "the prompt frame carries the question: {stream_text}"
    );
    assert!(
        stream_text.contains("always"),
        "the offered options include the standing choices: {stream_text}"
    );
    assert!(
        stream_text.contains("event: done"),
        "the turn completed after being answered: {stream_text}"
    );
    assert!(
        stream_text.contains("all done"),
        "the final answer streamed: {stream_text}"
    );
}

#[test]
fn an_answer_with_nothing_pending_is_refused() {
    short_answer_timeout();
    let ext_dir = common::repo_root().join("ext");
    if !common::guests_staged(&["provider-openai.wasm"]) {
        return;
    }

    let dir = std::env::temp_dir().join(format!("jk-apiprompt-idle-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let _guard = common::TempDir(dir.clone());
    let config = write_config(&dir);

    let runtime = Runtime::boot(&config, &ext_dir).expect("runtime boots");
    let factory = tool_then_answer_http;
    let mut agent = runtime.build_agent(&factory).expect("agent boots");

    let server = Server::http("127.0.0.1:0").expect("binds an ephemeral port");
    let port = server.server_addr().to_ip().expect("ip addr").port();

    let client = thread::spawn(move || post_answer(port, "p-1", "yes"));
    serve_once(&server, &mut agent).expect("serves the stray answer");
    let response = client.join().expect("client thread");

    // A route that exists but has nothing to answer says so, rather than 404-ing
    // as if the client had invented the endpoint.
    assert!(response.contains("409"), "conflict status: {response}");
    assert!(
        response.contains("no confirmation is pending"),
        "explains itself: {response}"
    );
}
