//! An extension's contributions, from the guest to a client and back.
//!
//! `interceptor-system` declares a command and a status item; the host reads
//! them off the component and the rpc surface carries them. Unit tests cover
//! each half — what the guest builds, what the host maps — and neither shows
//! that a client attaching to a real runtime is handed anything.
//!
//! The third test is the one that will rot first: a client that ignores
//! contributions must run turns exactly as before. `wit/client-surface.wit`
//! promises it, and a renderer added later is what would break it.
//!
//! Skips if guests are not staged in `ext/`; build with `make ext`.

use std::io::Cursor;

use jan_klod_core::Runtime;
use jan_klod_host::rpc;
use jan_klod_protocol::{jsonrpc, Notification, PROTOCOL_VERSION};

use crate::common;

/// Guests these tests need: a provider to answer, and the one extension that
/// contributes anything.
const GUESTS: [&str; 2] = ["provider-openai.wasm", "interceptor-system.wasm"];

/// An offline agent, with `interceptor-system` on when `contributing`.
fn booted(tag: &str, contributing: bool) -> Option<(common::TempDir, jan_klod_core::AgentSession)> {
    if !common::guests_staged(&GUESTS) {
        return None;
    }
    let dir = common::TempDir(
        std::env::temp_dir().join(format!("jk-surface-{tag}-{}", std::process::id())),
    );
    std::fs::create_dir_all(&dir.0).expect("creates the temp dir");
    let config = dir.0.join("config.yaml");
    std::fs::write(
        &config,
        format!(
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
      enabled: {contributing}
"
        ),
    )
    .expect("writes the config");
    let runtime = Runtime::boot(&config, common::repo_root().join("ext")).expect("runtime boots");
    let agent = runtime
        .build_agent(&|| common::canned_http("pong"))
        .expect("agent boots");
    Some((dir, agent))
}

/// Drive a scripted exchange and hand back what the core wrote, in order.
fn exchange(agent: &mut jan_klod_core::AgentSession, script: &[String]) -> Vec<String> {
    let input = Cursor::new(script.join("\n").into_bytes());
    let mut output = Vec::new();
    rpc::serve(input, &mut output, agent).expect("the loop runs to EOF");
    String::from_utf8(output)
        .expect("frames are utf-8")
        .lines()
        .map(ToOwned::to_owned)
        .collect()
}

/// The `protocol/hello` frame, at whatever version this build speaks.
fn hello() -> String {
    format!(
        r#"{{"jsonrpc":"2.0","id":1,"method":"protocol/hello","params":{{"version":"{PROTOCOL_VERSION}"}}}}"#
    )
}

/// The contributions notification in `lines`, if one was sent.
fn contributions(lines: &[String]) -> Option<Notification> {
    lines
        .iter()
        .filter_map(|line| serde_json::from_str::<jsonrpc::Notification>(line).ok())
        .map(|frame| frame.notification)
        .find(|notification| matches!(notification, Notification::SurfaceContributions { .. }))
}

/// A response's result, or a panic naming the error.
fn result(line: &str) -> serde_json::Value {
    let response: jsonrpc::Response = serde_json::from_str(line).expect("a response");
    match response.outcome {
        jsonrpc::Outcome::Result(value) => value,
        jsonrpc::Outcome::Error(error) => {
            panic!("expected a result, got {}: {}", error.code, error.message)
        }
    }
}

#[test]
fn a_client_is_told_what_is_contributed_as_soon_as_it_connects() {
    let Some((_dir, mut agent)) = booted("connect", true) else {
        return;
    };
    let lines = exchange(&mut agent, &[hello()]);
    let Some(Notification::SurfaceContributions { extensions }) = contributions(&lines) else {
        panic!("the handshake did not carry a contributions notification: {lines:?}");
    };

    let system = extensions
        .iter()
        .find(|set| set.extension == "interceptor.system")
        .expect("interceptor.system contributes");
    // Named by the host from the instance id, not by the guest about itself.
    assert_eq!(system.extension, "interceptor.system");
    assert!(
        system
            .commands
            .iter()
            .any(|command| command.name == "prompt"),
        "the `prompt` command is declared: {:?}",
        system.commands
    );
    assert!(
        system
            .status_items
            .iter()
            .any(|item| item.name == "prompt-source"),
        "the `prompt-source` status item is declared: {:?}",
        system.status_items
    );
}

/// Nothing enabled contributes, so nothing is said. An absent notification and
/// an empty one mean the same thing, and the quiet case is the common one.
#[test]
fn a_runtime_whose_extensions_contribute_nothing_says_nothing() {
    let Some((_dir, mut agent)) = booted("quiet", false) else {
        return;
    };
    let lines = exchange(&mut agent, &[hello()]);
    assert!(
        contributions(&lines).is_none(),
        "nothing contributes, so no notification should be sent: {lines:?}"
    );
}

#[test]
fn invoking_a_contributed_command_answers_from_the_extension() {
    let Some((_dir, mut agent)) = booted("invoke", true) else {
        return;
    };
    let lines = exchange(
        &mut agent,
        &[
            hello(),
            r#"{"jsonrpc":"2.0","id":2,"method":"surface/invoke","params":{"extension":"interceptor.system","name":"prompt","arguments":[{"name":"verbose","value":"true"}]}}"#.to_owned(),
        ],
    );
    let answered = lines
        .iter()
        .filter(|line| line.contains(r#""id":2"#))
        .map(String::as_str)
        .next()
        .expect("the invocation is answered");
    let outcome = result(answered);
    // `verbose` asks for the whole prompt, and the built-in text opens with
    // this line — so the answer came from the guest, not from a stub here.
    assert!(
        outcome["text"]
            .as_str()
            .expect("text")
            .contains("You are jan-klod"),
        "the extension's own answer: {outcome}"
    );
    assert_eq!(
        outcome["contributions-changed"],
        serde_json::json!(false),
        "reading the prompt changes nothing about what is contributed"
    );
}

/// A name nobody contributed is the caller's mistake, and is refused as one
/// rather than silently succeeding.
#[test]
fn invoking_something_nobody_contributes_is_refused() {
    let Some((_dir, mut agent)) = booted("unknown", true) else {
        return;
    };
    let lines = exchange(
        &mut agent,
        &[
            hello(),
            r#"{"jsonrpc":"2.0","id":2,"method":"surface/invoke","params":{"extension":"interceptor.system","name":"not-a-command","arguments":[]}}"#
                .to_owned(),
        ],
    );
    let answered: jsonrpc::Response = lines
        .iter()
        .filter(|line| line.contains(r#""id":2"#))
        .find_map(|line| serde_json::from_str(line).ok())
        .expect("the invocation is answered");
    let jsonrpc::Outcome::Error(error) = answered.outcome else {
        panic!("an unknown contribution must not answer with a result");
    };
    assert_eq!(error.code, jsonrpc::METHOD_NOT_FOUND);
}

/// The degradation guarantee: contributions are additive, and a client that
/// never reads the notification takes the same turn as one that does.
///
/// Asserted by comparing two runtimes — one whose extension contributes and
/// one where it is off — rather than by a client that looks away, because
/// "ignored it" is not observable and "same answer either way" is.
#[test]
fn a_client_that_ignores_contributions_runs_the_same_turn() {
    let Some((_with_dir, mut contributing)) = booted("turn-with", true) else {
        return;
    };
    let Some((_without_dir, mut plain)) = booted("turn-without", false) else {
        return;
    };

    let turn = r#"{"jsonrpc":"2.0","id":2,"method":"session/message","params":{"session":"s1","message":"hello"}}"#;
    let with = exchange(&mut contributing, &[hello(), turn.to_owned()]);
    let without = exchange(&mut plain, &[hello(), turn.to_owned()]);

    let answer_of = |lines: &[String]| -> serde_json::Value {
        let line = lines
            .iter()
            .find(|line| line.contains(r#""id":2"#))
            .expect("the turn is answered")
            .clone();
        result(&line)
    };
    assert_eq!(
        answer_of(&with),
        answer_of(&without),
        "a contributing extension must not change the turn a client gets"
    );
}
