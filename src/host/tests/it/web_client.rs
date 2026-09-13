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
//! Bodies are asserted, not codes alone — an empty `dist/` would answer `200`
//! to both routes and prove nothing about the bundle.
//!
//! # What proves Acceptance line 1, and what does not
//!
//! [#119](https://github.com/PromptPasture/jan-klod/issues/119) asks that a
//! browser at `/` run a full turn with `ask` and cancel. No browser runs here;
//! pretending otherwise by ticking a served file violates the plan. The honest
//! split:
//!
//! **Asserted, in this gate.** The core serves the real bundle at the two paths
//! above and refuses the API without a token (this file). The routes that
//! bundle calls answer over a real socket against a booted agent — `api_rest`
//! for sessions and messages, `api_prompt` for the `ask` round trip,
//! `prompt_disconnect` for cancel, which on this surface *is* a dropped SSE
//! connection rather than a route. And
//! [`the_web_client_answers_every_frame_the_core_emits`] pins the one seam the
//! other two halves cannot see between them.
//!
//! **Asserted, but not here.** `src/web/tests/turn.test.ts` drives the client's
//! own logic — every frame kind rendered, an `ask` answered on a second
//! request, cancel dropping the connection — against a stub. It is a real test
//! and it passes, but nothing in `make gate` runs it, so this file does not
//! lean on it. Wiring it in is
//! [#127](https://github.com/PromptPasture/jan-klod/issues/127).
//!
//! **Inferred.** That a browser's JavaScript engine, executing these exact
//! bytes, makes those exact calls. Closing that needs a real browser, which
//! [#118](https://github.com/PromptPasture/jan-klod/issues/118) costed and
//! declined — and the decision there is worth keeping: a stub grown to imitate
//! a DOM is the thing that passes while the client is broken.
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

/// Minimal config to boot an agent. Neither route touches the agent, but
/// `serve_once_authed` needs one to hand the other routes.
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

/// `GET <target>` with no `Authorization` header — a browser's first request
/// for an unfamiliar page.
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
    // test; with `None`, the test would pass on any surface.
    for _ in 0..2 {
        serve_once_authed(&server, &mut agent, Some(TOKEN)).expect("serves");
    }
    let (page, script) = client.join().expect("client thread");

    assert!(page.contains("200 OK"), "GET / is served: {page}");
    assert!(
        page.contains("text/html"),
        "and as HTML, or the browser shows the source: {page}"
    );
    // Not just any 200: the bytes are the committed bundle's, and its name is
    // the other route below.
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

/// The web client has an answer for every frame the core can send.
///
/// `SSE_FRAME_KINDS`' own docs name the two tests that hold its ends:
/// `core/tests/protocol_events.rs` proves the core emits exactly these, and
/// `ui/tests/parse_frame.rs` proves *a* client handles each. That second test
/// is the **TUI**. The web client is a second client carrying its own copy of
/// the list in TypeScript, and until this test it was outside both — so adding
/// a frame to the core would fail neither while the browser quietly rendered it
/// as unknown.
///
/// That is not a hypothetical failure mode in this repository: it is one that
/// already shipped. The TUI called every `tool-result` an unknown frame while
/// the stdio transport dropped them without a word.
///
/// This reads the TypeScript as text instead of running Node, since the core
/// builds without Node on the path. A seven-string list is within what a grep
/// can check honestly; anything more would be a parser, and a parser that
/// silently matches nothing is the failure this test exists to prevent — hence
/// the count assertion before the comparison.
#[test]
fn the_web_client_answers_every_frame_the_core_emits() {
    let path = common::repo_root().join("src/web/src/frames.ts");
    let source = std::fs::read_to_string(&path).expect("frames.ts is readable");

    let list = source
        .split_once("export const FRAME_KINDS = [")
        .map_or_else(
            || panic!("{} must export FRAME_KINDS", path.display()),
            |(_, rest)| rest,
        )
        .split_once("] as const;")
        .map_or_else(
            || panic!("FRAME_KINDS must be closed by `] as const;`"),
            |(inside, _)| inside,
        );
    let kinds: Vec<&str> = list
        .split(['"', '\n'])
        .map(str::trim)
        .filter(|s| !s.is_empty() && *s != ",")
        .collect();

    // Before comparing: a grep that matched nothing would make the comparison
    // below read "the client handles no frames", and an empty-vs-empty bug is
    // exactly how a text check passes while proving nothing.
    assert_eq!(
        kinds.len(),
        jan_klod_protocol::SSE_FRAME_KINDS.len(),
        "parsed {kinds:?} out of {} — if that list looks wrong, the parse broke, \
         not the client",
        path.display()
    );
    assert_eq!(
        kinds,
        jan_klod_protocol::SSE_FRAME_KINDS.to_vec(),
        "the web client's FRAME_KINDS has drifted from the core's \
         SSE_FRAME_KINDS — a frame the core emits and the browser renders as \
         unknown. Update {} to match.",
        path.display()
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
