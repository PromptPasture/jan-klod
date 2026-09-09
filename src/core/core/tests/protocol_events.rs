//! Every turn event the conductor emits has a place in the client protocol.
//!
//! Without this, adding a `conductor::Event` variant is a silent hole: the
//! event reaches the SSE sink and never reaches a protocol client, and nothing
//! fails. The mapping below has no wildcard arm, so a new variant stops this
//! file from compiling — which is the whole point of it existing.
//!
//! The mapping lives in the test rather than in either crate because this
//! slice adds no transport: `serve.rs` is not rewired, so the core has no
//! runtime reason to hold a protocol type yet (#41, "No transport in this
//! slice"). Slice 13b is where a real `From` impl belongs, and it can lift this
//! function verbatim.

use jan_klod_core::conductor::Event;
use jan_klod_core::intercept::{ToolCall, ToolOutcome, UserPrompt};
use jan_klod_core::serve;
use jan_klod_protocol::Notification;

/// The notification each event becomes.
///
/// Exhaustive by construction — no `_ =>` arm.
fn notification_for(event: &Event) -> Notification {
    match event {
        Event::TextDelta(text) => Notification::TextDelta { text: text.clone() },
        Event::ToolInvoked(call) => Notification::ToolInvoked {
            id: call.id.clone(),
            name: call.name.clone(),
            arguments: call.arguments.clone(),
        },
        Event::ToolResult(outcome) => Notification::ToolResult {
            id: outcome.tool_call_id.clone(),
            content: outcome.content.clone(),
        },
        Event::Warning(message) => Notification::Warning {
            message: message.clone(),
        },
        Event::Done { text, agentic } => Notification::Done {
            answer: text.clone(),
            agentic: *agentic,
        },
    }
}

/// One event of each variant, every field distinguishable, so a field mapped to
/// the wrong place shows up as a mismatch rather than as two equal strings.
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
        // Compares against the variant this event must become, so a mapping
        // that returns a plausible-but-wrong notification fails here.
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
        };
        assert_eq!(name, expected, "mapping of {event:?}");
    }
}

/// The mapping must be lossless: everything an event carries has to survive.
/// `arguments` is the one to watch — the SSE projection drops it, so a
/// notification built by copying that projection would lose it too.
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
        "the call's arguments must survive, unlike in the SSE `tool` frame"
    );
    assert_eq!(
        notification_for(&Event::ToolResult(ToolOutcome {
            tool_call_id: "call-1".to_owned(),
            content: "# Jan-Klod".to_owned(),
        })),
        Notification::ToolResult {
            id: "call-1".to_owned(),
            content: "# Jan-Klod".to_owned(),
        }
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

/// Three notifications answer to no event, and that is not an oversight: `ask`
/// comes from `intercept::Driver::ask` blocking a turn, `error` from a failed
/// turn or an unservable request, and `session/updated` from the store — none
/// of which passes through `EventSink`.
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
            )
        })
        .count();
    assert_eq!(
        without_events, 0,
        "no conductor event may map to `ask`, `error` or `session/updated`"
    );
}

// ─── SSE compatibility ───────────────────────────────────────────────────────
//
// REST + SSE becomes one projection of the protocol rather than a second
// contract, which only holds if the projection says nothing the protocol cannot
// say. So: every key an SSE frame carries must reach the notification with an
// equal value. The reverse is deliberately not asserted — a notification may
// carry more, and `tool-invoked` does, since the SSE `tool` frame drops the
// call's arguments.

/// A frame's payload keys the protocol spells differently.
///
/// One today: the `error` frame's key is `error`, which the protocol calls
/// `message` to match `warning`. Consistency inside the contract is worth more
/// than agreeing with one legacy key, and the rename is recorded here rather
/// than tolerated by a loose assertion.
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

/// The `prompt` frame carries `session`, because the client answers it over a
/// separate request. The notification has to carry it too or the answer has
/// nothing to name — this is what the test caught.
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

/// The frames above are the whole SSE surface. If a new one appears, this list
/// is what a later reader checks against `serve.rs` — and the keepalive is a
/// `:` comment rather than a frame precisely so it needs no notification.
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
        vec![
            "delta",
            "done",
            "error",
            "prompt",
            "tool",
            "tool-result",
            "warning"
        ]
    );
}
