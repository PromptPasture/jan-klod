//! A client drives the core over newline-delimited JSON-RPC.
//!
//! No subprocess and no threads: `rpc::serve` is generic over its streams, so
//! the whole exchange is a `Cursor` of frames in and a `Vec<u8>` of frames out.
//! That works because every command here is answered without the core asking
//! anything back — a scripted client is enough. The turn commands are not, and
//! are refused for now, which is what the last case below asserts.
//!
//! Skips (passes as a no-op) when the guests are not staged in `ext/`.

use std::io::{BufRead, BufReader, Cursor, Write};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

// The client's transport trait, so its methods are callable here the same way
// the `jan-klod` binary calls them.
use jan_klod_client::transport::Transport as _;
use jan_klod_core::http::WireResponse;
use jan_klod_core::route::HttpFn;
use jan_klod_core::rpc;
use jan_klod_core::Runtime;
use jan_klod_protocol::{jsonrpc, PROTOCOL_VERSION};

use crate::common;

/// Boot an offline agent with a canned "pong" provider.
fn booted(tag: &str) -> Option<(common::TempDir, jan_klod_core::AgentSession)> {
    booted_with(tag, || common::canned_http("pong"))
}

/// Boot an offline agent driven by `http`. No live endpoint either way.
fn booted_with(
    tag: &str,
    http: impl Fn() -> HttpFn,
) -> Option<(common::TempDir, jan_klod_core::AgentSession)> {
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

/// One line the core wrote: an answer to a request, or an unprompted report.
#[derive(Debug)]
enum Frame {
    Response(jsonrpc::Response),
    Notification(jsonrpc::Notification),
}

/// Read the core's output back as frames. A response carries an `id` and a
/// notification does not, which is the only thing that tells them apart — so
/// that is what this uses, rather than guessing from the method name.
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

/// Drive a scripted exchange: every frame is queued up front, so this only
/// works where the client needs to react to nothing. The turn commands are
/// mostly like that — a cancel is not a reply to anything — but a confirmation
/// is not, which is why the `ask` case below uses real pipes.
fn exchange(agent: &mut jan_klod_core::AgentSession, script: &[&str]) -> Vec<Frame> {
    let input = Cursor::new(script.join("\n").into_bytes());
    let mut output = Vec::new();
    rpc::serve(input, &mut output, agent).expect("the loop runs to EOF");
    frames(output)
}

/// Just the answers, in order.
fn responses(frames: &[Frame]) -> Vec<&jsonrpc::Response> {
    frames
        .iter()
        .filter_map(|frame| match frame {
            Frame::Response(response) => Some(response),
            Frame::Notification(_) => None,
        })
        .collect()
}

/// The method of every notification, in order.
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
    let Some((_dir, mut agent)) = booted("readonly") else {
        return;
    };
    let served = exchange(
        &mut agent,
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
        ],
    );
    let responses = responses(&served);
    assert_eq!(responses.len(), 6, "one answer per frame");
    assert!(
        reported(&served).is_empty(),
        "no turn ran, so nothing was reported unprompted"
    );

    // 1 — nothing is served before the handshake.
    assert_eq!(code(responses[0]), Some(jsonrpc::INVALID_REQUEST));
    assert_eq!(responses[0].id, jsonrpc::Id::Number(1));

    // 2 — the core answers with its own version.
    assert_eq!(
        result(responses[1])["version"],
        serde_json::json!(PROTOCOL_VERSION)
    );

    // 3 — a fresh session id, minted the way every transport mints them.
    let created = result(responses[2])["id"]
        .as_str()
        .expect("an id")
        .to_owned();
    assert!(!created.is_empty(), "a created session has an id");

    // 4 — the list is a list, and the string id came back a string.
    assert_eq!(responses[3].id, jsonrpc::Id::Text("four".to_owned()));
    assert!(
        result(responses[3])["sessions"].is_array(),
        "sessions is a list: {}",
        result(responses[3])
    );

    // 5 — an unknown session is an empty transcript, not an error. The same
    // answer `GET /session/:id` gives, because it is the same projection.
    assert_eq!(
        result(responses[4]),
        &serde_json::json!({ "id": "nothing-here", "messages": [] })
    );

    // 6 — forking a session with no events is the caller's mistake, and is
    // reported as one rather than as a broken store.
    assert_eq!(code(responses[5]), Some(jsonrpc::INVALID_PARAMS));
}

/// The refusal has to *end* the connection, not merely say no. Asserted by
/// sending a frame after it and showing nothing answers: an error the client can
/// keep talking past is a warning, not a refusal.
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
    let Some((_dir, mut agent)) = booted("minor") else {
        return;
    };
    // Guard the premise: if PROTOCOL_VERSION ever leaves 0.1, this frame stops
    // testing what it says it tests.
    assert!(
        PROTOCOL_VERSION.starts_with("0.1."),
        "this case is written against a 0.1 core, not {PROTOCOL_VERSION}"
    );
    let served = exchange(
        &mut agent,
        &[r#"{"jsonrpc":"2.0","id":1,"method":"protocol/hello","params":{"version":"0.2.0"}}"#],
    );
    let responses = responses(&served);
    assert_eq!(responses.len(), 1);
    assert_eq!(code(responses[0]), Some(jsonrpc::INCOMPATIBLE_VERSION));
}

// ─── Turns ───────────────────────────────────────────────────────────────────

/// The four guests a confirmation needs: something to route intent, something
/// to pick a tool, and the gate that asks.
const GATED: [&str; 4] = [
    "provider-openai.wasm",
    "interceptor-intent-router.wasm",
    "interceptor-tool-selector.wasm",
    "interceptor-permission.wasm",
];

/// A provider that answers whatever it was actually asked.
///
/// Keyed off the request rather than off a call counter, because the first
/// request of a turn is **not** the turn's: `interceptor-intent-router`
/// classifies the message with its own grammar-constrained completion first.
/// A counter-driven fake hands that one the turn's answer and the turn the
/// classifier's, and then nothing behaves as the test says it does. (The
/// equivalent helper in `api_prompt.rs` survives this only because each
/// provider *instance* gets its own counter, so the classifier's instance eats
/// the mismatch on its own.)
///
/// The returned count is of **post-tool** completions: exactly the request a
/// turn makes after a tool has run, and exactly the one a cancelled turn never
/// makes. That is the invariant, so that is what is counted.
fn tool_then_answer() -> (Arc<AtomicU32>, impl Fn() -> HttpFn) {
    let resumed = Arc::new(AtomicU32::new(0));
    let shared = Arc::clone(&resumed);
    let factory = move || -> HttpFn {
        let counted = Arc::clone(&shared);
        Box::new(move |_m, _u, _h, body, _t| {
            let request = String::from_utf8_lossy(body.unwrap_or(b"")).into_owned();
            let body = if request.contains("\"grammar\"") {
                // The intent classifier: one word, and it wants the loop.
                serde_json::json!({
                    "choices": [{
                        "message": { "role": "assistant", "content": "agentic" },
                        "finish_reason": "stop"
                    }]
                })
            } else if request.contains(r#""role":"tool""#) {
                // The turn resumed after a tool ran. A cancelled turn never
                // gets here, which is what the count proves.
                counted.fetch_add(1, Ordering::Relaxed);
                serde_json::json!({
                    "choices": [{
                        "message": { "role": "assistant", "content": "all done" },
                        "finish_reason": "stop"
                    }]
                })
            } else {
                // The turn's first completion: call a tool the gate will ask
                // about.
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

/// Boot with the permission gate enabled and a provider of the caller's choice.
fn booted_gated(
    tag: &str,
    http: impl Fn() -> HttpFn,
) -> Option<(common::TempDir, jan_klod_core::AgentSession)> {
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

/// A turn reports as it goes *and* answers the request that started it, so a
/// client can use either and need not implement both.
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

    // A *second* connection, because a fork queued behind the turn would be
    // refused as mid-turn — the same `409` REST gives, and correctly so. Which
    // makes this the assertion that the session outlives the connection that
    // created it, and that a new connection has to say hello again.
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
    // The session the first connection's turn created is forkable from here,
    // which is the first time a fork's *success* path can be reached at all.
    let forked = result(next[1]);
    assert!(
        forked["copied"].as_u64().unwrap_or(0) >= 1,
        "a fork of a session that has events copies some: {forked}"
    );
    assert!(forked["id"].as_str().is_some_and(|id| !id.is_empty()));
}

/// A cancel that arrives while the turn is running stops it. Asserted by what
/// the provider was *not* asked for: the turn needed a second completion to
/// finish, and a cancelled turn never asks for it. A test that only checked the
/// cancel was accepted would pass against a transport that dropped it.
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
            // Queued before the turn starts, read by the sink between two of
            // the turn's own events — which is the only way a cancel can be
            // seen while this thread is inside the conductor.
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

/// A confirmation, asked and answered over the one pipe both directions share.
///
/// This is the case a scripted client cannot test: an answer that arrives
/// before its question is refused (`no confirmation is pending`), on purpose —
/// a stashed answer would approve the *next* question. So the client here runs
/// on its own thread and replies to what it reads, while `AgentSession` stays
/// on this one.
#[test]
fn a_confirmation_is_asked_and_answered_over_the_same_pipe() {
    let (resumed, provider) = tool_then_answer();
    let Some((_dir, mut agent)) = booted_gated("ask", provider) else {
        return;
    };
    // A lost answer must fail fast rather than wait out the three-minute
    // default and then pass on the prompt's own denial. The same five seconds
    // `api_prompt.rs` sets, deliberately: this is a process-global, and two
    // modules asking for different windows would race under `cargo test`
    // (nextest, which the gate uses, gives each test its own process).
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
                // Only now, in reply to the question, is an answer accepted.
                writeln!(
                    to_core,
                    r#"{{"jsonrpc":"2.0","id":3,"method":"turn/answer","params":{{"session":"rpc-ask","answer":"yes"}}}}"#
                )
                .expect("writes the answer");
            }
            if seen.iter().any(|l| l.contains(r#""id":2"#)) {
                break; // the turn has been answered
            }
        }
        drop(to_core); // EOF, so the core's loop returns
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

/// Steering: a `turn/follow-up` sent while the turn runs is injected when the
/// turn would otherwise have ended, so the answer is the *steered* one.
///
/// This is the first transport that can do it at all — `Driver::follow_up` has
/// existed since the conductor did, and REST has no way to deliver one.
#[test]
fn a_follow_up_steers_the_turn_it_arrives_during() {
    // Answers "pong" normally, and "steered" once it can see the follow-up in
    // the messages it was sent. Asserting on the answer text rather than on a
    // call count, because a follow-up that was queued and then dropped would
    // still produce a second completion of the *unsteered* request.
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

// ─── The subcommand ──────────────────────────────────────────────────────────

/// `jan-klod-gateway rpc` exists, and **stdout carries the protocol and nothing
/// else**.
///
/// The only case here that needs a subprocess: everything above drives
/// `rpc::serve` directly and so cannot notice a `println!` in the boot path, a
/// warning on the wrong stream, or a subcommand that was never wired into the
/// `match` in `main.rs`. A client splitting stdout on newlines reads any of
/// those as a frame.
///
/// No model is needed — the handshake is answered before anything is asked of a
/// provider — so this stays cheap enough to keep.
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
    drop(stdin); // EOF, so the loop returns and the process exits

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

    // Not merely "stdout was clean": the boot chatter exists and went to the
    // other stream. A run that printed nothing anywhere would pass the
    // assertion above while proving nothing about the separation.
    assert!(
        stderr.contains("INFO ["),
        "the boot log is on stderr, where a client will not parse it: {stderr:?}"
    );
}

/// The shipped client library drives the shipped gateway binary over stdio.
///
/// Everything above tests one side or the other. This is the pair: the real
/// `jan-klod` client spawns the real `jan-klod-gateway rpc`, negotiates, and
/// drives a turn — which is what a user gets by typing `jan-klod`.
///
/// The provider is pointed at a port nothing listens on, so the turn fails.
/// That is the deterministic half of what matters here: the failure has to
/// *arrive*, and the stream has to stay usable afterwards, because a response
/// correlated to the wrong id would leave the client reading one turn's answer
/// as the next one's.
#[test]
fn the_shipped_client_drives_the_shipped_gateway_over_stdio() {
    if !common::guests_staged(&["provider-openai.wasm", "interceptor-intent-router.wasm"]) {
        return;
    }
    let dir =
        common::TempDir(std::env::temp_dir().join(format!("jk-rpc-client-{}", std::process::id())));
    std::fs::create_dir_all(&dir.0).expect("creates the temp dir");
    let config = dir.0.join("config.yaml");
    // Port 1, not a hostname: nothing listens there and nothing has to be
    // resolved, so the provider fails immediately rather than at a DNS timeout.
    // Egress permits it because `base-url` names it.
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

    // `spawn_from` returning `Ok` *is* the handshake: it sends
    // `protocol/hello`, reads the answer and compares versions before handing
    // the transport back.
    let transport = jan_klod_client::transport::Stdio::spawn_from(
        std::path::Path::new(env!("CARGO_BIN_EXE_jan-klod-gateway")),
        &[
            config.to_str().expect("utf-8 path"),
            ext.to_str().expect("utf-8 path"),
        ],
        jan_klod_client::transport::Logs::Discard,
    )
    .expect("the client spawns the gateway and negotiates a protocol version");

    for attempt in 1..=2 {
        let mut events = Vec::new();
        let outcome = transport.stream_turn("client-stdio", "hello", &mut |event| {
            events.push(event);
        });
        // Named, not merely failed: a bare `is_err` would also be satisfied by
        // "the gateway closed the connection", which is what a crashed child
        // looks like. This says the turn reached the provider chain and came
        // back with its verdict.
        let Err(err) = outcome else {
            panic!("attempt {attempt}: nothing is listening on port 1, so the turn cannot succeed")
        };
        assert!(
            err.contains("all providers failed"),
            "attempt {attempt}: the provider's own failure came back: {err}"
        );
        // And the turn's warnings arrived as notifications on the way, which is
        // the streaming half of the round trip.
        assert!(
            events
                .iter()
                .any(|event| matches!(event, jan_klod_client::StreamEvent::Warning(_))),
            "attempt {attempt}: the fallback warnings streamed to the client: {events:?}"
        );
    }
    // Twice, because the second turn is the one that would misbehave if the
    // first turn's answer had been matched to the wrong request id.
}
