//! The stored form of a turn's events: one [`Event`] becomes one row, a `kind` plus a versioned JSON envelope.

use std::sync::Mutex;

use serde_json::{json, Value};

use crate::store::{Store, StoreError};
use jk_protocol::turn::{Driver, Event, EventSink, Flow, ToolCall, ToolOutcome, UserPrompt};

/// The envelope version written, and the highest readable; bump only when an older reader would misread.
pub const EVENT_LOG_VERSION: u32 = 1;

/// Why a stored row could not be decoded.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DecodeError {
    /// A `kind` unknown to this build, likely written by a newer one.
    #[error("unknown event kind `{0}`")]
    UnknownKind(String),
    /// A known kind that is a record, not an event, asked of [`decode`].
    #[error("`{0}` is a record, not an event")]
    NotAnEvent(String),
    /// Written by a newer build; unreadable here.
    #[error("event log version {found} is newer than this build's {EVENT_LOG_VERSION}")]
    UnsupportedVersion {
        /// The version the row carries.
        found: u32,
    },
    /// Not an envelope, or a field is missing or mistyped.
    #[error("malformed payload: {0}")]
    Malformed(String),
}

/// The `kind` of the message that started the turn.
pub const KIND_USER_MESSAGE: &str = "user-message";
/// The `kind` of the prompt an interceptor blocked the turn on.
pub const KIND_ASK: &str = "ask";
/// The `kind` of the answer that unblocked it, including a default.
pub const KIND_ANSWER: &str = "answer";
/// The `kind` of a steering message injected mid-turn.
pub const KIND_FOLLOW_UP: &str = "follow-up";

/// The `kind` of a component adopted into the runtime or refused; a record, as loads happen between turns.
pub const KIND_EXTENSION_LOADED: &str = "extension-loaded";

/// Wraps `data` in the versioned envelope every row shares.
#[must_use]
pub fn envelope(data: &Value) -> String {
    json!({ "v": EVENT_LOG_VERSION, "data": data }).to_string()
}

/// The `kind` and JSON envelope of one event; no wildcard arm, so a new variant needs a kind.
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
            json!({
                "id": outcome.tool_call_id,
                "content": outcome.content,
                "failed": outcome.failed,
            }),
        ),
        Event::Warning(message) => ("warning", json!({ "message": message })),
        Event::Done { text, agentic } => ("done", json!({ "answer": text, "agentic": agentic })),
    };
    (kind, envelope(&data))
}

/// Anything a row can hold: a turn event, or a record that never passed through an `EventSink`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Record {
    /// The message that started the turn.
    UserMessage(String),
    /// A prompt an interceptor blocked the turn on.
    Ask {
        /// The question put to the user.
        question: String,
        /// Empty means free text; non-empty means choose one.
        options: Vec<String>,
        /// What was taken if nobody answered.
        default: String,
    },
    /// The answer that unblocked the turn, the user's or the default.
    Answer(String),
    /// A steering message injected mid-turn.
    FollowUp(String),
    /// A component adopted after boot, or the reason it was refused; refusals are logged too.
    ExtensionLoaded {
        /// The component stem, e.g. `tool-greet`.
        stem: String,
        /// Whether it is now part of the runtime.
        loaded: bool,
        /// The instance id when loaded, or why it was refused.
        detail: String,
    },
    /// One of the conductor's turn events.
    Event(Event),
}

fn open_envelope(payload: &str) -> Result<Value, DecodeError> {
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
    envelope
        .get("data")
        .cloned()
        .ok_or_else(|| DecodeError::Malformed("no `data` in the envelope".to_owned()))
}

fn text(kind: &str, data: &Value, field: &str) -> Result<String, DecodeError> {
    data.get(field)
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| DecodeError::Malformed(format!("`{kind}` has no string `{field}`")))
}

/// A non-string entry is an error, not skipped, so a prompt is never answered against unseen options.
fn strings(kind: &str, data: &Value, field: &str) -> Result<Vec<String>, DecodeError> {
    data.get(field)
        .and_then(Value::as_array)
        .ok_or_else(|| DecodeError::Malformed(format!("`{kind}` has no list `{field}`")))?
        .iter()
        .map(|item| {
            item.as_str().map(str::to_owned).ok_or_else(|| {
                DecodeError::Malformed(format!("`{kind}`'s `{field}` holds a non-string"))
            })
        })
        .collect()
}

/// Rebuilds whatever a row was written from.
///
/// # Errors
/// [`DecodeError`] when the kind is unknown, the version is newer, or a field is missing or mistyped.
pub fn decode_record(kind: &str, payload: &str) -> Result<Record, DecodeError> {
    let data = open_envelope(payload)?;
    match kind {
        KIND_USER_MESSAGE => Ok(Record::UserMessage(text(kind, &data, "message")?)),
        KIND_ASK => Ok(Record::Ask {
            question: text(kind, &data, "question")?,
            options: strings(kind, &data, "options")?,
            default: text(kind, &data, "default")?,
        }),
        KIND_ANSWER => Ok(Record::Answer(text(kind, &data, "answer")?)),
        KIND_FOLLOW_UP => Ok(Record::FollowUp(text(kind, &data, "message")?)),
        KIND_EXTENSION_LOADED => Ok(Record::ExtensionLoaded {
            stem: text(kind, &data, "stem")?,
            loaded: data.get("loaded").and_then(Value::as_bool).ok_or_else(|| {
                DecodeError::Malformed(format!("`{kind}` has no boolean `loaded`"))
            })?,
            detail: text(kind, &data, "detail")?,
        }),
        "text-delta" => Ok(Record::Event(Event::TextDelta(text(kind, &data, "text")?))),
        "tool-invoked" => Ok(Record::Event(Event::ToolInvoked(ToolCall {
            id: text(kind, &data, "id")?,
            name: text(kind, &data, "name")?,
            arguments: text(kind, &data, "arguments")?,
        }))),
        "tool-result" => Ok(Record::Event(Event::ToolResult(ToolOutcome {
            tool_call_id: text(kind, &data, "id")?,
            content: text(kind, &data, "content")?,
            failed: data.get("failed").and_then(Value::as_bool).unwrap_or(false),
        }))),
        "warning" => Ok(Record::Event(Event::Warning(text(kind, &data, "message")?))),
        "done" => Ok(Record::Event(Event::Done {
            text: text(kind, &data, "answer")?,
            agentic: data
                .get("agentic")
                .and_then(Value::as_bool)
                .ok_or_else(|| DecodeError::Malformed("`done` has no bool `agentic`".to_owned()))?,
        })),
        other => Err(DecodeError::UnknownKind(other.to_owned())),
    }
}

/// Rebuilds the event a row was written from, for callers that only handle events.
///
/// # Errors
/// [`DecodeError::NotAnEvent`] for a non-event record, plus anything [`decode_record`] returns.
pub fn decode(kind: &str, payload: &str) -> Result<Event, DecodeError> {
    match decode_record(kind, payload)? {
        Record::Event(event) => Ok(event),
        _ => Err(DecodeError::NotAnEvent(kind.to_owned())),
    }
}

/// An [`EventSink`] that logs each event, then forwards it; never holds the store lock across the inner sink (deadlock).
pub struct PersistingSink<'a> {
    inner: &'a mut dyn EventSink,
    store: &'a Mutex<Store>,
    session: String,
    warned: bool,
}

impl<'a> PersistingSink<'a> {
    /// Wraps `inner`, logging each event against `session`.
    pub fn new(inner: &'a mut dyn EventSink, store: &'a Mutex<Store>, session: &str) -> Self {
        Self {
            inner,
            store,
            session: session.to_string(),
            warned: false,
        }
    }
}

impl EventSink for PersistingSink<'_> {
    fn emit(&mut self, event: &Event) -> Flow {
        let (kind, payload) = encode(event);
        append_encoded(self.store, &self.session, &mut self.warned, kind, &payload);
        self.inner.emit(event)
    }
}

/// A `Driver` that logs each prompt and its answer, defaults included, then defers to the wrapped driver.
pub struct PersistingDriver<'a> {
    inner: &'a mut dyn Driver,
    store: &'a Mutex<Store>,
    session: String,
    warned: bool,
}

impl<'a> PersistingDriver<'a> {
    /// Wraps `inner`, logging its exchanges against `session`.
    pub fn new(inner: &'a mut dyn Driver, store: &'a Mutex<Store>, session: &str) -> Self {
        Self {
            inner,
            store,
            session: session.to_string(),
            warned: false,
        }
    }
}

impl Driver for PersistingDriver<'_> {
    fn ask(&mut self, prompt: &UserPrompt) -> String {
        append(
            self.store,
            &self.session,
            &mut self.warned,
            KIND_ASK,
            &json!({
                "question": prompt.question,
                "options": prompt.options,
                "default": prompt.default_answer,
            }),
        );
        let answer = self.inner.ask(prompt);
        append(
            self.store,
            &self.session,
            &mut self.warned,
            KIND_ANSWER,
            &json!({ "answer": answer }),
        );
        answer
    }

    fn follow_up(&mut self) -> Option<String> {
        let follow_up = self.inner.follow_up();
        if let Some(message) = &follow_up {
            append(
                self.store,
                &self.session,
                &mut self.warned,
                KIND_FOLLOW_UP,
                &json!({ "message": message }),
            );
        }
        follow_up
    }
}

fn append(store: &Mutex<Store>, session: &str, warned: &mut bool, kind: &str, data: &Value) {
    append_encoded(store, session, warned, kind, &envelope(data));
}

/// Warns once per turn on a store failure, then swallows it; sink and driver share this meaning.
fn append_encoded(
    store: &Mutex<Store>,
    session: &str,
    warned: &mut bool,
    kind: &str,
    payload: &str,
) {
    let outcome = store.lock().map_or_else(
        |_| {
            Err(StoreError::Backend {
                detail: "the store lock is poisoned".to_owned(),
            })
        },
        |store| store.append_event(session, kind, payload).map(|_| ()),
    );
    if let Err(err) = outcome {
        if !*warned {
            *warned = true;
            eprintln!(
                "WARN [core] logging events for session {session} failed: {err}. The turn \
                 continues; further failures this turn are not repeated."
            );
        }
    }
}

/// Whether a chain is intact or broken.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChainStatus {
    /// Every row's `prev_digest` matches the digest of the row before it.
    Intact,
    /// The event was edited, reordered, or a row was deleted.
    Broken {
        /// The `seq` where the chain breaks.
        seq: u64,
        /// The `kind` where the chain breaks.
        kind: String,
        /// Why the chain breaks.
        reason: String,
    },
}

/// Result of verifying a session's event chain.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChainVerification {
    /// The session that was verified.
    pub session: String,
    /// Whether the chain is intact or broken.
    pub status: ChainStatus,
}

/// Error when verifying a chain.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum VerifyError {
    /// The session does not exist.
    #[error("session not found: {0}")]
    NotFound(String),
    /// Backend storage error.
    #[error("storage error: {0}")]
    Backend(String),
}

impl From<StoreError> for VerifyError {
    fn from(err: StoreError) -> Self {
        match err {
            StoreError::NotFound => Self::NotFound("session not found".to_string()),
            StoreError::Backend { detail } => Self::Backend(detail),
        }
    }
}

/// Verifies a session's `prev_digest` chain; the first row's is NULL.
///
/// # Errors
/// [`VerifyError::NotFound`] if the session does not exist, or [`VerifyError::Backend`] on a storage error.
pub fn verify_chain(store: &Store, session: &str) -> Result<ChainVerification, VerifyError> {
    store.verify_chain_internal(session)
}

/// Converts pre-log transcripts into events, two per turn, skipping sessions that already have events.
///
/// # Errors
/// [`StoreError`] if the store cannot be read or written.
pub fn migrate_transcripts(store: &Store) -> Result<u64, StoreError> {
    let mut migrated = 0;
    for session in store.list_namespaces()? {
        if session.contains('/') || !store.session_events(&session)?.is_empty() {
            continue;
        }
        let mut turns: Vec<(u64, crate::store::Entry)> = store
            .recent(&session, u32::MAX)?
            .into_iter()
            .filter_map(|entry| {
                let number = entry.key.strip_prefix("turn-")?.parse::<u64>().ok()?;
                Some((number, entry))
            })
            .collect();
        if turns.is_empty() {
            continue;
        }
        turns.sort_by_key(|(number, _)| *number);

        for (_, entry) in turns {
            let Ok(turn) = serde_json::from_str::<Value>(&entry.value) else {
                continue;
            };
            let ts = entry.created_at;
            if let Some(user) = turn.get("user").and_then(Value::as_str) {
                store.append_event_at(
                    &session,
                    KIND_USER_MESSAGE,
                    &envelope(&json!({ "message": user })),
                    ts,
                )?;
            }
            if let Some(answer) = turn.get("answer").and_then(Value::as_str) {
                let (kind, payload) = encode(&Event::Done {
                    text: answer.to_owned(),
                    agentic: false,
                });
                store.append_event_at(&session, kind, &payload, ts)?;
            }
        }
        migrated += 1;
    }
    Ok(migrated)
}

/// Logs the message that starts a turn; must be what the model received, after any `before-loop` replace.
pub fn log_user_message(store: &Mutex<Store>, session: &str, message: &str) {
    let mut warned = false;
    append(
        store,
        session,
        &mut warned,
        KIND_USER_MESSAGE,
        &json!({ "message": message }),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    const fn expected_kind(event: &Event) -> &'static str {
        match event {
            Event::TextDelta(_) => "text-delta",
            Event::ToolInvoked(_) => "tool-invoked",
            Event::ToolResult(_) => "tool-result",
            Event::Warning(_) => "warning",
            Event::Done { .. } => "done",
        }
    }

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
    fn a_tool_result_written_before_the_failed_field_existed_decodes_as_not_failed() {
        let payload = envelope(&json!({ "id": "call-1", "content": "# Jan-Klod" }));
        assert_eq!(
            decode("tool-result", &payload).unwrap(),
            Event::ToolResult(ToolOutcome {
                tool_call_id: "call-1".to_owned(),
                content: "# Jan-Klod".to_owned(),
                failed: false,
            })
        );
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

    /// Older envelopes stay readable; tightening the check to `!=` would break that.
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
            decode_record("invented-by-a-later-build", &payload),
            Err(DecodeError::UnknownKind(
                "invented-by-a-later-build".to_owned()
            )),
            "a record kind this build does not know must say which"
        );
    }

    #[test]
    fn a_record_asked_of_the_event_decoder_says_so() {
        let payload = envelope(&json!({ "message": "hello" }));
        assert_eq!(
            decode(KIND_USER_MESSAGE, &payload),
            Err(DecodeError::NotAnEvent(KIND_USER_MESSAGE.to_owned()))
        );
        assert_eq!(
            decode_record(KIND_USER_MESSAGE, &payload).unwrap(),
            Record::UserMessage("hello".to_owned()),
            "and the wider decoder reads the same row fine"
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

    #[test]
    fn the_inner_sinks_stop_still_propagates() {
        let store = Mutex::new(Store::open_in_memory().unwrap());
        let mut inner = Recorder::new(Flow::Stop);
        let mut sink = PersistingSink::new(&mut inner, &store, "s1");
        assert_eq!(sink.emit(&Event::TextDelta("x".to_owned())), Flow::Stop);
    }

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

#[cfg(test)]
mod driver_tests {
    use super::*;

    struct Stub {
        answer: String,
        follow_ups: Vec<String>,
    }

    impl Driver for Stub {
        fn ask(&mut self, _prompt: &UserPrompt) -> String {
            self.answer.clone()
        }
        fn follow_up(&mut self) -> Option<String> {
            self.follow_ups.pop()
        }
    }

    fn prompt() -> UserPrompt {
        UserPrompt {
            question: "Run `rm -rf /`?".to_owned(),
            options: vec!["yes".to_owned(), "no".to_owned()],
            default_answer: "no".to_owned(),
        }
    }

    fn kinds(store: &Mutex<Store>, session: &str) -> Vec<String> {
        store
            .lock()
            .unwrap()
            .session_events(session)
            .unwrap()
            .into_iter()
            .map(|row| row.kind)
            .collect()
    }

    #[test]
    fn an_ask_logs_the_prompt_then_the_answer() {
        let store = Mutex::new(Store::open_in_memory().unwrap());
        let mut inner = Stub {
            answer: "yes".to_owned(),
            follow_ups: Vec::new(),
        };
        let answered = {
            let mut driver = PersistingDriver::new(&mut inner, &store, "s1");
            driver.ask(&prompt())
        };

        assert_eq!(answered, "yes", "the wrapper does not alter the answer");
        assert_eq!(
            kinds(&store, "s1"),
            vec![KIND_ASK.to_owned(), KIND_ANSWER.to_owned()],
            "the prompt is recorded before the answer, which is the order they happened"
        );

        let rows = store.lock().unwrap().session_events("s1").unwrap();
        assert!(
            rows[0].payload.contains("rm -rf") && rows[0].payload.contains("\"default\":\"no\""),
            "the prompt row carries the question and the default: {}",
            rows[0].payload
        );
        assert!(
            rows[1].payload.contains("\"answer\":\"yes\""),
            "and the answer row carries what came back: {}",
            rows[1].payload
        );
    }

    #[test]
    fn a_defaulted_answer_is_logged_like_any_other() {
        let store = Mutex::new(Store::open_in_memory().unwrap());
        let mut inner = Stub {
            answer: "no".to_owned(),
            follow_ups: Vec::new(),
        };
        {
            let mut driver = PersistingDriver::new(&mut inner, &store, "s1");
            driver.ask(&prompt());
        }
        let rows = store.lock().unwrap().session_events("s1").unwrap();
        assert!(
            rows[1].payload.contains("\"answer\":\"no\""),
            "a denial is a fact about the turn, not an absence: {}",
            rows[1].payload
        );
    }

    #[test]
    fn a_follow_up_is_logged_and_nothing_is_logged_when_there_is_none() {
        let store = Mutex::new(Store::open_in_memory().unwrap());
        let mut inner = Stub {
            answer: String::new(),
            follow_ups: vec!["actually, in Rust".to_owned()],
        };
        let (first, second) = {
            let mut driver = PersistingDriver::new(&mut inner, &store, "s1");
            (driver.follow_up(), driver.follow_up())
        };

        assert_eq!(first.as_deref(), Some("actually, in Rust"));
        assert_eq!(second, None, "the stub has no second follow-up");
        assert_eq!(
            kinds(&store, "s1"),
            vec![KIND_FOLLOW_UP.to_owned()],
            "one row for the steering message, none for the turn ending normally"
        );
    }

    #[test]
    fn a_broken_store_does_not_change_what_the_driver_answers() {
        let store = Mutex::new(Store::open_in_memory().unwrap());
        std::thread::scope(|scope| {
            let handle = scope.spawn(|| {
                let _guard = store.lock().unwrap();
                panic!("poisoning the lock on purpose");
            });
            assert!(handle.join().is_err());
        });

        let mut inner = Stub {
            answer: "yes".to_owned(),
            follow_ups: vec!["steer".to_owned()],
        };
        let mut driver = PersistingDriver::new(&mut inner, &store, "s1");
        assert_eq!(driver.ask(&prompt()), "yes");
        assert_eq!(driver.follow_up().as_deref(), Some("steer"));
    }
}

#[cfg(test)]
mod record_tests {
    use super::*;

    struct Stub;
    impl Driver for Stub {
        fn ask(&mut self, _: &UserPrompt) -> String {
            "yes".to_owned()
        }
        fn follow_up(&mut self) -> Option<String> {
            Some("actually, in Rust".to_owned())
        }
    }

    #[test]
    fn the_non_event_records_round_trip_from_what_is_written() {
        let store = Mutex::new(Store::open_in_memory().unwrap());

        log_user_message(&store, "s1", "what does this repo do?");

        let mut inner = Stub;
        {
            let mut driver = PersistingDriver::new(&mut inner, &store, "s1");
            driver.ask(&UserPrompt {
                question: "Run `cargo build`?".to_owned(),
                options: vec!["yes".to_owned(), "no".to_owned()],
                default_answer: "no".to_owned(),
            });
            driver.follow_up();
        }

        let rows = store.lock().unwrap().session_events("s1").unwrap();
        let decoded: Vec<Record> = rows
            .iter()
            .map(|row| {
                decode_record(&row.kind, &row.payload)
                    .unwrap_or_else(|e| panic!("`{}` did not decode: {e}", row.kind))
            })
            .collect();

        assert_eq!(
            decoded,
            vec![
                Record::UserMessage("what does this repo do?".to_owned()),
                Record::Ask {
                    question: "Run `cargo build`?".to_owned(),
                    options: vec!["yes".to_owned(), "no".to_owned()],
                    default: "no".to_owned(),
                },
                Record::Answer("yes".to_owned()),
                Record::FollowUp("actually, in Rust".to_owned()),
            ]
        );
    }

    #[test]
    fn a_free_text_ask_keeps_its_empty_option_list() {
        let payload = envelope(&json!({
            "question": "What should I call it?",
            "options": [],
            "default": "",
        }));
        assert_eq!(
            decode_record(KIND_ASK, &payload).unwrap(),
            Record::Ask {
                question: "What should I call it?".to_owned(),
                options: Vec::new(),
                default: String::new(),
            }
        );
    }

    #[test]
    fn an_option_that_is_not_a_string_is_malformed() {
        let payload = envelope(&json!({
            "question": "q",
            "options": ["yes", 7],
            "default": "no",
        }));
        assert!(matches!(
            decode_record(KIND_ASK, &payload),
            Err(DecodeError::Malformed(ref m)) if m.contains("non-string")
        ));
    }

    #[test]
    fn a_record_missing_a_field_is_malformed() {
        let payload = envelope(&json!({ "question": "q", "default": "no" }));
        assert!(
            matches!(
                decode_record(KIND_ASK, &payload),
                Err(DecodeError::Malformed(ref m)) if m.contains("options")
            ),
            "the missing field is named"
        );
    }

    #[test]
    fn a_newer_version_is_refused_for_records_too() {
        let payload = json!({ "v": EVENT_LOG_VERSION + 1, "data": { "message": "x" } }).to_string();
        assert_eq!(
            decode_record(KIND_USER_MESSAGE, &payload),
            Err(DecodeError::UnsupportedVersion {
                found: EVENT_LOG_VERSION + 1
            })
        );
    }

    #[test]
    fn events_read_through_the_record_decoder_as_well() {
        let (kind, payload) = encode(&Event::Warning("careful".to_owned()));
        assert_eq!(
            decode_record(kind, &payload).unwrap(),
            Record::Event(Event::Warning("careful".to_owned()))
        );
    }
}

#[cfg(test)]
mod migration_tests {
    use super::*;
    use crate::projection;

    fn old_transcript(store: &Store, session: &str, turns: &[(&str, &str)]) {
        for (index, (user, answer)) in turns.iter().enumerate() {
            store
                .set(
                    session,
                    &format!("turn-{}", index + 1),
                    &json!({ "user": user, "answer": answer }).to_string(),
                )
                .unwrap();
        }
    }

    #[test]
    fn an_old_transcript_becomes_a_readable_log() {
        let store = Store::open_in_memory().unwrap();
        old_transcript(
            &store,
            "old",
            &[
                ("first question", "first answer"),
                ("second", "second answer"),
            ],
        );

        assert_eq!(migrate_transcripts(&store).unwrap(), 1);

        let log = store.session_events("old").unwrap();
        assert_eq!(
            log.iter().map(|r| r.kind.as_str()).collect::<Vec<_>>(),
            vec![KIND_USER_MESSAGE, "done", KIND_USER_MESSAGE, "done"],
            "two turns, each a message and an answer"
        );
        assert_eq!(
            projection::transcript(&log)
                .iter()
                .map(|m| m.content.clone())
                .collect::<Vec<_>>(),
            vec!["first question", "first answer", "second", "second answer"],
            "and it projects to the conversation it was"
        );
    }

    /// `turn-10` sorts before `turn-9` as text; the bug only shows at ten turns.
    #[test]
    fn turns_are_ordered_by_number_not_by_key() {
        let store = Store::open_in_memory().unwrap();
        let turns: Vec<(String, String)> = (1..=12)
            .map(|i| (format!("q{i}"), format!("a{i}")))
            .collect();
        let borrowed: Vec<(&str, &str)> = turns
            .iter()
            .map(|(q, a)| (q.as_str(), a.as_str()))
            .collect();
        old_transcript(&store, "long", &borrowed);

        migrate_transcripts(&store).unwrap();
        let contents: Vec<String> = projection::transcript(&store.session_events("long").unwrap())
            .iter()
            .map(|m| m.content.clone())
            .collect();
        let expected: Vec<String> = (1..=12)
            .flat_map(|i| [format!("q{i}"), format!("a{i}")])
            .collect();
        assert_eq!(contents, expected, "in turn order, not lexical key order");
    }

    #[test]
    fn migrating_twice_changes_nothing() {
        let store = Store::open_in_memory().unwrap();
        old_transcript(&store, "old", &[("q", "a")]);

        assert_eq!(migrate_transcripts(&store).unwrap(), 1);
        let after_first = store.session_events("old").unwrap();
        assert_eq!(
            migrate_transcripts(&store).unwrap(),
            0,
            "nothing left to do"
        );
        assert_eq!(
            store.session_events("old").unwrap(),
            after_first,
            "and the log is untouched, not doubled"
        );
    }

    #[test]
    fn a_session_with_events_is_left_alone() {
        let store = Store::open_in_memory().unwrap();
        old_transcript(&store, "mixed", &[("q", "a")]);
        store
            .append_event(
                "mixed",
                KIND_USER_MESSAGE,
                &envelope(&json!({ "message": "live" })),
            )
            .unwrap();

        assert_eq!(migrate_transcripts(&store).unwrap(), 0);
        let log = store.session_events("mixed").unwrap();
        assert_eq!(log.len(), 1, "the native row is the only one");
        assert!(log[0].payload.contains("live"));
    }

    #[test]
    fn migrated_events_keep_the_transcripts_timestamps() {
        let store = Store::open_in_memory().unwrap();
        old_transcript(&store, "old", &[("q", "a")]);
        let original = store.get("old", "turn-1").unwrap().created_at;

        migrate_transcripts(&store).unwrap();
        assert!(
            store
                .session_events("old")
                .unwrap()
                .iter()
                .all(|row| row.ts == original),
            "every converted row carries the turn's own timestamp"
        );
    }

    #[test]
    fn interceptor_namespaces_are_not_sessions() {
        let store = Store::open_in_memory().unwrap();
        store
            .set("ext/interceptor.permission/grants", "turn-1", "{}")
            .unwrap();
        assert_eq!(migrate_transcripts(&store).unwrap(), 0);
        assert_eq!(
            store.event_sessions().unwrap(),
            [] as [std::string::String; 0]
        );
    }

    #[test]
    fn a_namespace_without_turn_rows_is_skipped() {
        let store = Store::open_in_memory().unwrap();
        store.set("notes", "scratch", "{}").unwrap();
        assert_eq!(migrate_transcripts(&store).unwrap(), 0);
    }
}
