//! Drives the core over newline-delimited JSON-RPC through `rpc::serve` with in-memory streams.

use std::io::{BufRead, BufReader, Cursor, Write};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

use jk_agent::Runtime;
use jk_host::rpc;
use jk_protocol::{jsonrpc, PROTOCOL_VERSION};
use jk_tui::transport::Transport as _;
use jk_wasm::http::WireResponse;
use jk_wasm::route::HttpFn;

use crate::common;

fn booted(tag: &str) -> Option<(common::TempDir, jk_agent::AgentSession)> {
    booted_with(tag, || common::canned_http("pong"))
}

fn booted_with(
    tag: &str,
    http: impl Fn() -> HttpFn,
) -> Option<(common::TempDir, jk_agent::AgentSession)> {
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
    let agent = runtime.build_agent(&http).expect("agent boots");
    Some((dir, agent))
}

#[derive(Debug)]
enum Frame {
    Response(jsonrpc::Response),
    Notification(jsonrpc::Notification),
}

fn frames(output: Vec<u8>) -> Vec<Frame> {
    String::from_utf8(output)
        .expect("frames are utf-8")
        .lines()
        .map(|line| {
            serde_json::from_str::<jsonrpc::Response>(line)
                .map(Frame::Response)
                .or_else(|_| {
                    serde_json::from_str::<jsonrpc::Notification>(line).map(Frame::Notification)
                })
                .unwrap_or_else(|e| panic!("neither a response nor a notification: {line}: {e}"))
        })
        .collect()
}

fn exchange(agent: &mut jk_agent::AgentSession, script: &[&str]) -> Vec<Frame> {
    let input = Cursor::new(script.join("\n").into_bytes());
    let mut output = Vec::new();
    rpc::serve(input, &mut output, agent).expect("the loop runs to EOF");
    frames(output)
}

fn responses(frames: &[Frame]) -> Vec<&jsonrpc::Response> {
    frames
        .iter()
        .filter_map(|frame| match frame {
            Frame::Response(response) => Some(response),
            Frame::Notification(_) => None,
        })
        .collect()
}

fn reported(frames: &[Frame]) -> Vec<String> {
    frames
        .iter()
        .filter_map(|frame| match frame {
            Frame::Notification(notification) => Some(
                serde_json::to_value(&notification.notification).expect("serializes")["method"]
                    .as_str()
                    .expect("a method")
                    .to_owned(),
            ),
            Frame::Response(_) => None,
        })
        .collect()
}

const fn code(response: &jsonrpc::Response) -> Option<i64> {
    match &response.outcome {
        jsonrpc::Outcome::Error(error) => Some(error.code),
        jsonrpc::Outcome::Result(_) => None,
    }
}

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
    let Some((_dir, mut agent)) = booted("readonly") else {
        return;
    };
    let served = exchange(
        &mut agent,
        &[
            r#"{"jsonrpc":"2.0","id":1,"method":"session/list"}"#,
            &format!(
                r#"{{"jsonrpc":"2.0","id":2,"method":"protocol/hello","params":{{"version":"{PROTOCOL_VERSION}"}}}}"#
            ),
            r#"{"jsonrpc":"2.0","id":3,"method":"session/create"}"#,
            r#"{"jsonrpc":"2.0","id":"four","method":"session/list"}"#,
            r#"{"jsonrpc":"2.0","id":5,"method":"session/get","params":{"session":"nothing-here"}}"#,
            r#"{"jsonrpc":"2.0","id":6,"method":"session/fork","params":{"session":"nothing-here","at-seq":3}}"#,
        ],
    );
    let responses = responses(&served);
    assert_eq!(responses.len(), 6, "one answer per frame");
    assert!(
        reported(&served).is_empty(),
        "no turn ran, so nothing was reported unprompted"
    );

    assert_eq!(code(responses[0]), Some(jsonrpc::INVALID_REQUEST));
    assert_eq!(responses[0].id, jsonrpc::Id::Number(1));

    assert_eq!(
        result(responses[1])["version"],
        serde_json::json!(PROTOCOL_VERSION)
    );

    let created = result(responses[2])["id"]
        .as_str()
        .expect("an id")
        .to_owned();
    assert!(!created.is_empty(), "a created session has an id");

    assert_eq!(responses[3].id, jsonrpc::Id::Text("four".to_owned()));
    assert!(
        result(responses[3])["sessions"].is_array(),
        "sessions is a list: {}",
        result(responses[3])
    );

    assert_eq!(
        result(responses[4]),
        &serde_json::json!({ "id": "nothing-here", "messages": [] })
    );

    assert_eq!(code(responses[5]), Some(jsonrpc::INVALID_PARAMS));
}

#[test]
fn an_incompatible_client_is_refused_and_hung_up_on() {
    let Some((_dir, mut agent)) = booted("version") else {
        return;
    };
    let served = exchange(
        &mut agent,
        &[
            r#"{"jsonrpc":"2.0","id":1,"method":"protocol/hello","params":{"version":"9.9.9"}}"#,
            r#"{"jsonrpc":"2.0","id":2,"method":"session/list"}"#,
        ],
    );
    let responses = responses(&served);
    assert_eq!(
        responses.len(),
        1,
        "the second frame was never served: {responses:#?}"
    );
    assert_eq!(code(responses[0]), Some(jsonrpc::INCOMPATIBLE_VERSION));
    let jsonrpc::Outcome::Error(error) = &responses[0].outcome else {
        panic!("an error")
    };
    assert_eq!(
        error.data.as_ref().expect("carries data")["version"],
        serde_json::json!(PROTOCOL_VERSION)
    );
}

#[test]
fn a_client_one_minor_behind_is_refused() {
    let Some((_dir, mut agent)) = booted("minor") else {
        return;
    };
    assert!(
        PROTOCOL_VERSION.starts_with("0.2."),
        "this case is written against a 0.2 core, not {PROTOCOL_VERSION}"
    );
    let served = exchange(
        &mut agent,
        &[r#"{"jsonrpc":"2.0","id":1,"method":"protocol/hello","params":{"version":"0.1.0"}}"#],
    );
    let responses = responses(&served);
    assert_eq!(responses.len(), 1);
    assert_eq!(code(responses[0]), Some(jsonrpc::INCOMPATIBLE_VERSION));
}

const GATED: [&str; 4] = [
    "provider-openai.wasm",
    "interceptor-intent-router.wasm",
    "interceptor-tool-selector.wasm",
    "interceptor-permission.wasm",
];

/// Answers by request content, not call count: the intent classifier runs first.
fn tool_then_answer() -> (Arc<AtomicU32>, impl Fn() -> HttpFn) {
    let resumed = Arc::new(AtomicU32::new(0));
    let shared = Arc::clone(&resumed);
    let factory = move || -> HttpFn {
        let counted = Arc::clone(&shared);
        Box::new(move |_m, _u, _h, body, _t| {
            let request = String::from_utf8_lossy(body.unwrap_or(b"")).into_owned();
            let body = if request.contains("\"grammar\"") {
                serde_json::json!({
                    "choices": [{
                        "message": { "role": "assistant", "content": "agentic" },
                        "finish_reason": "stop"
                    }]
                })
            } else if request.contains(r#""role":"tool""#) {
                counted.fetch_add(1, Ordering::Relaxed);
                serde_json::json!({
                    "choices": [{
                        "message": { "role": "assistant", "content": "all done" },
                        "finish_reason": "stop"
                    }]
                })
            } else {
                serde_json::json!({
                    "choices": [{
                        "message": {
                            "role": "assistant",
                            "tool_calls": [{
                                "id": "call-1",
                                "function": { "name": "bash", "arguments": "{}" }
                            }]
                        },
                        "finish_reason": "tool_calls"
                    }]
                })
            };
            Ok(WireResponse {
                status: 200,
                headers: vec![],
                body: serde_json::to_vec(&body).expect("serializes"),
            })
        })
    };
    (resumed, factory)
}

fn booted_gated(
    tag: &str,
    http: impl Fn() -> HttpFn,
) -> Option<(common::TempDir, jk_agent::AgentSession)> {
    if !common::guests_staged(&GATED) {
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
    tool-selector:
      enabled: true
    permission:
      enabled: true
",
    )
    .expect("writes the config");
    let runtime = Runtime::boot(&config, common::repo_root().join("ext")).expect("runtime boots");
    let agent = runtime.build_agent(&http).expect("agent boots");
    Some((dir, agent))
}

#[test]
fn a_turn_streams_its_events_and_answers_the_request() {
    let Some((_dir, mut agent)) = booted("turn") else {
        return;
    };
    let served = exchange(
        &mut agent,
        &[
            &format!(
                r#"{{"jsonrpc":"2.0","id":1,"method":"protocol/hello","params":{{"version":"{PROTOCOL_VERSION}"}}}}"#
            ),
            r#"{"jsonrpc":"2.0","id":2,"method":"session/message","params":{"session":"rpc-turn","message":"hello"}}"#,
        ],
    );
    let reported = reported(&served);
    assert!(
        reported.contains(&"done".to_owned()),
        "a turn ends in `done`: {reported:?}"
    );
    assert_eq!(
        reported.last().map(String::as_str),
        Some("done"),
        "`done` is terminal: {reported:?}"
    );

    let responses = responses(&served);
    assert_eq!(responses.len(), 2);
    let answered = result(responses[1]);
    assert_eq!(
        answered["answer"], "pong",
        "the request is answered with the turn's own answer: {answered}"
    );
    assert!(answered["agentic"].is_boolean(), "{answered}");

    let next = exchange(
        &mut agent,
        &[
            &format!(
                r#"{{"jsonrpc":"2.0","id":1,"method":"protocol/hello","params":{{"version":"{PROTOCOL_VERSION}"}}}}"#
            ),
            r#"{"jsonrpc":"2.0","id":2,"method":"session/fork","params":{"session":"rpc-turn","at-seq":1}}"#,
        ],
    );
    let next = self::responses(&next);
    assert_eq!(next.len(), 2);
    let forked = result(next[1]);
    assert!(
        forked["copied"].as_u64().unwrap_or(0) >= 1,
        "a fork of a session that has events copies some: {forked}"
    );
    assert!(forked["id"].as_str().is_some_and(|id| !id.is_empty()));
}

#[test]
fn a_client_can_fork_at_a_seq_it_read_from_session_get() {
    let Some((_dir, mut agent)) = booted("fork-from-read") else {
        return;
    };
    let hello = format!(
        r#"{{"jsonrpc":"2.0","id":1,"method":"protocol/hello","params":{{"version":"{PROTOCOL_VERSION}"}}}}"#
    );

    exchange(
        &mut agent,
        &[
            &hello,
            r#"{"jsonrpc":"2.0","id":2,"method":"session/message","params":{"session":"fork-src","message":"hello"}}"#,
        ],
    );

    let served = exchange(
        &mut agent,
        &[
            &hello,
            r#"{"jsonrpc":"2.0","id":2,"method":"session/get","params":{"session":"fork-src"}}"#,
        ],
    );
    let responses = self::responses(&served);
    let transcript = result(responses[1]);

    let typed: jk_protocol::SessionGetResult = serde_json::from_value(transcript.clone())
        .unwrap_or_else(|e| panic!("session/get must match SessionGetResult: {e}: {transcript}"));
    assert_eq!(typed.id, "fork-src");

    let messages = transcript["messages"]
        .as_array()
        .expect("session/get returns messages");
    assert!(
        !messages.is_empty(),
        "the turn must have left something to fork: {transcript}"
    );

    let first = &messages[0];
    let at_seq = first["seq"]
        .as_u64()
        .unwrap_or_else(|| panic!("every message carries the seq it was projected from: {first}"));

    let next = exchange(
        &mut agent,
        &[
            &hello,
            &format!(
                r#"{{"jsonrpc":"2.0","id":2,"method":"session/fork","params":{{"session":"fork-src","at-seq":{at_seq}}}}}"#
            ),
        ],
    );
    let next = self::responses(&next);
    let forked = result(next[1]);
    let child = forked["id"].as_str().expect("a fork names its new session");

    let read_back = exchange(
        &mut agent,
        &[
            &hello,
            &format!(
                r#"{{"jsonrpc":"2.0","id":2,"method":"session/get","params":{{"session":"{child}"}}}}"#
            ),
        ],
    );
    let read_back = self::responses(&read_back);
    let child_messages = result(read_back[1])["messages"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let last = child_messages
        .last()
        .expect("forking at a message's seq copies at least that message");
    assert_eq!(
        (last["role"].as_str(), last["content"].as_str()),
        (first["role"].as_str(), first["content"].as_str()),
        "`at-seq` is inclusive, so the child ends with the message it was forked \
         at — parent's first: {first}, child's last: {last}"
    );
}

#[test]
fn a_cancel_stops_the_turn_instead_of_letting_it_finish() {
    let (resumed, provider) = tool_then_answer();
    let Some((_dir, mut agent)) = booted_gated("cancel", provider) else {
        return;
    };
    let served = exchange(
        &mut agent,
        &[
            &format!(
                r#"{{"jsonrpc":"2.0","id":1,"method":"protocol/hello","params":{{"version":"{PROTOCOL_VERSION}"}}}}"#
            ),
            r#"{"jsonrpc":"2.0","id":2,"method":"session/message","params":{"session":"rpc-cancel","message":"use bash to clean up, then report"}}"#,
            r#"{"jsonrpc":"2.0","id":3,"method":"turn/cancel","params":{"session":"rpc-cancel"}}"#,
        ],
    );
    let responses = responses(&served);
    let cancelled = responses
        .iter()
        .find(|response| response.id == jsonrpc::Id::Number(3))
        .expect("the cancel was answered");
    assert_eq!(
        result(cancelled)["cancelling"],
        serde_json::json!(true),
        "the cancel is acknowledged"
    );

    let turn = responses
        .iter()
        .find(|response| response.id == jsonrpc::Id::Number(2))
        .expect("the turn was answered");
    let answer = result(turn)["answer"]
        .as_str()
        .unwrap_or_default()
        .to_owned();
    assert!(
        answer.contains("stopped before the turn finished"),
        "a cancelled turn is marked as unfinished, not passed off as an answer: {answer:?}"
    );
    assert_eq!(
        resumed.load(Ordering::Relaxed),
        0,
        "the turn stopped, so it never asked for the completion that follows a tool"
    );
}

#[test]
fn a_confirmation_is_asked_and_answered_over_the_same_pipe() {
    let (resumed, provider) = tool_then_answer();
    let Some((_dir, mut agent)) = booted_gated("ask", provider) else {
        return;
    };
    std::env::set_var("JK_ANSWER_TIMEOUT_SECS", "5");

    let (core_in, mut to_core) = std::io::pipe().expect("a pipe");
    let (from_core, core_out) = std::io::pipe().expect("a pipe");

    let client = std::thread::spawn(move || -> Vec<String> {
        let mut lines = BufReader::new(from_core).lines();
        writeln!(
            to_core,
            r#"{{"jsonrpc":"2.0","id":1,"method":"protocol/hello","params":{{"version":"{PROTOCOL_VERSION}"}}}}"#
        )
        .expect("writes hello");
        let hello = lines.next().expect("a hello answer").expect("readable");
        assert!(hello.contains(PROTOCOL_VERSION), "{hello}");

        writeln!(
            to_core,
            r#"{{"jsonrpc":"2.0","id":2,"method":"session/message","params":{{"session":"rpc-ask","message":"use bash to clean up, then report"}}}}"#
        )
        .expect("writes the message");

        let mut seen = Vec::new();
        for line in lines {
            let line = line.expect("readable");
            let asked = line.contains(r#""method":"ask""#);
            seen.push(line);
            if asked {
                writeln!(
                    to_core,
                    r#"{{"jsonrpc":"2.0","id":3,"method":"turn/answer","params":{{"session":"rpc-ask","answer":"yes"}}}}"#
                )
                .expect("writes the answer");
            }
            if seen.iter().any(|l| l.contains(r#""id":2"#)) {
                break;
            }
        }
        drop(to_core);
        seen
    });

    rpc::serve(BufReader::new(core_in), core_out, &mut agent).expect("the loop runs to EOF");
    let seen = client.join().expect("the client thread finishes");

    assert!(
        seen.iter().any(|line| line.contains(r#""method":"ask""#)),
        "the gate's question reached the client: {seen:#?}"
    );
    assert!(
        seen.iter().any(|line| line.contains(r#""accepted":true"#)),
        "the parked turn took the answer — a refused one would say `no confirmation is pending`: {seen:#?}"
    );
    assert_eq!(
        resumed.load(Ordering::Relaxed),
        1,
        "the turn resumed after the answer and asked for the completion that follows a tool"
    );
}

#[test]
fn a_follow_up_steers_the_turn_it_arrives_during() {
    let provider = || -> HttpFn {
        Box::new(move |_m, _u, _h, body, _t| {
            let request = String::from_utf8_lossy(body.unwrap_or(b"")).into_owned();
            let content = if request.contains("in Rust, please") {
                "steered"
            } else {
                "pong"
            };
            Ok(WireResponse {
                status: 200,
                headers: vec![],
                body: serde_json::to_vec(&serde_json::json!({
                    "choices": [{
                        "message": { "role": "assistant", "content": content },
                        "finish_reason": "stop"
                    }]
                }))
                .expect("serializes"),
            })
        })
    };
    let Some((_dir, mut agent)) = booted_with("followup", provider) else {
        return;
    };
    let served = exchange(
        &mut agent,
        &[
            &format!(
                r#"{{"jsonrpc":"2.0","id":1,"method":"protocol/hello","params":{{"version":"{PROTOCOL_VERSION}"}}}}"#
            ),
            r#"{"jsonrpc":"2.0","id":2,"method":"session/message","params":{"session":"rpc-steer","message":"write it"}}"#,
            r#"{"jsonrpc":"2.0","id":3,"method":"turn/follow-up","params":{"session":"rpc-steer","message":"in Rust, please"}}"#,
        ],
    );
    let responses = self::responses(&served);
    let queued = responses
        .iter()
        .find(|response| response.id == jsonrpc::Id::Number(3))
        .expect("the follow-up was answered");
    assert_eq!(result(queued)["queued"], serde_json::json!(true));

    let turn = responses
        .iter()
        .find(|response| response.id == jsonrpc::Id::Number(2))
        .expect("the turn was answered");
    assert_eq!(
        result(turn)["answer"],
        "steered",
        "the follow-up was injected, not merely acknowledged: {}",
        result(turn)
    );
}

#[test]
fn the_gateway_subcommand_puts_the_protocol_on_stdout_and_logs_on_stderr() {
    if !common::guests_staged(&["provider-openai.wasm", "interceptor-intent-router.wasm"]) {
        return;
    }
    let dir =
        common::TempDir(std::env::temp_dir().join(format!("jk-rpc-subcmd-{}", std::process::id())));
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

    let mut child = std::process::Command::new(env!("CARGO_BIN_EXE_jan-klod-gateway"))
        .arg("rpc")
        .arg(&config)
        .arg(common::repo_root().join("ext"))
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("the gateway spawns");
    let mut stdin = child.stdin.take().expect("piped stdin");
    writeln!(
        stdin,
        r#"{{"jsonrpc":"2.0","id":1,"method":"protocol/hello","params":{{"version":"{PROTOCOL_VERSION}"}}}}"#
    )
    .expect("writes hello");
    drop(stdin);

    let finished = child.wait_with_output().expect("the gateway runs");
    let stdout = String::from_utf8(finished.stdout).expect("stdout is utf-8");
    let stderr = String::from_utf8(finished.stderr).expect("stderr is utf-8");
    assert!(
        finished.status.success(),
        "exited {:?}; stderr: {stderr}",
        finished.status.code()
    );

    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(
        lines.len(),
        1,
        "one frame answered one request, and nothing else was printed: {stdout:?}"
    );
    let response: jsonrpc::Response =
        serde_json::from_str(lines[0]).unwrap_or_else(|e| panic!("{}: {e}", lines[0]));
    assert_eq!(response.id, jsonrpc::Id::Number(1));
    assert_eq!(
        result(&response)["version"],
        serde_json::json!(PROTOCOL_VERSION)
    );

    assert!(
        stderr.contains("INFO ["),
        "the boot log is on stderr, where a client will not parse it: {stderr:?}"
    );
}

#[test]
fn the_shipped_client_drives_the_shipped_gateway_over_stdio() {
    if !common::guests_staged(&["provider-openai.wasm", "interceptor-intent-router.wasm"]) {
        return;
    }
    let dir =
        common::TempDir(std::env::temp_dir().join(format!("jk-rpc-client-{}", std::process::id())));
    std::fs::create_dir_all(&dir.0).expect("creates the temp dir");
    let config = dir.0.join("config.yaml");
    std::fs::write(
        &config,
        "
extensions:
  provider:
    openai:
      enabled: true
      base-url: http://127.0.0.1:1/v1
      model: mock-1
      api-key: test
  interceptor:
    intent-router:
      enabled: true
",
    )
    .expect("writes the config");
    let ext = common::repo_root().join("ext");

    let transport = jk_tui::transport::Stdio::spawn_from(
        std::path::Path::new(env!("CARGO_BIN_EXE_jan-klod-gateway")),
        &[
            config.to_str().expect("utf-8 path"),
            ext.to_str().expect("utf-8 path"),
        ],
        jk_tui::transport::Logs::Discard,
    )
    .expect("the client spawns the gateway and negotiates a protocol version");

    for attempt in 1..=2 {
        let mut events = Vec::new();
        let outcome = transport.stream_turn("client-stdio", "hello", &mut |event| {
            events.push(event);
        });
        let Err(err) = outcome else {
            panic!("attempt {attempt}: nothing is listening on port 1, so the turn cannot succeed")
        };
        assert!(
            err.contains("all providers failed"),
            "attempt {attempt}: the provider's own failure came back: {err}"
        );
        assert!(
            events
                .iter()
                .any(|event| matches!(event, jk_tui::StreamEvent::Warning(_))),
            "attempt {attempt}: the fallback warnings streamed to the client: {events:?}"
        );
    }
}
