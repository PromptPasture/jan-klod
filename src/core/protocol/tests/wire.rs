//! The wire names are the contract, so they are asserted rather than assumed.
//!
//! A rename is a major-version change (see the crate docs), and a rename is
//! exactly what a careless `#[serde(rename)]` edit or a variant reshuffle
//! produces silently. These tests make it loud.

use jan_klod_protocol::{Command, HelloResult, Notification, PROTOCOL_VERSION};

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

/// The `method` each notification must serialize to. Exhaustive for the same
/// reason [`expected_method`] is.
const fn expected_notification_method(notification: &Notification) -> &'static str {
    match notification {
        Notification::TextDelta { .. } => "text-delta",
        Notification::ToolInvoked { .. } => "tool-invoked",
        Notification::ToolResult { .. } => "tool-result",
        Notification::Warning { .. } => "warning",
        Notification::Done { .. } => "done",
        Notification::Ask { .. } => "ask",
        Notification::Error { .. } => "error",
        Notification::SessionUpdated { .. } => "session/updated",
    }
}

/// One sample per notification, every field populated.
fn every_notification() -> Vec<Notification> {
    vec![
        Notification::TextDelta {
            text: "the answer is".to_owned(),
        },
        Notification::ToolInvoked {
            id: "c1".to_owned(),
            name: "fs.read".to_owned(),
            arguments: r#"{"path":"README.md"}"#.to_owned(),
        },
        Notification::ToolResult {
            id: "c1".to_owned(),
            content: "# Jan-Klod".to_owned(),
        },
        Notification::Warning {
            message: "provider fell back".to_owned(),
        },
        Notification::Done {
            answer: "42".to_owned(),
            agentic: true,
        },
        Notification::Ask {
            session: "s1".to_owned(),
            question: "Run `rm -rf`?".to_owned(),
            options: vec!["yes".to_owned(), "no".to_owned()],
            default: "no".to_owned(),
        },
        Notification::Error {
            message: "the provider could not be reached".to_owned(),
        },
        Notification::SessionUpdated {
            session: "s1".to_owned(),
            preview: "hello".to_owned(),
        },
    ]
}

#[test]
fn every_notification_serializes_to_its_documented_method() {
    for notification in every_notification() {
        let json: serde_json::Value =
            serde_json::to_value(&notification).expect("a notification serializes");
        assert_eq!(
            json.get("method").and_then(serde_json::Value::as_str),
            Some(expected_notification_method(&notification)),
            "wire name of {notification:?}"
        );
    }
}

#[test]
fn every_notification_round_trips() {
    for notification in every_notification() {
        let text = serde_json::to_string(&notification).expect("serializes");
        let back: Notification = serde_json::from_str(&text).expect("deserializes");
        assert_eq!(back, notification, "round-trip of {text}");
    }
}

#[test]
fn each_notification_has_its_own_method() {
    let mut methods: Vec<&str> = every_notification()
        .iter()
        .map(expected_notification_method)
        .collect();
    let total = methods.len();
    methods.sort_unstable();
    methods.dedup();
    assert_eq!(
        methods.len(),
        total,
        "two notifications share a method: {methods:?}"
    );
}

/// A command and a notification are told apart by their `method`, not by the
/// envelope, so the two name spaces must not collide — a transport that routes
/// on `method` alone would otherwise dispatch one as the other.
#[test]
fn no_notification_shares_a_method_with_a_command() {
    for notification in every_notification() {
        let name = expected_notification_method(&notification);
        for command in every_command() {
            assert_ne!(name, expected_method(&command), "{name} is used by both");
        }
    }
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

// ─── JSON Schema export ──────────────────────────────────────────────────────
//
// The schema under `schema/` is what a non-Rust client generates types from, so
// it has to describe these types and not a past version of them. It is written
// by the generator below rather than by hand, and this file is the single source
// of the samples both the wire tests and the schema are built from — so a new
// command or notification cannot reach the schema without also reaching the
// tests above.
//
// `schemars` would do this with a derive. It was measured and rejected: it adds
// seven packages to the audited tree, one of them a second major version of
// `syn` next to the one already there, and this repository treats build
// footprint as a real cost (#41). `## Scope` permits either.

/// Where the committed schema lives.
fn schema_path() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("schema/protocol.schema.json")
}

/// The JSON Schema for one field, inferred from a populated sample value.
///
/// Panics on a kind it does not handle, rather than guessing: a field whose type
/// stops being a string, a bool or a list of strings must be described
/// deliberately, not approximated.
fn field_schema(value: &serde_json::Value) -> serde_json::Value {
    match value {
        serde_json::Value::String(_) => serde_json::json!({ "type": "string" }),
        serde_json::Value::Bool(_) => serde_json::json!({ "type": "boolean" }),
        serde_json::Value::Array(items) => {
            assert!(
                !items.is_empty() && items.iter().all(serde_json::Value::is_string),
                "extend field_schema: a sample list must be non-empty and all strings, got {value}"
            );
            serde_json::json!({ "type": "array", "items": { "type": "string" } })
        }
        other => panic!("extend field_schema for {other}"),
    }
}

/// The schema for one tagged variant, derived from its serialized sample.
fn variant_schema(envelope: &serde_json::Value) -> serde_json::Value {
    let object = envelope.as_object().expect("an envelope is an object");
    let method = object
        .get("method")
        .and_then(serde_json::Value::as_str)
        .expect("every envelope is tagged");
    let Some(params) = object.get("params") else {
        // A variant with no fields: serde omits `params` entirely.
        return serde_json::json!({
            "title": method,
            "type": "object",
            "properties": { "method": { "const": method } },
            "required": ["method"],
            "additionalProperties": false,
        });
    };
    let fields = params.as_object().expect("params is an object");
    let properties: serde_json::Map<String, serde_json::Value> = fields
        .iter()
        .map(|(key, value)| (key.clone(), field_schema(value)))
        .collect();
    serde_json::json!({
        "title": method,
        "type": "object",
        "properties": {
            "method": { "const": method },
            "params": {
                "type": "object",
                "properties": properties,
                "required": fields.keys().cloned().collect::<Vec<String>>(),
                "additionalProperties": false,
            },
        },
        "required": ["method", "params"],
        "additionalProperties": false,
    })
}

/// The whole schema, built from the samples above.
fn generated_schema() -> serde_json::Value {
    let one_of = |mut variants: Vec<serde_json::Value>| {
        // Sorted by title, so reordering the samples is not a schema change.
        variants.sort_by(|a, b| a["title"].as_str().cmp(&b["title"].as_str()));
        serde_json::json!({ "oneOf": variants })
    };
    serde_json::json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "title": "Jan-Klod client protocol",
        "description": "Commands a client sends and notifications the core reports. \
                        The transport wraps each in its own envelope; the `method` \
                        and `params` members here are the contract.",
        "x-protocol-version": PROTOCOL_VERSION,
        "$defs": {
            "Command": one_of(
                every_command()
                    .iter()
                    .map(|c| variant_schema(&serde_json::to_value(c).expect("serializes")))
                    .collect(),
            ),
            "Notification": one_of(
                every_notification()
                    .iter()
                    .map(|n| variant_schema(&serde_json::to_value(n).expect("serializes")))
                    .collect(),
            ),
            "HelloResult": {
                "type": "object",
                "properties": { "version": { "type": "string" } },
                "required": ["version"],
                "additionalProperties": false,
            },
        },
    })
}

/// Regenerating the committed schema must be a no-op.
///
/// Set `JK_UPDATE_SCHEMA=1` to rewrite it after a deliberate change; the
/// rewritten file is what gets reviewed and committed.
#[test]
fn the_committed_schema_matches_the_types() {
    let mut expected = serde_json::to_string_pretty(&generated_schema()).expect("serializes");
    expected.push('\n');
    let path = schema_path();
    if std::env::var("JK_UPDATE_SCHEMA").is_ok() {
        std::fs::create_dir_all(path.parent().expect("has a parent")).expect("creates schema/");
        std::fs::write(&path, &expected).expect("writes the schema");
        return;
    }
    let committed = std::fs::read_to_string(&path).unwrap_or_else(|e| {
        panic!(
            "{}: {e}. Generate it with `JK_UPDATE_SCHEMA=1 cargo test -p jan-klod-protocol`",
            path.display()
        )
    });
    assert_eq!(
        committed, expected,
        "the committed schema no longer describes these types. Regenerate it with \
         `JK_UPDATE_SCHEMA=1 cargo test -p jan-klod-protocol` and commit the result"
    );
}

/// Every wire name reaches the schema. The test above compares whole documents,
/// which fails loudly but says little; this one names what is missing.
#[test]
fn the_schema_covers_every_command_and_notification() {
    let schema = generated_schema();
    for (def, names) in [
        (
            "Command",
            every_command()
                .iter()
                .map(expected_method)
                .collect::<Vec<_>>(),
        ),
        (
            "Notification",
            every_notification()
                .iter()
                .map(expected_notification_method)
                .collect::<Vec<_>>(),
        ),
    ] {
        let titles: Vec<&str> = schema["$defs"][def]["oneOf"]
            .as_array()
            .expect("oneOf is a list")
            .iter()
            .map(|v| v["title"].as_str().expect("each has a title"))
            .collect();
        for name in names {
            assert!(titles.contains(&name), "{def} schema is missing `{name}`");
        }
    }
}
