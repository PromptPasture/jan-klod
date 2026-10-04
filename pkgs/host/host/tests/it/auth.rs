//! The REST surface refuses unauthenticated callers when a token is set.
//!
//! Until now it refused nobody: `config.yaml` advertised an `api-key` that
//! nothing read, so a surface was open to anyone who could reach it. This drives
//! the real server and asserts on **status codes off the wire**.
//!
//! Skips (passes as a no-op) when the guests are not staged in `ext/`.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::thread;

use jan_klod_core::Runtime;
use jan_klod_host::serve::Surface;
use jan_klod_host::sessions::Agents;

use crate::common;
use std::collections::HashMap;

const TOKEN: &str = "s3cret-token";

/// Keep a lost answer loud: the confirmation wait defaults to three minutes,
/// and a lost answer falls back to a denial-timeout, so the test can pass either way.
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

/// Send a raw request with an optional `Authorization` header and return
/// the status line plus body.
fn request(port: u16, target: &str, auth: Option<&str>) -> String {
    let header = auth.map_or_else(String::new, |t| format!("Authorization: Bearer {t}\r\n"));
    // The answer route takes a different body — a 400 here reads to the waiting
    // driver as "still no answer" and the test would stall for the full timeout.
    let body = if target.ends_with("/answer") {
        r#"{"answer":"yes"}"#
    } else {
        r#"{"message":"hello"}"#
    };
    // Use GET for open paths and /health, POST for everything else
    let raw = if matches!(target, "/health" | "/" | "/app.js") {
        format!("GET {target} HTTP/1.1\r\nHost: localhost\r\n{header}Connection: close\r\n\r\n")
    } else {
        format!(
            "POST {target} HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\n\
             {header}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
    };
    let mut stream = TcpStream::connect(("127.0.0.1", port)).expect("connects");
    // A lost response must fail the test, not hang it: this is a raw socket, and
    // `read_to_string` otherwise waits for the server to close forever (#286).
    stream
        .set_read_timeout(Some(std::time::Duration::from_secs(60)))
        .unwrap();
    stream.write_all(raw.as_bytes()).unwrap();
    let mut response = String::new();
    stream
        .read_to_string(&mut response)
        .expect("the response arrives within 60s");
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
    let agents = Agents::new(runtime, std::sync::Arc::new(factory));
    let surface = Surface::bind("127.0.0.1:0").expect("binds an ephemeral port");

    // One surface for all four requests: a `serve_once_authed` per request
    // races, because a surface that is stopping can still accept the next
    // connection and then drop it unanswered (#246).
    let (none, wrong, right, health) = surface.serve_while(
        &agents,
        {
            let mut principals = HashMap::new();
            principals.insert("operator".to_string(), TOKEN.to_string());
            principals
        },
        |port| {
            let none = request(port, "/session/a/message", None);
            let wrong = request(port, "/session/a/message", Some("not-the-token"));
            let right = request(port, "/session/a/message", Some(TOKEN));
            let health = request(port, "/health", None);
            (none, wrong, right, health)
        },
    );

    assert!(none.contains("401"), "no header is refused: {none}");
    assert!(wrong.contains("401"), "a wrong token is refused: {wrong}");
    assert!(
        !none.contains("pong") && !wrong.contains("pong"),
        "a refused request never reached the agent"
    );
    assert!(
        right.contains("200 OK"),
        "the right token is served: {right}"
    );
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
    let agents = Agents::new(runtime, std::sync::Arc::new(factory));
    let surface = Surface::bind("127.0.0.1:0").expect("binds an ephemeral port");
    let port = surface.port();

    let client = thread::spawn(move || request(port, "/session/a/message", None));
    surface
        .serve_once_authed(&agents, HashMap::new())
        .expect("serves");
    let response = client.join().expect("client thread");

    // Requiring a secret to talk to your own loopback is friction without a
    // threat, so the default stays open.
    assert!(
        response.contains("200 OK"),
        "no token configured means no gate: {response}"
    );
    assert!(response.contains("pong"), "and the turn runs: {response}");
}

/// The one endpoint that must never be open: while a turn is parked on a
/// confirmation, the waiting driver serves the socket itself, bypassing the
/// router, so it needs its own token check.
/// Drive one confirmation: open the stream, let an outsider try to answer,
/// then answer properly. Returns the outsider's reply, the real one, and
/// the stream text.
///
/// The assertions stayed in the test; this is the plumbing. A surface that
/// serves while a closure runs (#224) puts the client's whole conversation
/// in the test body, and this is what takes it back out.
fn drive_confirmation(port: u16) -> (String, String, String) {
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
            // Outsider tries first…
            refused = request(port, "/session/p/answer", None);
            // …then the legitimate client answers. Keep the reply to confirm
            // it wasn't lost to the timeout.
            accepted = request(port, "/session/p/answer", Some(TOKEN));
        }
    }
    (refused, accepted, collected)
}

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
    let agents = Agents::new(runtime, std::sync::Arc::new(factory));
    let surface = Surface::bind("127.0.0.1:0").expect("binds an ephemeral port");
    let port = surface.port();

    let (refused, accepted, stream_text) = surface.serve_while(
        &agents,
        {
            let mut principals = HashMap::new();
            principals.insert("operator".to_string(), TOKEN.to_string());
            principals
        },
        move |_| drive_confirmation(port),
    );

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
    // And it completed *because it was answered*, not because the wait expired
    // into the same denial these assertions would otherwise also satisfy.
    assert!(
        accepted.contains("200 OK") && accepted.contains("accepted"),
        "the authenticated answer was accepted rather than lost to the timeout: {accepted}"
    );
}

#[test]
fn principal_resolves_from_credentials_in_the_guard() {
    short_answer_timeout();
    if !common::guests_staged(&["provider-openai.wasm"]) {
        return;
    }
    let dir = std::env::temp_dir().join(format!("jk-principal-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let _guard = common::TempDir(dir.clone());

    let ext_dir = common::repo_root().join("ext");
    let config = write_config(&dir);
    let runtime = Runtime::boot(&config, &ext_dir).expect("runtime boots");
    let factory = || common::canned_http("pong");
    let agents = Agents::new(runtime, std::sync::Arc::new(factory));
    let surface = Surface::bind("127.0.0.1:0").expect("binds an ephemeral port");

    let mut principals = HashMap::new();
    principals.insert("operator".to_string(), "operator-secret".to_string());
    principals.insert("admin".to_string(), "admin-secret".to_string());

    // Multiple principals with different tokens, all on one surface (#246).
    let (
        no_token,
        wrong_token,
        op_token,
        admin_token,
        health_no_auth,
        health_with_auth,
        index_no_auth,
        app_js_no_auth,
    ) = surface.serve_while(&agents, principals, |port| {
        let no_token = request(port, "/session/a/message", None);
        let wrong_token = request(port, "/session/a/message", Some("wrong-token"));
        let op_token = request(port, "/session/a/message", Some("operator-secret"));
        let admin_token = request(port, "/session/a/message", Some("admin-secret"));
        let health_no_auth = request(port, "/health", None);
        let health_with_auth = request(port, "/health", Some("operator-secret"));
        let index_no_auth = request(port, "/", None);
        let app_js_no_auth = request(port, "/app.js", None);
        (
            no_token,
            wrong_token,
            op_token,
            admin_token,
            health_no_auth,
            health_with_auth,
            index_no_auth,
            app_js_no_auth,
        )
    });

    // No token -> 401
    assert!(
        no_token.contains("401"),
        "no token should be refused: {no_token}"
    );

    // Wrong token -> 401
    assert!(
        wrong_token.contains("401"),
        "wrong token should be refused: {wrong_token}"
    );

    // Correct token -> 200
    assert!(
        op_token.contains("200 OK"),
        "operator token should be accepted: {op_token}"
    );
    assert!(
        op_token.contains("pong"),
        "and should reach the agent: {op_token}"
    );

    // Another principal's token -> 200
    assert!(
        admin_token.contains("200 OK"),
        "admin token should be accepted: {admin_token}"
    );
    assert!(
        admin_token.contains("pong"),
        "and should reach the agent: {admin_token}"
    );

    // /health stays open without credentials
    assert!(
        health_no_auth.contains("200 OK"),
        "/health should be open without token: {health_no_auth}"
    );
    assert!(
        !health_no_auth.contains("401"),
        "/health should not be refused: {health_no_auth}"
    );

    // /health stays open even with credentials
    assert!(
        health_with_auth.contains("200 OK"),
        "/health should be open with token: {health_with_auth}"
    );

    // / (index) stays open without credentials
    assert!(
        index_no_auth.contains("200 OK"),
        "/ should be open without token: {index_no_auth}"
    );

    // /app.js stays open without credentials
    assert!(
        app_js_no_auth.contains("200 OK"),
        "/app.js should be open without token: {app_js_no_auth}"
    );
}
