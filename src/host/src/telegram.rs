//! Telegram bot integration (headless). Bot API is outbound HTTP only
//! (`getUpdates` long-poll, `sendMessage` POST); no `host-socket` needed.
//! HTTP injected as [`Fetch`] closure for offline testing.
//!
//! Message → turn → answer (chat id = session id).
//!
//! ## Confirmations in chat
//!
//! Headless path still gates on permissions. [`ChatDriver`] asks as a message,
//! treats next user message as answer, long-polls `getUpdates` while blocked.
//!
//! Other chats' updates deferred, not dropped (preserve poll offset), run after
//! current turn. Wait bounded by [`MAX_ANSWER_POLLS`]; expiry takes prompt default.

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::rc::Rc;

use jan_klod_core::conductor::RunResult;
use jan_klod_core::intercept::{Driver, UserPrompt};
use jan_klod_core::AgentSession;

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

/// How many long-polls a turn waits for its confirmation before giving up and
/// taking the prompt's default.
///
/// Bounded by *polls* rather than wall-clock so the behaviour is identical under a
/// canned `fetch` in a test and a real 30s-blocking one in production: four polls
/// is roughly two minutes on the wire, and exactly four iterations in a test.
const MAX_ANSWER_POLLS: u32 = 4;

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
    let body = fetch("GET", &get_updates_url(token, offset), &[], None)?;
    let updates = parse_updates(&body);

    // The offset and the deferred queue are shared with the driver: a turn that
    // stops to ask a question polls for its answer, which consumes updates the
    // outer loop must not re-fetch (offset) or lose (deferred).
    let cursor = Rc::new(Cell::new(next_offset(&updates).unwrap_or(offset)));
    let deferred = Rc::new(RefCell::new(Vec::new()));

    let mut queue: VecDeque<Update> = updates.into_iter().collect();
    while let Some(update) = queue.pop_front() {
        let session = update.chat_id.to_string();
        let mut driver = ChatDriver {
            fetch,
            token,
            chat_id: update.chat_id,
            cursor: Rc::clone(&cursor),
            deferred: Rc::clone(&deferred),
        };
        let answer = match agent.run_with_driver(&mut driver, &session, &update.text) {
            RunResult::Answered { text, .. } => text,
            RunResult::Failed(reason) => format!("(sorry — the turn failed: {reason})"),
        };
        send_message(fetch, token, update.chat_id, &answer)?;
        // Anything that arrived while this turn was waiting runs next, in order.
        queue.extend(deferred.borrow_mut().drain(..));
    }

    Ok(cursor.get())
}

/// The `getUpdates` long-poll URL for `offset`.
fn get_updates_url(token: &str, offset: i64) -> String {
    format!(
        "https://api.telegram.org/bot{token}/getUpdates?timeout={LONG_POLL_SECS}&offset={offset}"
    )
}

/// Send `text` to `chat_id`.
fn send_message(fetch: Fetch, token: &str, chat_id: i64, text: &str) -> Result<(), String> {
    let url = format!("https://api.telegram.org/bot{token}/sendMessage");
    let payload = serde_json::json!({ "chat_id": chat_id, "text": text }).to_string();
    fetch(
        "POST",
        &url,
        &[("Content-Type", "application/json")],
        Some(payload.as_bytes()),
    )?;
    Ok(())
}

/// Puts an interceptor's question to the chat and waits for the user's next
/// message there to answer it.
struct ChatDriver<'a> {
    fetch: Fetch<'a>,
    token: &'a str,
    /// The chat being asked — only its messages answer the question.
    chat_id: i64,
    /// Shared poll offset, advanced as this driver consumes updates.
    cursor: Rc<Cell<i64>>,
    /// Updates from other chats seen while waiting, to run after this turn.
    deferred: Rc<RefCell<Vec<Update>>>,
}

impl Driver for ChatDriver<'_> {
    fn ask(&mut self, prompt: &UserPrompt) -> String {
        let question = if prompt.options.is_empty() {
            prompt.question.clone()
        } else {
            format!(
                "{}\n\nReply with: {}",
                prompt.question,
                prompt.options.join(" / ")
            )
        };
        if send_message(self.fetch, self.token, self.chat_id, &question).is_err() {
            // The question never reached anyone, so nobody can answer it.
            return prompt.default_answer.clone();
        }
        self.wait_for_reply()
            .unwrap_or_else(|| prompt.default_answer.clone())
    }
}

impl ChatDriver<'_> {
    /// Long-poll until this chat says something, deferring other chats' messages.
    fn wait_for_reply(&self) -> Option<String> {
        for _ in 0..MAX_ANSWER_POLLS {
            let url = get_updates_url(self.token, self.cursor.get());
            let body = (self.fetch)("GET", &url, &[], None).ok()?;
            let updates = parse_updates(&body);
            if let Some(next) = next_offset(&updates) {
                self.cursor.set(next);
            }
            let mut reply = None;
            for update in updates {
                if reply.is_none() && update.chat_id == self.chat_id {
                    reply = Some(update.text);
                } else {
                    // Not an answer to this question — queue it, never drop it.
                    self.deferred.borrow_mut().push(update);
                }
            }
            if reply.is_some() {
                return reply;
            }
        }
        None
    }
}
