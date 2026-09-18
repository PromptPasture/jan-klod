//! Contributions over REST, which is how a browser can see them at all.
//!
//! The stdio transport pushes the set at its handshake and again when it
//! changes, because that connection stays open. A browser has neither: it
//! asks (`GET /contributions`) and, when it invokes one, is told in the answer
//! whether the set moved. These two turns are that difference, tested.
//!
//! Skips if guests are not staged in `ext/`; build with `make ext`.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::thread;

use jan_klod_core::Runtime;
use jan_klod_host::serve::serve_once;
use tiny_http::Server;

use crate::common;

/// The provider, and the one extension that contributes anything.
const GUESTS: [&str; 2] = ["provider-openai.wasm", "interceptor-system.wasm"];

/// An offline agent with `interceptor-system` enabled, and a bound server.
fn booted(tag: &str) -> Option<(common::TempDir, jan_klod_core::AgentSession, Server, u16)> {
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
    let agent = runtime
        .build_agent(&|| common::canned_http("pong"))
        .expect("agent boots");
    let server = Server::http("127.0.0.1:0").expect("binds an ephemeral port");
    let port = server.server_addr().to_ip().expect("ip addr").port();
    Some((dir, agent, server, port))
}

/// Send one request on its own thread while the session thread serves it.
fn exchange(
    agent: &mut jan_klod_core::AgentSession,
    server: &Server,
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
    serve_once(server, agent).expect("serves the request");
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
    let Some((_dir, mut agent, server, port)) = booted("read") else {
        return;
    };
    let response = exchange(&mut agent, &server, port, get("/contributions"));
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
    let Some((_dir, mut agent, server, port)) = booted("invoke") else {
        return;
    };
    let response = exchange(
        &mut agent,
        &server,
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

/// A name nobody contributes is the caller's mistake, and says so in the
/// status code rather than answering with an empty success.
#[test]
fn invoking_something_nobody_contributes_is_a_404() {
    let Some((_dir, mut agent, server, port)) = booted("unknown") else {
        return;
    };
    let response = exchange(
        &mut agent,
        &server,
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
    let Some((_dir, mut agent, server, port)) = booted("malformed") else {
        return;
    };
    let response = exchange(
        &mut agent,
        &server,
        port,
        post(
            "/contributions/invoke",
            r#"{"extension":"interceptor.system"}"#,
        ),
    );
    assert!(response.contains("400"), "{response}");
}
