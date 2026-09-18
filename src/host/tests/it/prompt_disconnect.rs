//! A dropped client does not hold the agent.
//!
//! `JK_ANSWER_TIMEOUT_SECS` is process-global; this test needs it long
//! (heartbeat ends the wait, not the deadline) while `api_prompt.rs` needs it short.
//! nextest's per-test isolation lets both set it independently.

use jan_klod_core::route::HttpFn;

use jan_klod_host::serve::Surface;

use crate::common;

const GUESTS: [&str; 3] = [
    "provider-openai.wasm",
    "interceptor-tool-selector.wasm",
    "interceptor-permission.wasm",
];

/// Provider that calls a dangerous tool, parking the turn.
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

/// Client disconnection doesn't hold the agent.
///
/// A dropped connection may leave a *successful* write (bytes buffered, FIN
/// not yet seen), unnoticed until the confirmation timeout. SSE heartbeats
/// now surface a dead peer as a write error before the deadline.
/// This test drops mid-prompt and asserts the turn finishes quickly;
/// timeout is long so regressions show as slow tests, not silent passes.
#[test]
fn a_disconnected_client_does_not_hold_the_turn_open() {
    if !common::guests_staged(&GUESTS) {
        return;
    }
    // Intentionally long: heartbeat (not deadline) should end the wait.
    std::env::set_var("JK_ANSWER_TIMEOUT_SECS", "120");

    let dir = std::env::temp_dir().join(format!("jk-gone-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let _guard = common::TempDir(dir.clone());
    let config = write_config(&dir);

    let runtime = jan_klod_core::Runtime::boot(&config, common::repo_root().join("ext"))
        .expect("runtime boots");
    let factory = tool_then_answer_http;
    let mut agent = runtime.build_agent(&factory).expect("agent boots");
    let surface = Surface::bind("127.0.0.1:0").expect("binds an ephemeral port");
    let port = surface.port();

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
    surface
        .serve_once_authed(&mut agent, None)
        .expect("serves the turn");
    let elapsed = started.elapsed();
    let _ = client.join();

    assert!(
        elapsed < std::time::Duration::from_secs(30),
        "the turn took {elapsed:?} after the client left; a vanished client must not \
         hold the agent for the whole confirmation timeout"
    );
}
