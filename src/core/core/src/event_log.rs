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

use std::sync::Mutex;

use serde_json::{json, Value};

use crate::conductor::{Event, EventSink, Flow};
use crate::intercept::{ToolCall, ToolOutcome};
use crate::store::{Store, StoreError};

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

/// An [`EventSink`] that appends every event to a session's log, then forwards
/// it to the sink that was already there.
///
/// A fan-out rather than a replacement: the SSE stream and the TUI transcript
/// still get every event, and the log is a third reader rather than a new owner
/// of the stream.
///
/// Two rules it follows, both taken from how the transcript append already
/// behaves in `run_and_persist`:
///
/// * **A store failure never affects the turn.** [`Self::emit`] returns whatever
///   the inner sink returned, always. Cancelling a turn because its *log* could
///   not be written would let a full disk stop a conversation.
/// * **The lock is taken per append and released before forwarding.** The core
///   shares one `Mutex<Store>` with every interceptor's `host-storage`, so
///   holding it across the inner sink's work — which can run arbitrary guest
///   code — would deadlock the first guest that remembered anything.
pub struct PersistingSink<'a> {
    inner: &'a mut dyn EventSink,
    store: &'a Mutex<Store>,
    session: String,
    /// Whether a failure has already been reported. One warning per turn, not
    /// one per event: whatever breaks the store usually breaks it for every
    /// event, and a text-delta storm would bury the notice in copies of itself.
    warned: bool,
}

impl<'a> PersistingSink<'a> {
    /// Wrap `inner`, logging each event against `session`.
    pub fn new(inner: &'a mut dyn EventSink, store: &'a Mutex<Store>, session: &str) -> Self {
        Self {
            inner,
            store,
            session: session.to_string(),
            warned: false,
        }
    }

    /// Append one event, reporting the first failure and swallowing the rest.
    fn append(&mut self, event: &Event) {
        let (kind, payload) = encode(event);
        // Scoped so the guard is dropped before `emit` forwards.
        let outcome = match self.store.lock() {
            Ok(store) => store
                .append_event(&self.session, kind, &payload)
                .map(|_| ()),
            Err(_) => Err(StoreError::Backend {
                detail: "the store lock is poisoned".to_owned(),
            }),
        };
        if let Err(err) = outcome {
            if !self.warned {
                self.warned = true;
                eprintln!(
                    "WARN [core] logging events for session {} failed: {err}. The turn \
                     continues; further failures this turn are not repeated.",
                    self.session
                );
            }
        }
    }
}

impl EventSink for PersistingSink<'_> {
    fn emit(&mut self, event: &Event) -> Flow {
        // Logged before forwarding, so an event still reaches the record when
        // the inner sink answers `Stop` — a client disconnecting does not make
        // what already happened un-happen.
        self.append(event);
        self.inner.emit(event)
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

#[cfg(test)]
mod sink_tests {
    use super::*;

    /// Records what it was given, and can be told to cancel.
    struct Recorder {
        seen: Vec<Event>,
        answer: Flow,
    }

    impl Recorder {
        const fn new(answer: Flow) -> Self {
            Self {
                seen: Vec::new(),
                answer,
            }
        }
    }

    impl EventSink for Recorder {
        fn emit(&mut self, event: &Event) -> Flow {
            self.seen.push(event.clone());
            self.answer
        }
    }

    fn sample() -> Vec<Event> {
        vec![
            Event::TextDelta("a".to_owned()),
            Event::Warning("b".to_owned()),
            Event::Done {
                text: "c".to_owned(),
                agentic: false,
            },
        ]
    }

    #[test]
    fn every_event_reaches_both_the_log_and_the_inner_sink() {
        let store = Mutex::new(Store::open_in_memory().unwrap());
        let mut inner = Recorder::new(Flow::Continue);
        {
            let mut sink = PersistingSink::new(&mut inner, &store, "s1");
            for event in sample() {
                assert_eq!(sink.emit(&event), Flow::Continue);
            }
        }

        assert_eq!(inner.seen, sample(), "the inner sink still sees everything");

        let logged = store.lock().unwrap().session_events("s1").unwrap();
        assert_eq!(logged.len(), 3);
        assert_eq!(
            logged.iter().map(|e| e.seq).collect::<Vec<_>>(),
            vec![1, 2, 3],
            "in the order they were emitted"
        );
        let decoded: Vec<Event> = logged
            .iter()
            .map(|row| decode(&row.kind, &row.payload).unwrap())
            .collect();
        assert_eq!(
            decoded,
            sample(),
            "and they decode back to what was emitted"
        );
    }

    /// The inner sink owns cancellation. A `Stop` from it — a disconnected SSE
    /// client — must still reach the conductor through the wrapper.
    #[test]
    fn the_inner_sinks_stop_still_propagates() {
        let store = Mutex::new(Store::open_in_memory().unwrap());
        let mut inner = Recorder::new(Flow::Stop);
        let mut sink = PersistingSink::new(&mut inner, &store, "s1");
        assert_eq!(sink.emit(&Event::TextDelta("x".to_owned())), Flow::Stop);
    }

    /// And the event is logged even then: the client going away does not make
    /// what already happened un-happen.
    #[test]
    fn an_event_is_logged_even_when_the_inner_sink_cancels() {
        let store = Mutex::new(Store::open_in_memory().unwrap());
        let mut inner = Recorder::new(Flow::Stop);
        {
            let mut sink = PersistingSink::new(&mut inner, &store, "s1");
            sink.emit(&Event::TextDelta("x".to_owned()));
        }
        assert_eq!(store.lock().unwrap().session_events("s1").unwrap().len(), 1);
    }

    /// A store that cannot be reached must not cancel the turn. A poisoned lock
    /// is the reachable version of "the store is broken" — a real SQL failure
    /// needs the disk to fail, which a unit test cannot arrange — and it
    /// exercises the same swallow-and-continue path.
    #[test]
    fn a_broken_store_does_not_cancel_the_turn() {
        let store = Mutex::new(Store::open_in_memory().unwrap());
        std::thread::scope(|scope| {
            let handle = scope.spawn(|| {
                let _guard = store.lock().unwrap();
                panic!("poisoning the lock on purpose");
            });
            assert!(handle.join().is_err(), "the thread panicked as intended");
        });
        assert!(store.lock().is_err(), "the lock is poisoned");

        let mut inner = Recorder::new(Flow::Continue);
        let mut sink = PersistingSink::new(&mut inner, &store, "s1");
        for event in sample() {
            assert_eq!(
                sink.emit(&event),
                Flow::Continue,
                "a turn is not cancelled because its log could not be written"
            );
        }
        assert_eq!(inner.seen.len(), 3, "and the inner sink is unaffected");
    }

    /// Two sessions logged through two wrappers keep separate sequences, which
    /// is what makes a session's log readable as a sequence.
    #[test]
    fn sessions_do_not_share_a_sequence() {
        let store = Mutex::new(Store::open_in_memory().unwrap());
        let mut inner = Recorder::new(Flow::Continue);
        {
            let mut first = PersistingSink::new(&mut inner, &store, "a");
            first.emit(&Event::TextDelta("1".to_owned()));
        }
        {
            let mut second = PersistingSink::new(&mut inner, &store, "b");
            second.emit(&Event::TextDelta("2".to_owned()));
        }
        let guard = store.lock().unwrap();
        assert_eq!(guard.session_events("a").unwrap()[0].seq, 1);
        assert_eq!(guard.session_events("b").unwrap()[0].seq, 1);
    }
}
