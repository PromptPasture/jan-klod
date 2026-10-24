//! `jan-klod-ui`: a thin client for a spawned or running core, over stdio or REST + SSE
//! (see [`transport::Transport`]). Depends only on the `jk-protocol` wire contract.

pub mod app;
pub mod blocks;
pub mod commands;
pub mod composer;
pub mod diff;
pub mod keymap;
pub mod layout;
pub mod markdown;
pub mod paths;
pub mod sidebar;
pub mod theme;
pub mod transport;
pub mod untrusted;
pub mod viewport;
pub mod wrap;

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Duration;

/// Resolve one repository binary: sibling exe first, then `PATH`.
///
/// "Sibling, else PATH" works for bundles (all together) and dev trees
/// (different `target/` dirs).
#[must_use]
pub fn sibling_bin(name: &str) -> PathBuf {
    if let Ok(exe) = std::env::current_exe() {
        let sibling = exe.with_file_name(name);
        if sibling.exists() {
            return sibling;
        }
    }
    PathBuf::from(name)
}

/// Resolve the `jan-klod-gateway` binary. See [`sibling_bin`].
#[must_use]
pub fn gateway_bin() -> PathBuf {
    sibling_bin("jan-klod-gateway")
}

/// `Authorization` header line (read per-request, not cached, so token changes take effect).
fn auth_header() -> String {
    std::env::var("JAN_KLOD_TOKEN")
        .ok()
        .filter(|t| !t.trim().is_empty())
        .map_or_else(String::new, |token| {
            format!("Authorization: Bearer {token}\r\n")
        })
}

/// One event streamed back over SSE as a turn runs (mirrors the core's
/// `conductor::Event`, parsed from `event:/data:` frames).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StreamEvent {
    /// A chunk of assistant text (preview).
    Delta(String),
    /// Tool about to run. `arguments` is `None` when the data did not arrive, not when the call took none.
    Tool {
        /// Call id, matched by the [`StreamEvent::ToolResult`] that answers it.
        id: String,
        /// Tool name.
        name: String,
        /// JSON-encoded arguments, where the transport carries them.
        arguments: Option<String>,
    },
    /// A tool returned. `id` is the call this answers, and matches the
    /// [`StreamEvent::Tool`] that opened it.
    ToolResult {
        /// The call this answers.
        id: String,
        /// What the tool returned.
        content: String,
        /// Call failed (denial, trap, error, or unknown tool).
        failed: bool,
    },
    /// A non-fatal notice (provider fallback, retry).
    Warning(String),
    /// The turn finished with the authoritative answer.
    Done(String),
    /// The turn stopped to ask the user something and is blocked until answered
    /// (see [`answer_prompt`]). An unanswered prompt eventually times out on the
    /// core side and takes `default`.
    Prompt {
        /// The session the question was asked on, not necessarily the client's
        /// current one. `turn/answer` must name it (#103).
        session: String,
        /// What the interceptor wants to know.
        question: String,
        /// Answers to offer, in order. Empty means free text.
        options: Vec<String>,
        /// What core assumes if nobody answers — a denial, for the permission gate.
        default: String,
    },
    /// The turn failed.
    Error(String),
    /// What the extensions contribute to this client's interface, as a whole
    /// set (`wit/client-surface.wit`). Not a turn event: sent at the handshake
    /// and whenever the set changes.
    Contributions(Vec<jk_protocol::Contributions>),
}

/// Parse an SSE frame (event kind + data JSON) into a [`StreamEvent`], or `None`.
/// Mirrors [`transport::event_for`]. Unparseable JSON is an error; an unknown kind is dropped.
#[must_use]
pub fn parse_frame(kind: &str, data: &str) -> Option<StreamEvent> {
    let value: serde_json::Value = match serde_json::from_str(data) {
        Ok(v) => v,
        Err(err) => {
            return Some(StreamEvent::Error(format!(
                "malformed SSE frame ({kind}): {err}"
            )))
        }
    };
    let field = |k: &str| {
        value
            .get(k)
            .and_then(serde_json::Value::as_str)
            .unwrap_or("")
            .to_string()
    };
    Some(match kind {
        "delta" => StreamEvent::Delta(field("text")),
        "tool" => StreamEvent::Tool {
            id: field("id"),
            name: field("name"),
            arguments: Some(field("arguments")),
        },
        "tool-result" => StreamEvent::ToolResult {
            id: field("id"),
            content: field("content"),
            failed: value
                .get("failed")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false),
        },
        "warning" => StreamEvent::Warning(field("message")),
        "done" => StreamEvent::Done(field("answer")),
        "prompt" => StreamEvent::Prompt {
            session: field("session"),
            question: field("question"),
            options: value
                .get("options")
                .and_then(serde_json::Value::as_array)
                .map(|items| {
                    items
                        .iter()
                        .filter_map(|v| v.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default(),
            default: field("default"),
        },
        "error" => StreamEvent::Error(field("error")),
        _ => return None,
    })
}

/// Clears the live-turn socket on every exit, including `?` and panic.
struct ClearLiveOnDrop<'a> {
    live_socket: &'a Mutex<Option<TcpStream>>,
}

impl Drop for ClearLiveOnDrop<'_> {
    fn drop(&mut self) {
        if let Ok(mut slot) = self.live_socket.lock() {
            *slot = None;
        }
    }
}

/// Drive one turn with **streaming**: `POST` with `Accept: text/event-stream` and
/// invoke `on_event` for each SSE frame as it arrives (a live transcript).
///
/// # Errors
/// Returns a human-readable error if the connection or read fails.
pub fn stream_turn(
    addr: &str,
    session: &str,
    message: &str,
    on_event: &mut dyn FnMut(StreamEvent),
) -> Result<(), String> {
    stream_turn_cancellable(addr, session, message, on_event, &Mutex::new(None))
}

/// [`stream_turn`], plus a slot that parks the connection so another thread can cancel it.
/// Crate-private: only [`transport::Rest`] has a socket to cancel.
///
/// # Errors
/// Returns a human-readable error if the connection or read fails.
pub(crate) fn stream_turn_cancellable(
    addr: &str,
    session: &str,
    message: &str,
    on_event: &mut dyn FnMut(StreamEvent),
    live_socket: &Mutex<Option<TcpStream>>,
) -> Result<(), String> {
    let body = serde_json::json!({ "message": message }).to_string();
    let request = format!(
        "POST /session/{session}/message HTTP/1.1\r\nHost: {addr}\r\nContent-Type: application/json\r\n\
         Accept: text/event-stream\r\n{}Content-Length: {}\r\nConnection: close\r\n\r\n{}",
        auth_header(),
        body.len(),
        body
    );
    let mut stream =
        TcpStream::connect(addr).map_err(|err| format!("connecting to {addr}: {err}"))?;
    #[allow(clippy::duration_suboptimal_units)]
    let read_timeout = Duration::from_secs(300);
    stream
        .set_read_timeout(Some(read_timeout))
        .map_err(|err| err.to_string())?;

    let clone = stream
        .try_clone()
        .map_err(|err| format!("cloning the turn's socket: {err}"))?;
    *live_socket
        .lock()
        .map_err(|_| "the live turn's socket is poisoned".to_owned())? = Some(clone);
    let _clear = ClearLiveOnDrop { live_socket };

    stream
        .write_all(request.as_bytes())
        .map_err(|err| format!("sending request: {err}"))?;

    let reader = BufReader::new(stream);
    let mut in_body = false;
    let mut kind: Option<String> = None;
    let mut data: Option<String> = None;
    for line in reader.lines() {
        let line = match line {
            Ok(line) => line,
            Err(_) if live_socket.lock().is_ok_and(|slot| slot.is_none()) => break,
            Err(err) => return Err(format!("reading response: {err}")),
        };
        if !in_body {
            if line.is_empty() {
                in_body = true;
            }
            continue;
        }
        if line.is_empty() {
            if let (Some(k), Some(d)) = (kind.take(), data.take()) {
                if let Some(event) = parse_frame(&k, &d) {
                    on_event(event);
                }
            }
        } else if let Some(rest) = line.strip_prefix("event: ") {
            kind = Some(rest.to_string());
        } else if let Some(rest) = line.strip_prefix("data: ") {
            data = Some(rest.to_string());
        }
    }
    Ok(())
}

/// Answer a [`StreamEvent::Prompt`] that is holding a turn open.
///
/// This opens a **second connection** while the turn's SSE stream is still open;
/// the turn blocks until this request arrives. Core answers other requests with `409` mid-turn.
///
/// # Errors
/// Returns a human-readable error if the connection fails or core rejects the
/// answer (e.g. nothing was actually pending).
pub fn answer_prompt(addr: &str, session: &str, answer: &str) -> Result<(), String> {
    let body = serde_json::json!({ "answer": answer }).to_string();
    let request = format!(
        "POST /session/{session}/answer HTTP/1.1\r\nHost: {addr}\r\nContent-Type: application/json\r\n\
         {}Content-Length: {}\r\nConnection: close\r\n\r\n{}",
        auth_header(),
        body.len(),
        body
    );
    let mut stream =
        TcpStream::connect(addr).map_err(|err| format!("connecting to {addr}: {err}"))?;
    stream
        .set_read_timeout(Some(Duration::from_secs(30)))
        .map_err(|err| err.to_string())?;
    stream
        .write_all(request.as_bytes())
        .map_err(|err| format!("sending answer: {err}"))?;

    let mut raw = String::new();
    stream
        .read_to_string(&mut raw)
        .map_err(|err| format!("reading response: {err}"))?;
    let body = raw
        .split_once("\r\n\r\n")
        .map_or(raw.as_str(), |(_h, b)| b)
        .trim();
    let value: serde_json::Value = serde_json::from_str(body)
        .map_err(|err| format!("malformed response body: {err} (in {body:?})"))?;
    if value.get("accepted").and_then(serde_json::Value::as_bool) == Some(true) {
        Ok(())
    } else {
        Err(value
            .get("error")
            .and_then(serde_json::Value::as_str)
            .map_or_else(
                || format!("unexpected response: {body}"),
                |e| format!("core: {e}"),
            ))
    }
}

/// Allocate a new session on the core at `addr` (`host:port`): `POST /sessions`
/// and return its id.
///
/// # Errors
/// Returns a human-readable error if the connection fails or the response
/// carries no `id`.
pub fn create_session(addr: &str) -> Result<String, String> {
    let request = format!(
        "POST /sessions HTTP/1.1\r\nHost: {addr}\r\n{}Content-Length: 0\r\nConnection: close\r\n\r\n",
        auth_header()
    );
    let mut stream =
        TcpStream::connect(addr).map_err(|err| format!("connecting to {addr}: {err}"))?;
    stream
        .set_read_timeout(Some(Duration::from_secs(30)))
        .map_err(|err| err.to_string())?;
    stream
        .write_all(request.as_bytes())
        .map_err(|err| format!("sending request: {err}"))?;

    let mut raw = String::new();
    stream
        .read_to_string(&mut raw)
        .map_err(|err| format!("reading response: {err}"))?;
    let body = raw
        .split_once("\r\n\r\n")
        .map_or(raw.as_str(), |(_h, b)| b)
        .trim();
    let value: serde_json::Value = serde_json::from_str(body)
        .map_err(|err| format!("malformed response body: {err} (in {body:?})"))?;
    value
        .get("id")
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| format!("unexpected response: {body}"))
}

/// Send a pre-framed `GET` request and parse the body as JSON. Shared by
/// [`list_sessions`] and [`get_session`].
///
/// # Errors
/// Returns a human-readable error if the connection fails or the body is not
/// well-formed JSON.
fn get_json(addr: &str, request: &str) -> Result<serde_json::Value, String> {
    let mut stream =
        TcpStream::connect(addr).map_err(|err| format!("connecting to {addr}: {err}"))?;
    stream
        .set_read_timeout(Some(Duration::from_secs(30)))
        .map_err(|err| err.to_string())?;
    stream
        .write_all(request.as_bytes())
        .map_err(|err| format!("sending request: {err}"))?;

    let mut raw = String::new();
    stream
        .read_to_string(&mut raw)
        .map_err(|err| format!("reading response: {err}"))?;
    let body = raw
        .split_once("\r\n\r\n")
        .map_or(raw.as_str(), |(_h, b)| b)
        .trim();
    serde_json::from_str(body)
        .map_err(|err| format!("malformed response body: {err} (in {body:?})"))
}

/// List every session on the core at `addr` (`host:port`), with a preview.
///
/// # Errors
/// Returns a human-readable error if the connection fails or the response is
/// not well-formed.
pub fn list_sessions(addr: &str) -> Result<Vec<transport::SessionSummary>, String> {
    let request = format!(
        "GET /sessions HTTP/1.1\r\nHost: {addr}\r\n{}Connection: close\r\n\r\n",
        auth_header()
    );
    let value = get_json(addr, &request)?;
    let sessions = value
        .get("sessions")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| format!("unexpected response: {value}"))?;
    sessions
        .iter()
        .map(|entry| {
            let id = entry
                .get("id")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| format!("a session carried no id: {entry}"))?
                .to_owned();
            let preview = entry
                .get("preview")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("")
                .to_owned();
            Ok(transport::SessionSummary { id, preview })
        })
        .collect()
}

/// One session's transcript and metadata.
///
/// # Errors
/// Returns a human-readable error if the connection fails or the response is
/// not well-formed.
pub fn get_session(addr: &str, session: &str) -> Result<jk_protocol::SessionGetResult, String> {
    let request = format!(
        "GET /session/{session} HTTP/1.1\r\nHost: {addr}\r\n{}Connection: close\r\n\r\n",
        auth_header()
    );
    let value = get_json(addr, &request)?;
    serde_json::from_value(value).map_err(|err| format!("malformed session/get response: {err}"))
}

/// Drive one turn against the core at `addr` (`host:port`): `POST` the message and
/// return the answer text.
///
/// # Errors
/// Returns a human-readable error if the connection fails, the response is not
/// well-formed, or core reports a turn failure (`{"error": …}`).
pub fn send_turn(addr: &str, session: &str, message: &str) -> Result<String, String> {
    let body = serde_json::json!({ "message": message }).to_string();
    let request = format!(
        "POST /session/{session}/message HTTP/1.1\r\nHost: {addr}\r\nContent-Type: application/json\r\n\
         {}Content-Length: {}\r\nConnection: close\r\n\r\n{}",
        auth_header(),
        body.len(),
        body
    );

    let mut stream =
        TcpStream::connect(addr).map_err(|err| format!("connecting to {addr}: {err}"))?;
    #[allow(clippy::duration_suboptimal_units)]
    let read_timeout = Duration::from_secs(300);
    stream
        .set_read_timeout(Some(read_timeout))
        .map_err(|err| err.to_string())?;
    stream
        .write_all(request.as_bytes())
        .map_err(|err| format!("sending request: {err}"))?;

    let mut raw = String::new();
    stream
        .read_to_string(&mut raw)
        .map_err(|err| format!("reading response: {err}"))?;
    parse_answer(&raw)
}

/// Extract the answer from a raw HTTP response (headers + JSON body).
fn parse_answer(raw: &str) -> Result<String, String> {
    let body = raw
        .split_once("\r\n\r\n")
        .map_or(raw, |(_headers, body)| body)
        .trim();
    let value: serde_json::Value = serde_json::from_str(body)
        .map_err(|err| format!("malformed response body: {err} (in {body:?})"))?;
    let answer = value.get("answer").and_then(serde_json::Value::as_str);
    let error = value.get("error").and_then(serde_json::Value::as_str);
    match (answer, error) {
        (Some(answer), _) => Ok(answer.to_string()),
        (None, Some(error)) => Err(format!("core: {error}")),
        (None, None) => Err(format!("unexpected response: {body}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_answer_reads_the_answer_field() {
        let raw = "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\r\n{\"answer\":\"hi\",\"agentic\":false}";
        assert_eq!(parse_answer(raw).unwrap(), "hi");
    }

    #[test]
    fn parse_answer_surfaces_a_core_error() {
        let raw = "HTTP/1.1 502 Bad Gateway\r\n\r\n{\"error\":\"all providers failed\"}";
        let err = parse_answer(raw).unwrap_err();
        assert!(err.contains("all providers failed"), "{err}");
    }

    #[test]
    fn parse_answer_rejects_garbage() {
        assert!(parse_answer("HTTP/1.1 200 OK\r\n\r\nnot json").is_err());
    }
}
