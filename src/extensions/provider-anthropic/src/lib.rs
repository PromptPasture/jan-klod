//! `provider-anthropic` — native Anthropic Messages API implementation of the
//! `llm-provider` interface.
//!
//! Calls `/v1/messages` via `host-http`. Handles:
//! - System messages → top-level `system` field (not in the messages array).
//! - Tool-result messages → `{"role":"user","content":[{"type":"tool_result",…}]}`.
//! - Tool-use blocks → `CompletionChunk::ToolCallRequest`.
//! - `401/403` → `AuthFailed`, `429` → `RateLimited`.
//! - `api-key` is never logged.

#[allow(unsafe_code, missing_docs, clippy::all, clippy::pedantic, clippy::nursery)]
mod bindings {
    wit_bindgen::generate!({
        world: "provider-world",
        path: "../../../wit",
    });
}

use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};

use serde_json::{json, Map, Value};

use bindings::exports::jan_klod::interfaces::extension_lifecycle::{
    ExtensionContext, Guest as Lifecycle, HealthStatus,
};
use bindings::exports::jan_klod::interfaces::llm_provider::{
    CompletionChunk, CompletionRequest, Guest as LlmProvider, Message, ProviderError, ProviderInfo,
    Role, StreamHandle, ToolCall, ToolDefinition,
};
use bindings::jan_klod::interfaces::host_config;
use bindings::jan_klod::interfaces::host_http::{self, HttpError, HttpHeader, HttpRequest};
use bindings::jan_klod::interfaces::host_log::{self, LogLevel};

/// Anthropic API version header value.
const ANTHROPIC_VERSION: &str = "2023-06-01";
/// Fallback max-tokens when the request omits it (Anthropic requires the field).
const DEFAULT_MAX_TOKENS: u32 = 8192;

#[derive(Clone, Default)]
struct ProviderConfig {
    api_key: String,
    model: String,
}

thread_local! {
    static CONFIG: RefCell<ProviderConfig> = RefCell::new(ProviderConfig::default());
    static STREAMS: RefCell<HashMap<StreamHandle, VecDeque<CompletionChunk>>> =
        RefCell::new(HashMap::new());
    static NEXT_HANDLE: RefCell<StreamHandle> = const { RefCell::new(1) };
}

fn log(level: LogLevel, message: &str) {
    host_log::log(level, "provider-anthropic", message, &[]);
}

fn config_str(section: &Value, key: &str) -> String {
    section.get(key).and_then(Value::as_str).unwrap_or_default().to_owned()
}

fn next_handle() -> StreamHandle {
    NEXT_HANDLE.with(|h| {
        let mut n = h.borrow_mut();
        let current = *n;
        *n = n.wrapping_add(1);
        current
    })
}

/// Read `obj[key]` as an owned string, defaulting to empty.
fn str_field(obj: &Value, key: &str) -> String {
    obj.get(key).and_then(Value::as_str).unwrap_or_default().to_owned()
}

/// Convert one WIT message to Anthropic API JSON.
///
/// System messages must be extracted separately (see `build_request_body`).
/// Tool-result messages become `{"role":"user","content":[{"type":"tool_result",…}]}`.
fn message_to_json(msg: &Message) -> Value {
    match msg.role {
        Role::System => {
            // Should have been stripped out before this is called; include as user
            // fallback so the conversation still makes sense.
            json!({"role": "user", "content": msg.content})
        }
        Role::User => json!({"role": "user", "content": msg.content}),
        Role::Assistant => json!({"role": "assistant", "content": msg.content}),
        Role::Tool => {
            let tool_use_id = msg.tool_call_id.clone().unwrap_or_default();
            json!({
                "role": "user",
                "content": [{
                    "type": "tool_result",
                    "tool_use_id": tool_use_id,
                    "content": msg.content,
                }]
            })
        }
    }
}

/// Convert a WIT `ToolDefinition` to Anthropic's `{name, description, input_schema}`.
fn tool_to_json(tool: &ToolDefinition) -> Value {
    let input_schema: Value =
        serde_json::from_str(&tool.parameters_schema).unwrap_or_else(|_| json!({}));
    json!({
        "name": tool.name,
        "description": tool.description,
        "input_schema": input_schema,
    })
}

/// Build the Anthropic `/v1/messages` request body.
///
/// Extracts the first `Role::System` message into the top-level `system` field;
/// all others are serialised in order into `messages`.
fn build_request_body(model: &str, request: &CompletionRequest) -> Value {
    let mut obj = Map::new();
    obj.insert("model".to_owned(), json!(model));
    obj.insert(
        "max_tokens".to_owned(),
        json!(request.max_tokens.unwrap_or(DEFAULT_MAX_TOKENS)),
    );

    // Extract system prompt (Anthropic puts it at the top level, not in messages).
    let system: Option<String> = request
        .messages
        .iter()
        .find(|m| m.role == Role::System)
        .map(|m| m.content.clone());
    if let Some(sys) = system {
        obj.insert("system".to_owned(), json!(sys));
    }

    let messages: Vec<Value> = request
        .messages
        .iter()
        .filter(|m| m.role != Role::System)
        .map(message_to_json)
        .collect();
    obj.insert("messages".to_owned(), json!(messages));

    if !request.tools.is_empty() {
        obj.insert(
            "tools".to_owned(),
            json!(request.tools.iter().map(tool_to_json).collect::<Vec<_>>()),
        );
    }
    if let Some(temp) = request.temperature {
        obj.insert("temperature".to_owned(), json!(temp));
    }

    Value::Object(obj)
}

/// Parse an Anthropic `/v1/messages` response into a chunk queue.
fn parse_response(body: &[u8]) -> Result<VecDeque<CompletionChunk>, ProviderError> {
    let value: Value = serde_json::from_slice(body).map_err(|_| ProviderError::Transient)?;

    // Surface API-level errors embedded in a 200 body (rare but possible).
    if value.get("type").and_then(Value::as_str) == Some("error") {
        let kind = value
            .pointer("/error/type")
            .and_then(Value::as_str)
            .unwrap_or("");
        return Err(match kind {
            "authentication_error" | "permission_error" => ProviderError::AuthFailed,
            "rate_limit_error" => ProviderError::RateLimited,
            _ => ProviderError::Transient,
        });
    }

    let content = value
        .get("content")
        .and_then(Value::as_array)
        .ok_or(ProviderError::Transient)?;

    let mut chunks = VecDeque::new();
    for block in content {
        match block.get("type").and_then(Value::as_str) {
            Some("text") => {
                let text = str_field(block, "text");
                if !text.is_empty() {
                    chunks.push_back(CompletionChunk::TextDelta(text));
                }
            }
            Some("tool_use") => {
                let input = block
                    .get("input")
                    .map(|v| {
                        if v.is_string() {
                            v.as_str().unwrap_or_default().to_owned()
                        } else {
                            v.to_string()
                        }
                    })
                    .unwrap_or_default();
                chunks.push_back(CompletionChunk::ToolCallRequest(ToolCall {
                    id: str_field(block, "id"),
                    name: str_field(block, "name"),
                    arguments: input,
                }));
            }
            _ => {}
        }
    }

    let stop_reason = value
        .get("stop_reason")
        .and_then(Value::as_str)
        .unwrap_or("end_turn")
        .to_owned();
    // Normalise to the common finish-reason vocabulary the core expects.
    let finish = match stop_reason.as_str() {
        "tool_use" => "tool-calls",
        "max_tokens" => "length",
        _ => "stop",
    };
    chunks.push_back(CompletionChunk::Done(finish.to_owned()));
    Ok(chunks)
}

const fn map_http_error(err: HttpError) -> ProviderError {
    match err {
        HttpError::ClientError(401 | 403) => ProviderError::AuthFailed,
        HttpError::ClientError(404) => ProviderError::ModelNotFound,
        HttpError::ClientError(429) => ProviderError::RateLimited,
        // Unreachable, not transient: a refused connection, a DNS failure or a
        // TLS error means the address is wrong, the server is not running, or
        // egress is not granted for it. None of those is fixed by retrying, and
        // calling them "transient" sends the reader looking for a flake.
        HttpError::ConnectionFailed | HttpError::Timeout | HttpError::TlsError => {
            ProviderError::Unreachable
        }
        HttpError::ClientError(_)
        | HttpError::ServerError(_)
        | HttpError::InvalidUrl
        | HttpError::Backend => ProviderError::Transient,
    }
}

struct Component;

impl Lifecycle for Component {
    fn init(ctx: ExtensionContext) -> Result<(), String> {
        let raw = host_config::all().unwrap_or_else(|_| "{}".to_owned());
        let section: Value = serde_json::from_str(&raw).unwrap_or(Value::Null);
        let config = ProviderConfig {
            api_key: config_str(&section, "api-key"),
            model: config_str(&section, "model"),
        };
        // Never log the api-key.
        log(
            LogLevel::Info,
            &format!(
                "init id={} version={} model={}",
                ctx.id,
                ctx.version,
                if config.model.is_empty() { "<none>" } else { &config.model },
            ),
        );
        CONFIG.with(|c| *c.borrow_mut() = config);
        Ok(())
    }

    fn start() -> Result<(), String> {
        log(LogLevel::Info, "started; ready to serve completions");
        Ok(())
    }

    fn stop() {
        log(LogLevel::Info, "stopping; dropping open streams");
        STREAMS.with(|s| s.borrow_mut().clear());
    }

    fn health() -> HealthStatus {
        HealthStatus::Up
    }
}

impl LlmProvider for Component {
    fn complete(request: CompletionRequest) -> Result<StreamHandle, ProviderError> {
        let config = CONFIG.with(|c| c.borrow().clone());
        let model = if request.model.is_empty() { config.model.clone() } else { request.model.clone() };
        if model.is_empty() {
            log(LogLevel::Error, "complete: no model in request or config");
            return Err(ProviderError::ModelNotFound);
        }

        let payload = build_request_body(&model, &request);
        let body = serde_json::to_vec(&payload).map_err(|_| ProviderError::Transient)?;

        let mut headers = vec![
            HttpHeader { name: "Content-Type".to_owned(), value: "application/json".to_owned() },
            HttpHeader { name: "anthropic-version".to_owned(), value: ANTHROPIC_VERSION.to_owned() },
        ];
        if !config.api_key.is_empty() {
            headers.push(HttpHeader { name: "x-api-key".to_owned(), value: config.api_key });
        }

        let http_request = HttpRequest {
            method: "POST".to_owned(),
            url: "https://api.anthropic.com/v1/messages".to_owned(),
            headers,
            body: Some(body),
            timeout_ms: 0,
        };

        let response = host_http::fetch(&http_request).map_err(|err| {
            log(LogLevel::Warn, &format!("http error: {err:?}"));
            map_http_error(err)
        })?;
        let chunks = parse_response(&response.body)?;

        let handle = next_handle();
        STREAMS.with(|s| s.borrow_mut().insert(handle, chunks));
        Ok(handle)
    }

    fn next_chunk(handle: StreamHandle) -> Option<CompletionChunk> {
        STREAMS.with(|s| s.borrow_mut().get_mut(&handle).and_then(VecDeque::pop_front))
    }

    fn close_stream(handle: StreamHandle) {
        STREAMS.with(|s| {
            s.borrow_mut().remove(&handle);
        });
    }

    fn info() -> ProviderInfo {
        let config = CONFIG.with(|c| c.borrow().clone());
        let supported_models =
            if config.model.is_empty() { Vec::new() } else { vec![config.model] };
        ProviderInfo { id: "anthropic".to_owned(), supported_models }
    }
}

#[allow(unsafe_code, missing_docs, clippy::all, clippy::pedantic, clippy::nursery)]
mod glue {
    use crate::{bindings, Component};
    bindings::export!(Component with_types_in bindings);
}
