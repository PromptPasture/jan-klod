//! Phase 3 Slice 3b — an external HTTP client drives the loop.
//!
//! Boots a `Runtime`, binds the host-side REST surface on an ephemeral port, and
//! from a **separate client thread** `POST`s a turn and reads the streamed
//! response — proving the outside world can reach and drive the loop offline. The
//! `AgentSession` is `!Send` (Wasmtime-backed), so it stays on the main thread and
//! the client runs on the spawned thread.
//!
//! Skips (passes as a no-op) when the guests are not staged in `ext/`.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::PathBuf;
use std::thread;

use jan_klod_core::http::WireResponse;
use jan_klod_core::route::HttpFn;
use jan_klod_core::serve::serve_once;
use jan_klod_core::Runtime;
use tiny_http::Server;

fn repo_root() -> PathBuf {
    [env!("CARGO_MANIFEST_DIR"), "..", "..", ".."].iter().collect()
}

fn canned_http(content: &'static str) -> HttpFn {
    Box::new(move |_m, _u, _h, _b, _t| {
        let body = serde_json::json!({
            "choices": [{
                "message": { "role": "assistant", "content": content },
                "finish_reason": "stop"
            }]
        });
        Ok(WireResponse { status: 200, headers: vec![], body: serde_json::to_vec(&body).unwrap() })
    })
}

#[test]
fn external_client_drives_the_loop_over_http() {
    let ext_dir = repo_root().join("ext");
    for guest in ["provider-openai.wasm", "interceptor-intent-router.wasm"] {
        if !ext_dir.join(guest).exists() {
            eprintln!("skipping: {guest} not staged — run `make ext`");
            return;
        }
    }

    let dir = std::env::temp_dir().join(format!("jk-apirest-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let config = dir.join("config.yaml");
    std::fs::write(
        &config,
        "
extensions:
  store:
    memory:
      enabled: true
  provider:
    openai:
      enabled: true
      base-url: http://mock/v1
      model: mock-1
      api-key: test
  interceptor:
    intent-router:
      enabled: true
",
    )
    .unwrap();

    let runtime = Runtime::boot(&config, &ext_dir).expect("runtime boots");
    let factory = || canned_http("pong");
    let mut agent = runtime.build_agent(&factory).expect("agent boots");

    let server = Server::http("127.0.0.1:0").expect("binds an ephemeral port");
    let port = server.server_addr().to_ip().expect("ip addr").port();

    // Client on a separate thread: POST a turn, read the raw HTTP response.
    let client = thread::spawn(move || {
        let mut stream = TcpStream::connect(("127.0.0.1", port)).expect("connects");
        let body = r#"{"session":"http-1","message":"hello"}"#;
        let request = format!(
            "POST /turn HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\n\
             Content-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        );
        stream.write_all(request.as_bytes()).unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).unwrap();
        response
    });

    // Server: handle exactly one request on the session-owning thread.
    serve_once(&server, &mut agent).expect("serves one request");

    let response = client.join().expect("client thread");
    assert!(response.contains("200 OK"), "status line present: {response}");
    assert!(response.contains("\"answer\":\"pong\""), "answer in body: {response}");

    // A second round-trip: GET /health returns liveness (the supervisor's probe).
    let health_client = thread::spawn(move || {
        let mut stream = TcpStream::connect(("127.0.0.1", port)).expect("connects");
        stream
            .write_all(b"GET /health HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
            .unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).unwrap();
        response
    });
    serve_once(&server, &mut agent).expect("serves the health request");
    let health = health_client.join().expect("health client thread");
    assert!(health.contains("200 OK"), "health status line: {health}");
    assert!(health.contains("\"status\":\"ok\""), "health body: {health}");

    // A third round-trip: an SSE client (Accept: text/event-stream) gets streamed
    // event frames ending in a `done` frame with the answer.
    let sse_client = thread::spawn(move || {
        let mut stream = TcpStream::connect(("127.0.0.1", port)).expect("connects");
        let body = r#"{"session":"http-1","message":"hello"}"#;
        let request = format!(
            "POST /turn HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\n\
             Accept: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        );
        stream.write_all(request.as_bytes()).unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).unwrap();
        response
    });
    serve_once(&server, &mut agent).expect("serves the SSE request");
    let sse = sse_client.join().expect("sse client thread");
    assert!(sse.contains("Content-Type: text/event-stream"), "SSE content-type: {sse}");
    assert!(sse.contains("event: done"), "SSE has a done frame: {sse}");
    assert!(sse.contains("\"answer\":\"pong\""), "SSE done carries the answer: {sse}");

    std::fs::remove_dir_all(&dir).ok();
}
