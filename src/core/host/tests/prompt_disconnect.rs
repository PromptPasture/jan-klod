//! A client that vanishes mid-prompt does not pin the agent.
//!
//! **Its own binary on purpose.** The confirmation wait is configured by
//! `JK_ANSWER_TIMEOUT_SECS`, an environment variable, and environment variables
//! are process-wide while tests run in parallel. This test needs a *long* timeout
//! — the whole point is that the heartbeat ends the wait rather than the deadline
//! — and `api_prompt.rs` needs a short one so a lost answer fails fast. Sharing a
//! process, whichever test ran last would decide, and this one would pass on the
//! deadline while appearing to prove the heartbeat.

use jan_klod_core::route::HttpFn;

mod common;

const GUESTS: [&str; 3] =
    ["provider-openai.wasm", "interceptor-tool-selector.wasm", "interceptor-permission.wasm"];

/// A provider that calls a dangerous tool, so the gate parks the turn.
fn tool_then_answer_http() -> HttpFn {
    let calls = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0));
    Box::new(move |_m, _u, _h, _b, _t| {
        let n = calls.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let body = if n == 0 {
            serde_json::json!({"choices":[{"message":{"role":"assistant","tool_calls":[
                {"id":"c1","function":{"name":"bash","arguments":"{}"}}]},
                "finish_reason":"tool_calls"}]})
        } else {
            serde_json::json!({"choices":[{"message":{"role":"assistant",
                "content":"done"},"finish_reason":"stop"}]})
        };
        Ok(jan_klod_core::http::WireResponse {
            status: 200,
            headers: vec![],
            body: serde_json::to_vec(&body).unwrap(),
        })
    })
}

fn write_config(dir: &std::path::Path) -> std::path::PathBuf {
    let path = dir.join("config.yaml");
    std::fs::write(
        &path,
        "
extensions:
  provider:
    openai:
      enabled: true
      base-url: http://mock/v1
      model: mock-1
      api-key: test
  interceptor:
    tool-selector:
      enabled: true
    permission:
      enabled: true
",
    )
    .unwrap();
    path
}

/// A client that vanishes mid-prompt does not pin the agent.
///
/// `ask` already handles the case where writing the prompt frame *fails* — the
/// client was gone before it was asked. But a client that disappears a moment
/// later leaves a write that succeeds: the bytes go into the socket buffer and
/// the FIN has not been processed. Nothing then noticed for the full confirmation
/// timeout, three minutes by default, during which the agent serves nobody and
/// answers `409` to everyone else. This module's own comment claimed the timeout
/// prevented exactly that.
///
/// So the wait ticks, writing an SSE comment each time; a dead peer surfaces as a
/// write error within one interval. This test drops the connection the moment the
/// prompt arrives and asserts the turn finishes in seconds rather than at the far
/// end of the wait — with the timeout deliberately left long, so that a
/// regression shows up as a slow test rather than an unnoticed one.
#[test]
fn a_disconnected_client_does_not_hold_the_turn_open() {
    if !common::guests_staged(&GUESTS) {
        return;
    }
    // Long on purpose: the point is that the heartbeat ends the wait, not the
    // deadline. With this at 5s the test would pass without the fix.
    std::env::set_var("JK_ANSWER_TIMEOUT_SECS", "120");

    let dir = std::env::temp_dir().join(format!("jk-gone-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let _guard = common::TempDir(dir.clone());
    let config = write_config(&dir);

    let runtime = jan_klod_core::Runtime::boot(&config, common::repo_root().join("ext"))
        .expect("runtime boots");
    let factory = tool_then_answer_http;
    let mut agent = runtime.build_agent(&factory).expect("agent boots");
    let server = tiny_http::Server::http("127.0.0.1:0").expect("binds");
    let port = server.server_addr().to_ip().expect("ip").port();

    let client = std::thread::spawn(move || {
        use std::io::{BufRead, BufReader, Write};
        let mut stream = std::net::TcpStream::connect(("127.0.0.1", port)).expect("connects");
        let body = r#"{"message":"use bash"}"#;
        let raw = format!(
            "POST /session/g/message HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\n\
             Accept: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        stream.write_all(raw.as_bytes()).unwrap();
        let reader = BufReader::new(stream.try_clone().expect("clone"));
        for line in reader.lines() {
            let Ok(line) = line else { break };
            if line.starts_with("event: prompt") {
                // Walk away without answering.
                let _ = stream.shutdown(std::net::Shutdown::Both);
                return;
            }
        }
    });

    let started = std::time::Instant::now();
    jan_klod_core::serve::serve_once_authed(&server, &mut agent, None).expect("serves the turn");
    let elapsed = started.elapsed();
    let _ = client.join();

    assert!(
        elapsed < std::time::Duration::from_secs(30),
        "the turn took {elapsed:?} after the client left; a vanished client must not \
         hold the agent for the whole confirmation timeout"
    );
}
