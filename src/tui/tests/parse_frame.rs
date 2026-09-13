#![allow(missing_docs)]
use jan_klod::{parse_frame, StreamEvent};
use jan_klod_protocol::SSE_FRAME_KINDS;

/// The guard this file most needs, and the one that did not exist when #81
/// happened: a frame kind the core can send that this client has no answer for.
///
/// It matters more since an unknown kind became a silent drop. Before that a
/// missing arm was at least loud, if wrongly so — it reached the user as an
/// error. Now nothing at runtime would say a word, so this test is the only
/// thing between a new frame kind and a client that quietly ignores it.
///
/// The list is `jan-klod-protocol`'s because this crate cannot see the core;
/// `core/tests/protocol_events.rs` holds the other end, proving the same
/// constant is exactly what `serve` emits for every `conductor::Event`.
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
    // The flag is read, not merely carried (#162) — a frame saying the call
    // failed must not arrive as one that succeeded.
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
    // A core older than the flag sends no `failed` key. That is a compatible
    // mismatch, not a malformed frame: refusing it would turn a version
    // disagreement into a broken turn, and `hello` is where versions are
    // supposed to be settled. Its failures then render as successes, which is
    // exactly what happened before the flag existed.
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

/// The regression this file exists to hold: the `tool-result` kind had no arm,
/// so every tool result in a **healthy** turn took the fallback and reached the
/// user as an "unknown event" error. Stated separately from the mapping test
/// above because it is an invariant about the fallback rather than about a
/// shape — it stays true if the variant's fields ever change.
#[test]
fn a_tool_result_is_never_reported_as_an_error() {
    let ev = parse_frame("tool-result", r#"{"id":"c1","content":"42"}"#);
    assert!(
        !matches!(ev, Some(StreamEvent::Error(_))),
        "a tool result is a normal turn's progress, not a failure: {ev:?}"
    );
}

/// A kind this client does not know is **dropped**, not reported. It means the
/// core is newer than this client, or a frame was added without updating it —
/// neither is a failed turn, and calling it one is what #81 was. Drift is caught
/// by a test instead, which is where a claim about two files belongs.
#[test]
fn an_unknown_kind_is_dropped_rather_than_shown_as_a_failure() {
    assert_eq!(parse_frame("weird", "{}"), None);
    // Including one that looks plausible: this is the case that actually
    // happens, a core one frame ahead of its client.
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

/// #103's first design point, at the SSE surface: the session on the frame is
/// what the client must answer with — not necessarily its own current one.
#[test]
fn parse_frame_carries_the_prompts_own_session() {
    let data = r#"{"question":"Proceed?","default":"no","session":"a-different-session"}"#;
    let Some(StreamEvent::Prompt { session, .. }) = parse_frame("prompt", data) else {
        panic!("expected a Prompt event");
    };
    assert_eq!(session, "a-different-session");
}
