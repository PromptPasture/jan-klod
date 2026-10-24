//! `provider-openai` — OpenAI-compatible implementation of `llm-provider`.
//! Reference network extension; one build serves every OpenAI-compatible endpoint via `base-url`, `api-key` and `model`.

#[allow(
    unsafe_code,
    missing_docs,
    clippy::all,
    clippy::pedantic,
    clippy::nursery
)]
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
use bindings::jan_klod::interfaces::host_secrets;

/// Resolved endpoint settings, read once at `init`.
#[derive(Clone, Default)]
struct ProviderConfig {
    /// API root, e.g. `https://api.openai.com/v1`.
    base_url: String,
    /// Bearer token; empty for keyless local endpoints.
    api_key: String,
    /// Default model when a request omits one.
    model: String,
}

thread_local! {
    static CONFIG: RefCell<ProviderConfig> = RefCell::new(ProviderConfig::default());
    /// Open streams: handle -> chunks.
    static STREAMS: RefCell<HashMap<StreamHandle, VecDeque<CompletionChunk>>> =
        RefCell::new(HashMap::new());
    /// Stream handle source (starts at 1; 0 reserved).
    static NEXT_HANDLE: RefCell<StreamHandle> = const { RefCell::new(1) };
}

fn log(level: LogLevel, message: &str) {
    host_log::log(level, "provider-openai", message, &[]);
}

/// Read a string field out of the parsed config section (absent -> empty).
fn config_str(section: &Value, key: &str) -> String {
    section
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned()
}

/// Wire name for a conversation role.
const fn role_str(role: Role) -> &'static str {
    match role {
        Role::System => "system",
        Role::User => "user",
        Role::Assistant => "assistant",
        Role::Tool => "tool",
    }
}

/// One conversation message in `OpenAI` wire shape.
fn message_to_json(msg: &Message) -> Value {
    let mut obj = Map::new();
    obj.insert("role".to_owned(), json!(role_str(msg.role)));
    obj.insert("content".to_owned(), json!(msg.content));
    if let Some(id) = &msg.tool_call_id {
        obj.insert("tool_call_id".to_owned(), json!(id));
    }
    Value::Object(obj)
}

/// Convert a tool to `OpenAI`'s `{type:"function", function:{…}}` shape.
fn tool_to_json(tool: &ToolDefinition) -> Value {
    let parameters: Value =
        serde_json::from_str(&tool.parameters_schema).unwrap_or_else(|_| json!({}));
    json!({
        "type": "function",
        "function": {
            "name": tool.name,
            "description": tool.description,
            "parameters": parameters,
        }
    })
}

/// Build the Chat Completions body. `stream` is false: `host-http` buffers the whole response anyway.
fn build_request_body(model: &str, request: &CompletionRequest) -> Value {
    let mut obj = Map::new();
    obj.insert("model".to_owned(), json!(model));
    obj.insert(
        "messages".to_owned(),
        json!(request
            .messages
            .iter()
            .map(message_to_json)
            .collect::<Vec<_>>()),
    );
    obj.insert("stream".to_owned(), json!(false));
    if !request.tools.is_empty() {
        obj.insert(
            "tools".to_owned(),
            json!(request.tools.iter().map(tool_to_json).collect::<Vec<_>>()),
        );
    }
    if let Some(max) = request.max_tokens {
        obj.insert("max_tokens".to_owned(), json!(max));
    }
    if let Some(temp) = request.temperature {
        obj.insert("temperature".to_owned(), json!(temp));
    }
    if let Some(grammar) = &request.grammar {
        obj.insert("grammar".to_owned(), json!(grammar));
    }
    Value::Object(obj)
}

/// Parse a Chat Completions response into an ordered chunk stream.
fn parse_response(body: &[u8]) -> Result<VecDeque<CompletionChunk>, ProviderError> {
    let value: Value = serde_json::from_slice(body).map_err(|_| ProviderError::Transient)?;
    let choice = value
        .get("choices")
        .and_then(|choices| choices.get(0))
        .ok_or(ProviderError::Transient)?;
    let message = choice.get("message").ok_or(ProviderError::Transient)?;

    let mut chunks = VecDeque::new();
    if let Some(content) = message.get("content").and_then(Value::as_str) {
        if !content.is_empty() {
            chunks.push_back(CompletionChunk::TextDelta(content.to_owned()));
        }
    }
    if let Some(tool_calls) = message.get("tool_calls").and_then(Value::as_array) {
        for call in tool_calls {
            let function = call.get("function");
            chunks.push_back(CompletionChunk::ToolCallRequest(ToolCall {
                id: str_field(call, "id"),
                name: function.map_or_else(String::new, |f| str_field(f, "name")),
                arguments: function.map_or_else(String::new, |f| str_field(f, "arguments")),
            }));
        }
    }

    let finish = choice
        .get("finish_reason")
        .and_then(Value::as_str)
        .unwrap_or("stop")
        .to_owned();
    chunks.push_back(CompletionChunk::Done(finish));
    Ok(chunks)
}

/// Read `obj[key]` as an owned string, defaulting to empty.
fn str_field(obj: &Value, key: &str) -> String {
    obj.get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned()
}

/// Describe a `host-http` error in words, not a Rust identifier (e.g. `HttpError::ConnectionFailed`).
const fn describe_http(err: HttpError) -> &'static str {
    match err {
        HttpError::ConnectionFailed => {
            "the endpoint refused the connection or could not be resolved"
        }
        HttpError::Timeout => "the endpoint did not answer in time",
        HttpError::TlsError => "TLS negotiation failed",
        HttpError::InvalidUrl => "the configured URL could not be parsed",
        HttpError::ClientError(status) => match status {
            401 | 403 => "the endpoint rejected the credentials",
            404 => "the endpoint has no such model or route",
            429 => "the endpoint is rate-limiting requests",
            _ => "the endpoint rejected the request",
        },
        HttpError::ServerError(_) => "the endpoint reported an internal error",
        HttpError::Backend => "the host could not complete the request",
    }
}

const fn map_http_error(err: HttpError) -> ProviderError {
    match err {
        HttpError::ClientError(401 | 403) => ProviderError::AuthFailed,
        HttpError::ClientError(404) => ProviderError::ModelNotFound,
        HttpError::ClientError(429) => ProviderError::RateLimited,
        HttpError::ConnectionFailed | HttpError::Timeout | HttpError::TlsError => {
            ProviderError::Unreachable
        }
        HttpError::ClientError(_)
        | HttpError::ServerError(_)
        | HttpError::InvalidUrl
        | HttpError::Backend => ProviderError::Transient,
    }
}

/// Allocate the next stream handle; wrapping is accepted.
fn next_handle() -> StreamHandle {
    NEXT_HANDLE.with(|h| {
        let mut n = h.borrow_mut();
        let current = *n;
        *n = n.wrapping_add(1);
        current
    })
}

/// The single type implementing every interface `provider-world` exports.
struct Component;

impl Lifecycle for Component {
    fn init(ctx: ExtensionContext) -> Result<(), String> {
        let raw = host_config::all().unwrap_or_else(|_| "{}".to_owned());
        let section: Value = serde_json::from_str(&raw).unwrap_or(Value::Null);

        let api_key = section
            .get("api-key-secret")
            .and_then(Value::as_str)
            .map_or_else(
                || config_str(&section, "api-key"),
                |secret_name| {
                    host_secrets::get(secret_name)
                        .unwrap_or_else(|_| config_str(&section, "api-key"))
                },
            );

        let config = ProviderConfig {
            base_url: config_str(&section, "base-url"),
            api_key,
            model: config_str(&section, "model"),
        };
        if config.base_url.is_empty() {
            return Err("provider-openai: `base-url` is required".to_owned());
        }
        log(
            LogLevel::Info,
            &format!(
                "init id={} version={} base-url={} model={}",
                ctx.id,
                ctx.version,
                config.base_url,
                if config.model.is_empty() {
                    "<none>"
                } else {
                    &config.model
                },
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
        let model = if request.model.is_empty() {
            config.model.clone()
        } else {
            request.model.clone()
        };
        if model.is_empty() {
            log(LogLevel::Error, "complete: no model in request or config");
            return Err(ProviderError::ModelNotFound);
        }

        let payload = build_request_body(&model, &request);
        let body = serde_json::to_vec(&payload).map_err(|_| ProviderError::Transient)?;

        let mut headers = vec![HttpHeader {
            name: "Content-Type".to_owned(),
            value: "application/json".to_owned(),
        }];
        if !config.api_key.is_empty() {
            headers.push(HttpHeader {
                name: "Authorization".to_owned(),
                value: format!("Bearer {}", config.api_key),
            });
        }

        let http_request = HttpRequest {
            method: "POST".to_owned(),
            url: format!("{}/chat/completions", config.base_url.trim_end_matches('/')),
            headers,
            body: Some(body),
            timeout_ms: 0,
        };

        let response = host_http::fetch(&http_request).map_err(|err| {
            log(
                LogLevel::Warn,
                &format!("request failed: {}", describe_http(err)),
            );
            map_http_error(err)
        })?;
        let chunks = parse_response(&response.body)?;

        let handle = next_handle();
        STREAMS.with(|s| s.borrow_mut().insert(handle, chunks));
        Ok(handle)
    }

    fn next_chunk(handle: StreamHandle) -> Option<CompletionChunk> {
        STREAMS.with(|s| {
            s.borrow_mut()
                .get_mut(&handle)
                .and_then(VecDeque::pop_front)
        })
    }

    fn close_stream(handle: StreamHandle) {
        STREAMS.with(|s| {
            s.borrow_mut().remove(&handle);
        });
    }

    fn info() -> ProviderInfo {
        let config = CONFIG.with(|c| c.borrow().clone());
        let supported_models = if config.model.is_empty() {
            Vec::new()
        } else {
            vec![config.model]
        };
        ProviderInfo {
            id: "openai".to_owned(),
            supported_models,
        }
    }
}

#[allow(
    unsafe_code,
    missing_docs,
    clippy::all,
    clippy::pedantic,
    clippy::nursery
)]
mod glue {
    use crate::{bindings, Component};
    bindings::export!(Component with_types_in bindings);
}
