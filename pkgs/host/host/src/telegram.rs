//! Telegram bot (headless): `getUpdates` long-poll and `sendMessage` over an injected [`Fetch`]; chat id is session id.

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::rc::Rc;

use jk_agent::conductor::RunResult;
use jk_agent::AgentSession;
use jk_protocol::turn::{Driver, UserPrompt};

/// One inbound Telegram message worth answering.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Update {
    /// Monotonic update id (drives the poll offset).
    pub update_id: i64,
    /// Sender id, checked against the allowlist and used as the principal.
    pub sender_id: i64,
    /// Chat to reply to — also used as the loop session id.
    pub chat_id: i64,
    /// The message text.
    pub text: String,
}

/// The outbound HTTP the poller needs: `(method, url, headers, body) -> body`. Injected so tests can feed canned responses.
pub type Fetch<'a> =
    &'a dyn Fn(&str, &str, &[(&str, &str)], Option<&[u8]>) -> Result<Vec<u8>, String>;

const LONG_POLL_SECS: u32 = 30;

/// Long-polls a turn waits for its answer, counted in polls so a canned test `fetch` behaves like the real one.
const MAX_ANSWER_POLLS: u32 = 4;

/// Parses a `getUpdates` body into its text messages, skipping non-message updates.
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
                sender_id: message.get("from")?.get("id")?.as_i64()?,
                chat_id: message.get("chat")?.get("id")?.as_i64()?,
                text: message.get("text")?.as_str()?.to_string(),
            })
        })
        .collect()
}

/// Next poll offset: one past the highest update id, or `None` when there was nothing to advance past.
#[must_use]
pub fn next_offset(updates: &[Update]) -> Option<i64> {
    updates.iter().map(|u| u.update_id).max().map(|max| max + 1)
}

/// One poll cycle returning the next offset; `allowed_senders` (empty denies all) gates who may drive turns.
///
/// # Errors
/// Transport error string if `getUpdates` or a `sendMessage` fails.
pub fn poll_once(
    agent: &mut AgentSession,
    fetch: Fetch,
    token: &str,
    offset: i64,
    allowed_senders: Option<&[i64]>,
) -> Result<i64, String> {
    let body = fetch("GET", &get_updates_url(token, offset), &[], None)?;
    let updates = parse_updates(&body);

    let cursor = Rc::new(Cell::new(next_offset(&updates).unwrap_or(offset)));
    let deferred = Rc::new(RefCell::new(Vec::new()));

    let mut queue: VecDeque<Update> = updates.into_iter().collect();
    while let Some(update) = queue.pop_front() {
        if let Some(allowed) = allowed_senders {
            if !allowed.contains(&update.sender_id) {
                eprintln!(
                    "jan-klod: telegram: sender {} not in allowlist — refusing before session creation",
                    update.sender_id
                );
                continue;
            }
        }

        let session = update.chat_id.to_string();
        let principal = Some(format!("telegram:{}", update.sender_id));
        let mut driver = ChatDriver {
            fetch,
            token,
            chat_id: update.chat_id,
            cursor: Rc::clone(&cursor),
            deferred: Rc::clone(&deferred),
        };
        let answer =
            match agent.run_with_driver_principal(&mut driver, &session, &update.text, principal) {
                RunResult::Answered { text, .. } => text,
                RunResult::Failed(reason) => format!("(sorry — the turn failed: {reason})"),
            };
        send_message(fetch, token, update.chat_id, &answer)?;
        queue.extend(deferred.borrow_mut().drain(..));
    }

    Ok(cursor.get())
}

fn get_updates_url(token: &str, offset: i64) -> String {
    format!(
        "https://api.telegram.org/bot{token}/getUpdates?timeout={LONG_POLL_SECS}&offset={offset}"
    )
}

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

struct ChatDriver<'a> {
    fetch: Fetch<'a>,
    token: &'a str,
    chat_id: i64,
    cursor: Rc<Cell<i64>>,
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
            return prompt.default_answer.clone();
        }
        self.wait_for_reply()
            .unwrap_or_else(|| prompt.default_answer.clone())
    }
}

impl ChatDriver<'_> {
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
