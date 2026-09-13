//! Component test harness — load a staged guest, wire host capabilities, and
//! verify its WIT interface end-to-end through the Component Model, offline and
//! deterministically (unlike `provider_probe`, which drives a live endpoint). It
//! instantiates each category world, backs imports with a reusable [`TestHost`]
//! (config section, captured logs, a canned `host-http`), then runs lifecycle
//! plus the guest's own interface.
//!
//! Each test skips with a note when its component is not staged in `ext/`, so a
//! bare `cargo test` (no guests built) stays green. Build the guests first
//! (`make extensions`) or run the bundled target (`make harness`) to exercise
//! them.

// Dominated by `bindgen!`-generated code; exempt from the workspace lints, as the
// sibling `provider_probe` example is.
#![allow(missing_docs, clippy::all, clippy::pedantic, clippy::nursery)]

use std::path::PathBuf;

use jan_klod_core::ConfigSection;
use serde_json::json;
use wasmtime::component::{Component, HasSelf, Linker};
use wasmtime::{Engine, Result, Store};
use wasmtime_wasi::{ResourceTable, WasiCtx, WasiCtxBuilder, WasiCtxView, WasiView};

use crate::common;

/// `provider-world`: lifecycle + `llm-provider`, also imports `host-http`.
mod provider_bind {
    wasmtime::component::bindgen!({
        path: "../../wit",
        world: "provider-world",
    });
}

/// A canned `host-http` reply. The harness mirrors the host boundary in
/// [`jan_klod_core::http`]: 5xx -> `server-error`, 4xx -> `client-error`,
/// everything else -> a successful response. One mock serves both the happy path
/// and error-mapping tests.
#[derive(Clone)]
struct MockHttp {
    status: u16,
    body: Vec<u8>,
}

/// Reusable host backing every guest's imports in one struct: its config section,
/// a log buffer the tests can assert against, and the canned HTTP reply.
struct TestHost {
    wasi: WasiCtx,
    table: ResourceTable,
    section: ConfigSection,
    logs: Vec<String>,
    http: MockHttp,
}

impl TestHost {
    fn new(section: serde_json::Value, http: MockHttp) -> Self {
        Self {
            wasi: WasiCtxBuilder::new().inherit_stdio().build(),
            table: ResourceTable::new(),
            section: ConfigSection::new(section),
            logs: Vec::new(),
            http,
        }
    }
}

impl WasiView for TestHost {
    fn ctx(&mut self) -> WasiCtxView<'_> {
        WasiCtxView {
            ctx: &mut self.wasi,
            table: &mut self.table,
        }
    }
}

// --- host-log: capture lines for assertions (one impl per generated world) ---

impl provider_bind::jan_klod::interfaces::host_log::Host for TestHost {
    fn log(
        &mut self,
        _level: provider_bind::jan_klod::interfaces::host_log::LogLevel,
        component: String,
        message: String,
        _fields: Vec<provider_bind::jan_klod::interfaces::host_log::LogField>,
    ) {
        self.logs.push(format!("{component}: {message}"));
    }
}

// --- host-config: serve the instance section (one impl per generated world) ---

impl provider_bind::jan_klod::interfaces::host_config::Host for TestHost {
    fn get(
        &mut self,
        key: String,
    ) -> std::result::Result<String, provider_bind::jan_klod::interfaces::host_config::ConfigError>
    {
        self.section
            .get(&key)
            .ok_or(provider_bind::jan_klod::interfaces::host_config::ConfigError::KeyNotFound)
    }
    fn has(&mut self, key: String) -> bool {
        self.section.has(&key)
    }
    fn all(
        &mut self,
    ) -> std::result::Result<String, provider_bind::jan_klod::interfaces::host_config::ConfigError>
    {
        Ok(self.section.all())
    }
}

// --- host-http: canned reply, split into ok/err the way the real host does ---

impl provider_bind::jan_klod::interfaces::host_http::Host for TestHost {
    fn fetch(
        &mut self,
        _request: provider_bind::jan_klod::interfaces::host_http::HttpRequest,
    ) -> std::result::Result<
        provider_bind::jan_klod::interfaces::host_http::HttpResponse,
        provider_bind::jan_klod::interfaces::host_http::HttpError,
    > {
        use provider_bind::jan_klod::interfaces::host_http::{HttpError, HttpResponse};
        let MockHttp { status, body } = self.http.clone();
        if status >= 500 {
            Err(HttpError::ServerError(status))
        } else if status >= 400 {
            Err(HttpError::ClientError(status))
        } else {
            Ok(HttpResponse {
                status,
                headers: vec![],
                body,
            })
        }
    }
}

/// Resolve a staged guest at `<repo>/ext/<file>`, relative to this crate. Returns
/// `None` (with a skip note) when the component is absent, so an unbuilt tree
/// still passes.
fn staged_component(engine: &Engine, file: &str) -> Option<Component> {
    let path: PathBuf = [env!("CARGO_MANIFEST_DIR"), "..", "..", "ext", file]
        .iter()
        .collect();
    if !common::guests_staged(&[file]) {
        return None;
    }
    Some(Component::from_file(engine, &path).expect("staged component should compile"))
}

#[test]
fn provider_openai_complete_streams_text() -> Result<()> {
    use provider_bind::exports::jan_klod::interfaces::extension_lifecycle::ExtensionContext;
    use provider_bind::exports::jan_klod::interfaces::llm_provider::{
        CompletionChunk, CompletionRequest, Message, Role,
    };
    use provider_bind::jan_klod::interfaces::{host_config, host_http, host_log};
    use provider_bind::ProviderWorld;

    let engine = Engine::default();
    let Some(component) = staged_component(&engine, "provider-openai.wasm") else {
        return Ok(());
    };

    // A canned, non-streaming Chat Completions reply.
    let body = json!({
        "choices": [{
            "message": { "role": "assistant", "content": "pong" },
            "finish_reason": "stop"
        }]
    });
    let host = TestHost::new(
        json!({ "base-url": "http://mock/v1", "model": "mock-1", "api-key": "test" }),
        MockHttp {
            status: 200,
            body: serde_json::to_vec(&body).unwrap(),
        },
    );

    let mut linker: Linker<TestHost> = Linker::new(&engine);
    wasmtime_wasi::p2::add_to_linker_sync(&mut linker)?;
    host_log::add_to_linker::<_, HasSelf<_>>(&mut linker, |s| s)?;
    host_config::add_to_linker::<_, HasSelf<_>>(&mut linker, |s| s)?;
    host_http::add_to_linker::<_, HasSelf<_>>(&mut linker, |s| s)?;

    let mut store = Store::new(&engine, host);
    let world = ProviderWorld::instantiate(&mut store, &component, &linker)?;
    let lifecycle = world.jan_klod_interfaces_extension_lifecycle();
    let provider = world.jan_klod_interfaces_llm_provider();

    let ctx = ExtensionContext {
        id: "provider.openai".to_string(),
        version: "0.0.0".to_string(),
    };
    lifecycle
        .call_init(&mut store, &ctx)?
        .map_err(wasmtime::Error::msg)?;
    lifecycle
        .call_start(&mut store)?
        .map_err(wasmtime::Error::msg)?;

    let request = CompletionRequest {
        model: String::new(), // falls back to the configured model
        messages: vec![Message {
            role: Role::User,
            content: "ping".to_string(),
            tool_call_id: None,
        }],
        tools: vec![],
        grammar: None,
        max_tokens: Some(16),
        temperature: Some(0.0),
    };

    let handle = provider
        .call_complete(&mut store, &request)?
        .map_err(|e| wasmtime::Error::msg(format!("{e:?}")))?;

    // The canned body parses to exactly one text delta, closed by done("stop").
    let mut text = String::new();
    let mut done_reason = None;
    while let Some(chunk) = provider.call_next_chunk(&mut store, handle)? {
        match chunk {
            CompletionChunk::TextDelta(t) => text.push_str(&t),
            CompletionChunk::Done(reason) => {
                done_reason = Some(reason);
                break;
            }
            CompletionChunk::ToolCallRequest(call) => {
                panic!("unexpected tool call: {call:?}");
            }
        }
    }
    provider.call_close_stream(&mut store, handle)?;

    assert_eq!(text, "pong");
    assert_eq!(done_reason.as_deref(), Some("stop"));
    Ok(())
}

#[test]
fn provider_openai_maps_auth_error() -> Result<()> {
    use provider_bind::exports::jan_klod::interfaces::extension_lifecycle::ExtensionContext;
    use provider_bind::exports::jan_klod::interfaces::llm_provider::{
        CompletionRequest, Message, ProviderError, Role,
    };
    use provider_bind::jan_klod::interfaces::{host_config, host_http, host_log};
    use provider_bind::ProviderWorld;

    let engine = Engine::default();
    let Some(component) = staged_component(&engine, "provider-openai.wasm") else {
        return Ok(());
    };

    // A 401 at the host boundary becomes client-error(401); the guest must map it
    // to auth-failed.
    let host = TestHost::new(
        json!({ "base-url": "http://mock/v1", "model": "mock-1", "api-key": "bad" }),
        MockHttp {
            status: 401,
            body: vec![],
        },
    );

    let mut linker: Linker<TestHost> = Linker::new(&engine);
    wasmtime_wasi::p2::add_to_linker_sync(&mut linker)?;
    host_log::add_to_linker::<_, HasSelf<_>>(&mut linker, |s| s)?;
    host_config::add_to_linker::<_, HasSelf<_>>(&mut linker, |s| s)?;
    host_http::add_to_linker::<_, HasSelf<_>>(&mut linker, |s| s)?;

    let mut store = Store::new(&engine, host);
    let world = ProviderWorld::instantiate(&mut store, &component, &linker)?;
    let lifecycle = world.jan_klod_interfaces_extension_lifecycle();
    let provider = world.jan_klod_interfaces_llm_provider();

    let ctx = ExtensionContext {
        id: "provider.openai".to_string(),
        version: "0.0.0".to_string(),
    };
    lifecycle
        .call_init(&mut store, &ctx)?
        .map_err(wasmtime::Error::msg)?;
    lifecycle
        .call_start(&mut store)?
        .map_err(wasmtime::Error::msg)?;

    let request = CompletionRequest {
        model: String::new(),
        messages: vec![Message {
            role: Role::User,
            content: "ping".to_string(),
            tool_call_id: None,
        }],
        tools: vec![],
        grammar: None,
        max_tokens: Some(16),
        temperature: Some(0.0),
    };

    let result = provider.call_complete(&mut store, &request)?;
    assert!(
        matches!(result, Err(ProviderError::AuthFailed)),
        "401 should map to auth-failed, got {result:?}"
    );
    Ok(())
}

/// A reply carrying **both** a preamble and tool calls must yield both — a
/// parser that treats them as `if tool_calls ... else if content ...` silently
/// drops the preamble text that a model narrating before acting routinely sends.
#[test]
fn provider_openai_keeps_text_that_accompanies_tool_calls() -> Result<()> {
    use provider_bind::exports::jan_klod::interfaces::extension_lifecycle::ExtensionContext;
    use provider_bind::exports::jan_klod::interfaces::llm_provider::{
        CompletionChunk, CompletionRequest, Message, Role,
    };
    use provider_bind::jan_klod::interfaces::{host_config, host_http, host_log};
    use provider_bind::ProviderWorld;

    let engine = Engine::default();
    let Some(component) = staged_component(&engine, "provider-openai.wasm") else {
        return Ok(());
    };

    let body = json!({
        "choices": [{
            "message": {
                "role": "assistant",
                "content": "I'll read the file first.",
                "tool_calls": [{
                    "id": "call-1",
                    "function": { "name": "fs", "arguments": "{\"op\":\"read\"}" }
                }]
            },
            "finish_reason": "tool_calls"
        }]
    });
    let host = TestHost::new(
        json!({ "base-url": "http://mock/v1", "model": "mock-1", "api-key": "test" }),
        MockHttp {
            status: 200,
            body: serde_json::to_vec(&body).unwrap(),
        },
    );

    let mut linker: Linker<TestHost> = Linker::new(&engine);
    wasmtime_wasi::p2::add_to_linker_sync(&mut linker)?;
    host_log::add_to_linker::<_, HasSelf<_>>(&mut linker, |s| s)?;
    host_config::add_to_linker::<_, HasSelf<_>>(&mut linker, |s| s)?;
    host_http::add_to_linker::<_, HasSelf<_>>(&mut linker, |s| s)?;

    let mut store = Store::new(&engine, host);
    let world = ProviderWorld::instantiate(&mut store, &component, &linker)?;
    let lifecycle = world.jan_klod_interfaces_extension_lifecycle();
    let provider = world.jan_klod_interfaces_llm_provider();
    let ctx = ExtensionContext {
        id: "provider.openai".to_string(),
        version: "0.0.0".to_string(),
    };
    lifecycle
        .call_init(&mut store, &ctx)?
        .map_err(wasmtime::Error::msg)?;
    lifecycle
        .call_start(&mut store)?
        .map_err(wasmtime::Error::msg)?;

    let request = CompletionRequest {
        model: String::new(),
        messages: vec![Message {
            role: Role::User,
            content: "read it".to_string(),
            tool_call_id: None,
        }],
        tools: vec![],
        grammar: None,
        max_tokens: None,
        temperature: None,
    };
    let handle = provider
        .call_complete(&mut store, &request)?
        .map_err(|e| wasmtime::Error::msg(format!("{e:?}")))?;

    let mut text = String::new();
    let mut calls = Vec::new();
    let mut done_reason = None;
    while let Some(chunk) = provider.call_next_chunk(&mut store, handle)? {
        match chunk {
            CompletionChunk::TextDelta(t) => text.push_str(&t),
            CompletionChunk::ToolCallRequest(call) => calls.push(call.name),
            CompletionChunk::Done(reason) => {
                done_reason = Some(reason);
                break;
            }
        }
    }
    provider.call_close_stream(&mut store, handle)?;

    assert_eq!(
        text, "I'll read the file first.",
        "the preamble survives alongside the calls"
    );
    assert_eq!(calls, vec!["fs".to_string()], "the tool call still arrives");
    assert_eq!(done_reason.as_deref(), Some("tool_calls"));
    Ok(())
}
