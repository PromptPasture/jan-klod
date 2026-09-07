#![allow(missing_docs)]
use jan_klod::{parse_frame, StreamEvent};

#[test]
fn parse_frame_maps_each_event_kind() {
    assert_eq!(
        parse_frame("delta", r#"{"text":"hi"}"#),
        StreamEvent::Delta("hi".into())
    );
    assert_eq!(
        parse_frame("tool", r#"{"name":"bash","id":"1"}"#),
        StreamEvent::Tool("bash".into())
    );
    assert_eq!(
        parse_frame("warning", r#"{"message":"falling back"}"#),
        StreamEvent::Warning("falling back".into())
    );
    assert_eq!(
        parse_frame("done", r#"{"answer":"result","agentic":true}"#),
        StreamEvent::Done("result".into())
    );
    assert_eq!(
        parse_frame("error", r#"{"error":"boom"}"#),
        StreamEvent::Error("boom".into())
    );
}

#[test]
fn parse_frame_flags_unknown_kinds() {
    assert!(matches!(parse_frame("weird", "{}"), StreamEvent::Error(_)));
}

#[test]
fn parse_frame_surfaces_malformed_json_as_error() {
    let ev = parse_frame("delta", "not json at all");
    assert!(
        matches!(ev, StreamEvent::Error(_)),
        "expected Error, got {ev:?}"
    );
    if let StreamEvent::Error(msg) = ev {
        assert!(msg.contains("malformed SSE frame"), "{msg}");
    }
}

#[test]
fn parse_frame_reads_a_prompt_with_its_options() {
    let data = r#"{"question":"Allow tool `bash`?","options":["yes","no","always","never"],"default":"no","session":"s1"}"#;
    assert_eq!(
        parse_frame("prompt", data),
        StreamEvent::Prompt {
            question: "Allow tool `bash`?".into(),
            options: vec!["yes".into(), "no".into(), "always".into(), "never".into()],
            default: "no".into(),
        }
    );
}

#[test]
fn a_prompt_without_options_still_parses() {
    // A future/odd prompt must not turn into an Error the user cannot answer.
    assert_eq!(
        parse_frame("prompt", r#"{"question":"Proceed?","default":"no"}"#),
        StreamEvent::Prompt {
            question: "Proceed?".into(),
            options: vec![],
            default: "no".into()
        }
    );
}
