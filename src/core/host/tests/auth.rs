//! The REST surface refuses unauthenticated callers when a token is set.
//!
//! Until now it refused nobody: `config.yaml` advertised an `api-key` that
//! nothing read, so a surface bound anywhere was open to anyone who could reach
//! it. This drives the real server and asserts on **status codes off the wire**,
//! because "the request was rejected" is only true if the socket says so.
//!
//! Skips (passes as a no-op) when the guests are not staged in `ext/`.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::thread;

use jan_klod_core::serve::serve_once_authed;
use jan_klod_core::Runtime;
use tiny_http::Server;

mod common;

const TOKEN: &str = "s3cret-token";

fn write_config(dir: &std::path::Path) -> std::path::PathBuf {
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
",
    )
    .unwrap();
    config
}

/// Send a raw request with an optional `Authorization` header, return the status
/// line plus body.
fn request(port: u16, target: &str, auth: Option<&str>) -> String {
    let header = auth.map_or_else(String::new, |t| format!("Authorization: Bearer {t}\r\n"));
    let body = r#"{"message":"hello"}"#;
    let raw = if target == "/health" {
        format!("GET /health HTTP/1.1\r\nHost: localhost\r\n{header}Connection: close\r\n\r\n")
    } else {
        format!(
            "POST {target} HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\n\
             {header}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
    };
    let mut stream = TcpStream::connect(("127.0.0.1", port)).expect("connects");
    stream.write_all(raw.as_bytes()).unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).unwrap();
    response
}

#[test]
fn without_a_token_a_turn_is_refused_and_never_reaches_the_agent() {
    if !common::guests_staged(&["provider-openai.wasm"]) {
        return;
    }
    let dir = std::env::temp_dir().join(format!("jk-auth-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let _guard = common::TempDir(dir.clone());

    // The server side runs on this thread (the session is !Send), so the client
    // has to be the one that moves.
    let ext_dir = common::repo_root().join("ext");
    let config = write_config(&dir);
    let runtime = Runtime::boot(&config, &ext_dir).expect("runtime boots");
    let factory = || common::canned_http("pong");
    let mut agent = runtime.build_agent(&factory).expect("agent boots");
    let server = Server::http("127.0.0.1:0").expect("binds");
    let port = server.server_addr().to_ip().expect("ip").port();

    let client = thread::spawn(move || {
        let none = request(port, "/session/a/message", None);
        let wrong = request(port, "/session/a/message", Some("not-the-token"));
        let right = request(port, "/session/a/message", Some(TOKEN));
        let health = request(port, "/health", None);
        (none, wrong, right, health)
    });

    for _ in 0..4 {
        serve_once_authed(&server, &mut agent, Some(TOKEN)).expect("serves");
    }
    let (none, wrong, right, health) = client.join().expect("client thread");

    assert!(none.contains("401"), "no header is refused: {none}");
    assert!(wrong.contains("401"), "a wrong token is refused: {wrong}");
    assert!(
        !none.contains("pong") && !wrong.contains("pong"),
        "a refused request never reached the agent"
    );
    assert!(right.contains("200 OK"), "the right token is served: {right}");
    assert!(right.contains("pong"), "and gets a real answer: {right}");
    // The supervisor probes this without credentials during a blue/green flip.
    assert!(health.contains("200 OK"), "/health stays open: {health}");
}

#[test]
fn with_no_token_configured_the_surface_behaves_as_before() {
    if !common::guests_staged(&["provider-openai.wasm"]) {
        return;
    }
    let dir = std::env::temp_dir().join(format!("jk-auth-open-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let _guard = common::TempDir(dir.clone());

    let ext_dir = common::repo_root().join("ext");
    let config = write_config(&dir);
    let runtime = Runtime::boot(&config, &ext_dir).expect("runtime boots");
    let factory = || common::canned_http("pong");
    let mut agent = runtime.build_agent(&factory).expect("agent boots");
    let server = Server::http("127.0.0.1:0").expect("binds");
    let port = server.server_addr().to_ip().expect("ip").port();

    let client = thread::spawn(move || request(port, "/session/a/message", None));
    serve_once_authed(&server, &mut agent, None).expect("serves");
    let response = client.join().expect("client thread");

    // Requiring a secret to talk to your own loopback would be friction without a
    // threat, so the default stays open — and stays warned about when the bind is
    // not loopback.
    assert!(response.contains("200 OK"), "no token configured means no gate: {response}");
    assert!(response.contains("pong"), "and the turn runs: {response}");
}
