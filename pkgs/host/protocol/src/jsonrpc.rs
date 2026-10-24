//! The JSON-RPC 2.0 envelope that transports wrap a command or notification in.

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
/// The client's protocol version is incompatible; see [`incompatible_version`].
pub const INCOMPATIBLE_VERSION: i64 = -32000;

/// A request id, echoed back on the response; both spellings the spec allows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Id {
    /// A numeric id.
    Number(i64),
    /// A string id.
    Text(String),
    /// No id could be read from the frame; answered with `null`. A client must not send this.
    Null,
}

/// One command, framed: a request that expects a [`Response`] carrying this id.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Request {
    /// Always [`VERSION`]; anything else is [`INVALID_REQUEST`].
    pub jsonrpc: String,
    /// Answered with this same id.
    pub id: Id,
    /// The command's `method` and `params`, flattened.
    #[serde(flatten)]
    pub command: crate::Command,
}

impl Request {
    /// Frames `command` as a request with `id`.
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
    /// Frames `notification` for sending.
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
    /// Either a result or an error.
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

/// What a [`Response`] carries; an enum so it cannot have both or neither of `result`/`error`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Outcome {
    /// The command succeeded; the payload shape depends on the command.
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

    /// The same error, carrying `data`.
    #[must_use]
    pub fn with_data(mut self, data: serde_json::Value) -> Self {
        self.data = Some(data);
        self
    }
}

/// The error answering an incompatible hello; the core's version is in `data`, as the client gets no hello result.
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
