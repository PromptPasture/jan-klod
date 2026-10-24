//! Contributions over REST: unlike stdio, a browser must poll `GET /contributions`
//! and learns from an invoke's answer whether the set changed.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::thread;

use jk_agent::Runtime;
use jk_host::serve::Surface;
use jk_host::sessions::Agents;

use crate::common;

/// The provider, and the one extension that contributes anything.
const GUESTS: [&str; 2] = ["provider-openai.wasm", "interceptor-system.wasm"];

/// An offline agent with `interceptor-system` enabled, and a bound server.
fn booted(tag: &str) -> Option<(common::TempDir, std::sync::Arc<Agents>, Surface, u16)> {
    if !common::guests_staged(&GUESTS) {
        return None;
    }
    let dir = common::TempDir(
        std::env::temp_dir().join(format!("jk-contrib-rest-{tag}-{}", std::process::id())),
    );
    std::fs::create_dir_all(&dir.0).expect("creates the temp dir");
    let config = dir.0.join("config.yaml");
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
    system:
      enabled: true
",
    )
    .expect("writes the config");
    let runtime = Runtime::boot(&config, common::repo_root().join("ext")).expect("runtime boots");
    let agents = Agents::new(runtime, std::sync::Arc::new(|| common::canned_http("pong")));
    let surface = Surface::bind("127.0.0.1:0").expect("binds an ephemeral port");
    let port = surface.port();
    Some((dir, agents, surface, port))
}

/// Send one request on its own thread while the session thread serves it.
fn exchange(
    agents: &std::sync::Arc<Agents>,
    surface: &Surface,
    port: u16,
    request: String,
) -> String {
    let client = thread::spawn(move || {
        let mut stream = TcpStream::connect(("127.0.0.1", port)).expect("connects");
        stream.write_all(request.as_bytes()).expect("writes");
        let mut response = String::new();
        stream.read_to_string(&mut response).expect("reads");
        response
    });
    surface.serve_once(agents).expect("serves the request");
    client.join().expect("client thread")
}

fn get(path: &str) -> String {
    format!("GET {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
}

fn post(path: &str, body: &str) -> String {
    format!(
        "POST {path} HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\n\
         Content-Length: {}\r\nConnection: close\r\n\r\n{}",
        body.len(),
        body
    )
}

#[test]
fn a_browser_can_read_what_the_extensions_contribute() {
    let Some((_dir, agents, surface, port)) = booted("read") else {
        return;
    };
    let response = exchange(&agents, &surface, port, get("/contributions"));
    assert!(response.contains("200 OK"), "{response}");
    assert!(
        response.contains("\"extension\":\"interceptor.system\""),
        "the set names who declared it: {response}"
    );
    assert!(
        response.contains("\"name\":\"prompt\""),
        "the contributed command is there: {response}"
    );
    assert!(
        response.contains("\"status-items\""),
        "status items travel too, spelled as the protocol spells them: {response}"
    );
}

#[test]
fn a_browser_can_invoke_one_and_is_told_whether_the_set_moved() {
    let Some((_dir, agents, surface, port)) = booted("invoke") else {
        return;
    };
    let response = exchange(
        &agents,
        &surface,
        port,
        post(
            "/contributions/invoke",
            r#"{"extension":"interceptor.system","name":"prompt","arguments":[{"name":"verbose","value":"true"}]}"#,
        ),
    );
    assert!(response.contains("200 OK"), "{response}");
    assert!(
        response.contains("You are jan-klod"),
        "the extension's own answer came back: {response}"
    );
    assert!(
        response.contains("\"contributions-changed\":false"),
        "and whether to re-read the set: {response}"
    );
}

/// A name nobody contributes is refused with an error status, not an empty success.
#[test]
fn invoking_something_nobody_contributes_is_a_404() {
    let Some((_dir, agents, surface, port)) = booted("unknown") else {
        return;
    };
    let response = exchange(
        &agents,
        &surface,
        port,
        post(
            "/contributions/invoke",
            r#"{"extension":"interceptor.system","name":"not-a-command"}"#,
        ),
    );
    assert!(response.contains("404"), "{response}");
}

#[test]
fn an_invocation_without_a_name_is_refused_before_it_reaches_an_extension() {
    let Some((_dir, agents, surface, port)) = booted("malformed") else {
        return;
    };
    let response = exchange(
        &agents,
        &surface,
        port,
        post(
            "/contributions/invoke",
            r#"{"extension":"interceptor.system"}"#,
        ),
    );
    assert!(response.contains("400"), "{response}");
}
