//! The wire names are the contract, so they are asserted rather than assumed.
//!
//! A rename is a major-version change (see the crate docs), and a rename is
//! exactly what a careless `#[serde(rename)]` edit or a variant reshuffle
//! produces silently. These tests make it loud.

use jan_klod_protocol::{Command, HelloResult, PROTOCOL_VERSION};

/// The `method` each command must serialize to.
///
/// The match is deliberately exhaustive — no wildcard arm — so adding a
/// command stops this file from compiling until its wire name is written down
/// here **and** a sample for it is added to [`every_command`] below.
const fn expected_method(command: &Command) -> &'static str {
    match command {
        Command::Hello { .. } => "protocol/hello",
        Command::SessionCreate => "session/create",
        Command::SessionList => "session/list",
        Command::SessionGet { .. } => "session/get",
        Command::SessionMessage { .. } => "session/message",
        Command::TurnAnswer { .. } => "turn/answer",
        Command::TurnCancel { .. } => "turn/cancel",
        Command::TurnFollowUp { .. } => "turn/follow-up",
    }
}

/// One sample per command, with every field populated — an empty string would
/// round-trip even if a field were dropped from the type.
fn every_command() -> Vec<Command> {
    vec![
        Command::Hello {
            version: PROTOCOL_VERSION.to_owned(),
        },
        Command::SessionCreate,
        Command::SessionList,
        Command::SessionGet {
            session: "s1".to_owned(),
        },
        Command::SessionMessage {
            session: "s1".to_owned(),
            message: "hello".to_owned(),
        },
        Command::TurnAnswer {
            session: "s1".to_owned(),
            answer: "yes".to_owned(),
        },
        Command::TurnCancel {
            session: "s1".to_owned(),
        },
        Command::TurnFollowUp {
            session: "s1".to_owned(),
            message: "actually, in Rust".to_owned(),
        },
    ]
}

#[test]
fn every_command_serializes_to_its_documented_method() {
    for command in every_command() {
        let json: serde_json::Value = serde_json::to_value(&command).expect("a command serializes");
        assert_eq!(
            json.get("method").and_then(serde_json::Value::as_str),
            Some(expected_method(&command)),
            "wire name of {command:?}"
        );
    }
}

#[test]
fn every_command_round_trips() {
    for command in every_command() {
        let text = serde_json::to_string(&command).expect("serializes");
        let back: Command = serde_json::from_str(&text).expect("deserializes");
        assert_eq!(back, command, "round-trip of {text}");
    }
}

#[test]
fn each_command_has_its_own_method() {
    let mut methods: Vec<&str> = every_command().iter().map(expected_method).collect();
    let total = methods.len();
    methods.sort_unstable();
    methods.dedup();
    assert_eq!(
        methods.len(),
        total,
        "two commands share a method: {methods:?}"
    );
}

/// Pins the shape a transport wraps, for both kinds of variant: a command with
/// params, and one without.
#[test]
fn the_envelope_is_method_plus_params() {
    assert_eq!(
        serde_json::to_string(&Command::SessionMessage {
            session: "s1".to_owned(),
            message: "hello".to_owned(),
        })
        .expect("serializes"),
        r#"{"method":"session/message","params":{"session":"s1","message":"hello"}}"#
    );
    assert_eq!(
        serde_json::to_string(&Command::SessionCreate).expect("serializes"),
        r#"{"method":"session/create"}"#
    );
}

#[test]
fn the_handshake_answers_with_this_builds_version() {
    let hello = HelloResult::default();
    assert_eq!(hello.version, PROTOCOL_VERSION);
    let text = serde_json::to_string(&hello).expect("serializes");
    assert_eq!(text, format!(r#"{{"version":"{PROTOCOL_VERSION}"}}"#));
}

/// The version is what tells a client whether it can talk to this core, so it
/// has to be a version — `"dev"` or `"1"` would parse as neither.
#[test]
fn the_protocol_version_is_semver() {
    let parts: Vec<&str> = PROTOCOL_VERSION.split('.').collect();
    assert_eq!(parts.len(), 3, "major.minor.patch: {PROTOCOL_VERSION}");
    for part in parts {
        assert!(
            !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit()),
            "each component is a number: {PROTOCOL_VERSION}"
        );
    }
}
