//! The MCP port, driven as an editor drives it.
//!
//! `core::mcp`'s own tests cover the frame rules without a runtime. What only
//! this module can show is a **whole exchange against a real agent**:
//! `initialize`, the notification every client sends next, `tools/list`, and a
//! `tools/call ask` that runs an actual turn against a canned provider and
//! comes back with the answer.
//!
//! The provider is canned rather than live, so this is offline — which is what
//! `## Acceptance` asks for, and what lets it run in `make gate`.

use jan_klod_core::Runtime;

use crate::common;

/// Boot an offline agent with a canned provider, as `rpc.rs` does.
fn booted(
    tag: &str,
    reply: &'static str,
) -> Option<(common::TempDir, jan_klod_core::AgentSession)> {
    if !common::guests_staged(&["provider-openai.wasm", "interceptor-intent-router.wasm"]) {
        return None;
    }
    let dir =
        common::TempDir(std::env::temp_dir().join(format!("jk-mcp-{tag}-{}", std::process::id())));
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
    let http = || common::canned_http(reply);
    let agent = runtime.build_agent(&http).expect("agent boots");
    Some((dir, agent))
}

/// Feed `lines` to the server and read every frame it writes back.
fn exchange(agent: &mut jan_klod_core::AgentSession, lines: &[&str]) -> Vec<serde_json::Value> {
    let input = lines.join("\n") + "\n";
    let mut output = Vec::new();
    jan_klod_core::mcp::serve(std::io::Cursor::new(input), &mut output, agent)
        .expect("the server writes its frames");
    let text = String::from_utf8(output).expect("frames are UTF-8");
    text.lines()
        .map(|line| {
            // Every line must parse: the stdio transport says frames are
            // newline-delimited and stdout carries nothing else, so a line that
            // is not JSON would mean something leaked into the protocol stream.
            serde_json::from_str(line)
                .unwrap_or_else(|err| panic!("{line:?} is not a frame: {err}"))
        })
        .collect()
}

/// The exchange an editor actually performs, end to end.
#[test]
fn an_editor_initializes_lists_tools_and_calls_ask() {
    let Some((_dir, mut agent)) = booted("full", "the answer from the model") else {
        return;
    };

    let frames = exchange(
        &mut agent,
        &[
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"editor","version":"1"}}}"#,
            r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
            r#"{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}"#,
            r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"ask","arguments":{"question":"what is this repo"}}}"#,
        ],
    );

    // Three requests, one notification, three frames — the notification earns
    // none, and a server that answered it would be talking to nobody.
    assert_eq!(frames.len(), 3, "{frames:#?}");
    assert_eq!(frames[0]["id"], 1);
    assert_eq!(frames[0]["result"]["protocolVersion"], "2025-11-25");
    assert_eq!(frames[1]["id"], 2);
    assert_eq!(
        frames[1]["result"]["tools"].as_array().map(Vec::len),
        Some(3)
    );

    // The turn ran and its answer came back as tool content.
    let call = &frames[2];
    assert_eq!(call["id"], 3);
    assert_eq!(
        call["result"]["isError"],
        serde_json::Value::Bool(false),
        "a turn that answered is not an error: {call:#?}"
    );
    assert_eq!(call["result"]["content"][0]["type"], "text");
    assert_eq!(
        call["result"]["content"][0]["text"], "the answer from the model",
        "the model's answer is what the client receives"
    );
    assert!(
        call.get("error").is_none(),
        "and it arrives as a result, not a JSON-RPC error: {call:#?}"
    );
}

/// `ask` without its required argument is refused **in band**.
///
/// The distinction this pins: a bad argument is the *tool* failing, so it comes
/// back as `isError: true` with a reason the model can read and correct. A
/// JSON-RPC error would tell the client its request was malformed, which is a
/// different claim and one an editor may surface as a broken server.
#[test]
fn a_missing_question_is_an_is_error_not_a_protocol_error() {
    let Some((_dir, mut agent)) = booted("noq", "unused") else {
        return;
    };
    let frames = exchange(
        &mut agent,
        &[
            r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"ask","arguments":{}}}"#,
        ],
    );
    assert_eq!(frames.len(), 1);
    assert_eq!(
        frames[0]["result"]["isError"],
        serde_json::Value::Bool(true)
    );
    assert!(
        frames[0]["result"]["content"][0]["text"]
            .as_str()
            .unwrap_or_default()
            .contains("question"),
        "the reason names what was missing: {:#?}",
        frames[0]
    );
    assert!(frames[0].get("error").is_none(), "{:#?}", frames[0]);
}

/// Two calls without a session id share one, so a conversation is possible.
///
/// Worth asserting because the alternative — a fresh session per call — would
/// look identical for one call and lose all context on the second, which is the
/// kind of thing a single-request test never notices.
#[test]
fn two_calls_without_a_session_continue_the_same_one() {
    let Some((_dir, mut agent)) = booted("session", "ack") else {
        return;
    };
    let frames = exchange(
        &mut agent,
        &[
            r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"ask","arguments":{"question":"first"}}}"#,
            r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"ask","arguments":{"question":"second"}}}"#,
        ],
    );
    assert_eq!(frames.len(), 2);
    for frame in &frames {
        assert_eq!(
            frame["result"]["isError"],
            serde_json::Value::Bool(false),
            "{frame:#?}"
        );
    }

    // The transcript is the evidence: one session holding both turns, rather
    // than two sessions holding one each.
    let sessions = jan_klod_core::serve::sessions_payload(&agent);
    let ids: Vec<&str> = sessions["sessions"]
        .as_array()
        .map(|rows| {
            rows.iter()
                .filter_map(|row| row["id"].as_str())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    assert_eq!(
        ids,
        ["mcp"],
        "one session, the server's default: {sessions}"
    );
    assert_eq!(
        sessions["sessions"][0]["preview"], "first",
        "and it is the session the *first* call opened, not one the second replaced: {sessions}"
    );
}
