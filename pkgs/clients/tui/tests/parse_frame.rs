#![allow(missing_docs)]
use jk_protocol::SSE_FRAME_KINDS;
use jk_tui::{parse_frame, StreamEvent};

/// Guards #81: every frame kind the core sends needs a client arm, or it is silently dropped.
#[test]
fn every_sse_frame_kind_the_core_emits_is_handled() {
    for kind in SSE_FRAME_KINDS {
        assert!(
            parse_frame(kind, "{}").is_some(),
            "the core emits `{kind}` and this client drops it"
        );
    }
}

#[test]
fn parse_frame_maps_each_event_kind() {
    assert_eq!(
        parse_frame("delta", r#"{"text":"hi"}"#),
        Some(StreamEvent::Delta("hi".into()))
    );
    assert_eq!(
        parse_frame("tool", r#"{"name":"bash","id":"1","arguments":"{}"}"#),
        Some(StreamEvent::Tool {
            id: "1".into(),
            name: "bash".into(),
            arguments: Some("{}".into()),
        })
    );
    assert_eq!(
        parse_frame(
            "tool-result",
            r#"{"id":"c1","content":"42","failed":false}"#
        ),
        Some(StreamEvent::ToolResult {
            id: "c1".into(),
            content: "42".into(),
            failed: false,
        })
    );
    // `failed` must be read (#162): a failed call must not arrive as a success.
    assert_eq!(
        parse_frame(
            "tool-result",
            r#"{"id":"c1","content":"the sandbox said no","failed":true}"#
        ),
        Some(StreamEvent::ToolResult {
            id: "c1".into(),
            content: "the sandbox said no".into(),
            failed: true,
        })
    );
    // An older core omits `failed`. That is a compatible mismatch, not malformed: refusing it would break the turn.
    assert_eq!(
        parse_frame("tool-result", r#"{"id":"c1","content":"42"}"#),
        Some(StreamEvent::ToolResult {
            id: "c1".into(),
            content: "42".into(),
            failed: false,
        })
    );
    assert_eq!(
        parse_frame("warning", r#"{"message":"falling back"}"#),
        Some(StreamEvent::Warning("falling back".into()))
    );
    assert_eq!(
        parse_frame("done", r#"{"answer":"result","agentic":true}"#),
        Some(StreamEvent::Done("result".into()))
    );
    assert_eq!(
        parse_frame("error", r#"{"error":"boom"}"#),
        Some(StreamEvent::Error("boom".into()))
    );
}

/// Regression: `tool-result` had no arm, so healthy tool results fell through to the error fallback.
#[test]
fn a_tool_result_is_never_reported_as_an_error() {
    let ev = parse_frame("tool-result", r#"{"id":"c1","content":"42"}"#);
    assert!(
        !matches!(ev, Some(StreamEvent::Error(_))),
        "a tool result is a normal turn's progress, not a failure: {ev:?}"
    );
}

/// An unknown kind is dropped, not reported: the core is newer, which is not a failed turn (#81).
#[test]
fn an_unknown_kind_is_dropped_rather_than_shown_as_a_failure() {
    assert_eq!(parse_frame("weird", "{}"), None);
    // A plausible but unknown kind: the realistic case of a core one frame ahead.
    assert_eq!(
        parse_frame("tool-progress", r#"{"id":"c1","pct":40}"#),
        None
    );
}

#[test]
fn parse_frame_surfaces_malformed_json_as_error() {
    let ev = parse_frame("delta", "not json at all");
    assert!(
        matches!(ev, Some(StreamEvent::Error(_))),
        "expected Error, got {ev:?}"
    );
    if let Some(StreamEvent::Error(msg)) = ev {
        assert!(msg.contains("malformed SSE frame"), "{msg}");
    }
}

#[test]
fn parse_frame_reads_a_prompt_with_its_options() {
    let data = r#"{"question":"Allow tool `bash`?","options":["yes","no","always","never"],"default":"no","session":"s1"}"#;
    assert_eq!(
        parse_frame("prompt", data),
        Some(StreamEvent::Prompt {
            session: "s1".into(),
            question: "Allow tool `bash`?".into(),
            options: vec!["yes".into(), "no".into(), "always".into(), "never".into()],
            default: "no".into(),
        })
    );
}

#[test]
fn a_prompt_without_options_still_parses() {
    // A future/odd prompt must not turn into an Error the user cannot answer.
    assert_eq!(
        parse_frame(
            "prompt",
            r#"{"question":"Proceed?","default":"no","session":"s1"}"#
        ),
        Some(StreamEvent::Prompt {
            session: "s1".into(),
            question: "Proceed?".into(),
            options: vec![],
            default: "no".into()
        })
    );
}

/// #103: a prompt is answered on the session the frame names, not necessarily the current one.
#[test]
fn parse_frame_carries_the_prompts_own_session() {
    let data = r#"{"question":"Proceed?","default":"no","session":"a-different-session"}"#;
    let Some(StreamEvent::Prompt { session, .. }) = parse_frame("prompt", data) else {
        panic!("expected a Prompt event");
    };
    assert_eq!(session, "a-different-session");
}
