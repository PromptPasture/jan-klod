//! Mid-turn confirmation over HTTP. Permission gate parks, asking via SSE.
//! Answer arrives on separate connection. `AgentSession` !Send on main thread.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::thread;

use jan_klod_core::http::WireResponse;
use jan_klod_core::route::HttpFn;
use jan_klod_core::Runtime;
use jan_klod_host::serve::{serve_once, serve_requests};
use tiny_http::Server;

use crate::common;

/// Set short answer timeout for fast failure
fn short_answer_timeout() {
    std::env::set_var("JK_ANSWER_TIMEOUT_SECS", "5");
}

const GUESTS: [&str; 4] = [
    "provider-openai.wasm",
    "interceptor-intent-router.wasm",
    "interceptor-tool-selector.wasm",
    "interceptor-permission.wasm",
];

/// First completion calls dangerous tool (gate asks), then final answer
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

/// POST `answer` for `session`, return HTTP response.
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

    // Client A: read frames. On `prompt` frame, client B answers on second
    // connection while stream stays open.
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
            // Prompt frame is cue: turn is parked waiting.
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

    // Two requests: the turn, and the answer that arrives while it is
    // parked. They used to be one, because the parked driver served the
    // socket itself — #225 took that away, and the count is what the
    // difference looks like from a test.
    serve_requests(&server, &mut agent, None, 2).expect("serves the turn and its answer");

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

    // Route exists but nothing to answer returns 409, not 404 (no fake endpoint).
    assert!(response.contains("409"), "conflict status: {response}");
    assert!(
        response.contains("no confirmation is pending"),
        "explains itself: {response}"
    );
}
