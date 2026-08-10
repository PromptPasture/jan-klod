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

/// Keep a lost answer loud.
///
/// The confirmation wait defaults to three minutes, which is right for a person
/// and disastrous for a test: an answer that goes astray does not fail the test,
/// it stalls it and then passes, because the timeout takes the prompt's default
/// and the default is a denial. This suite spent 360 seconds — two full timeouts
/// — on some runs and one second on others, reporting success either way.
fn short_answer_timeout() {
    // Set before any server thread starts, and every test in this binary wants
    // the same value.
    std::env::set_var("JK_ANSWER_TIMEOUT_SECS", "5");
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
",
    )
    .unwrap();
    config
}

/// Send a raw request with an optional `Authorization` header, return the status
/// line plus body.
fn request(port: u16, target: &str, auth: Option<&str>) -> String {
    let header = auth.map_or_else(String::new, |t| format!("Authorization: Bearer {t}\r\n"));
    // The answer route takes a different body. Sending the wrong one gets a 400,
    // which the waiting driver treats as "still no answer" — so a test that got
    // this wrong would sit out the full answer timeout and look like a deadlock.
    let body = if target.ends_with("/answer") {
        r#"{"answer":"yes"}"#
    } else {
        r#"{"message":"hello"}"#
    };
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
    short_answer_timeout();
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
    short_answer_timeout();
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

/// The one endpoint that must never be open.
///
/// While a turn is parked on a confirmation, the waiting driver serves the socket
/// itself — it does not pass through the router, so it needs its own token check.
/// Without one, the single unguarded route on an otherwise authenticated surface
/// would be the route that **approves a write or a command**: an unauthenticated
/// caller could answer "yes" to a permission prompt.
#[test]
fn an_unauthenticated_caller_cannot_answer_a_permission_prompt() {
    short_answer_timeout();
    if !common::guests_staged(&[
        "provider-openai.wasm",
        "interceptor-tool-selector.wasm",
        "interceptor-permission.wasm",
    ]) {
        return;
    }
    let dir = std::env::temp_dir().join(format!("jk-auth-prompt-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let _guard = common::TempDir(dir.clone());

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
    tool-selector:
      enabled: true
    permission:
      enabled: true
",
    )
    .unwrap();

    // First completion calls a dangerous tool, so the gate asks; then it answers.
    let calls = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0));
    let counted = std::sync::Arc::clone(&calls);
    let factory = move || -> jan_klod_core::route::HttpFn {
        let calls = std::sync::Arc::clone(&counted);
        Box::new(move |_m, _u, _h, _b, _t| {
            let n = calls.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let body = if n == 0 {
                serde_json::json!({"choices":[{"message":{"role":"assistant","tool_calls":[
                    {"id":"c1","function":{"name":"bash","arguments":"{}"}}]},
                    "finish_reason":"tool_calls"}]})
            } else {
                serde_json::json!({"choices":[{"message":{"role":"assistant",
                    "content":"all done"},"finish_reason":"stop"}]})
            };
            Ok(jan_klod_core::http::WireResponse {
                status: 200,
                headers: vec![],
                body: serde_json::to_vec(&body).unwrap(),
            })
        })
    };

    let ext_dir = common::repo_root().join("ext");
    let runtime = Runtime::boot(&config, &ext_dir).expect("runtime boots");
    let mut agent = runtime.build_agent(&factory).expect("agent boots");
    let server = Server::http("127.0.0.1:0").expect("binds");
    let port = server.server_addr().to_ip().expect("ip").port();

    let client = thread::spawn(move || {
        use std::io::{BufRead, BufReader};
        let mut stream = TcpStream::connect(("127.0.0.1", port)).expect("connects");
        let body = r#"{"message":"use bash to clean up"}"#;
        let raw = format!(
            "POST /session/p/message HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\n\
             Accept: text/event-stream\r\nAuthorization: Bearer {TOKEN}\r\n\
             Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        stream.write_all(raw.as_bytes()).unwrap();

        let mut refused = String::new();
        let mut accepted = String::new();
        let mut collected = String::new();
        let reader = BufReader::new(stream);
        for line in reader.lines() {
            let Ok(line) = line else { break };
            collected.push_str(&line);
            collected.push('\n');
            if line.starts_with("event: prompt") && refused.is_empty() {
                // An outsider tries to approve the tool call first…
                refused = request(port, "/session/p/answer", None);
                // …then the legitimate client answers. Keep the reply: this is
                // the request the rest of the test depends on, and discarding it
                // is what let a lost answer pass as success. The turn would still
                // reach `event: done` — via the timeout, whose default is a
                // denial — so every assertion below held while the suite spent
                // three minutes per lost answer.
                accepted = request(port, "/session/p/answer", Some(TOKEN));
            }
        }
        (refused, accepted, collected)
    });

    serve_once_authed(&server, &mut agent, Some(TOKEN)).expect("serves the turn");
    let (refused, accepted, stream_text) = client.join().expect("client thread");

    assert!(
        refused.contains("401"),
        "an unauthenticated answer to a permission prompt must be refused: {refused}"
    );
    assert!(
        !refused.contains("accepted"),
        "and must not be accepted: {refused}"
    );
    // The turn still completed — refusing the outsider did not consume the wait.
    assert!(
        stream_text.contains("event: done"),
        "the legitimate answer still landed: {stream_text}"
    );
    // And it completed *because it was answered*, not because the wait expired.
    // Without this the test passes either way, since a timeout takes the prompt's
    // default and the default is the denial these assertions already expect.
    assert!(
        accepted.contains("200 OK") && accepted.contains("accepted"),
        "the authenticated answer was accepted rather than lost to the timeout: {accepted}"
    );
}
