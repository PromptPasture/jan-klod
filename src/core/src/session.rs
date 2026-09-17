//! Session operations every transport shares.
//!
//! REST, JSON-RPC, ACP and MCP all mint session ids, list sessions, read one
//! session's messages, fork at a seq, and wait the same span for a confirmation.
//! None of that is transport-specific, and a second implementation of any of it
//! would drift: two id schemes collide in the store, two payload shapes make one
//! client render two things, two timeouts surprise whoever switches transport.
//!
//! This module is the kernel-side home for those five. The surfaces that call
//! them live in the host binary (#179); the operations stay here because they
//! are session behaviour, not surface behaviour.

use std::time::Duration;

use crate::intercept::{Message, Role};
use crate::AgentSession;

/// How long a turn waits for a confirmation before giving up and taking the
/// prompt's default answer. Long enough for a person to read and decide; short
/// enough that a client that vanished mid-prompt cannot pin the server open.
#[allow(clippy::duration_suboptimal_units)] // no stable `Duration::from_mins`
const DEFAULT_ANSWER_TIMEOUT: Duration = Duration::from_secs(180);

/// Overrides [`DEFAULT_ANSWER_TIMEOUT`], in seconds. Read per wait, not cached,
/// so a test can set it per process; three minutes is right for humans, wrong
/// for tests (unanswered tests still pass with default answer, just slowly).
const TIMEOUT_ENV: &str = "JK_ANSWER_TIMEOUT_SECS";

/// The configured confirmation timeout. Exported so both REST and stdio
/// transports use the same window; varying by transport would surprise users.
#[must_use]
pub fn answer_timeout() -> Duration {
    std::env::var(TIMEOUT_ENV)
        .ok()
        .and_then(|raw| raw.parse::<u64>().ok())
        .map_or(DEFAULT_ANSWER_TIMEOUT, Duration::from_secs)
}

/// Every session with a preview, as any transport reports it.
///
/// Shared across transports so a client that lists sessions over stdio and over
/// REST must render one shape, not two.
#[must_use]
pub fn sessions_payload(agent: &AgentSession) -> serde_json::Value {
    let sessions: Vec<serde_json::Value> = agent
        .list_sessions()
        .into_iter()
        .map(|id| {
            // First user message (readable in picker). From projection, not log
            // row 1, as row 1 need not be a user message (steered/interrupted turns).
            let preview = agent
                .transcript(&id)
                .into_iter()
                .find(|message| message.role == Role::User)
                .map(|message| message.content.chars().take(80).collect::<String>())
                .unwrap_or_default();
            serde_json::json!({ "id": id, "preview": preview })
        })
        .collect();
    serde_json::json!({ "sessions": sessions })
}

/// One session's message list. Shared across transports like [`sessions_payload`].
#[must_use]
pub fn session_payload(agent: &AgentSession, id: &str) -> serde_json::Value {
    let messages: Vec<serde_json::Value> = agent
        .placed_transcript(id)
        .iter()
        .map(|(seq, message)| as_json(*seq, message))
        .collect();
    serde_json::json!({ "id": id, "messages": messages })
}

/// One message as a transport serves it.
///
/// `messages`, not the `turns` this used to return. A turn was a `{user, answer}`
/// pair because that's what the transcript row held; now the session projects from
/// its event log, which includes tool results. Pairing those back into turns would
/// drop them or invent a shape. A message list is what the projection produces.
fn as_json(seq: u64, message: &Message) -> serde_json::Value {
    let role = match message.role {
        Role::System => "system",
        Role::User => "user",
        Role::Assistant => "assistant",
        Role::Tool => "tool",
    };
    // `seq` is the log position; it is what `session/fork` takes as `at-seq`.
    // Without it, clients cannot specify a fork point (#106). Sparse on purpose:
    // events with no message (ask, answer, delta) still consume a seq.
    let mut object = serde_json::json!({ "seq": seq, "role": role, "content": message.content });
    // Present only on tool results, tying it to the call it answers.
    if let Some(id) = &message.tool_call_id {
        object["tool-call-id"] = serde_json::json!(id);
    }
    object
}

/// What a fork did, in the three ways it can end.
///
/// Three variants rather than `Result<Value, String>` because the two failures
/// are not the same: the caller asked for something missing (`404`) or the store
/// broke (`500`). Collapsing them would make a mistyped `at-seq` look like
/// a broken database.
pub enum Forked {
    /// The child session, as `{"id": …, "copied": …}`.
    Created(serde_json::Value),
    /// Nothing was copied, so no fork was made.
    Empty(String),
    /// The store refused.
    Failed(String),
}

/// Fork `id` at `at_seq` into a freshly generated child session. Generated id
/// avoids collisions; client-picked ids could collide with live sessions.
#[must_use]
pub fn fork(agent: &AgentSession, id: &str, at_seq: u64) -> Forked {
    let child = new_session_id();
    match agent.fork_session(id, at_seq, &child) {
        // Empty fork (no events at seq) usually means wrong `at-seq` or session id;
        // report it rather than silently returning a non-fork.
        Ok(0) => Forked::Empty(format!(
            "session `{id}` has no events at or before seq {at_seq}"
        )),
        Ok(copied) => Forked::Created(serde_json::json!({ "id": child, "copied": copied })),
        Err(err) => Forked::Failed(format!("fork failed: {err}")),
    }
}

/// Generate random 16-hex-char session id (stdlib only). Exported so all
/// transports mint the same way; different schemes would collide in the store.
#[must_use]
pub fn new_session_id() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    // Mix timestamp nanos + counter for uniqueness without rand dependency.
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| u64::from(d.subsec_nanos()));
    let count = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    format!(
        "{:08x}{:08x}",
        ts ^ (count << 17),
        count.wrapping_mul(0x9e37_79b9)
    )
}

#[cfg(test)]
mod tests {
    use super::{as_json, Message, Role};

    #[test]
    fn a_message_serves_its_role_by_name() {
        for (role, expected) in [
            (Role::System, "system"),
            (Role::User, "user"),
            (Role::Assistant, "assistant"),
            (Role::Tool, "tool"),
        ] {
            let json = as_json(
                7,
                &Message {
                    role,
                    content: "x".to_owned(),
                    tool_call_id: None,
                },
            );
            assert_eq!(json["role"], serde_json::json!(expected));
        }
    }

    /// The id is present only on messages that have one, not null on others.
    /// Absent field avoids inviting clients to read it as "sometimes empty".
    #[test]
    fn only_a_tool_result_carries_a_call_id() {
        let plain = as_json(
            1,
            &Message {
                role: Role::User,
                content: "hello".to_owned(),
                tool_call_id: None,
            },
        );
        assert!(plain.get("tool-call-id").is_none(), "{plain}");

        let result = as_json(
            4,
            &Message {
                role: Role::Tool,
                content: "# Jan-Klod".to_owned(),
                tool_call_id: Some("call-1".to_owned()),
            },
        );
        assert_eq!(result["tool-call-id"], serde_json::json!("call-1"));
        assert_eq!(result["content"], serde_json::json!("# Jan-Klod"));
        // Log position travels with message; used by `session/fork` as `at-seq`.
        // Without it, clients cannot specify fork points (#106).
        assert_eq!(result["seq"], serde_json::json!(4));
        assert_eq!(plain["seq"], serde_json::json!(1));
    }
}
