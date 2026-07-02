//! Telegram chat integration (Phase 4 Slice 4b).
//!
//! Unlocks headless, UI-less access: a Telegram bot drives the loop. The Telegram
//! Bot API is **outbound HTTP** only — `getUpdates` (long-poll) and `sendMessage`
//! (POST) — so this needs no new `host-socket` capability; the existing outbound
//! HTTP suffices. Like the REST surface it is host-side (it drives the host-side
//! loop); HTTP is injected as a [`Fetch`] closure so the whole path is testable
//! offline.
//!
//! One inbound message → one turn (the chat id is the session, so a chat's history
//! is durable) → the answer sent back.

use crate::conductor::RunResult;
use crate::AgentSession;

/// One inbound Telegram message worth answering.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Update {
    /// Monotonic update id (drives the poll offset).
    pub update_id: i64,
    /// Chat to reply to — also used as the loop session id.
    pub chat_id: i64,
    /// The message text.
    pub text: String,
}

/// The outbound HTTP the poller needs: `(method, url, headers, body) -> body`.
/// Injected so tests can feed canned Telegram responses. The binary backs it with
/// the real `host-http` client.
pub type Fetch<'a> =
    &'a dyn Fn(&str, &str, &[(&str, &str)], Option<&[u8]>) -> Result<Vec<u8>, String>;

/// Long-poll seconds passed to `getUpdates` (server holds the request open).
const LONG_POLL_SECS: u32 = 30;

/// Parse a `getUpdates` response body into the text messages it carries.
/// Non-message updates (edits, joins, non-text) are skipped.
#[must_use]
pub fn parse_updates(body: &[u8]) -> Vec<Update> {
    let value: serde_json::Value = serde_json::from_slice(body).unwrap_or(serde_json::Value::Null);
    let Some(results) = value.get("result").and_then(serde_json::Value::as_array) else {
        return Vec::new();
    };
    results
        .iter()
        .filter_map(|update| {
            let message = update.get("message")?;
            Some(Update {
                update_id: update.get("update_id")?.as_i64()?,
                chat_id: message.get("chat")?.get("id")?.as_i64()?,
                text: message.get("text")?.as_str()?.to_string(),
            })
        })
        .collect()
}

/// The next poll offset after handling `updates`: one past the highest update id,
/// or `None` when there was nothing to advance past.
#[must_use]
pub fn next_offset(updates: &[Update]) -> Option<i64> {
    updates.iter().map(|u| u.update_id).max().map(|max| max + 1)
}

/// One poll cycle: fetch updates from `offset`, drive each message through the
/// loop, send the answer back, and return the next offset (unchanged if idle).
///
/// # Errors
/// Returns a transport error string if `getUpdates` or a `sendMessage` fails.
pub fn poll_once(
    agent: &mut AgentSession,
    fetch: Fetch,
    token: &str,
    offset: i64,
) -> Result<i64, String> {
    let get_url =
        format!("https://api.telegram.org/bot{token}/getUpdates?timeout={LONG_POLL_SECS}&offset={offset}");
    let body = fetch("GET", &get_url, &[], None)?;
    let updates = parse_updates(&body);

    for update in &updates {
        let session = update.chat_id.to_string();
        let answer = match agent.run(&session, &update.text) {
            RunResult::Answered { text, .. } => text,
            RunResult::Failed(reason) => format!("(sorry — the turn failed: {reason})"),
        };
        let send_url = format!("https://api.telegram.org/bot{token}/sendMessage");
        let payload =
            serde_json::json!({ "chat_id": update.chat_id, "text": answer }).to_string();
        fetch(
            "POST",
            &send_url,
            &[("Content-Type", "application/json")],
            Some(payload.as_bytes()),
        )?;
    }

    Ok(next_offset(&updates).unwrap_or(offset))
}
