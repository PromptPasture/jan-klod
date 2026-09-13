//! `provider_probe` — drive a provider extension's full `llm-provider.complete`
//! path end-to-end against a live OpenAI-compatible endpoint.
//!
//! Instantiates `provider-world` directly, wires the same three host
//! capabilities the core grants — with a real `host-http` reusing
//! [`jan_klod_core::http`] — then issues one completion and prints the streamed
//! chunks.
//!
//! Usage: `provider_probe [config] [ext-dir] [prompt]`. Requires the provider's
//! api-key env (e.g. `OPENAI_API_KEY`) and network access — it makes a real,
//! token-costing call.

// Dominated by `bindgen!`-generated code; exempt from the workspace lints.
#![allow(missing_docs, clippy::all, clippy::pedantic, clippy::nursery)]

use jan_klod_config::Config;
use serde_json::Value;
use wasmtime::component::{Component, HasSelf, Linker};
use wasmtime::{Engine, Result, Store};
use wasmtime_wasi::{ResourceTable, WasiCtx, WasiCtxBuilder, WasiCtxView, WasiView};

wasmtime::component::bindgen!({
    path: "../../wit",
    world: "provider-world",
});

use exports::jan_klod::interfaces::extension_lifecycle::ExtensionContext;
use exports::jan_klod::interfaces::llm_provider::{
    CompletionChunk, CompletionRequest, Message, Role,
};
use jan_klod::interfaces::{host_config, host_http, host_log};

/// Minimal host backing `provider-world`'s imports for the probe.
struct ProbeHost {
    wasi: WasiCtx,
    table: ResourceTable,
    component_id: String,
    /// The provider instance's resolved config section (env already expanded).
    section: Value,
}

impl WasiView for ProbeHost {
    fn ctx(&mut self) -> WasiCtxView<'_> {
        WasiCtxView {
            ctx: &mut self.wasi,
            table: &mut self.table,
        }
    }
}

impl host_log::Host for ProbeHost {
    fn log(
        &mut self,
        level: host_log::LogLevel,
        component: String,
        message: String,
        _fields: Vec<host_log::LogField>,
    ) {
        let level = match level {
            host_log::LogLevel::Debug => "DEBUG",
            host_log::LogLevel::Info => "INFO",
            host_log::LogLevel::Warn => "WARN",
            host_log::LogLevel::Error => "ERROR",
        };
        eprintln!("{level} [{}] {component}: {message}", self.component_id);
    }
}

impl host_config::Host for ProbeHost {
    fn get(&mut self, key: String) -> Result<String, host_config::ConfigError> {
        self.section
            .get(&key)
            .map(Value::to_string)
            .ok_or(host_config::ConfigError::KeyNotFound)
    }

    fn has(&mut self, key: String) -> bool {
        self.section.get(&key).is_some()
    }

    fn all(&mut self) -> Result<String, host_config::ConfigError> {
        Ok(self.section.to_string())
    }
}

impl host_http::Host for ProbeHost {
    fn fetch(
        &mut self,
        request: host_http::HttpRequest,
    ) -> Result<host_http::HttpResponse, host_http::HttpError> {
        let headers: Vec<(String, String)> = request
            .headers
            .into_iter()
            .map(|h| (h.name, h.value))
            .collect();
        match jan_klod_core::http::fetch(
            &request.method,
            &request.url,
            &headers,
            request.body.as_deref(),
            request.timeout_ms,
        ) {
            Ok(response) => Ok(host_http::HttpResponse {
                status: response.status,
                headers: response
                    .headers
                    .into_iter()
                    .map(|(name, value)| host_http::HttpHeader { name, value })
                    .collect(),
                body: response.body,
            }),
            Err(err) => Err(wire_to_http(&err)),
        }
    }
}

fn wire_to_http(err: &jan_klod_core::http::WireError) -> host_http::HttpError {
    use jan_klod_core::http::WireError;
    match err {
        WireError::InvalidUrl => host_http::HttpError::InvalidUrl,
        WireError::ConnectionFailed => host_http::HttpError::ConnectionFailed,
        WireError::Timeout => host_http::HttpError::Timeout,
        WireError::TlsError => host_http::HttpError::TlsError,
        WireError::ClientError(code) => host_http::HttpError::ClientError(*code),
        WireError::ServerError(code) => host_http::HttpError::ServerError(*code),
        WireError::Backend => host_http::HttpError::Backend,
    }
}

fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let config_path = args.next().unwrap_or_else(|| "config.yaml".to_string());
    let ext_dir = args.next().unwrap_or_else(|| "ext".to_string());
    let prompt = args
        .next()
        .unwrap_or_else(|| "Reply with exactly one word: pong".to_string());

    // First enabled provider instance from the real config.
    let config = Config::from_path(&config_path)?;
    let instance = config
        .enabled()
        .find(|i| i.category == "provider")
        .ok_or_else(|| wasmtime::Error::msg("no enabled provider instance in config"))?
        .clone();
    let component_path = format!("{ext_dir}/{}", instance.component_file());

    let engine = Engine::default();
    let component = Component::from_file(&engine, &component_path)
        .map_err(|e| e.context(format!("loading {component_path}")))?;

    let mut linker: Linker<ProbeHost> = Linker::new(&engine);
    wasmtime_wasi::p2::add_to_linker_sync(&mut linker)?;
    host_log::add_to_linker::<_, HasSelf<_>>(&mut linker, |s| s)?;
    host_config::add_to_linker::<_, HasSelf<_>>(&mut linker, |s| s)?;
    host_http::add_to_linker::<_, HasSelf<_>>(&mut linker, |s| s)?;

    let mut store = Store::new(
        &engine,
        ProbeHost {
            wasi: WasiCtxBuilder::new().inherit_stdio().build(),
            table: ResourceTable::new(),
            component_id: instance.id.clone(),
            section: instance.config.clone(),
        },
    );

    let world = ProviderWorld::instantiate(&mut store, &component, &linker)?;
    let lifecycle = world.jan_klod_interfaces_extension_lifecycle();
    let provider = world.jan_klod_interfaces_llm_provider();

    let ctx = ExtensionContext {
        id: instance.id.clone(),
        version: "0.0.0".to_string(),
    };
    lifecycle
        .call_init(&mut store, &ctx)?
        .map_err(wasmtime::Error::msg)?;
    lifecycle
        .call_start(&mut store)?
        .map_err(wasmtime::Error::msg)?;

    let model = instance
        .config
        .get("model")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let request = CompletionRequest {
        model,
        messages: vec![Message {
            role: Role::User,
            content: prompt,
            tool_call_id: None,
        }],
        tools: vec![],
        grammar: None,
        max_tokens: Some(64),
        temperature: Some(0.0),
    };

    let handle = provider
        .call_complete(&mut store, &request)?
        .map_err(|e| wasmtime::Error::msg(format!("provider error: {e:?}")))?;

    print!("completion: ");
    loop {
        match provider.call_next_chunk(&mut store, handle)? {
            Some(CompletionChunk::TextDelta(text)) => print!("{text}"),
            Some(CompletionChunk::ToolCallRequest(call)) => {
                print!("[tool-call {} {}({})]", call.id, call.name, call.arguments);
            }
            Some(CompletionChunk::Done(reason)) => {
                println!("\n[done: {reason}]");
                break;
            }
            None => {
                println!("\n[stream ended without done]");
                break;
            }
        }
    }
    provider.call_close_stream(&mut store, handle)?;
    Ok(())
}
