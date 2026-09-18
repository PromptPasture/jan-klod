//! HTTP client drives loop. Boots `Runtime`, binds REST on ephemeral port.
//! Client thread POSTs, reads streamed response. `AgentSession` !Send on main thread.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::thread;

use jan_klod_core::Runtime;
use jan_klod_host::serve::Surface;
use jan_klod_host::sessions::Agents;

use crate::common;

#[test]
fn external_client_drives_the_loop_over_http() {
    let ext_dir = common::repo_root().join("ext");
    if !common::guests_staged(&["provider-openai.wasm", "interceptor-intent-router.wasm"]) {
        return;
    }

    let dir = std::env::temp_dir().join(format!("jk-apirest-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
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
",
    )
    .unwrap();

    let runtime = Runtime::boot(&config, &ext_dir).expect("runtime boots");
    let factory = || common::canned_http("pong");
    let agents = std::sync::Arc::new(Agents::new(
        std::sync::Arc::new(runtime),
        std::sync::Arc::new(factory),
    ));

    let surface = Surface::bind("127.0.0.1:0").expect("binds an ephemeral port");
    let port = surface.port();

    // Client thread: POST message, read response
    let client = thread::spawn(move || {
        let mut stream = TcpStream::connect(("127.0.0.1", port)).expect("connects");
        let body = r#"{"message":"hello"}"#;
        let request = format!(
            "POST /session/http-1/message HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\n\
             Content-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        );
        stream.write_all(request.as_bytes()).unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).unwrap();
        response
    });

    // Server: handle one request on session thread.
    surface.serve_once(&agents).expect("serves one request");

    let response = client.join().expect("client thread");
    assert!(
        response.contains("200 OK"),
        "status line present: {response}"
    );
    assert!(
        response.contains("\"answer\":\"pong\""),
        "answer in body: {response}"
    );

    // Second: GET /health for liveness
    let health_client = thread::spawn(move || {
        let mut stream = TcpStream::connect(("127.0.0.1", port)).expect("connects");
        stream
            .write_all(b"GET /health HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
            .unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).unwrap();
        response
    });
    surface
        .serve_once(&agents)
        .expect("serves the health request");
    let health = health_client.join().expect("health client thread");
    assert!(health.contains("200 OK"), "health status line: {health}");
    assert!(
        health.contains("\"status\":\"ok\""),
        "health body: {health}"
    );

    // Third: SSE client (Accept: text/event-stream) gets streamed frames
    let sse_client = thread::spawn(move || {
        let mut stream = TcpStream::connect(("127.0.0.1", port)).expect("connects");
        let body = r#"{"message":"hello"}"#;
        let request = format!(
            "POST /session/http-1/message HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\n\
             Accept: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        );
        stream.write_all(request.as_bytes()).unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).unwrap();
        response
    });
    surface.serve_once(&agents).expect("serves the SSE request");
    let sse = sse_client.join().expect("sse client thread");
    // Case-insensitive because the header name's casing is the server's
    // choice, not the protocol's: `hyper` writes it lowercase where
    // `tiny_http` wrote it capitalised (#224). Every HTTP client compares
    // these case-insensitively; only a test reading the raw socket can
    // tell, and it should not be the thing that fails.
    assert!(
        sse.to_lowercase()
            .contains("content-type: text/event-stream"),
        "SSE content-type: {sse}"
    );
    assert!(sse.contains("event: done"), "SSE has a done frame: {sse}");
    assert!(
        sse.contains("\"answer\":\"pong\""),
        "SSE done carries the answer: {sse}"
    );

    std::fs::remove_dir_all(&dir).ok();
}
