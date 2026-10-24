//! The JSON-RPC 2.0 envelope the stdio and WebSocket transports wrap a
//! [`Command`](crate::Command) or [`Notification`](crate::Notification) in.
//! Lives here, not in the transport, so the core and every client share one frame type.
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

/// The JSON was not valid, so no `id` could be read.
pub const PARSE_ERROR: i64 = -32700;
/// Valid JSON, but not a well-formed request.
pub const INVALID_REQUEST: i64 = -32600;
/// No command carries this `method`.
pub const METHOD_NOT_FOUND: i64 = -32601;
/// The method exists but its `params` are wrong.
pub const INVALID_PARAMS: i64 = -32602;
/// The core failed while serving a command it understood.
pub const INTERNAL_ERROR: i64 = -32603;
/// Server-defined (spec reserves `-32000..=-32099`): the client's protocol
/// version is incompatible. See [`incompatible_version`].
pub const INCOMPATIBLE_VERSION: i64 = -32000;

/// A request id, echoed back on the response.
///
/// Both spellings the spec allows, since non-Rust clients generate from this schema.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Id {
    /// A numeric id, the usual choice.
    Number(i64),
    /// A string id.
    Text(String),
    /// No id could be read from the frame; answered with `null`. A client must not send this.
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
    /// The command's `method` and `params`, flattened (hence adjacent tagging).
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
/// An enum so a response with both or neither of `result`/`error` cannot be built.
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

/// The error answering an incompatible [`Command::Hello`] before closing.
///
/// The core's version travels in `data` because the client gets no
/// [`HelloResult`](crate::HelloResult) to learn it from.
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
