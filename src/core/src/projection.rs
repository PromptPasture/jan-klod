//! The conversation, derived from the log.
//!
//! A pure function over rows: no store, no I/O, nothing to mock. Produces the
//! `Vec<Message>` a following turn replays, derived from [`crate::event_log`]
//! instead of a separate transcript.
//!
//! # What each row becomes, and why
//!
//! The rule is *what the model saw*. A row never in the conversation is not
//! invented into one; a row that was is placed as `conductor::run_turn` places
//! it (the authority these mappings read off):
//!
//! | Row | Becomes | Why |
//! |---|---|---|
//! | `user-message` | `Role::User` | the turn's input |
//! | `follow-up` | `Role::User` | steering injected as a user message |
//! | `done` | `Role::Assistant` | the authoritative answer |
//! | `tool-result` | `Role::Tool` + `tool_call_id` | exactly how the loop feeds results back |
//! | `text-delta` | *dropped* | a preview `done` carries |
//! | `tool-invoked` | *dropped* | the request is not a message; only its result is |
//! | `warning` | *dropped* | operational notice, never in the conversation |
//! | `ask` / `answer` | *dropped* | the model never saw them — see below |
//!
//! **An `ask` is not a conversation turn.** It is a question put to the *user*
//! by an interceptor over a side channel the model has no part in; it never
//! reaches `PendingRequest.messages`. Rendering it as an assistant message puts
//! words in the model's mouth; rendering the answer as a user message makes a
//! permission click look like user input. Both are dropped: a replayed turn must
//! not teach the model that it asked "Run `rm -rf /`?".
//!
//! # Known limits
//!
//! Dropping `text-delta` in favour of `done` means agentic turns' *intermediate*
//! assistant texts — what the model said before calling a tool — are not in the
//! transcript. This matches the transcript it replaces, which stored only the
//! user message and final answer; nothing regresses, but "the whole conversation"
//! is what an event log invites you to assume.
//!
//! A `user-message` row holds the message the model actually received after
//! `before-loop` could `replace` it, not necessarily what a caller asked to send
//! (#84). This replays that exact message, correct for feeding into a following
//! turn.
//!
//! Logs written before #84 was fixed may hold the as-asked message where a
//! `before-loop` interceptor rewrote it. The row's *kind* didn't change, only
//! what a live turn puts in it, so old rows still decode and replay fine. No
//! interceptor in `config.yaml` rewrites messages, so in practice no session is
//! actually affected.

use crate::event_log::{decode_record, Record, KIND_USER_MESSAGE};
use crate::intercept::{Message, Role};
use crate::store::LoggedEvent;

/// The conversation a session's log describes, oldest first.
///
/// Undecidable rows are skipped, not fatal: a log written by a newer build
/// reads as much as this build understands. Skipped silently—the log itself
/// remains the record for auditing what was dropped.
#[must_use]
pub fn transcript(events: &[LoggedEvent]) -> Vec<Message> {
    placed_transcript(events)
        .into_iter()
        .map(|(_, message)| message)
        .collect()
}

/// [`transcript`], with each message paired to the log position it came from.
///
/// **Every message projects from exactly one event**, so each seq belongs to
/// that event—`message_for` takes one `Record`, returns at most one `Message`,
/// never folding. This lets a client name fork points: `session/fork`'s
/// `at-seq` is inclusive, so forking at the seq beside a message yields a
/// transcript ending with that message
/// ([#106](https://github.com/PromptPasture/jan-klod/issues/106)).
///
/// Seqs are **sparse**: `Ask`, `Answer`, `TextDelta`, `ToolInvoked`, `Warning`
/// all project to no message, so a seq is a log position, not a list index.
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
/// A turn begins at a `user-message` row; bounds are over turns, not rows.
/// Rows aren't a fixed per-turn count—one tool call adds two—so row-based
/// bounds cut turns in half and give the model conversations starting with an
/// orphaned tool result.
///
/// `turns == 0` returns an empty slice. A log with no `user-message` rows is
/// returned whole: one turn in progress, not zero.
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
/// Exhaustive over [`Record`] and [`crate::conductor::Event`] with no wildcard,
/// so new kinds cannot be silently dropped—the decision must be written here.
fn message_for(record: Record) -> Option<Message> {
    use crate::conductor::Event;
    match record {
        Record::UserMessage(content) | Record::FollowUp(content) => Some(Message {
            role: Role::User,
            content,
            tool_call_id: None,
        }),
        // None of these is conversation. An `Ask` and its `Answer` happened
        // between the host and the user, and an `ExtensionLoaded` between
        // turns — the model was told none of them, so replaying any into a
        // transcript would invent a turn that did not happen.
        Record::Ask { .. } | Record::Answer(_) | Record::ExtensionLoaded { .. } => None,
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

    /// Builds rows as the store does; tests never depend on `seq` beyond row order.
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

    /// The transcript it replaces stored `{user, answer}` per turn, so a simple
    /// turn must yield exactly those two messages—otherwise every following turn
    /// changes when swapping to this.
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

    /// Results are tied to their calls as `run_tool_calls` does: `Role::Tool`
    /// message with the call's `tool_call_id`.
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
                    failed: false,
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

    /// Permission prompts are between interceptor and user only; must not become
    /// something the model said or the user said to it.
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

    /// Unreadable rows are skipped, not fatal. Failing the whole projection
    /// would hide an entire history for one undecodable row.
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

    /// Order comes from `seq`; projection doesn't re-sort. Out-of-order logs are
    /// store bugs, and hiding them here makes them unfindable.
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

    /// Each turn is user message, tool result, answer—three rows; a row-based
    /// bound visibly cuts a turn open.
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
                failed: false,
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

    /// Row-based bounds would start conversations with orphaned tool results;
    /// this asserts they don't.
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

    /// Logs with no turn boundary (all events) are one turn in progress;
    /// returning nothing would lose it.
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

    /// `follow-up` is a user message but not a turn boundary—steering continues
    /// a turn rather than starting one, so it doesn't consume the bound.
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
