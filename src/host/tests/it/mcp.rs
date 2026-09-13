//! MCP port driven as an editor drives it (full exchange against real agent).
//!
//! `core::mcp` tests frame rules without runtime. This module shows a whole
//! exchange: `initialize`, `tools/list`, and a `tools/call ask` running an
//! actual turn against a canned provider. Offline (as Acceptance requires).

use std::path::PathBuf;

use jan_klod_core::Runtime;

use crate::common;

/// Offline agent with canned provider (like `rpc.rs`).
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

/// Send lines and collect all response frames.
fn exchange(agent: &mut jan_klod_core::AgentSession, lines: &[&str]) -> Vec<serde_json::Value> {
    let input = lines.join("\n") + "\n";
    let mut output = Vec::new();
    jan_klod_core::mcp::serve(std::io::Cursor::new(input), &mut output, agent)
        .expect("the server writes its frames");
    let text = String::from_utf8(output).expect("frames are UTF-8");
    text.lines()
        .map(|line| {
            // Every line must parse; transport says frames are newline-delimited.
            serde_json::from_str(line)
                .unwrap_or_else(|err| panic!("{line:?} is not a frame: {err}"))
        })
        .collect()
}

/// Typical editor exchange: initialize, list tools, call ask.
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

    // Three requests + one notification = three frames (notifications don't reply).
    assert_eq!(frames.len(), 3, "{frames:#?}");
    assert_eq!(frames[0]["id"], 1);
    assert_eq!(frames[0]["result"]["protocolVersion"], "2025-11-25");
    assert_eq!(frames[1]["id"], 2);
    assert_eq!(
        frames[1]["result"]["tools"].as_array().map(Vec::len),
        Some(3)
    );

    // Turn's answer came back as tool content.
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

/// Missing argument is `isError: true`, not a JSON-RPC error.
///
/// Bad argument = tool failure (model can correct); JSON-RPC error = malformed
/// request (editor sees broken server). Different claims.
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

/// Two unsessioned calls share one session (conversation possible).
///
/// Single-request tests miss the alternative (fresh per call losing context).
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

    // Transcript proves: one session with both turns, not two separate ones.
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

// ---- The read tools, and the refusal (#56 box 3) ----

/// Agent whose provider asks for workspace write first, then answers.
/// Temp dir allows test to verify the write didn't happen.
fn booted_writing(tag: &str) -> Option<(common::TempDir, jan_klod_core::AgentSession, PathBuf)> {
    const GUESTS: [&str; 4] = [
        "provider-openai.wasm",
        "interceptor-permission.wasm",
        "interceptor-tool-selector.wasm",
        "tool-fs.wasm",
    ];
    if !common::guests_staged(&GUESTS) {
        return None;
    }
    let dir =
        common::TempDir(std::env::temp_dir().join(format!("jk-mcp-{tag}-{}", std::process::id())));
    std::fs::create_dir_all(&dir.0).expect("creates the temp dir");
    let target = dir.0.join("written-by-the-model.txt");
    let config = dir.0.join("config.yaml");
    std::fs::write(
        &config,
        format!(
            "
workspace: {}
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
  tool:
    fs:
      enabled: true
",
            dir.0.display()
        ),
    )
    .expect("writes the config");

    // First call: model requests write. Second: model gives up, answers.
    let calls = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0));
    let http = move || -> jan_klod_core::route::HttpFn {
        let calls = std::sync::Arc::clone(&calls);
        Box::new(move |_m, _u, _h, _b, _t| {
            let n = calls.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let body = if n == 0 {
                serde_json::json!({"choices":[{"message":{"role":"assistant","tool_calls":[{
                    "id":"w1",
                    "function":{
                        "name":"fs",
                        "arguments":"{\"op\":\"write\",\"path\":\"written-by-the-model.txt\",\"contents\":\"leaked\"}"
                    }}]},"finish_reason":"tool_calls"}]})
            } else {
                serde_json::json!({"choices":[{"message":{"role":"assistant",
                    "content":"I could not write that file."},"finish_reason":"stop"}]})
            };
            Ok(jan_klod_core::http::WireResponse {
                status: 200,
                headers: vec![],
                body: serde_json::to_vec(&body).expect("serializes"),
            })
        })
    };
    let runtime = Runtime::boot(&config, common::repo_root().join("ext")).expect("runtime boots");
    let agent = runtime.build_agent(&http).expect("agent boots");
    Some((dir, agent, target))
}

/// Write-requiring turn is refused; file is not written (Acceptance line 2).
///
/// Assertion is on effect, not wording (absent file proves boundary, not story).
/// Critical here: MCP client cannot answer confirmation prompts; fail-open would go unnoticed.
#[test]
fn a_write_requiring_turn_is_refused_and_nothing_is_written() {
    let Some((_dir, mut agent, target)) = booted_writing("refuse") else {
        return;
    };
    let frames = exchange(
        &mut agent,
        &[
            r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"ask","arguments":{"question":"write a file"}}}"#,
        ],
    );
    assert_eq!(frames.len(), 1, "{frames:#?}");
    assert!(
        !target.exists(),
        "the model asked to write {} and the gate refused, with nobody to ask",
        target.display()
    );
    // Turn completed (refusal is answer, not transport failure).
    assert!(frames[0].get("error").is_none(), "{:#?}", frames[0]);
}

/// `session_list`/`session_get` match REST payloads (editor and browser align).
#[test]
fn the_read_tools_return_the_sessions_a_turn_created() {
    let Some((_dir, mut agent)) = booted("reads", "answered") else {
        return;
    };
    let frames = exchange(
        &mut agent,
        &[
            r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"ask","arguments":{"question":"hello there","session":"s1"}}}"#,
            r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"session_list"}}"#,
            r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"session_get","arguments":{"session":"s1"}}}"#,
        ],
    );
    assert_eq!(frames.len(), 3, "{frames:#?}");

    // Payload is text; parse back to verify it's client-usable JSON.
    let listed: serde_json::Value = serde_json::from_str(
        frames[1]["result"]["content"][0]["text"]
            .as_str()
            .unwrap_or("null"),
    )
    .expect("session_list returns JSON");
    assert_eq!(listed["sessions"][0]["id"], "s1", "{listed}");

    let got: serde_json::Value = serde_json::from_str(
        frames[2]["result"]["content"][0]["text"]
            .as_str()
            .unwrap_or("null"),
    )
    .expect("session_get returns JSON");
    assert_eq!(got["id"], "s1");
    let texts: Vec<&str> = got["messages"]
        .as_array()
        .map(|m| m.iter().filter_map(|x| x["content"].as_str()).collect())
        .unwrap_or_default();
    assert!(
        texts.contains(&"hello there") && texts.contains(&"answered"),
        "the transcript holds both sides of the turn: {got}"
    );
}

/// Missing argument is `isError: true`, like `ask`.
#[test]
fn session_get_without_an_id_is_an_is_error() {
    let Some((_dir, mut agent)) = booted("noid", "unused") else {
        return;
    };
    let frames = exchange(
        &mut agent,
        &[
            r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"session_get","arguments":{}}}"#,
        ],
    );
    assert_eq!(
        frames[0]["result"]["isError"],
        serde_json::Value::Bool(true)
    );
    assert!(frames[0].get("error").is_none(), "{:#?}", frames[0]);
}
