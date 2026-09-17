//! The JSON-RPC 2.0 envelope the stdio and WebSocket transports both wrap a
//! [`Command`](crate::Command) or a [`Notification`](crate::Notification) in.
//!
//! # Why this is in the contract crate and not in the transport
//!
//! The crate docs used to say framing belongs to transport—right for one, wrong
//! for two parties. The core writes frames (`jan_klod_host::rpc`, Slice 13b)
//! and *every client* reads them; `jan-klod` (the TUI client) depends on neither
//! `jan-klod-core` nor Wasmtime by design. A frame type reachable only from the
//! core would be hand-rolled twice, the divergence this contract prevents. So
//! framing lives here, beside the commands it carries; the transport keeps
//! what is genuinely its own: the sockets, pipes, and read loop.
//!
//! JSON is not a choice being made here. This is the *JSON*-RPC envelope; a
//! payload whose type varies per command needs a value type for any description
//! at all, and every transport in the roadmap — stdio, WebSocket, MCP, ACP — is
//! JSON. The contract types in the crate root stay format-free.
//!
//! # Shapes
//!
//! ```text
//! request       {"jsonrpc":"2.0","id":1,"method":"session/list"}
//! notification  {"jsonrpc":"2.0","method":"done","params":{…}}
//! response      {"jsonrpc":"2.0","id":1,"result":{…}}
//!               {"jsonrpc":"2.0","id":1,"error":{"code":-32000,"message":"…"}}
//! ```

use serde::{Deserialize, Serialize};

/// The value of the `jsonrpc` member on every frame.
pub const VERSION: &str = "2.0";

/// The JSON was not valid, so no `id` could be read from it.
pub const PARSE_ERROR: i64 = -32700;
/// Valid JSON, but not a well-formed request: a wrong `jsonrpc`, a missing
/// `method`, or params that do not match the method named.
pub const INVALID_REQUEST: i64 = -32600;
/// No command carries this `method`.
pub const METHOD_NOT_FOUND: i64 = -32601;
/// The method exists but its `params` are wrong.
pub const INVALID_PARAMS: i64 = -32602;
/// The core failed while serving a command it understood.
pub const INTERNAL_ERROR: i64 = -32603;
/// Server-defined (the spec reserves `-32000..=-32099` for these): the client
/// speaks a protocol version this core cannot talk to. See
/// [`incompatible_version`].
pub const INCOMPATIBLE_VERSION: i64 = -32000;

/// A request id, echoed back on the response that answers it.
///
/// Both spellings the spec allows: a client sending string ids is conforming,
/// and a core rejecting them would not be. This crate's schema is what non-Rust
/// clients generate from, so what it accepts is what they will send.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Id {
    /// A numeric id, the usual choice.
    Number(i64),
    /// A string id.
    Text(String),
    /// No id could be read from the frame — JSON did not parse, or it parsed
    /// to something other than a request. The spec requires `null` in the
    /// answer, the one thing distinguishing it from an answer the client sent.
    /// A client must not *send* this.
    Null,
}

/// One command, framed: a request that expects a [`Response`] carrying this id.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Request {
    /// Always [`VERSION`]. A frame declaring anything else is
    /// [`INVALID_REQUEST`].
    pub jsonrpc: String,
    /// Answered with this same id.
    pub id: Id,
    /// The command's `method` and `params`, flattened into this frame — which
    /// is why [`Command`](crate::Command) is adjacently tagged.
    #[serde(flatten)]
    pub command: crate::Command,
}

impl Request {
    /// Frame `command` as a request answered against `id`.
    #[must_use]
    pub fn new(id: Id, command: crate::Command) -> Self {
        Self {
            jsonrpc: VERSION.to_owned(),
            id,
            command,
        }
    }
}

/// One notification, framed: no `id`, because nothing answers it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Notification {
    /// Always [`VERSION`].
    pub jsonrpc: String,
    /// What the core is reporting.
    #[serde(flatten)]
    pub notification: crate::Notification,
}

impl Notification {
    /// Frame `notification` for sending.
    #[must_use]
    pub fn new(notification: crate::Notification) -> Self {
        Self {
            jsonrpc: VERSION.to_owned(),
            notification,
        }
    }
}

/// The answer to a [`Request`], carrying its id.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Response {
    /// Always [`VERSION`].
    pub jsonrpc: String,
    /// The id of the request this answers.
    pub id: Id,
    /// Either a result or an error — see [`Outcome`].
    #[serde(flatten)]
    pub outcome: Outcome,
}

impl Response {
    /// A successful answer.
    #[must_use]
    pub fn result(id: Id, result: serde_json::Value) -> Self {
        Self {
            jsonrpc: VERSION.to_owned(),
            id,
            outcome: Outcome::Result(result),
        }
    }

    /// A failed answer.
    #[must_use]
    pub fn error(id: Id, error: Error) -> Self {
        Self {
            jsonrpc: VERSION.to_owned(),
            id,
            outcome: Outcome::Error(error),
        }
    }
}

/// What a [`Response`] carries.
///
/// An enum rather than two `Option` fields, so a response with both — or with
/// neither, which the spec forbids just as firmly — cannot be built. Serialized
/// externally tagged, which is exactly the spec's `result`/`error` member.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Outcome {
    /// The command succeeded. The payload's shape depends on the command.
    Result(serde_json::Value),
    /// The command failed.
    Error(Error),
}

/// A JSON-RPC error object.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Error {
    /// One of the constants in this module.
    pub code: i64,
    /// What went wrong, as a person should read it.
    pub message: String,
    /// Machine-readable detail, when there is any worth sending.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
}

impl Error {
    /// An error with no `data`.
    #[must_use]
    pub fn new(code: i64, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            data: None,
        }
    }

    /// The same error, carrying detail a client can act on.
    #[must_use]
    pub fn with_data(mut self, data: serde_json::Value) -> Self {
        self.data = Some(data);
        self
    }
}

/// The error a core answers an incompatible [`Command::Hello`] with, before
/// closing the connection.
///
/// The core's own version travels in `data` rather than only inside the
/// message: a refused handshake is the one exchange where the client learns
/// nothing else about what this core speaks, since it gets no
/// [`HelloResult`](crate::HelloResult) at all.
///
/// [`Command::Hello`]: crate::Command::Hello
#[must_use]
pub fn incompatible_version(client: &str) -> Error {
    Error::new(
        INCOMPATIBLE_VERSION,
        format!(
            "client protocol {client} cannot talk to core protocol {}",
            crate::PROTOCOL_VERSION
        ),
    )
    .with_data(serde_json::json!({ "version": crate::PROTOCOL_VERSION }))
}
