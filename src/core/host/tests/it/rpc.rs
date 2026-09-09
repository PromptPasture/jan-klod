//! A client drives the core over newline-delimited JSON-RPC.
//!
//! No subprocess and no threads: `rpc::serve` is generic over its streams, so
//! the whole exchange is a `Cursor` of frames in and a `Vec<u8>` of frames out.
//! That works because every command here is answered without the core asking
//! anything back — a scripted client is enough. The turn commands are not, and
//! are refused for now, which is what the last case below asserts.
//!
//! Skips (passes as a no-op) when the guests are not staged in `ext/`.

use std::io::Cursor;

use jan_klod_core::rpc;
use jan_klod_core::Runtime;
use jan_klod_protocol::{jsonrpc, PROTOCOL_VERSION};

use crate::common;

/// Boot an offline agent: a canned provider, no live endpoint.
fn booted(tag: &str) -> Option<(common::TempDir, jan_klod_core::AgentSession)> {
    if !common::guests_staged(&["provider-openai.wasm", "interceptor-intent-router.wasm"]) {
        return None;
    }
    let dir =
        common::TempDir(std::env::temp_dir().join(format!("jk-rpc-{tag}-{}", std::process::id())));
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
    intent-router:
      enabled: true
",
    )
    .expect("writes the config");
    let runtime = Runtime::boot(&config, common::repo_root().join("ext")).expect("runtime boots");
    let factory = || common::canned_http("pong");
    let agent = runtime.build_agent(&factory).expect("agent boots");
    Some((dir, agent))
}

/// Every response, in order, with its id and outcome.
fn exchange(agent: &jan_klod_core::AgentSession, frames: &[&str]) -> Vec<jsonrpc::Response> {
    let input = Cursor::new(frames.join("\n").into_bytes());
    let mut output = Vec::new();
    rpc::serve(input, &mut output, agent).expect("the loop runs to EOF");
    String::from_utf8(output)
        .expect("frames are utf-8")
        .lines()
        .map(|line| serde_json::from_str(line).unwrap_or_else(|e| panic!("{line}: {e}")))
        .collect()
}

/// The code of an error response, or `None` when it carried a result.
const fn code(response: &jsonrpc::Response) -> Option<i64> {
    match &response.outcome {
        jsonrpc::Outcome::Error(error) => Some(error.code),
        jsonrpc::Outcome::Result(_) => None,
    }
}

/// The result of a response, panicking with the error if it failed.
fn result(response: &jsonrpc::Response) -> &serde_json::Value {
    match &response.outcome {
        jsonrpc::Outcome::Result(value) => value,
        jsonrpc::Outcome::Error(error) => {
            panic!("expected a result, got {}: {}", error.code, error.message)
        }
    }
}

#[test]
fn a_client_negotiates_then_drives_the_read_only_commands() {
    let Some((_dir, agent)) = booted("readonly") else {
        return;
    };
    let responses = exchange(
        &agent,
        &[
            // Before the handshake: refused, because a version nobody agreed is
            // a version nobody checked.
            r#"{"jsonrpc":"2.0","id":1,"method":"session/list"}"#,
            &format!(
                r#"{{"jsonrpc":"2.0","id":2,"method":"protocol/hello","params":{{"version":"{PROTOCOL_VERSION}"}}}}"#
            ),
            r#"{"jsonrpc":"2.0","id":3,"method":"session/create"}"#,
            r#"{"jsonrpc":"2.0","id":"four","method":"session/list"}"#,
            r#"{"jsonrpc":"2.0","id":5,"method":"session/get","params":{"session":"nothing-here"}}"#,
            r#"{"jsonrpc":"2.0","id":6,"method":"session/fork","params":{"session":"nothing-here","at-seq":3}}"#,
            r#"{"jsonrpc":"2.0","id":7,"method":"session/message","params":{"session":"s","message":"hi"}}"#,
        ],
    );
    assert_eq!(responses.len(), 7, "one answer per frame");

    // 1 — nothing is served before the handshake.
    assert_eq!(code(&responses[0]), Some(jsonrpc::INVALID_REQUEST));
    assert_eq!(responses[0].id, jsonrpc::Id::Number(1));

    // 2 — the core answers with its own version.
    assert_eq!(
        result(&responses[1])["version"],
        serde_json::json!(PROTOCOL_VERSION)
    );

    // 3 — a fresh session id, minted the way every transport mints them.
    let created = result(&responses[2])["id"]
        .as_str()
        .expect("an id")
        .to_owned();
    assert!(!created.is_empty(), "a created session has an id");

    // 4 — the list is a list, and the string id came back a string.
    assert_eq!(responses[3].id, jsonrpc::Id::Text("four".to_owned()));
    assert!(
        result(&responses[3])["sessions"].is_array(),
        "sessions is a list: {}",
        result(&responses[3])
    );

    // 5 — an unknown session is an empty transcript, not an error. The same
    // answer `GET /session/:id` gives, because it is the same projection.
    assert_eq!(
        result(&responses[4]),
        &serde_json::json!({ "id": "nothing-here", "messages": [] })
    );

    // 6 — forking a session with no events is the caller's mistake, and is
    // reported as one rather than as a broken store.
    assert_eq!(code(&responses[5]), Some(jsonrpc::INVALID_PARAMS));

    // 7 — a turn command is refused while this transport cannot run turns. A
    // client that is told so can fall back; one whose turn silently never
    // starts cannot.
    assert_eq!(code(&responses[6]), Some(jsonrpc::METHOD_NOT_FOUND));
    assert_eq!(responses[6].id, jsonrpc::Id::Number(7));
}

/// The refusal has to *end* the connection, not merely say no. Asserted by
/// sending a frame after it and showing nothing answers: an error the client can
/// keep talking past is a warning, not a refusal.
#[test]
fn an_incompatible_client_is_refused_and_hung_up_on() {
    let Some((_dir, agent)) = booted("version") else {
        return;
    };
    let responses = exchange(
        &agent,
        &[
            r#"{"jsonrpc":"2.0","id":1,"method":"protocol/hello","params":{"version":"9.9.9"}}"#,
            r#"{"jsonrpc":"2.0","id":2,"method":"session/list"}"#,
        ],
    );
    assert_eq!(
        responses.len(),
        1,
        "the second frame was never served: {responses:#?}"
    );
    assert_eq!(code(&responses[0]), Some(jsonrpc::INCOMPATIBLE_VERSION));
    // And the refusal says what this core does speak, since the client gets no
    // `HelloResult` to read it from.
    let jsonrpc::Outcome::Error(error) = &responses[0].outcome else {
        panic!("an error")
    };
    assert_eq!(
        error.data.as_ref().expect("carries data")["version"],
        serde_json::json!(PROTOCOL_VERSION)
    );
}

/// A `0.x` minor difference is incompatible too — the rule the protocol crate
/// spells out, checked here at the transport where it actually decides.
#[test]
fn a_client_one_minor_behind_is_refused() {
    let Some((_dir, agent)) = booted("minor") else {
        return;
    };
    // Guard the premise: if PROTOCOL_VERSION ever leaves 0.1, this frame stops
    // testing what it says it tests.
    assert!(
        PROTOCOL_VERSION.starts_with("0.1."),
        "this case is written against a 0.1 core, not {PROTOCOL_VERSION}"
    );
    let responses = exchange(
        &agent,
        &[r#"{"jsonrpc":"2.0","id":1,"method":"protocol/hello","params":{"version":"0.2.0"}}"#],
    );
    assert_eq!(responses.len(), 1);
    assert_eq!(code(&responses[0]), Some(jsonrpc::INCOMPATIBLE_VERSION));
}
