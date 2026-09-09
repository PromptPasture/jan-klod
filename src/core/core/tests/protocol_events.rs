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
use jan_klod_core::intercept::{ToolCall, ToolOutcome};
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

/// Two notifications answer to no event, and that is not an oversight: `ask`
/// comes from `intercept::Driver::ask` blocking a turn, and `session/updated`
/// from the store, neither of which passes through `EventSink`.
#[test]
fn the_two_notifications_without_an_event_are_accounted_for() {
    let without_events = every_event()
        .iter()
        .map(notification_for)
        .filter(|n| {
            matches!(
                n,
                Notification::Ask { .. } | Notification::SessionUpdated { .. }
            )
        })
        .count();
    assert_eq!(
        without_events, 0,
        "no conductor event may map to `ask` or `session/updated`"
    );
}
