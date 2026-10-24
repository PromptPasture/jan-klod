//! The wire names are the contract, so they are asserted: a rename is a major-version change.

use jk_protocol::{jsonrpc, Command, HelloResult, Notification, PROTOCOL_VERSION};

/// The `method` each command must serialize to.
///
/// Exhaustive: a new command fails to compile until named here and sampled in [`every_command`].
const fn expected_method(command: &Command) -> &'static str {
    match command {
        Command::Hello { .. } => "protocol/hello",
        Command::SessionCreate => "session/create",
        Command::SessionList => "session/list",
        Command::SessionGet { .. } => "session/get",
        Command::SessionFork { .. } => "session/fork",
        Command::SessionMessage { .. } => "session/message",
        Command::TurnAnswer { .. } => "turn/answer",
        Command::TurnCancel { .. } => "turn/cancel",
        Command::TurnFollowUp { .. } => "turn/follow-up",
        Command::SurfaceInvoke { .. } => "surface/invoke",
    }
}

/// One sample per command, every field populated so a dropped field fails the round-trip.
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
        Command::SessionFork {
            session: "s1".to_owned(),
            at_seq: 7,
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
        Command::SurfaceInvoke {
            extension: "interceptor.system".to_owned(),
            name: "prompt".to_owned(),
            arguments: vec![jk_protocol::ArgumentValue {
                name: "verbose".to_owned(),
                value: "true".to_owned(),
            }],
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

/// The shape a transport wraps, for a command with params and one without.
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

/// The `method` each notification must serialize to; exhaustive like [`expected_method`].
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
        Notification::SurfaceContributions { .. } => "surface/contributions",
    }
}

/// One sample per notification, all fields populated.
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
            failed: true,
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
        // Fully populated: the schema is derived from this value.
        Notification::SurfaceContributions {
            extensions: vec![jk_protocol::Contributions {
                extension: "interceptor.system".to_owned(),
                commands: vec![jk_protocol::SurfaceCommand {
                    name: "prompt".to_owned(),
                    title: "System prompt".to_owned(),
                    description: "Show the standing instructions".to_owned(),
                    arguments: vec![jk_protocol::Argument {
                        name: "verbose".to_owned(),
                        description: "Include the built-in text".to_owned(),
                        required: false,
                    }],
                }],
                status_items: vec![jk_protocol::StatusItem {
                    name: "prompt-source".to_owned(),
                    text: "built-in".to_owned(),
                    detail: "no `prompt` key is configured".to_owned(),
                }],
                forms: vec![jk_protocol::SurfaceForm {
                    name: "set-prompt".to_owned(),
                    title: "Replace the system prompt".to_owned(),
                    fields: vec![jk_protocol::Field {
                        name: "text".to_owned(),
                        label: "Instructions".to_owned(),
                        options: vec!["built-in".to_owned(), "custom".to_owned()],
                        default_value: "built-in".to_owned(),
                    }],
                }],
            }],
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

/// Commands and notifications are told apart by `method`, so the name spaces must not collide.
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

/// The version must parse as a version; `"dev"` or `"1"` would not.
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

/// A frame is the command's members plus the envelope's, flat; asserted as exact text because a shape check would pass on `{"command":{…}}`.
#[test]
fn a_request_is_the_command_flattened_into_the_envelope() {
    assert_eq!(
        serde_json::to_string(&jsonrpc::Request::new(
            jsonrpc::Id::Number(1),
            Command::SessionMessage {
                session: "s1".to_owned(),
                message: "hello".to_owned(),
            },
        ))
        .expect("serializes"),
        r#"{"jsonrpc":"2.0","id":1,"method":"session/message","params":{"session":"s1","message":"hello"}}"#
    );
    // No fields means no `params` member, which makes `flatten` legal over an adjacently tagged enum.
    assert_eq!(
        serde_json::to_string(&jsonrpc::Request::new(
            jsonrpc::Id::Number(2),
            Command::SessionCreate,
        ))
        .expect("serializes"),
        r#"{"jsonrpc":"2.0","id":2,"method":"session/create"}"#
    );
}

/// Every command survives the frame both ways; `flatten` buffers, so an unbufferable type
/// would serialize but fail to parse back.
#[test]
fn every_command_round_trips_inside_a_frame() {
    for command in every_command() {
        let framed = jsonrpc::Request::new(jsonrpc::Id::Number(7), command.clone());
        let text = serde_json::to_string(&framed).expect("serializes");
        let back: jsonrpc::Request = serde_json::from_str(&text)
            .unwrap_or_else(|e| panic!("a framed {text} does not parse back: {e}"));
        assert_eq!(back.command, command, "round-trip of {text}");
        assert_eq!(back.jsonrpc, jsonrpc::VERSION);
    }
}

/// A notification has no `id`, the JSON-RPC way to say "nothing answers this".
#[test]
fn a_notification_frame_carries_no_id() {
    let text = serde_json::to_string(&jsonrpc::Notification::new(Notification::Done {
        answer: "42".to_owned(),
        agentic: true,
    }))
    .expect("serializes");
    assert_eq!(
        text,
        r#"{"jsonrpc":"2.0","method":"done","params":{"answer":"42","agentic":true}}"#
    );
    assert!(!text.contains("\"id\""), "no id member: {text}");
}

/// Both id spellings round-trip unchanged; answering `"7"` with `7` would break clients keying on the literal.
#[test]
fn an_id_comes_back_the_way_it_was_sent() {
    for id in [
        jsonrpc::Id::Number(7),
        jsonrpc::Id::Number(-1),
        jsonrpc::Id::Text("abc".to_owned()),
    ] {
        let request = jsonrpc::Request::new(id.clone(), Command::SessionList);
        let text = serde_json::to_string(&request).expect("serializes");
        let back: jsonrpc::Request = serde_json::from_str(&text).expect("parses");
        assert_eq!(back.id, id, "id round-trip of {text}");

        let answered = jsonrpc::Response::result(back.id, serde_json::json!({}));
        let text = serde_json::to_string(&answered).expect("serializes");
        let back: jsonrpc::Response = serde_json::from_str(&text).expect("parses");
        assert_eq!(back.id, id, "the response echoes it: {text}");
    }
}

/// [`jsonrpc::Outcome`] is an enum, so a response with both members or neither cannot be built.
#[test]
fn a_response_carries_either_a_result_or_an_error() {
    assert_eq!(
        serde_json::to_string(&jsonrpc::Response::result(
            jsonrpc::Id::Number(1),
            serde_json::json!({ "version": PROTOCOL_VERSION }),
        ))
        .expect("serializes"),
        format!(r#"{{"jsonrpc":"2.0","id":1,"result":{{"version":"{PROTOCOL_VERSION}"}}}}"#)
    );
    assert_eq!(
        serde_json::to_string(&jsonrpc::Response::error(
            jsonrpc::Id::Text("a".to_owned()),
            jsonrpc::Error::new(jsonrpc::METHOD_NOT_FOUND, "no such command"),
        ))
        .expect("serializes"),
        r#"{"jsonrpc":"2.0","id":"a","error":{"code":-32601,"message":"no such command"}}"#
    );
    // `data` is absent, not null, when there is none.
    let with_data = serde_json::to_string(&jsonrpc::Response::error(
        jsonrpc::Id::Number(1),
        jsonrpc::Error::new(jsonrpc::INTERNAL_ERROR, "boom")
            .with_data(serde_json::json!({ "where": "store" })),
    ))
    .expect("serializes");
    assert!(
        with_data.contains(r#""data":{"where":"store"}"#),
        "{with_data}"
    );
}

/// The reserved codes are the spec's.
#[test]
fn the_error_codes_are_the_ones_the_spec_reserves() {
    assert_eq!(jsonrpc::PARSE_ERROR, -32700);
    assert_eq!(jsonrpc::INVALID_REQUEST, -32600);
    assert_eq!(jsonrpc::METHOD_NOT_FOUND, -32601);
    assert_eq!(jsonrpc::INVALID_PARAMS, -32602);
    assert_eq!(jsonrpc::INTERNAL_ERROR, -32603);
    // The server-defined range is -32000..=-32099.
    assert!(
        (-32099..=-32000).contains(&jsonrpc::INCOMPATIBLE_VERSION),
        "server-defined codes live in -32099..=-32000, got {}",
        jsonrpc::INCOMPATIBLE_VERSION
    );
}

/// `COMMAND_METHODS` must match the exhaustive `match` in [`expected_method`] both ways:
/// a missing command reports `method not found`, a stale one `invalid params`.
#[test]
fn the_method_list_holds_every_command_and_nothing_else() {
    let mut from_the_enum: Vec<&str> = every_command().iter().map(expected_method).collect();
    from_the_enum.sort_unstable();
    let mut listed: Vec<&str> = jk_protocol::COMMAND_METHODS.to_vec();
    listed.sort_unstable();
    assert_eq!(
        listed, from_the_enum,
        "COMMAND_METHODS has fallen behind Command"
    );
}

/// The spec's answer to a frame with no readable id; pins serde's untagged unit variant.
#[test]
fn an_unreadable_frame_is_answered_with_a_null_id() {
    let text = serde_json::to_string(&jsonrpc::Response::error(
        jsonrpc::Id::Null,
        jsonrpc::Error::new(jsonrpc::PARSE_ERROR, "not JSON"),
    ))
    .expect("serializes");
    assert_eq!(
        text,
        r#"{"jsonrpc":"2.0","id":null,"error":{"code":-32700,"message":"not JSON"}}"#
    );
    let back: jsonrpc::Response = serde_json::from_str(&text).expect("parses");
    assert_eq!(back.id, jsonrpc::Id::Null, "null reads back as Null");
    // Distinct from an id a client could send.
    assert_ne!(jsonrpc::Id::Null, jsonrpc::Id::Number(0));
    assert_ne!(jsonrpc::Id::Null, jsonrpc::Id::Text(String::new()));
}

/// While the protocol is `0.x`, a differing minor is refused.
#[test]
fn a_differing_minor_is_refused_while_the_major_is_zero() {
    assert!(jk_protocol::compatible("0.1.0", "0.1.0"));
    assert!(
        jk_protocol::compatible("0.1.0", "0.1.9"),
        "a patch bump is compatible"
    );
    for refused in ["0.2.0", "1.0.0", "0.0.1", "", "dev", "1", "x.y.z"] {
        assert!(
            !jk_protocol::compatible("0.1.0", refused),
            "{refused} must not be accepted by a 0.1 core"
        );
    }
    // From 1.0, additive minors pass.
    assert!(jk_protocol::compatible("1.3.0", "1.1.0"));
    assert!(!jk_protocol::compatible("1.3.0", "2.0.0"));
    // This build is compatible with itself, which fails if PROTOCOL_VERSION stops parsing.
    assert!(jk_protocol::compatible(PROTOCOL_VERSION, PROTOCOL_VERSION));
}

/// A refused handshake returns no [`HelloResult`], so the refusal must state the core's version.
#[test]
fn a_refused_handshake_says_what_the_core_speaks() {
    let error = jsonrpc::incompatible_version("9.9.9");
    assert_eq!(error.code, jsonrpc::INCOMPATIBLE_VERSION);
    assert!(
        error.message.contains("9.9.9") && error.message.contains(PROTOCOL_VERSION),
        "the message names both versions: {}",
        error.message
    );
    assert_eq!(
        error.data.expect("carries data")["version"],
        serde_json::json!(PROTOCOL_VERSION),
        "the core's version is machine-readable, not only prose"
    );
}

// ─── JSON Schema export ──────────────────────────────────────────────────────
//
// The schema under `schema/` is what non-Rust clients generate from. It is written by
// the generator below from the same samples the wire tests use.
//
// `schemars` was rejected: seven extra packages, including a second major of `syn` (#41).
/// Where the committed schema lives.
fn schema_path() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("schema/protocol.schema.json")
}

/// The JSON Schema for one field, inferred from a populated sample value.
///
/// Panics on an unhandled kind rather than guessing. Objects and lists recurse;
/// samples must be fully populated, since an absent field describes nothing.
fn field_schema(value: &serde_json::Value) -> serde_json::Value {
    match value {
        serde_json::Value::String(_) => serde_json::json!({ "type": "string" }),
        serde_json::Value::Bool(_) => serde_json::json!({ "type": "boolean" }),
        serde_json::Value::Object(members) => {
            let properties: serde_json::Map<String, serde_json::Value> = members
                .iter()
                .map(|(key, value)| (key.clone(), field_schema(value)))
                .collect();
            let required: Vec<&String> = members.keys().collect();
            serde_json::json!({
                "type": "object",
                "properties": properties,
                "required": required,
                "additionalProperties": false,
            })
        }
        serde_json::Value::Array(items) => {
            let first = items.first().unwrap_or_else(|| {
                panic!("extend field_schema: a sample list must be non-empty, got {value}")
            });
            // One element describes the list, so all elements must agree.
            let item = field_schema(first);
            assert!(
                items.iter().all(|other| field_schema(other) == item),
                "extend field_schema: a sample list must be homogeneous, got {value}"
            );
            serde_json::json!({ "type": "array", "items": item })
        }
        // Unsigned integer, not a bare `number`: every numeric field here is a count or
        // position. The first genuine float lands in the panic below, on purpose.
        serde_json::Value::Number(number) if number.is_u64() => {
            serde_json::json!({ "type": "integer", "minimum": 0 })
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
        // No fields: serde omits `params`.
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

/// The same variant, inside its JSON-RPC frame.
///
/// Expanded rather than `allOf` + `$ref`: variants carry `additionalProperties: false`,
/// so composition would reject the envelope's own members.
fn framed(variant: &serde_json::Value, with_id: bool) -> serde_json::Value {
    let mut schema = variant.clone();
    let object = schema
        .as_object_mut()
        .expect("a variant schema is an object");
    let properties = object
        .get_mut("properties")
        .and_then(serde_json::Value::as_object_mut)
        .expect("a variant schema has properties");
    properties.insert(
        "jsonrpc".to_owned(),
        serde_json::json!({ "const": jsonrpc::VERSION }),
    );
    if with_id {
        properties.insert(
            "id".to_owned(),
            serde_json::json!({ "$ref": "#/$defs/jsonrpc.Id" }),
        );
    }
    let required = object
        .get_mut("required")
        .and_then(serde_json::Value::as_array_mut)
        .expect("a variant schema lists required members");
    required.push(serde_json::json!("jsonrpc"));
    if with_id {
        required.push(serde_json::json!("id"));
    }
    schema
}

/// The two shapes a [`jsonrpc::Response`] takes.
///
/// Hand-written because `result` is any value (`true` is the 2020-12 spelling);
/// [`the_hand_written_frames_still_match_the_types`] guards drift.
fn response_schemas() -> Vec<serde_json::Value> {
    let id = serde_json::json!({ "$ref": "#/$defs/jsonrpc.Id" });
    vec![
        serde_json::json!({
            "title": "result",
            "type": "object",
            "properties": {
                "jsonrpc": { "const": jsonrpc::VERSION },
                "id": id,
                "result": true,
            },
            "required": ["jsonrpc", "id", "result"],
            "additionalProperties": false,
        }),
        serde_json::json!({
            "title": "error",
            "type": "object",
            "properties": {
                "jsonrpc": { "const": jsonrpc::VERSION },
                "id": id,
                "error": { "$ref": "#/$defs/jsonrpc.Error" },
            },
            "required": ["jsonrpc", "id", "error"],
            "additionalProperties": false,
        }),
    ]
}

/// The whole schema, built from the samples above.
fn generated_schema() -> serde_json::Value {
    let one_of = |mut variants: Vec<serde_json::Value>| {
        // Sorted by title so reordering samples is not a schema change.
        variants.sort_by(|a, b| a["title"].as_str().cmp(&b["title"].as_str()));
        serde_json::json!({ "oneOf": variants })
    };
    let commands: Vec<serde_json::Value> = every_command()
        .iter()
        .map(|c| variant_schema(&serde_json::to_value(c).expect("serializes")))
        .collect();
    let notifications: Vec<serde_json::Value> = every_notification()
        .iter()
        .map(|n| variant_schema(&serde_json::to_value(n).expect("serializes")))
        .collect();
    serde_json::json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "title": "Jan-Klod client protocol",
        "description": "Commands a client sends and notifications the core reports, \
                        as the `method`/`params` contract and again inside the \
                        JSON-RPC 2.0 frame the transports send. `Command` and \
                        `Notification` are the contract; the `jsonrpc.*` definitions \
                        are what goes over a pipe or a socket.",
        "x-protocol-version": PROTOCOL_VERSION,
        "$defs": {
            "Command": one_of(commands.clone()),
            "Notification": one_of(notifications.clone()),
            "HelloResult": {
                "type": "object",
                "properties": { "version": { "type": "string" } },
                "required": ["version"],
                "additionalProperties": false,
            },
            // Read side of the contract (#106): `session/fork` took a seq no client could read.
            "SessionGetResult": {
                "type": "object",
                "properties": {
                    "id": { "type": "string" },
                    "messages": {
                        "type": "array",
                        "items": { "$ref": "#/$defs/TranscriptMessage" },
                    },
                },
                "required": ["id", "messages"],
                "additionalProperties": false,
            },
            "TranscriptMessage": {
                "type": "object",
                "description": "One message of a transcript. `seq` is the log \
                                position it was projected from and what \
                                `session/fork` takes as `at-seq`, inclusive — \
                                sparse, because events projecting to no message \
                                still consume one, so it is not an index.",
                "properties": {
                    "seq": { "type": "integer", "minimum": 0 },
                    "role": { "enum": ["system", "user", "assistant", "tool"] },
                    "content": { "type": "string" },
                    "tool-call-id": { "type": "string" },
                },
                "required": ["seq", "role", "content"],
                "additionalProperties": false,
            },
            "jsonrpc.Id": {
                "description": "Echoed back on the response that answers a request.",
                "oneOf": [{ "type": "integer" }, { "type": "string" }, { "type": "null" }],
            },
            "jsonrpc.Request": one_of(
                commands.iter().map(|c| framed(c, true)).collect(),
            ),
            "jsonrpc.Notification": one_of(
                notifications.iter().map(|n| framed(n, false)).collect(),
            ),
            "jsonrpc.Response": one_of(response_schemas()),
            "jsonrpc.Error": {
                "type": "object",
                "properties": {
                    "code": { "type": "integer" },
                    "message": { "type": "string" },
                    "data": true,
                },
                "required": ["code", "message"],
                "additionalProperties": false,
            },
        },
    })
}

/// Regenerating the committed schema must be a no-op.
///
/// Set `JK_UPDATE_SCHEMA=1` to rewrite it after a deliberate change.
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
            "{}: {e}. Generate it with `JK_UPDATE_SCHEMA=1 cargo test -p jk-protocol`",
            path.display()
        )
    });
    assert_eq!(
        committed, expected,
        "the committed schema no longer describes these types. Regenerate it with \
         `JK_UPDATE_SCHEMA=1 cargo test -p jk-protocol` and commit the result"
    );
}

/// Every wire name reaches the schema; names what is missing.
#[test]
fn the_schema_covers_every_command_and_notification() {
    let schema = generated_schema();
    let commands = every_command()
        .iter()
        .map(expected_method)
        .collect::<Vec<_>>();
    let notifications = every_notification()
        .iter()
        .map(expected_notification_method)
        .collect::<Vec<_>>();
    // The framed definitions are checked too: a command absent from the frame cannot be sent.
    for (def, names) in [
        ("Command", &commands),
        ("jsonrpc.Request", &commands),
        ("Notification", &notifications),
        ("jsonrpc.Notification", &notifications),
    ] {
        let titles: Vec<&str> = schema["$defs"][def]["oneOf"]
            .as_array()
            .expect("oneOf is a list")
            .iter()
            .map(|v| v["title"].as_str().expect("each has a title"))
            .collect();
        for name in names {
            assert!(titles.contains(name), "{def} schema is missing `{name}`");
        }
    }
}

/// The hand-written `jsonrpc.Response` and `jsonrpc.Error` schemas (see [`response_schemas`])
/// can drift from the Rust types, so compare them to real values: every carried member
/// described, every required member present.
#[test]
fn the_hand_written_frames_still_match_the_types() {
    let schema = generated_schema();
    let members = |value: &serde_json::Value| -> Vec<String> {
        value
            .as_object()
            .expect("a frame is an object")
            .keys()
            .cloned()
            .collect()
    };
    let branch = |def: &str, title: &str| -> serde_json::Value {
        schema["$defs"][def]["oneOf"]
            .as_array()
            .expect("oneOf is a list")
            .iter()
            .find(|v| v["title"] == serde_json::json!(title))
            .unwrap_or_else(|| panic!("{def} has no `{title}` branch"))
            .clone()
    };

    for (title, sample) in [
        (
            "result",
            serde_json::to_value(jsonrpc::Response::result(
                jsonrpc::Id::Number(1),
                serde_json::json!({ "anything": true }),
            ))
            .expect("serializes"),
        ),
        (
            "error",
            serde_json::to_value(jsonrpc::Response::error(
                jsonrpc::Id::Number(1),
                jsonrpc::Error::new(jsonrpc::INTERNAL_ERROR, "boom"),
            ))
            .expect("serializes"),
        ),
    ] {
        let described = branch("jsonrpc.Response", title);
        let allowed = members(&described["properties"]);
        for member in members(&sample) {
            assert!(
                allowed.contains(&member),
                "a response carries `{member}`, which the `{title}` branch does not describe"
            );
        }
        for required in described["required"].as_array().expect("a list") {
            let name = required.as_str().expect("a member name");
            assert!(
                sample.get(name).is_some(),
                "the `{title}` branch requires `{name}`, which no response carries"
            );
        }
    }

    // The error object, with `data` absent and present.
    let described = members(&schema["$defs"]["jsonrpc.Error"]["properties"]);
    for error in [
        jsonrpc::Error::new(jsonrpc::PARSE_ERROR, "bad json"),
        jsonrpc::incompatible_version("9.9.9"),
    ] {
        for member in members(&serde_json::to_value(&error).expect("serializes")) {
            assert!(
                described.contains(&member),
                "an error carries `{member}`, which jsonrpc.Error does not describe"
            );
        }
    }
}
