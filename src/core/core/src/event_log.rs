//! The stored form of a turn's events.
//!
//! [`crate::store`] holds rows and never reads their payloads; this module owns
//! what goes in them. One [`Event`] becomes one row: a `kind` discriminator and
//! a versioned JSON envelope.
//!
//! # Why this has its own version
//!
//! [`EVENT_LOG_VERSION`] is not the client protocol's version, and the two must
//! not be merged. A stored log outlives the clients that watched it happen: a
//! database written a year ago has to be readable by a build whose protocol has
//! moved on twice, so the log's compatibility question ("can I read this row?")
//! is a different question from the protocol's ("can this client talk to this
//! core?"). Sharing one number would make every client-facing rename a reason
//! to migrate history.
//!
//! The two representations also happen to look alike — `jan-klod-protocol`'s
//! notifications mirror [`Event`] one for one — and reusing those types here
//! would reintroduce exactly the coupling the separate version exists to avoid.
//!
//! # Scope
//!
//! Only [`Event`] is covered. A turn also has records that never pass through
//! an `EventSink` — the user message that started it, an `ask` and its answer —
//! and those need their own kinds and a wider decode result than [`decode`]'s
//! `Event`.

use serde_json::{json, Value};

use crate::conductor::Event;
use crate::intercept::{ToolCall, ToolOutcome};

/// The envelope format this build writes, and the highest it can read.
///
/// Bump when a payload's shape changes in a way an older reader would
/// misinterpret. Adding a new `kind` is not such a change: an old reader
/// already has to cope with a kind it does not know.
pub const EVENT_LOG_VERSION: u32 = 1;

/// Why a stored row could not be turned back into an [`Event`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DecodeError {
    /// A `kind` this build does not know. Also what a non-event record reads as
    /// — see the module docs.
    #[error("unknown event kind `{0}`")]
    UnknownKind(String),
    /// Written by a newer build, in a shape this one cannot be trusted to read.
    #[error("event log version {found} is newer than this build's {EVENT_LOG_VERSION}")]
    UnsupportedVersion {
        /// The version the row carries.
        found: u32,
    },
    /// The payload is not an envelope, or a field is missing or the wrong type.
    #[error("malformed payload: {0}")]
    Malformed(String),
}

/// The row one event becomes: its `kind` and its JSON envelope.
///
/// The `match` is exhaustive with no wildcard arm, so a new [`Event`] variant
/// stops this compiling rather than being logged as nothing — the failure a
/// silent `_ => return` would produce is a hole in the record that only shows
/// up when someone replays a session and finds a step missing.
#[must_use]
pub fn encode(event: &Event) -> (&'static str, String) {
    let (kind, data) = match event {
        Event::TextDelta(text) => ("text-delta", json!({ "text": text })),
        Event::ToolInvoked(call) => (
            "tool-invoked",
            json!({ "id": call.id, "name": call.name, "arguments": call.arguments }),
        ),
        Event::ToolResult(outcome) => (
            "tool-result",
            json!({ "id": outcome.tool_call_id, "content": outcome.content }),
        ),
        Event::Warning(message) => ("warning", json!({ "message": message })),
        Event::Done { text, agentic } => ("done", json!({ "answer": text, "agentic": agentic })),
    };
    (
        kind,
        json!({ "v": EVENT_LOG_VERSION, "data": data }).to_string(),
    )
}

/// Rebuild the event a row was written from.
///
/// # Errors
/// [`DecodeError`] when the kind is unknown, the version is newer than this
/// build's, or a field is missing or of the wrong type.
pub fn decode(kind: &str, payload: &str) -> Result<Event, DecodeError> {
    let envelope: Value =
        serde_json::from_str(payload).map_err(|e| DecodeError::Malformed(e.to_string()))?;
    let version = envelope
        .get("v")
        .and_then(Value::as_u64)
        .ok_or_else(|| DecodeError::Malformed("no `v` in the envelope".to_owned()))?;
    let version = u32::try_from(version).unwrap_or(u32::MAX);
    if version > EVENT_LOG_VERSION {
        return Err(DecodeError::UnsupportedVersion { found: version });
    }
    let data = envelope
        .get("data")
        .ok_or_else(|| DecodeError::Malformed("no `data` in the envelope".to_owned()))?;
    let text = |field: &str| -> Result<String, DecodeError> {
        data.get(field)
            .and_then(Value::as_str)
            .map(str::to_owned)
            .ok_or_else(|| DecodeError::Malformed(format!("`{kind}` has no string `{field}`")))
    };
    match kind {
        "text-delta" => Ok(Event::TextDelta(text("text")?)),
        "tool-invoked" => Ok(Event::ToolInvoked(ToolCall {
            id: text("id")?,
            name: text("name")?,
            arguments: text("arguments")?,
        })),
        "tool-result" => Ok(Event::ToolResult(ToolOutcome {
            tool_call_id: text("id")?,
            content: text("content")?,
        })),
        "warning" => Ok(Event::Warning(text("message")?)),
        "done" => Ok(Event::Done {
            text: text("answer")?,
            agentic: data
                .get("agentic")
                .and_then(Value::as_bool)
                .ok_or_else(|| DecodeError::Malformed("`done` has no bool `agentic`".to_owned()))?,
        }),
        other => Err(DecodeError::UnknownKind(other.to_owned())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The kind each event must encode to.
    ///
    /// Exhaustive with no wildcard arm, and that is the point: [`encode`]'s own
    /// match already forces a new variant to be given a kind, but nothing would
    /// force it into [`every_event`] below — and `decode`'s match on `kind` *does*
    /// have a catch-all, so a variant added to `encode` alone would write rows
    /// that fail to decode at replay time. Requiring an arm here means adding a
    /// variant breaks this file until a sample exists, and the round-trip test
    /// then fails until `decode` handles it too.
    const fn expected_kind(event: &Event) -> &'static str {
        match event {
            Event::TextDelta(_) => "text-delta",
            Event::ToolInvoked(_) => "tool-invoked",
            Event::ToolResult(_) => "tool-result",
            Event::Warning(_) => "warning",
            Event::Done { .. } => "done",
        }
    }

    /// One of each variant, every field distinguishable so a field decoded into
    /// the wrong place fails rather than matching by coincidence.
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
    fn every_event_round_trips() {
        for event in every_event() {
            let (kind, payload) = encode(&event);
            let back =
                decode(kind, &payload).unwrap_or_else(|e| panic!("decoding `{kind}` failed: {e}"));
            assert_eq!(back, event, "round-trip of `{kind}`: {payload}");
        }
    }

    #[test]
    fn every_event_encodes_to_its_documented_kind() {
        for event in every_event() {
            assert_eq!(encode(&event).0, expected_kind(&event), "kind of {event:?}");
        }
    }

    #[test]
    fn each_event_has_its_own_kind() {
        let mut kinds: Vec<&str> = every_event().iter().map(|e| encode(e).0).collect();
        let total = kinds.len();
        kinds.sort_unstable();
        kinds.dedup();
        assert_eq!(kinds.len(), total, "two events share a kind: {kinds:?}");
    }

    #[test]
    fn every_payload_carries_the_version() {
        for event in every_event() {
            let (_, payload) = encode(&event);
            let envelope: Value = serde_json::from_str(&payload).unwrap();
            assert_eq!(envelope["v"], json!(EVENT_LOG_VERSION));
            assert!(envelope.get("data").is_some(), "{payload}");
        }
    }

    /// Acceptance line 2: a replaying client needs the deltas as they came, so
    /// two deltas are two rows and neither is merged into the other.
    #[test]
    fn text_deltas_are_not_coalesced() {
        let first = encode(&Event::TextDelta("the ".to_owned()));
        let second = encode(&Event::TextDelta("answer".to_owned()));
        assert_ne!(first.1, second.1);
        assert_eq!(
            decode(first.0, &first.1).unwrap(),
            Event::TextDelta("the ".to_owned()),
            "each delta decodes to exactly what was emitted, not to a join of them"
        );
    }

    /// The version's whole purpose: refuse a row this build cannot be trusted
    /// to read, rather than reading it wrongly.
    #[test]
    fn a_newer_version_is_refused_rather_than_guessed_at() {
        let payload = json!({ "v": EVENT_LOG_VERSION + 1, "data": { "text": "x" } }).to_string();
        assert_eq!(
            decode("text-delta", &payload),
            Err(DecodeError::UnsupportedVersion {
                found: EVENT_LOG_VERSION + 1
            })
        );
    }

    /// An older envelope stays readable — that is the other half of versioning,
    /// and the half that is easy to break by tightening the check to `!=`.
    #[test]
    fn the_current_version_is_accepted() {
        let payload = json!({ "v": EVENT_LOG_VERSION, "data": { "message": "hi" } }).to_string();
        assert_eq!(
            decode("warning", &payload).unwrap(),
            Event::Warning("hi".to_owned())
        );
    }

    #[test]
    fn an_unknown_kind_names_itself() {
        let payload = json!({ "v": EVENT_LOG_VERSION, "data": {} }).to_string();
        assert_eq!(
            decode("user-message", &payload),
            Err(DecodeError::UnknownKind("user-message".to_owned())),
            "a record kind this build does not know must say which"
        );
    }

    #[test]
    fn a_missing_field_is_malformed_rather_than_a_default() {
        let payload = json!({ "v": EVENT_LOG_VERSION, "data": { "answer": "42" } }).to_string();
        let error = decode("done", &payload).unwrap_err();
        assert!(
            matches!(error, DecodeError::Malformed(ref m) if m.contains("agentic")),
            "a missing `agentic` must not silently read as false: {error}"
        );
    }

    #[test]
    fn a_payload_that_is_not_an_envelope_is_malformed() {
        assert!(matches!(
            decode("text-delta", "not json"),
            Err(DecodeError::Malformed(_))
        ));
        assert!(matches!(
            decode("text-delta", r#"{"data":{"text":"x"}}"#),
            Err(DecodeError::Malformed(_))
        ));
    }
}
