//! Session operations shared by every transport, so ids, payloads and timeouts cannot drift.

use std::time::Duration;

use crate::AgentSession;
use jk_protocol::turn::{Message, Role};

/// Short enough that a client that vanished mid-prompt cannot pin the server open.
#[allow(clippy::duration_suboptimal_units)]
const DEFAULT_ANSWER_TIMEOUT: Duration = Duration::from_secs(180);

/// Read per wait so tests can set it.
const TIMEOUT_ENV: &str = "JK_ANSWER_TIMEOUT_SECS";

/// The configured confirmation timeout, shared so every transport waits the same span.
#[must_use]
pub fn answer_timeout() -> Duration {
    std::env::var(TIMEOUT_ENV)
        .ok()
        .and_then(|raw| raw.parse::<u64>().ok())
        .map_or(DEFAULT_ANSWER_TIMEOUT, Duration::from_secs)
}

/// Every session with a preview, in the shape every transport serves.
#[must_use]
pub fn sessions_payload(agent: &AgentSession) -> serde_json::Value {
    let sessions: Vec<serde_json::Value> = agent
        .list_sessions()
        .into_iter()
        .map(|id| {
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

/// One session's message list, in the shape every transport serves.
#[must_use]
pub fn session_payload(agent: &AgentSession, id: &str) -> serde_json::Value {
    let messages: Vec<serde_json::Value> = agent
        .placed_transcript(id)
        .iter()
        .map(|(seq, message)| as_json(*seq, message))
        .collect();
    serde_json::json!({ "id": id, "messages": messages })
}

/// The projection includes tool results, which a user/answer pair cannot hold.
fn as_json(seq: u64, message: &Message) -> serde_json::Value {
    let role = match message.role {
        Role::System => "system",
        Role::User => "user",
        Role::Assistant => "assistant",
        Role::Tool => "tool",
    };
    let mut object = serde_json::json!({ "seq": seq, "role": role, "content": message.content });
    if let Some(id) = &message.tool_call_id {
        object["tool-call-id"] = serde_json::json!(id);
    }
    object
}

/// What a fork did; distinct so a mistyped `at-seq` (404) is not reported as a broken store (500).
pub enum Forked {
    /// The child session, as `{"id": …, "copied": …}`.
    Created(serde_json::Value),
    /// Nothing was copied, so no fork was made.
    Empty(String),
    /// The store refused.
    Failed(String),
}

/// Forks `id` at `at_seq` into a generated child id, so client ids cannot collide with live sessions.
#[must_use]
pub fn fork(agent: &AgentSession, id: &str, at_seq: u64) -> Forked {
    let child = new_session_id();
    match agent.fork_session(id, at_seq, &child) {
        Ok(0) => Forked::Empty(format!(
            "session `{id}` has no events at or before seq {at_seq}"
        )),
        Ok(copied) => Forked::Created(serde_json::json!({ "id": child, "copied": copied })),
        Err(err) => Forked::Failed(format!("fork failed: {err}")),
    }
}

/// Generates a 16-hex-char session id without `rand`; every transport must mint ids this way.
#[must_use]
pub fn new_session_id() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
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
        assert_eq!(result["seq"], serde_json::json!(4));
        assert_eq!(plain["seq"], serde_json::json!(1));
    }
}
