//! The conversation, derived from the log by a pure projection from rows to `Message`s.
//!
//! Only what the model saw is replayed: `ask`, `answer`, `text-delta` and `warning` are dropped,
//! so a replayed turn never shows the model asking a permission question it did not ask.

use crate::event_log::{decode_record, Record, KIND_USER_MESSAGE};
use crate::store::LoggedEvent;
use jk_protocol::turn::{Message, Role};

/// The conversation a session's log describes, oldest first; unreadable rows are skipped.
#[must_use]
pub fn transcript(events: &[LoggedEvent]) -> Vec<Message> {
    placed_transcript(events)
        .into_iter()
        .map(|(_, message)| message)
        .collect()
}

/// [`transcript`] with each message paired to the log position it came from.
///
/// Each message comes from exactly one event, so a seq names a valid fork point:
/// `session/fork`'s `at-seq` is inclusive ([#106](https://github.com/PromptPasture/jan-klod/issues/106)).
/// Seqs are sparse, so they are log positions, not list indices.
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
/// Bounds count turns, not rows: a turn's row count varies, so a row bound would orphan tool results.
/// `turns == 0` gives an empty slice; a log with no `user-message` rows is returned whole.
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

/// The message one record contributes, or `None`. Matches exhaustively, so a new kind must be decided here.
fn message_for(record: Record) -> Option<Message> {
    use jk_protocol::turn::Event;
    match record {
        Record::UserMessage(content) | Record::FollowUp(content) => Some(Message {
            role: Role::User,
            content,
            tool_call_id: None,
        }),
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
    use crate::event_log::{encode, envelope, KIND_ANSWER, KIND_ASK, KIND_USER_MESSAGE};
    use jk_protocol::turn::{Event, ToolCall, ToolOutcome};
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

    /// A simple turn must yield exactly the `{user, answer}` pair the old transcript stored.
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

    /// Tool results are `Role::Tool` messages carrying the call's `tool_call_id`, as `run_tool_calls` does.
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

    /// Permission prompts are between interceptor and user only; they must not become model or user messages.
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

    /// Unreadable rows are skipped; one bad row must not hide the whole history.
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

    /// Order comes from `seq`; out-of-order logs are store bugs, so this does not re-sort.
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
    use crate::event_log::{encode, envelope};
    use jk_protocol::turn::{Event, ToolOutcome};
    use serde_json::json;

    /// Each turn is a user message, a tool result, and an answer: a row-based bound would cut one open.
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

    /// Row-based bounds would orphan tool results; this asserts they do not.
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
        assert_eq!(last_turns(&log_of(3), 0), []);
        assert_eq!(last_turns(&[], 20), []);
    }

    /// A log with no turn boundary is one turn in progress, not zero turns.
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

    /// `follow-up` is a user message but not a turn boundary: steering continues a turn.
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
