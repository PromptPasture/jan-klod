//! `jan-klod-ui` — a thin client that drives a running core over its REST surface.
//!
//! A UI is a **separate process**, not an extension: it connects to core the way
//! any HTTP client would (the LSP/server model). This library holds the transport
//! ([`send_turn`]); the binary layers a REPL over it (a `ratatui` TUI is a later
//! step). It depends on neither the core runtime nor Wasmtime — only the REST
//! contract: `POST /turn` with `{session, message}` → `{answer, agentic}`.

pub mod app;

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::time::Duration;

/// One event streamed back over SSE as a turn runs (mirrors the core's
/// `conductor::Event`, parsed from `event:/data:` frames).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StreamEvent {
    /// A chunk of assistant text (preview).
    Delta(String),
    /// A tool is running.
    Tool(String),
    /// A non-fatal notice (provider fallback, retry).
    Warning(String),
    /// The turn finished with the authoritative answer.
    Done(String),
    /// The turn failed.
    Error(String),
}

/// Parse one SSE frame (its `event` kind + `data` JSON) into a [`StreamEvent`].
#[must_use]
pub fn parse_frame(kind: &str, data: &str) -> StreamEvent {
    let value: serde_json::Value = match serde_json::from_str(data) {
        Ok(v) => v,
        Err(err) => return StreamEvent::Error(format!("malformed SSE frame ({kind}): {err}")),
    };
    let field = |k: &str| value.get(k).and_then(serde_json::Value::as_str).unwrap_or("").to_string();
    match kind {
        "delta" => StreamEvent::Delta(field("text")),
        "tool" => StreamEvent::Tool(field("name")),
        "warning" => StreamEvent::Warning(field("message")),
        "done" => StreamEvent::Done(field("answer")),
        _ => StreamEvent::Error(if kind == "error" { field("error") } else { format!("unknown event `{kind}`") }),
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
    let body = serde_json::json!({ "session": session, "message": message }).to_string();
    let request = format!(
        "POST /turn HTTP/1.1\r\nHost: {addr}\r\nContent-Type: application/json\r\n\
         Accept: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        body.len(),
        body
    );
    let mut stream = TcpStream::connect(addr).map_err(|err| format!("connecting to {addr}: {err}"))?;
    #[allow(clippy::duration_suboptimal_units)] // no stable Duration::from_mins
    let read_timeout = Duration::from_secs(300);
    stream
        .set_read_timeout(Some(read_timeout))
        .map_err(|err| err.to_string())?;
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
                on_event(parse_frame(&k, &d));
            }
        } else if let Some(rest) = line.strip_prefix("event: ") {
            kind = Some(rest.to_string());
        } else if let Some(rest) = line.strip_prefix("data: ") {
            data = Some(rest.to_string());
        }
    }
    Ok(())
}

/// Drive one turn against the core at `addr` (`host:port`): `POST` the message and
/// return the answer text.
///
/// # Errors
/// Returns a human-readable error if the connection fails, the response is not
/// well-formed, or core reports a turn failure (`{"error": …}`).
pub fn send_turn(addr: &str, session: &str, message: &str) -> Result<String, String> {
    let body = serde_json::json!({ "session": session, "message": message }).to_string();
    let request = format!(
        "POST /turn HTTP/1.1\r\nHost: {addr}\r\nContent-Type: application/json\r\n\
         Content-Length: {}\r\nConnection: close\r\n\r\n{}",
        body.len(),
        body
    );

    let mut stream = TcpStream::connect(addr).map_err(|err| format!("connecting to {addr}: {err}"))?;
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
    let value: serde_json::Value =
        serde_json::from_str(body).map_err(|err| format!("malformed response body: {err} (in {body:?})"))?;
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
