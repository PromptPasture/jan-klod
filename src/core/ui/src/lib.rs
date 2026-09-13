//! `jan-klod-ui` — a thin client that drives a core it spawned, or one that is
//! already running.
//!
//! A UI is a **separate process**, not an extension (the LSP/server model). Two
//! ways to reach one, both behind [`transport::Transport`]:
//!
//! * **stdio**, the default: spawn `jan-klod-gateway rpc` and speak
//!   newline-delimited JSON-RPC over its pipes. No port, no token, nothing left
//!   running.
//! * **REST + SSE**, when the user names an address: `POST
//!   /session/:id/message` with `{"message":"..."}`, streamed back as
//!   `event:`/`data:` frames ([`stream_turn`], [`answer_prompt`]).
//!
//! It depends on neither the core runtime nor Wasmtime — only on the wire
//! contract in `jan-klod-protocol`, which carries nothing beyond serde for
//! exactly this reason.

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
pub mod viewport;
pub mod wrap;

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Duration;

/// Resolve one of this repository's binaries: sibling of the current exe first,
/// then `PATH`.
///
/// Here rather than in the binary because [`transport::Stdio`] spawns the
/// gateway, and a second copy of this rule would eventually find a different
/// gateway than the one the REST path starts. `--gui` needs the same rule for
/// `jan-klod-gui`, which is why this is by name rather than one function per
/// binary — a bundle puts all three side by side, and a developer tree puts
/// them in different `target/` directories, so "sibling, else PATH" is the
/// answer for each of them.
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

/// Resolve the `jan-klod-gui` binary — the Tauri shell in `src/gui`, a separate
/// workspace and therefore a separate build (see [`sibling_bin`]).
///
/// Unlike the gateway, this one is genuinely **optional**: `src/gui` is not part
/// of the host workspace, so a plain `cargo build` never produces it and most
/// bundles do not ship it. `--gui` checks for it and says so rather than
/// spawning a name that is not there and reporting the OS's error.
#[must_use]
pub fn gui_bin() -> PathBuf {
    sibling_bin("jan-klod-gui")
}

/// The `Authorization` header line to send, or empty when no token is set.
///
/// Read from the environment on every request rather than cached: a client is a
/// short-lived process, and a token that changed under a long-running TUI should
/// take effect on the next turn rather than at the next restart.
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
    /// A tool is about to run.
    ///
    /// `arguments` is `Option` rather than `String`: before #161 the SSE `tool`
    /// frame carried no `arguments` at all, so `None` said "this transport did
    /// not send them" rather than "the call took none" — pretending it was
    /// always present would have made a client that silently showed less over
    /// REST than over stdio. Both `parse_frame` and `transport::event_for` now
    /// populate it from the wire, so in practice it is always `Some`; kept as
    /// `Option` rather than narrowed to `String` so a caller still cannot
    /// mistake "no arguments" for the data not having arrived.
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
        /// Whether the call failed — a denial, a trap, an error the tool
        /// reported, or no tool by that name.
        ///
        /// A fact the core establishes and both surfaces now carry (#162).
        /// Before that this client recognised a failure by matching the
        /// sentences the core happens to write into `content`, which is a
        /// coupling to wording nobody owned: renaming "trapped" to "panicked"
        /// would have turned a failure green with no test failing anywhere.
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
        /// The session this question was asked on — **not necessarily the
        /// client's current one**, and what `turn/answer` must be sent with
        /// (#103). A client may be driving more than one session, which is
        /// exactly why the notification carries it.
        session: String,
        /// What the interceptor wants to know.
        question: String,
        /// The answers it recognises, in the order to offer them. Empty means
        /// free text, the protocol's own convention.
        options: Vec<String>,
        /// What core assumes if nobody answers — a denial, for the permission gate.
        default: String,
    },
    /// The turn failed.
    Error(String),
}

/// Parse one SSE frame (its `event` kind + `data` JSON) into a [`StreamEvent`],
/// or `None` when this client has nowhere to put it.
///
/// Mirrors [`transport::event_for`] on the stdio path, deliberately: both
/// answer "what does this client do with an event it was sent", and `None` is
/// how each says *nothing*. That one could not say it is what made an unknown
/// kind a user-visible error.
///
/// Two failures are told apart here, having previously shared one arm:
///
/// * **data that is not JSON** is a protocol violation whatever the kind is, so
///   it is reported;
/// * **a kind this client does not know** is not a failure at all — it means
///   the core is newer, or a frame was added without updating this — so it is
///   dropped. Catching *that* is a test's job, not a running client's.
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
        // The frame has carried `id` all along — `serve::sse_frame` emits
        // `{ id, name }` — and this client threw it away, which is why an
        // invocation and its result could not be paired (#154). `arguments`
        // used to be genuinely absent from the frame (#161); `serve::sse_frame`
        // now sends it, so this reads it rather than hard-coding `None`.
        "tool" => StreamEvent::Tool {
            id: field("id"),
            name: field("name"),
            arguments: Some(field("arguments")),
        },
        // The field names are `serve::sse_frame`'s for `Event::ToolResult`.
        // Without this arm the frame took the fallback below and every tool
        // result in a healthy turn reached the user as an error.
        //
        // `failed` defaults to `false` when absent rather than being treated as
        // a malformed frame (#162): a core older than the flag sends no such
        // key, and refusing its results would turn a compatible mismatch into a
        // broken turn. The cost is that an old core's failures render as
        // successes — which is exactly what happened *before* the flag existed,
        // so nothing regresses, and `hello` is where a version disagreement is
        // supposed to be caught.
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
        // `serve::prompt_frame` carries `session` too — check the field name
        // there before changing this, since #103 asks for exactly that
        // caution: the client answers with this session, not its own.
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
        // Its own arm now. A real turn failure and a frame this client has
        // never heard of used to arrive by the same path and be told apart by
        // comparing `kind` to a string — so the one case that genuinely is an
        // error was reached only by failing to be anything else.
        "error" => StreamEvent::Error(field("error")),
        _ => return None,
    })
}

/// Clears a `Rest`'s live-turn socket when a turn ends, however it ends.
///
/// A plain assignment after the read loop would not run if the loop returned
/// early on an error, and would not run at all on a panic — either of which
/// would leave `cancel` shutting down a socket whose turn is already over. A
/// guard runs on every path out of the function that created it, `?` and
/// panic included.
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
    // No caller outside this crate can cancel a turn it did not offer a
    // socket for, so a throwaway `Mutex` is exactly as capable as this
    // signature ever was before #157 — a fresh, never-shared slot.
    stream_turn_cancellable(addr, session, message, on_event, &Mutex::new(None))
}

/// [`stream_turn`], plus a place to park the connection so another thread can
/// cancel it.
///
/// `live_socket` is where this stores a clone of the connection for the length
/// of the turn, so [`transport::Rest::cancel`] can shut it down from another
/// thread — see that impl for why dropping the connection is what cancelling
/// means on this transport. Not `pub`: nothing outside `transport::Rest` has a
/// socket to cancel through, so nothing outside it needs this over
/// [`stream_turn`].
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
    #[allow(clippy::duration_suboptimal_units)] // no stable Duration::from_mins
    let read_timeout = Duration::from_secs(300);
    stream
        .set_read_timeout(Some(read_timeout))
        .map_err(|err| err.to_string())?;

    // Stored before the request is even written, so a `cancel` racing the
    // very start of the turn still has a socket to shut down. Cleared on
    // every exit from this function by `ClearLiveOnDrop`, including an early
    // `?` return and a panic.
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

    // Read line by line: skip HTTP headers (up to the first blank line), then parse
    // SSE frames (`event:`/`data:` lines, blank line = frame boundary).
    let reader = BufReader::new(stream);
    let mut in_body = false;
    let mut kind: Option<String> = None;
    let mut data: Option<String> = None;
    for line in reader.lines() {
        let line = line.map_err(|err| format!("reading response: {err}"))?;
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
/// This is a **second connection** while the turn's SSE stream is still open —
/// which is the whole point: the turn blocks on this request arriving. Core is
/// single-threaded and mid-turn, so it refuses anything else with `409` until the
/// answer lands.
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
    // A turn can take a while (model latency); 5-minute read cap. (No stable
    // `Duration::from_mins`, so `from_secs` is the readable option here.)
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

// parse_frame tests cover the public API and live in tests/parse_frame.rs.
// parse_answer is private so its tests stay here.
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
