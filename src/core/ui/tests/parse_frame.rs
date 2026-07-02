#![allow(missing_docs)]
use jan_klod_ui::{parse_frame, StreamEvent};

#[test]
fn parse_frame_maps_each_event_kind() {
    assert_eq!(parse_frame("delta", r#"{"text":"hi"}"#), StreamEvent::Delta("hi".into()));
    assert_eq!(parse_frame("tool", r#"{"name":"bash","id":"1"}"#), StreamEvent::Tool("bash".into()));
    assert_eq!(
        parse_frame("warning", r#"{"message":"falling back"}"#),
        StreamEvent::Warning("falling back".into())
    );
    assert_eq!(
        parse_frame("done", r#"{"answer":"result","agentic":true}"#),
        StreamEvent::Done("result".into())
    );
    assert_eq!(parse_frame("error", r#"{"error":"boom"}"#), StreamEvent::Error("boom".into()));
}

#[test]
fn parse_frame_flags_unknown_kinds() {
    assert!(matches!(parse_frame("weird", "{}"), StreamEvent::Error(_)));
}

#[test]
fn parse_frame_surfaces_malformed_json_as_error() {
    let ev = parse_frame("delta", "not json at all");
    assert!(matches!(ev, StreamEvent::Error(_)), "expected Error, got {ev:?}");
    if let StreamEvent::Error(msg) = ev {
        assert!(msg.contains("malformed SSE frame"), "{msg}");
    }
}
