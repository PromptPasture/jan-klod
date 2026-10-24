//! The bearer-token principal reaches the before-loop interceptor through the
//! REST surface, per session. A guest that ignores `principal` still works.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::thread;

use jk_agent::Runtime;
use jk_host::serve::Surface;
use jk_host::sessions::Agents;

use crate::common;

const ALICE_TOKEN: &str = "alice-token-secret";
const BOB_TOKEN: &str = "bob-token-secret";

fn write_config(dir: &std::path::Path, with_principal_guest: bool) -> std::path::PathBuf {
    std::fs::create_dir_all(dir).unwrap();
    let config = dir.join("config.yaml");
    let config_content = if with_principal_guest {
        "
extensions:
  provider:
    openai:
      enabled: true
      base-url: http://mock/v1
      model: mock-1
      api-key: test
  interceptor:
    interceptor-principal-probe:
      enabled: true
"
    } else {
        "
extensions:
  provider:
    openai:
      enabled: true
      base-url: http://mock/v1
      model: mock-1
      api-key: test
"
    };
    std::fs::write(&config, config_content).unwrap();
    config
}

/// Sends a raw HTTP request, with an optional bearer token, and returns the response.
fn send_message(port: u16, session: &str, auth: Option<&str>, message: &str) -> String {
    let header = auth.map_or_else(String::new, |t| format!("Authorization: Bearer {t}\r\n"));
    let body = format!(r#"{{"message":"{message}"}}"#);
    let raw = format!(
        "POST /session/{session}/message HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\n\
         {header}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let mut stream = TcpStream::connect(("127.0.0.1", port)).expect("connects to localhost");
    // A lost response must fail the test, not hang it (#286).
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
fn two_sessions_with_different_principals_each_see_their_own_principal() {
    let ext_dir = common::repo_root().join("ext");
    // Build the probe with `make extensions`.
    if !common::guests_staged(&["provider-openai.wasm", "interceptor-principal-probe.wasm"]) {
        return;
    }

    let dir = std::env::temp_dir().join(format!("jk-principal-boundary-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let _guard = common::TempDir(dir.clone());

    let config = write_config(&dir, true);
    let runtime = Runtime::boot(&config, &ext_dir).expect("runtime boots");
    let factory = || common::canned_http("pong");
    let agents = Agents::new(runtime, std::sync::Arc::new(factory));
    let surface = Surface::bind("127.0.0.1:0").expect("binds an ephemeral port");

    let principals = {
        let mut p = HashMap::new();
        p.insert("alice".to_string(), ALICE_TOKEN.to_string());
        p.insert("bob".to_string(), BOB_TOKEN.to_string());
        p
    };

    // One surface for all requests: a per-request serve_once_authed races a stopping surface (#246).
    let (alice_1, bob_1, alice_2) = surface.serve_while(&agents, principals, |port| {
        let alice_1 = send_message(port, "session-alice", Some(ALICE_TOKEN), "hello from alice");
        let bob_1 = send_message(port, "session-bob", Some(BOB_TOKEN), "hello from bob");
        let alice_2 = send_message(
            port,
            "session-alice",
            Some(ALICE_TOKEN),
            "another from alice",
        );
        (alice_1, bob_1, alice_2)
    });

    // A 200 means the probe ran at before-loop without error.
    assert!(
        alice_1.contains("200") && bob_1.contains("200") && alice_2.contains("200"),
        "all three requests should succeed: alice_1={alice_1} bob_1={bob_1} alice_2={alice_2}"
    );
}

/// A guest that ignores `principal` still works after the `user-turn` contract change.
#[test]
fn a_guest_ignoring_the_principal_field_still_works() {
    let ext_dir = common::repo_root().join("ext");
    if !common::guests_staged(&["provider-openai.wasm"]) {
        return;
    }

    let dir = std::env::temp_dir().join(format!("jk-principal-ignore-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let _guard = common::TempDir(dir.clone());

    let config = write_config(&dir, false);
    let runtime = Runtime::boot(&config, &ext_dir).expect("runtime boots");
    let factory = || common::canned_http("pong");
    let agents = Agents::new(runtime, std::sync::Arc::new(factory));
    let surface = Surface::bind("127.0.0.1:0").expect("binds an ephemeral port");
    let port = surface.port();

    let client = thread::spawn(move || send_message(port, "session-test", None, "hello"));

    // No principals configured: serve.rs allows loopback-only access.
    let principals = HashMap::new();

    surface
        .serve_once_authed(&agents, principals)
        .expect("serves");

    let response = client.join().expect("client thread");

    assert!(
        response.contains("200"),
        "request should succeed (loopback-only allows all): {response}"
    );
}
