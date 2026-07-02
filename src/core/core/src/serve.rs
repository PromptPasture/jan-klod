//! Host-side inbound HTTP surface (Phase 3 Slice 3b).
//!
//! A small **synchronous** REST endpoint that drives the thin loop: an external
//! client `POST`s a turn and gets the answer back. It is host-side (not a wasm
//! guest) by design — the loop it drives is host mechanism, and the wasip2 sandbox
//! grants no inbound sockets. Synchronous/blocking (`tiny_http`) fits the sync loop
//! and the `!Send` Wasmtime-backed [`AgentSession`]: one request is served at a
//! time, in the thread that owns the session.
//!
//! v1 routes: `POST /turn` request→response JSON (`{ "session", "message" }` →
//! `{ "answer", "agentic" }`) and `GET /health` (liveness for the blue/green
//! supervisor). Server-Sent-Events streaming of `next-event` is a follow-up once
//! the streaming run-handle lands.

use tiny_http::{Header, Method, Response, Server};

use crate::conductor::RunResult;
use crate::AgentSession;

/// A ready HTTP reply: status code + JSON body.
pub struct Reply {
    /// HTTP status code.
    pub status: u16,
    /// JSON body.
    pub body: String,
}

/// Handle one turn request body, driving the loop and returning the JSON reply.
///
/// The body is `{ "session"?, "message" }`; `session` defaults to `"default"`.
/// Pure over the body, so it is unit-testable without a socket.
#[must_use]
pub fn handle_turn(agent: &mut AgentSession, body: &str) -> Reply {
    let value: serde_json::Value = match serde_json::from_str(body) {
        Ok(value) => value,
        Err(err) => return error_reply(400, &format!("invalid JSON body: {err}")),
    };
    let Some(message) = value.get("message").and_then(serde_json::Value::as_str) else {
        return error_reply(400, "missing string field `message`");
    };
    let session = value
        .get("session")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("default");

    match agent.run(session, message) {
        RunResult::Answered { text, agentic } => Reply {
            status: 200,
            body: serde_json::json!({ "answer": text, "agentic": agentic }).to_string(),
        },
        RunResult::Failed(reason) => error_reply(502, &reason),
    }
}

/// Serve one request from `server` through `agent`, then respond. Returns after a
/// single request (the unit the accept loop repeats), so a test can drive exactly
/// one round-trip.
///
/// # Errors
/// Returns the underlying I/O error if the request cannot be received, read, or
/// answered.
pub fn serve_once(server: &Server, agent: &mut AgentSession) -> std::io::Result<()> {
    let mut request = server.recv()?;
    let is_health = *request.method() == Method::Get && request.url().starts_with("/health");
    let reply = if is_health {
        health()
    } else if *request.method() == Method::Post {
        let mut body = String::new();
        request.as_reader().read_to_string(&mut body)?;
        handle_turn(agent, &body)
    } else {
        error_reply(405, "use POST /turn or GET /health")
    };
    let response = Response::from_string(reply.body)
        .with_status_code(reply.status)
        .with_header(json_content_type());
    request.respond(response)
}

/// Serve requests forever (the accept loop). Blocks the calling thread; one turn
/// is handled at a time.
///
/// # Errors
/// Propagates the first I/O error from [`serve_once`].
pub fn serve(server: &Server, agent: &mut AgentSession) -> std::io::Result<()> {
    loop {
        serve_once(server, agent)?;
    }
}

/// The liveness reply for `GET /health` — a cheap 200 the supervisor probes after
/// a blue/green flip to confirm the swapped core booted and can serve.
#[must_use]
pub fn health() -> Reply {
    Reply {
        status: 200,
        body: serde_json::json!({ "status": "ok", "version": env!("CARGO_PKG_VERSION") }).to_string(),
    }
}

fn error_reply(status: u16, message: &str) -> Reply {
    Reply {
        status,
        body: serde_json::json!({ "error": message }).to_string(),
    }
}

fn json_content_type() -> Header {
    // Infallible for this constant; fall back to an empty header if it ever isn't.
    Header::from_bytes(&b"Content-Type"[..], &b"application/json"[..])
        .unwrap_or_else(|()| Header::from_bytes(&b"X-Content"[..], &b"json"[..]).expect("static header"))
}

#[cfg(test)]
mod tests {
    use super::health;

    #[test]
    fn health_reports_ok_with_a_version() {
        let reply = health();
        assert_eq!(reply.status, 200);
        assert!(reply.body.contains("\"status\":\"ok\""), "{}", reply.body);
        assert!(reply.body.contains(env!("CARGO_PKG_VERSION")), "{}", reply.body);
    }
}
