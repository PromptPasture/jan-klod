//! The embedded web client is served, and serving it opened nothing else.
//!
//! `serve.rs` embeds `src/web/dist/` with `include_str!` and answers `GET /`
//! and `GET /app.js` from it. Those two paths are exempt from the bearer-token
//! check, because a browser cannot put an `Authorization` header on the
//! navigation that fetches the page it is about to run.
//!
//! **That exemption is the risk, and it is why these two tests are one file.**
//! A static handler mounted at `/` that matched by prefix rather than by exact
//! path would serve the page *and* shadow every API route behind it —
//! unauthenticated. So the claim being made here is a conjunction:
//!
//! 1. the page and its script come back to a caller with no credentials, and
//! 2. an API route on the same surface still refuses that same caller.
//!
//! The second is what makes the first safe. Either one alone reads green while
//! the pair is broken: (1) passes on a handler that serves everything, and (2)
//! passes on a build that serves nothing.
//!
//! Bodies are asserted, not just status codes — an empty `dist/` would answer
//! `200` to both routes and prove nothing about the bundle.
//!
//! Skips (passes as a no-op) when the guests are not staged in `ext/`.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::thread;

use jan_klod_core::serve::serve_once_authed;
use jan_klod_core::Runtime;
use tiny_http::Server;

use crate::common;

const TOKEN: &str = "s3cret-token";

/// The smallest config that boots an agent. Nothing here reaches a provider:
/// neither route under test touches the agent at all, but `serve_once_authed`
/// needs one to hand the other routes.
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

/// `GET <target>` with no `Authorization` header at all — a browser's first
/// request for a page it has never seen.
fn get_unauthenticated(port: u16, target: &str) -> String {
    let raw = format!("GET {target} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n");
    let mut stream = TcpStream::connect(("127.0.0.1", port)).expect("connects");
    stream.write_all(raw.as_bytes()).unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).unwrap();
    response
}

/// Claim 1: the page and its script are served to a caller with no token.
#[test]
fn the_web_client_is_served_without_a_token() {
    if !common::guests_staged(&["provider-openai.wasm"]) {
        return;
    }
    let dir = std::env::temp_dir().join(format!("jk-web-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let _guard = common::TempDir(dir.clone());

    let ext_dir = common::repo_root().join("ext");
    let config = write_config(&dir);
    let runtime = Runtime::boot(&config, &ext_dir).expect("runtime boots");
    let factory = || common::canned_http("pong");
    let mut agent = runtime.build_agent(&factory).expect("agent boots");
    let server = Server::http("127.0.0.1:0").expect("binds");
    let port = server.server_addr().to_ip().expect("ip").port();

    let client = thread::spawn(move || {
        let page = get_unauthenticated(port, "/");
        let script = get_unauthenticated(port, "/app.js");
        (page, script)
    });
    // A token *is* configured. Serving these two anyway is the exemption under
    // test; with `None` here the test would pass on a surface with no gate at
    // all.
    for _ in 0..2 {
        serve_once_authed(&server, &mut agent, Some(TOKEN)).expect("serves");
    }
    let (page, script) = client.join().expect("client thread");

    assert!(page.contains("200 OK"), "GET / is served: {page}");
    assert!(
        page.contains("text/html"),
        "and as HTML, or the browser shows the source: {page}"
    );
    // Not just any 200: the bytes are the committed bundle's. `dist/index.html`
    // loads exactly one script, and its name is the other route below.
    assert!(
        page.contains("app.js"),
        "the page served is the bundle's index, which loads app.js: {page}"
    );

    assert!(script.contains("200 OK"), "GET /app.js is served: {script}");
    assert!(
        script.contains("text/javascript"),
        "and as JavaScript, or the browser refuses to execute it: {script}"
    );
    // `app.js` is the client that talks to this same surface; the session route
    // it calls is the cheapest proof the body is the real bundle and not an
    // error page that happened to arrive with a 200.
    assert!(
        script.contains("/session/"),
        "the script served is the client, which calls the session API: {script}"
    );
}

/// Claim 2, and the one that makes claim 1 safe: the exemption is two exact
/// paths, not a prefix, so the API is still shut to the same caller.
///
/// `GET /sessions` is the sharp probe on purpose. It is a `GET`, like the two
/// exempt routes, and it lists every session on the box — so a prefix match at
/// `/` that swallowed it would hand session titles to anyone who could reach
/// the port. `auth.rs` covers the `POST` side of the same gate.
#[test]
fn an_api_route_still_refuses_without_a_token() {
    if !common::guests_staged(&["provider-openai.wasm"]) {
        return;
    }
    let dir = std::env::temp_dir().join(format!("jk-web-closed-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let _guard = common::TempDir(dir.clone());

    let ext_dir = common::repo_root().join("ext");
    let config = write_config(&dir);
    let runtime = Runtime::boot(&config, &ext_dir).expect("runtime boots");
    let factory = || common::canned_http("pong");
    let mut agent = runtime.build_agent(&factory).expect("agent boots");
    let server = Server::http("127.0.0.1:0").expect("binds");
    let port = server.server_addr().to_ip().expect("ip").port();

    let client = thread::spawn(move || {
        let sessions = get_unauthenticated(port, "/sessions");
        // A path that merely *starts* with an exempt one. `/app.jsx` is not
        // `/app.js`, and the difference is a `starts_with` away.
        let near_miss = get_unauthenticated(port, "/app.jsx");
        (sessions, near_miss)
    });
    for _ in 0..2 {
        serve_once_authed(&server, &mut agent, Some(TOKEN)).expect("serves");
    }
    let (sessions, near_miss) = client.join().expect("client thread");

    assert!(
        sessions.contains("401"),
        "GET /sessions is still refused without a token: {sessions}"
    );
    assert!(
        near_miss.contains("401"),
        "an exempt path is an exact path, not a prefix: {near_miss}"
    );
}
