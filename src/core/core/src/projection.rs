//! The conversation, derived from the log.
//!
//! A pure function over rows: no store, no I/O, nothing to mock. What it
//! produces is the history a following turn replays — the same `Vec<Message>`
//! the conductor would have been handed before, now derived from
//! [`crate::event_log`] rather than from a separate transcript.
//!
//! # What each row becomes, and why
//!
//! The rule is *what the model saw*. A row that never entered the conversation
//! is not invented into one, and a row that did is placed the way the conductor
//! places it (`conductor::run_turn`, which is the authority these mappings were
//! read off rather than guessed at):
//!
//! | Row | Becomes | Why |
//! |---|---|---|
//! | `user-message` | `Role::User` | the turn's input |
//! | `follow-up` | `Role::User` | steering is injected as a user message |
//! | `done` | `Role::Assistant` | the authoritative answer |
//! | `tool-result` | `Role::Tool` + `tool_call_id` | exactly how the loop feeds a result back |
//! | `text-delta` | *dropped* | a preview of the answer `done` carries |
//! | `tool-invoked` | *dropped* | the request is not a message; only its result is |
//! | `warning` | *dropped* | operational notice, never in the conversation |
//! | `ask` / `answer` | *dropped* | the model never saw them — see below |
//!
//! **An `ask` is not a conversation turn.** It is a question put to the *user*
//! by an interceptor, over a side channel the model has no part in; it never
//! reaches `PendingRequest.messages`. Rendering the question as an assistant
//! message would put words in the model's mouth, and rendering the answer as a
//! user message would make a permission click look like something the user
//! said. Both are dropped, and a test asserts it: a replayed turn must not
//! teach the model that it once asked "Run `rm -rf /`?".
//!
//! # Known limits
//!
//! `text-delta` being dropped in favour of `done` means an agentic turn's
//! *intermediate* assistant texts — what the model said on the way to calling a
//! tool — are not in the transcript. That matches the transcript this replaces,
//! which stored only the user message and the final answer, so nothing regresses;
//! it is written down because "the whole conversation" is what an event log
//! invites you to assume.
//!
//! A `user-message` row holds the message the model actually received — after
//! `before-loop` has had its chance to `replace` it — not necessarily the one a
//! caller asked to send (#84). This replays exactly that message, which is what
//! makes it correct to feed back into a following turn.
//!
//! A log written before #84 was fixed can still hold the as-asked message for
//! a turn where a `before-loop` interceptor rewrote it — the row's *kind* did
//! not change, only what a live turn puts in it, so an old row still decodes
//! and replays fine; it is simply a record of what that older build logged. No
//! interceptor shipped in `config.yaml` rewrites the message, so in practice no
//! existing session is actually affected.

use crate::event_log::{decode_record, Record, KIND_USER_MESSAGE};
use crate::intercept::{Message, Role};
use crate::store::LoggedEvent;

/// The conversation a session's log describes, oldest first.
///
/// A row that cannot be decoded is skipped rather than failing the whole
/// projection: a log written by a newer build should read as much of a session
/// as this one understands, not as no session at all. Skipped silently because
/// this is a pure function by contract — the log itself remains the record for
/// anything that wants to audit what was dropped.
#[must_use]
pub fn transcript(events: &[LoggedEvent]) -> Vec<Message> {
    placed_transcript(events)
        .into_iter()
        .map(|(_, message)| message)
        .collect()
}

/// [`transcript`], with each message paired to the log position it came from.
///
/// **Every message is projected from exactly one event**, so the seq is that
/// event's and needs no rule about which of several it means — `message_for`
/// takes one `Record` and returns at most one `Message`, never folding. That is
/// what lets a client name a fork point: `session/fork`'s `at-seq` is inclusive,
/// so forking at the seq beside a message yields a session whose transcript ends
/// with that message
/// ([#106](https://github.com/PromptPasture/jan-klod/issues/106)).
///
/// The seqs are **sparse**. An `Ask`, an `Answer`, a `TextDelta`, a
/// `ToolInvoked` and a `Warning` all project to no message, so a seq is a
/// position in the log and not an index into this list.
#[must_use]
pub fn placed_transcript(events: &[LoggedEvent]) -> Vec<(u64, Message)> {
    events
        .iter()
        .filter_map(|row| {
            let record = decode_record(&row.kind, &row.payload).ok()?;
            message_for(record).map(|message| (row.seq, message))
        })
        .collect()
}

/// The tail of `events` holding at most the last `turns` turns.
///
/// A turn begins at a `user-message` row, so the bound is expressed over those
/// and not over rows. Counting rows would be the obvious thing and the wrong
/// one: turns are not a fixed number of rows — one tool call adds two — so a
/// row bound would cut a turn in half and hand the model a conversation that
/// begins with a tool result answering a call it cannot see.
///
/// `turns == 0` is an empty slice. A log with no `user-message` row at all is
/// returned whole: it is one turn in progress, not zero turns.
#[must_use]
pub fn last_turns(events: &[LoggedEvent], turns: u32) -> &[LoggedEvent] {
    if turns == 0 {
        return &[];
    }
    let starts: Vec<usize> = events
        .iter()
        .enumerate()
        .filter(|(_, row)| row.kind == KIND_USER_MESSAGE)
        .map(|(index, _)| index)
        .collect();
    let keep = turns as usize;
    if starts.len() <= keep {
        return events;
    }
    &events[starts[starts.len() - keep]..]
}

/// The message one record contributes, or `None` when it contributes nothing.
///
/// Exhaustive over [`Record`] and over [`crate::conductor::Event`] with no
/// wildcard arm, so a new kind of either cannot be dropped from the transcript
/// by default — the decision has to be written here.
fn message_for(record: Record) -> Option<Message> {
    use crate::conductor::Event;
    match record {
        Record::UserMessage(content) | Record::FollowUp(content) => Some(Message {
            role: Role::User,
            content,
            tool_call_id: None,
        }),
        Record::Ask { .. } | Record::Answer(_) => None,
        Record::Event(event) => match event {
            Event::Done { text, .. } => Some(Message {
                role: Role::Assistant,
                content: text,
                tool_call_id: None,
            }),
            Event::ToolResult(outcome) => Some(Message {
                role: Role::Tool,
                content: outcome.content,
                tool_call_id: Some(outcome.tool_call_id),
            }),
            Event::TextDelta(_) | Event::ToolInvoked(_) | Event::Warning(_) => None,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::conductor::Event;
    use crate::event_log::{encode, envelope, KIND_ANSWER, KIND_ASK, KIND_USER_MESSAGE};
    use crate::intercept::{ToolCall, ToolOutcome};
    use serde_json::json;

    /// Builds rows the way the store would, so a test never depends on `seq`
    /// being anything other than the order the rows are given in.
    fn row(seq: u64, kind: &str, payload: String) -> LoggedEvent {
        LoggedEvent {
            session: "s1".to_owned(),
            seq,
            ts: 100 + seq,
            kind: kind.to_owned(),
            payload,
        }
    }

    fn event_row(seq: u64, event: &Event) -> LoggedEvent {
        let (kind, payload) = encode(event);
        row(seq, kind, payload)
    }

    /// The transcript this replaces stored `{user, answer}` per turn, so a
    /// simple turn must come out as exactly those two messages — otherwise
    /// swapping `replay` over to this changes what every following turn sees.
    #[test]
    fn a_simple_turn_is_the_user_message_and_the_answer() {
        let log = vec![
            row(
                1,
                KIND_USER_MESSAGE,
                envelope(&json!({ "message": "hello" })),
            ),
            event_row(2, &Event::TextDelta("pong".to_owned())),
            event_row(
                3,
                &Event::Done {
                    text: "pong".to_owned(),
                    agentic: false,
                },
            ),
        ];
        let messages = transcript(&log);
        assert_eq!(messages.len(), 2, "the delta does not become a third turn");
        assert_eq!(messages[0].role, Role::User);
        assert_eq!(messages[0].content, "hello");
        assert_eq!(messages[1].role, Role::Assistant);
        assert_eq!(messages[1].content, "pong");
    }

    /// A result is tied to the call it answers, the way `run_tool_calls` ties
    /// it — a `Role::Tool` message whose `tool_call_id` is the call's id.
    #[test]
    fn a_tool_result_carries_its_call_id() {
        let log = vec![
            row(
                1,
                KIND_USER_MESSAGE,
                envelope(&json!({ "message": "read it" })),
            ),
            event_row(
                2,
                &Event::ToolInvoked(ToolCall {
                    id: "call-1".to_owned(),
                    name: "fs.read".to_owned(),
                    arguments: r#"{"path":"README.md"}"#.to_owned(),
                }),
            ),
            event_row(
                3,
                &Event::ToolResult(ToolOutcome {
                    tool_call_id: "call-1".to_owned(),
                    content: "# Jan-Klod".to_owned(),
                }),
            ),
            event_row(
                4,
                &Event::Done {
                    text: "it is an agent runtime".to_owned(),
                    agentic: true,
                },
            ),
        ];
        let messages = transcript(&log);
        assert_eq!(
            messages.len(),
            3,
            "user, tool result, answer — the invocation itself is not a message"
        );
        assert_eq!(messages[1].role, Role::Tool);
        assert_eq!(messages[1].content, "# Jan-Klod");
        assert_eq!(
            messages[1].tool_call_id.as_deref(),
            Some("call-1"),
            "the result is tied to the call it answers, as the loop ties it"
        );
    }

    /// The decision this box exists to make: a permission prompt is between an
    /// interceptor and the user, and must not become something the model said
    /// or something the user said to it.
    #[test]
    fn a_permission_prompt_and_its_answer_are_not_conversation() {
        let log = vec![
            row(
                1,
                KIND_USER_MESSAGE,
                envelope(&json!({ "message": "tidy up" })),
            ),
            row(
                2,
                KIND_ASK,
                envelope(&json!({
                    "question": "Run `rm -rf /`?",
                    "options": ["yes", "no"],
                    "default": "no",
                })),
            ),
            row(3, KIND_ANSWER, envelope(&json!({ "answer": "no" }))),
            event_row(
                4,
                &Event::Done {
                    text: "I did not run it".to_owned(),
                    agentic: true,
                },
            ),
        ];
        let messages = transcript(&log);
        assert_eq!(messages.len(), 2, "only the user's message and the answer");
        assert!(
            messages.iter().all(|m| !m.content.contains("rm -rf")),
            "the prompt must not reappear as anything the model said: {messages:?}"
        );
        assert!(
            messages.iter().all(|m| m.content != "no"),
            "nor the click as something the user typed"
        );
    }

    #[test]
    fn steering_is_a_user_message_because_that_is_how_it_was_injected() {
        let log = vec![
            row(
                1,
                KIND_USER_MESSAGE,
                envelope(&json!({ "message": "write it" })),
            ),
            row(
                2,
                "follow-up",
                envelope(&json!({ "message": "actually, in Rust" })),
            ),
            event_row(
                3,
                &Event::Done {
                    text: "done".to_owned(),
                    agentic: true,
                },
            ),
        ];
        let messages = transcript(&log);
        assert_eq!(
            messages
                .iter()
                .map(|m| (m.role, m.content.as_str()))
                .collect::<Vec<_>>(),
            vec![
                (Role::User, "write it"),
                (Role::User, "actually, in Rust"),
                (Role::Assistant, "done"),
            ]
        );
    }

    #[test]
    fn a_warning_is_not_conversation() {
        let log = vec![event_row(
            1,
            &Event::Warning("provider fell back".to_owned()),
        )];
        assert!(transcript(&log).is_empty());
    }

    /// A row this build cannot read costs that row, not the session. The
    /// alternative — failing the whole projection — would make one unreadable
    /// row hide an entire history.
    #[test]
    fn an_undecodable_row_is_skipped_not_fatal() {
        let log = vec![
            row(
                1,
                KIND_USER_MESSAGE,
                envelope(&json!({ "message": "hello" })),
            ),
            row(2, "from-a-later-build", envelope(&json!({}))),
            row(3, KIND_USER_MESSAGE, "not even json".to_owned()),
            event_row(
                4,
                &Event::Done {
                    text: "pong".to_owned(),
                    agentic: false,
                },
            ),
        ];
        let messages = transcript(&log);
        assert_eq!(
            messages
                .iter()
                .map(|m| m.content.as_str())
                .collect::<Vec<_>>(),
            vec!["hello", "pong"]
        );
    }

    #[test]
    fn an_empty_log_is_an_empty_conversation() {
        assert!(transcript(&[]).is_empty());
    }

    /// Order comes from the rows, and the rows come from `seq`. The projection
    /// does not re-sort: a log read out of order is a store bug, and hiding it
    /// here would make it unfindable.
    #[test]
    fn messages_follow_the_order_of_the_rows() {
        let log = vec![
            event_row(
                1,
                &Event::Done {
                    text: "second".to_owned(),
                    agentic: false,
                },
            ),
            row(
                2,
                KIND_USER_MESSAGE,
                envelope(&json!({ "message": "first" })),
            ),
        ];
        assert_eq!(
            transcript(&log)
                .iter()
                .map(|m| m.content.as_str())
                .collect::<Vec<_>>(),
            vec!["second", "first"],
            "given in this order, returned in this order"
        );
    }
}

#[cfg(test)]
mod bound_tests {
    use super::*;
    use crate::conductor::Event;
    use crate::event_log::{encode, envelope};
    use crate::intercept::ToolOutcome;
    use serde_json::json;

    /// `turns` turns, each a user message, a tool result and an answer — so a
    /// turn is three rows and a row-based bound would visibly cut one open.
    fn log_of(turns: usize) -> Vec<LoggedEvent> {
        let mut rows = Vec::new();
        for turn in 0..turns {
            let mut push = |kind: &str, payload: String| {
                rows.push(LoggedEvent {
                    session: "s".to_owned(),
                    seq: rows.len() as u64 + 1,
                    ts: 100,
                    kind: kind.to_owned(),
                    payload,
                });
            };
            push(
                KIND_USER_MESSAGE,
                envelope(&json!({ "message": format!("q{turn}") })),
            );
            let (kind, payload) = encode(&Event::ToolResult(ToolOutcome {
                tool_call_id: "c".to_owned(),
                content: format!("t{turn}"),
            }));
            push(kind, payload);
            let (kind, payload) = encode(&Event::Done {
                text: format!("a{turn}"),
                agentic: true,
            });
            push(kind, payload);
        }
        rows
    }

    #[test]
    fn a_short_log_is_returned_whole() {
        let log = log_of(2);
        assert_eq!(last_turns(&log, 20).len(), log.len());
    }

    #[test]
    fn the_bound_keeps_whole_turns_from_the_end() {
        let log = log_of(5);
        let kept = last_turns(&log, 2);
        assert_eq!(kept.len(), 6, "two turns of three rows each");
        assert_eq!(
            kept[0].kind, KIND_USER_MESSAGE,
            "the tail begins at a turn boundary, never mid-turn"
        );
        assert_eq!(
            transcript(kept)
                .iter()
                .map(|m| m.content.clone())
                .collect::<Vec<_>>(),
            vec!["q3", "t3", "a3", "q4", "t4", "a4"],
            "the last two turns, in order"
        );
    }

    /// The failure a row-based bound would cause, asserted directly: the
    /// conversation handed to the model must not start with a tool result.
    #[test]
    fn a_bounded_conversation_never_opens_with_a_tool_result() {
        let log = log_of(9);
        for turns in 1..=9 {
            let messages = transcript(last_turns(&log, turns));
            assert_eq!(
                messages.first().map(|m| m.role),
                Some(Role::User),
                "bounded to {turns} turns, the conversation opens with a user message"
            );
        }
    }

    #[test]
    fn a_zero_bound_is_empty_and_an_empty_log_stays_empty() {
        assert!(last_turns(&log_of(3), 0).is_empty());
        assert!(last_turns(&[], 20).is_empty());
    }

    /// A log whose rows are all events — no turn boundary — is one turn in
    /// progress. Returning nothing would lose it.
    #[test]
    fn a_log_with_no_turn_boundary_is_returned_whole() {
        let (kind, payload) = encode(&Event::Warning("standalone".to_owned()));
        let log = vec![LoggedEvent {
            session: "s".to_owned(),
            seq: 1,
            ts: 1,
            kind: kind.to_owned(),
            payload,
        }];
        assert_eq!(last_turns(&log, 20).len(), 1);
    }

    /// A `follow-up` is a user message but not a turn boundary — steering
    /// continues a turn rather than starting one, so it must not consume the
    /// bound.
    #[test]
    fn steering_does_not_start_a_new_turn() {
        let mut log = log_of(1);
        log.push(LoggedEvent {
            session: "s".to_owned(),
            seq: 4,
            ts: 100,
            kind: "follow-up".to_owned(),
            payload: envelope(&json!({ "message": "and also" })),
        });
        assert_eq!(
            last_turns(&log, 1).len(),
            4,
            "one turn, and the steering that belongs to it"
        );
    }
}
