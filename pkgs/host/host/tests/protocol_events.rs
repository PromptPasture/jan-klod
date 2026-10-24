//! Every turn event has a place in the client protocol. `notification_for` has no
//! wildcard arm, so a new `Event` variant fails to compile here. This asserts the
//! mapping in `jk_host::rpc`, not a copy that could drift from what stdio sends.

use jk_host::rpc::notification_for;
use jk_host::serve;
use jk_protocol::turn::Event;
use jk_protocol::turn::{ToolCall, ToolOutcome, UserPrompt};
use jk_protocol::Notification;

/// One event per variant, with distinguishable fields so misplaced values show as mismatches.
fn every_event() -> Vec<Event> {
    vec![
        Event::TextDelta("the answer is".to_owned()),
        Event::ToolInvoked(ToolCall {
            id: "call-1".to_owned(),
            name: "fs.read".to_owned(),
            arguments: r#"{"path":"README.md"}"#.to_owned(),
        }),
        Event::ToolResult(ToolOutcome {
            tool_call_id: "call-1".to_owned(),
            content: "# Jan-Klod".to_owned(),
            failed: true,
        }),
        Event::Warning("provider fell back".to_owned()),
        Event::Done {
            text: "42".to_owned(),
            agentic: true,
        },
    ]
}

#[test]
fn every_event_maps_to_a_notification() {
    for event in every_event() {
        // Catches a plausible but wrong notification.
        let expected = match &event {
            Event::TextDelta(_) => "TextDelta",
            Event::ToolInvoked(_) => "ToolInvoked",
            Event::ToolResult(_) => "ToolResult",
            Event::Warning(_) => "Warning",
            Event::Done { .. } => "Done",
        };
        let got = notification_for(&event);
        let name = match got {
            Notification::TextDelta { .. } => "TextDelta",
            Notification::ToolInvoked { .. } => "ToolInvoked",
            Notification::ToolResult { .. } => "ToolResult",
            Notification::Warning { .. } => "Warning",
            Notification::Done { .. } => "Done",
            Notification::Ask { .. } => "Ask",
            Notification::Error { .. } => "Error",
            Notification::SessionUpdated { .. } => "SessionUpdated",
            Notification::SurfaceContributions { .. } => "SurfaceContributions",
        };
        assert_eq!(name, expected, "mapping of {event:?}");
    }
}

/// The mapping is lossless: everything an event carries survives.
#[test]
fn nothing_an_event_carries_is_dropped() {
    assert_eq!(
        notification_for(&Event::TextDelta("chunk".to_owned())),
        Notification::TextDelta {
            text: "chunk".to_owned()
        }
    );
    assert_eq!(
        notification_for(&Event::ToolInvoked(ToolCall {
            id: "call-1".to_owned(),
            name: "fs.read".to_owned(),
            arguments: r#"{"path":"README.md"}"#.to_owned(),
        })),
        Notification::ToolInvoked {
            id: "call-1".to_owned(),
            name: "fs.read".to_owned(),
            arguments: r#"{"path":"README.md"}"#.to_owned(),
        },
        "the call's arguments must survive"
    );
    assert_eq!(
        notification_for(&Event::ToolResult(ToolOutcome {
            tool_call_id: "call-1".to_owned(),
            content: "# Jan-Klod".to_owned(),
            failed: true,
        })),
        Notification::ToolResult {
            id: "call-1".to_owned(),
            content: "# Jan-Klod".to_owned(),
            failed: true,
        },
        "whether the call failed (#162) must survive"
    );
    assert_eq!(
        notification_for(&Event::Warning("fell back".to_owned())),
        Notification::Warning {
            message: "fell back".to_owned()
        }
    );
    assert_eq!(
        notification_for(&Event::Done {
            text: "42".to_owned(),
            agentic: true,
        }),
        Notification::Done {
            answer: "42".to_owned(),
            agentic: true,
        },
        "`Event::Done.text` is the final answer, so it maps to `answer`"
    );
}

/// Some notifications come from no event: `ask` from `Driver::ask`, `error` from failed
/// turns, and `session/updated` from the store. None of them passes through `EventSink`.
#[test]
fn the_notifications_without_an_event_are_accounted_for() {
    let without_events = every_event()
        .iter()
        .map(notification_for)
        .filter(|n| {
            matches!(
                n,
                Notification::Ask { .. }
                    | Notification::Error { .. }
                    | Notification::SessionUpdated { .. }
                    | Notification::SurfaceContributions { .. }
            )
        })
        .count();
    assert_eq!(
        without_events, 0,
        "no conductor event may map to `ask`, `error`, `session/updated` or \
         `surface/contributions`"
    );
}

// SSE frames are a projection of the protocol: every key a frame carries must reach
// its notification with an equal value.

/// Frame keys the protocol spells differently: the `error` frame's `error` is `message`, to match `warning`.
const RENAMES: &[(&str, &str)] = &[("error", "message")];

/// Assert every key `data` carries survives into `notification`.
fn assert_frame_fits(kind: &str, data: &serde_json::Value, notification: &Notification) {
    let envelope = serde_json::to_value(notification).expect("a notification serializes");
    let params = envelope
        .get("params")
        .expect("every notification carries params");
    let frame = data.as_object().expect("frame data is a JSON object");
    assert!(!frame.is_empty(), "the `{kind}` frame carries nothing");
    for (key, value) in frame {
        let target = RENAMES
            .iter()
            .find_map(|(from, to)| (from == key).then_some(*to))
            .unwrap_or(key.as_str());
        assert_eq!(
            params.get(target),
            Some(value),
            "the SSE `{kind}` frame's `{key}` does not reach the notification as `{target}`"
        );
    }
}

#[test]
fn every_turn_event_frame_fits_its_notification() {
    for event in every_event() {
        let (kind, data) = serve::sse_frame(&event);
        assert_frame_fits(kind, &data, &notification_for(&event));
    }
}

/// `tool` and `tool-result` carry exactly the notification's fields. This is stricter than
/// `assert_frame_fits`, and limited to these kinds because the others carry transport fields.
#[test]
fn tool_invocation_frames_and_notifications_carry_exactly_the_same_fields() {
    for event in every_event() {
        let (kind, data) = serve::sse_frame(&event);
        if kind != "tool" && kind != "tool-result" {
            continue;
        }
        let envelope = serde_json::to_value(notification_for(&event)).expect("serializes");
        let params = envelope["params"]
            .as_object()
            .expect("every notification carries params");
        let frame = data.as_object().expect("frame data is a JSON object");
        assert_eq!(
            frame.len(),
            params.len(),
            "`{kind}` and its notification disagree on field count: {frame:?} vs {params:?}"
        );
    }
}

/// The `prompt` frame carries `session`, which the client needs to answer it.
#[test]
fn the_prompt_frame_fits_the_ask_notification() {
    let prompt = UserPrompt {
        question: "Run `rm -rf /`?".to_owned(),
        options: vec!["yes".to_owned(), "no".to_owned()],
        default_answer: "no".to_owned(),
    };
    let (kind, data) = serve::prompt_frame(&prompt, "s1");
    assert_frame_fits(
        kind,
        &data,
        &Notification::Ask {
            session: "s1".to_owned(),
            question: prompt.question.clone(),
            options: prompt.options.clone(),
            default: prompt.default_answer.clone(),
        },
    );
}

#[test]
fn the_error_frame_fits_the_error_notification() {
    let (kind, data) = serve::error_frame("the provider could not be reached");
    assert_frame_fits(
        kind,
        &data,
        &Notification::Error {
            message: "the provider could not be reached".to_owned(),
        },
    );
}

/// The shared `SSE_FRAME_KINDS` constant must list exactly the frame kinds the core emits.
/// The client's check (`ui/tests/parse_frame.rs`) reads the same constant.
#[test]
fn the_projected_frame_kinds_are_the_ones_the_protocol_covers() {
    let mut kinds: Vec<&str> = every_event()
        .iter()
        .map(|event| serve::sse_frame(event).0)
        .collect();
    kinds.push(
        serve::prompt_frame(
            &UserPrompt {
                question: String::new(),
                options: Vec::new(),
                default_answer: String::new(),
            },
            "s",
        )
        .0,
    );
    kinds.push(serve::error_frame("").0);
    kinds.sort_unstable();
    assert_eq!(
        kinds,
        jk_protocol::SSE_FRAME_KINDS,
        "the shared list and what the core actually emits have diverged"
    );
}
