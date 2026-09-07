//! Provider instantiation and the conductor's provider adapter.
//!
//! The core lets one extension consume another's interface by instantiating the
//! implementing extension and delegating into it. In the thin-loop architecture
//! the only such consumer is the loop itself, which drives providers through the
//! conductor's [`Completer`](crate::conductor::Completer) trait — see
//! [`ProviderCompleter`]. The provider's `host-http` is injected as an [`HttpFn`]
//! so a turn can run live (real client) or offline (canned reply) without
//! changing the adapter.
//!
//! (This module previously also hand-wired the v0 `manager-agent-loop` guest via
//! `llm-provider` / `memory-store` routing; that path is retired — the loop is now
//! core mechanism in [`crate::conductor`], booted by `Runtime::build_agent`.)

use std::fmt::Write as _;

use wasmtime::component::{Component, HasSelf, Linker};
use wasmtime::{Engine, Store};
use wasmtime_wasi::{ResourceTable, WasiCtx, WasiCtxBuilder, WasiCtxView, WasiView};

use jan_klod_config::ExtensionInstance;

use crate::host::ConfigSection;
use crate::http::{WireError, WireResponse};
use crate::CoreError;

// Generated bindings for the provider world (to *call* a provider's exports and
// *satisfy* its host imports); lint exemptions scoped to the macro output.
#[allow(missing_docs, clippy::all, clippy::pedantic, clippy::nursery)]
mod provider_bind {
    wasmtime::component::bindgen!({
        path: "../../../wit",
        world: "provider-world",
    });
}

use provider_bind::exports::jan_klod::interfaces::llm_provider as p_llm;

/// Host-side HTTP backend injected into the provider's `host-http` import. The
/// signature mirrors [`crate::http::fetch`], so a live caller passes that
/// function and a test passes a canned closure.
pub type HttpFn = Box<
    dyn Fn(&str, &str, &[(String, String)], Option<&[u8]>, u32) -> Result<WireResponse, WireError>
        + Send
        + Sync,
>;

// ---------------------------------------------------------------------------
// CapHost — host state for a provider instance: WASI + the provider-world host
// capabilities, with an *injected* HTTP backend so a turn can run offline.
// ---------------------------------------------------------------------------

struct CapHost {
    wasi: WasiCtx,
    table: ResourceTable,
    component_id: String,
    section: ConfigSection,
    http: HttpFn,
}

impl CapHost {
    fn new(component_id: impl Into<String>, section: &serde_json::Value, http: HttpFn) -> Self {
        Self {
            wasi: WasiCtxBuilder::new().inherit_stderr().build(),
            table: ResourceTable::new(),
            component_id: component_id.into(),
            section: ConfigSection::new(section.clone()),
            http,
        }
    }
}

impl WasiView for CapHost {
    fn ctx(&mut self) -> WasiCtxView<'_> {
        WasiCtxView {
            ctx: &mut self.wasi,
            table: &mut self.table,
        }
    }
}

impl provider_bind::jan_klod::interfaces::host_log::Host for CapHost {
    fn log(
        &mut self,
        level: provider_bind::jan_klod::interfaces::host_log::LogLevel,
        component: String,
        message: String,
        fields: Vec<provider_bind::jan_klod::interfaces::host_log::LogField>,
    ) {
        use provider_bind::jan_klod::interfaces::host_log::LogLevel;
        let level = match level {
            LogLevel::Debug => "DEBUG",
            LogLevel::Info => "INFO",
            LogLevel::Warn => "WARN",
            LogLevel::Error => "ERROR",
        };
        let mut line = format!("{level} [{}] {component}: {message}", self.component_id);
        for field in fields {
            let _ = write!(line, " {}={}", field.key, field.value);
        }
        eprintln!("{line}");
    }
}

impl provider_bind::jan_klod::interfaces::host_config::Host for CapHost {
    fn get(
        &mut self,
        key: String,
    ) -> Result<String, provider_bind::jan_klod::interfaces::host_config::ConfigError> {
        self.section
            .get(&key)
            .ok_or(provider_bind::jan_klod::interfaces::host_config::ConfigError::KeyNotFound)
    }
    fn has(&mut self, key: String) -> bool {
        self.section.has(&key)
    }
    fn all(
        &mut self,
    ) -> Result<String, provider_bind::jan_klod::interfaces::host_config::ConfigError> {
        Ok(self.section.all())
    }
}

impl provider_bind::jan_klod::interfaces::host_http::Host for CapHost {
    fn fetch(
        &mut self,
        request: provider_bind::jan_klod::interfaces::host_http::HttpRequest,
    ) -> Result<
        provider_bind::jan_klod::interfaces::host_http::HttpResponse,
        provider_bind::jan_klod::interfaces::host_http::HttpError,
    > {
        use provider_bind::jan_klod::interfaces::host_http::{HttpError, HttpHeader, HttpResponse};
        let headers: Vec<(String, String)> = request
            .headers
            .into_iter()
            .map(|h| (h.name, h.value))
            .collect();
        match (self.http)(
            &request.method,
            &request.url,
            &headers,
            request.body.as_deref(),
            request.timeout_ms,
        ) {
            Ok(response) => Ok(HttpResponse {
                status: response.status,
                headers: response
                    .headers
                    .into_iter()
                    .map(|(name, value)| HttpHeader { name, value })
                    .collect(),
                body: response.body,
            }),
            Err(err) => Err(match err {
                WireError::InvalidUrl => HttpError::InvalidUrl,
                WireError::ConnectionFailed => HttpError::ConnectionFailed,
                WireError::Timeout => HttpError::Timeout,
                WireError::TlsError => HttpError::TlsError,
                WireError::ClientError(code) => HttpError::ClientError(code),
                WireError::ServerError(code) => HttpError::ServerError(code),
                WireError::Backend => HttpError::Backend,
            }),
        }
    }
}

// ---------------------------------------------------------------------------
// Provider instantiation + the Completer adapter.
// ---------------------------------------------------------------------------

/// Instantiate a provider extension in its own store with an injected `host-http`,
/// and drive its lifecycle to `start`.
fn instantiate_provider(
    engine: &Engine,
    inst: &ExtensionInstance,
    component: &Component,
    http: HttpFn,
) -> Result<(Store<CapHost>, provider_bind::ProviderWorld), CoreError> {
    let mut linker: Linker<CapHost> = Linker::new(engine);
    wasmtime_wasi::p2::add_to_linker_sync(&mut linker).map_err(CoreError::linker)?;
    provider_bind::jan_klod::interfaces::host_log::add_to_linker::<_, HasSelf<_>>(
        &mut linker,
        |s| s,
    )
    .map_err(CoreError::linker)?;
    provider_bind::jan_klod::interfaces::host_config::add_to_linker::<_, HasSelf<_>>(
        &mut linker,
        |s| s,
    )
    .map_err(CoreError::linker)?;
    provider_bind::jan_klod::interfaces::host_http::add_to_linker::<_, HasSelf<_>>(
        &mut linker,
        |s| s,
    )
    .map_err(CoreError::linker)?;
    let mut store = Store::new(engine, CapHost::new(&inst.id, &inst.config, http));
    let world = provider_bind::ProviderWorld::instantiate(&mut store, component, &linker)
        .map_err(|source| CoreError::instantiate(&inst.id, source))?;

    let lifecycle = world.jan_klod_interfaces_extension_lifecycle();
    let ctx = provider_bind::exports::jan_klod::interfaces::extension_lifecycle::ExtensionContext {
        id: inst.id.clone(),
        version: "0.0.0".to_string(),
    };
    drive(&inst.id, lifecycle.call_init(&mut store, &ctx), "init")?;
    drive(&inst.id, lifecycle.call_start(&mut store), "start")?;
    Ok((store, world))
}

/// Collapse a lifecycle call's `Result<Result<(), String>, Error>` into a
/// [`CoreError`]: a trap, or the extension's own rejection.
fn drive(
    id: &str,
    result: wasmtime::Result<Result<(), String>>,
    phase: &'static str,
) -> Result<(), CoreError> {
    match result {
        Ok(Ok(())) => Ok(()),
        Ok(Err(message)) => Err(CoreError::LifecycleRejected {
            id: id.to_string(),
            phase,
            message,
        }),
        Err(source) => Err(CoreError::Lifecycle {
            id: id.to_string(),
            phase,
            source: source.into(),
        }),
    }
}

/// A provider extension adapted to the conductor's [`Completer`](crate::conductor::Completer)
/// trait: the provider becomes one link in the loop's fallback chain.
pub struct ProviderCompleter {
    id: String,
    /// The instance's configured endpoint, kept only to name it in a failure.
    ///
    /// A provider error without the address is most of a diagnosis withheld: the
    /// three most likely causes are a wrong URL, a server that is not running, and
    /// an origin egress does not allow, and all three are questions about *which*
    /// endpoint.
    endpoint: String,
    store: Store<CapHost>,
    world: provider_bind::ProviderWorld,
}

/// A provider failure in the words the person running this needs.
///
/// This used to be `format!("provider error: {err:?}")` — the Debug of a generated
/// binding — so a refused connection to a local model read as
/// `ProviderError { code: 5, name: "transient", message: "Any other transient
/// error." }`. Three separate problems: a wasm-binding internal reached the user,
/// the word "transient" invited retrying something that would never succeed, and
/// nothing named the endpoint the reader needed to look at.
fn describe(err: p_llm::ProviderError, endpoint: &str) -> String {
    use p_llm::ProviderError as E;
    match err {
        E::AuthFailed => format!(
            "the API key was rejected by {endpoint}. Check the key this instance \
             reads (the shipped config uses $OPENAI_API_KEY)."
        ),
        E::ModelNotFound => format!(
            "{endpoint} does not offer the configured model. Check `model:` — for a \
             local server, that it is pulled."
        ),
        E::RateLimited => "rate-limited or out of quota".to_string(),
        E::OutOfMemory => "the model ran out of memory — usually a local model too large for this \
             machine"
            .to_string(),
        E::ContextOverflow => {
            "the request exceeded the model's context window. `interceptor.context` \
             trims history to a budget; lower its `context-tokens` if it is on."
                .to_string()
        }
        E::Unreachable => format!(
            "{endpoint} could not be reached. Is the server running, is the address \
             right, and is that origin allowed egress? Loopback and private \
             addresses are refused unless config names them — a provider's \
             `base-url` counts, so a typo in it fails here rather than being \
             quietly allowed."
        ),
        E::Transient => "a transient provider failure; worth retrying".to_string(),
    }
}

impl ProviderCompleter {
    /// Instantiate a provider component as a completer. `http` backs its
    /// `host-http` (live client or a canned test reply).
    ///
    /// # Errors
    /// Returns a [`CoreError`] if the component cannot be wired, instantiated, or
    /// started.
    pub fn instantiate(
        engine: &Engine,
        inst: &ExtensionInstance,
        component: &Component,
        http: HttpFn,
    ) -> Result<Self, CoreError> {
        let (store, world) = instantiate_provider(engine, inst, component, http)?;
        Ok(Self {
            id: inst.id.clone(),
            endpoint: inst
                .config
                .get("base-url")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("the configured endpoint")
                .to_string(),
            store,
            world,
        })
    }
}

impl crate::conductor::Completer for ProviderCompleter {
    fn id(&self) -> &str {
        &self.id
    }

    fn complete(
        &mut self,
        request: &crate::intercept::PendingRequest,
    ) -> Result<crate::conductor::Completion, String> {
        let preq = intercept_to_p_request(request);
        let iface = self.world.jan_klod_interfaces_llm_provider();
        let handle = match iface.call_complete(&mut self.store, &preq) {
            Ok(Ok(handle)) => handle,
            Ok(Err(err)) => return Err(describe(err, &self.endpoint)),
            Err(_) => return Err("provider trapped".to_string()),
        };

        // Drain the stream into text + tool calls.
        let mut text = String::new();
        let mut tool_calls = Vec::new();
        let mut finish_reason = String::new();
        // Ok(None) / Err both end the stream by exiting the while-let.
        while let Ok(Some(chunk)) = iface.call_next_chunk(&mut self.store, handle) {
            match chunk {
                p_llm::CompletionChunk::TextDelta(delta) => text.push_str(&delta),
                p_llm::CompletionChunk::ToolCallRequest(call) => {
                    tool_calls.push(crate::intercept::ToolCall {
                        id: call.id,
                        name: call.name,
                        arguments: call.arguments,
                    });
                }
                p_llm::CompletionChunk::Done(reason) => {
                    finish_reason = reason;
                    break;
                }
            }
        }
        let _ = iface.call_close_stream(&mut self.store, handle);
        Ok(crate::conductor::Completion {
            text,
            tool_calls,
            finish_reason,
        })
    }
}

/// Map the conductor's `pending-request` to the provider's `completion-request`.
fn intercept_to_p_request(request: &crate::intercept::PendingRequest) -> p_llm::CompletionRequest {
    use crate::intercept::Role;
    p_llm::CompletionRequest {
        model: request.model.clone().unwrap_or_default(),
        messages: request
            .messages
            .iter()
            .map(|m| p_llm::Message {
                role: match m.role {
                    Role::System => p_llm::Role::System,
                    Role::User => p_llm::Role::User,
                    Role::Assistant => p_llm::Role::Assistant,
                    Role::Tool => p_llm::Role::Tool,
                },
                content: m.content.clone(),
                tool_call_id: m.tool_call_id.clone(),
            })
            .collect(),
        tools: request
            .tools
            .iter()
            .map(|t| p_llm::ToolDefinition {
                name: t.name.clone(),
                description: t.description.clone(),
                parameters_schema: t.parameters_schema.clone(),
            })
            .collect(),
        grammar: request.grammar.clone(),
        max_tokens: request.max_tokens,
        temperature: request.temperature,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::conductor::Completer;
    use crate::http::WireResponse;
    use crate::intercept::{Message, PendingRequest, Role};
    use std::path::PathBuf;

    fn repo_root() -> PathBuf {
        [env!("CARGO_MANIFEST_DIR"), "..", "..", ".."]
            .iter()
            .collect()
    }

    fn canned_http(content: &'static str) -> HttpFn {
        Box::new(move |_m, _u, _h, _b, _t| {
            let body = serde_json::json!({
                "choices": [{
                    "message": { "role": "assistant", "content": content },
                    "finish_reason": "stop"
                }]
            });
            Ok(WireResponse {
                status: 200,
                headers: vec![],
                body: serde_json::to_vec(&body).unwrap(),
            })
        })
    }

    fn user_request(text: &str) -> PendingRequest {
        PendingRequest {
            model: Some("mock-1".into()),
            messages: vec![Message {
                role: Role::User,
                content: text.into(),
                tool_call_id: None,
            }],
            tools: vec![],
            grammar: None,
            max_tokens: None,
            temperature: None,
        }
    }

    #[test]
    fn provider_completer_completes_through_the_sandbox() {
        let path = repo_root().join("ext").join("provider-openai.wasm");
        if !path.exists() {
            eprintln!("skipping: provider-openai.wasm not staged — run `make ext`");
            return;
        }
        let engine = Engine::default();
        let component = Component::from_file(&engine, &path).expect("component compiles");
        let inst = ExtensionInstance {
            id: "provider.openai".into(),
            category: "provider".into(),
            name: "openai".into(),
            kind: "openai".into(),
            component: "provider-openai".into(),
            enabled: true,
            config: serde_json::json!({
                "base-url": "http://mock/v1",
                "model": "mock-1",
                "api-key": "test"
            }),
        };
        let mut completer =
            ProviderCompleter::instantiate(&engine, &inst, &component, canned_http("pong"))
                .expect("provider instantiates");
        let completion = completer
            .complete(&user_request("ping"))
            .expect("completes");
        assert_eq!(completion.text, "pong");
        assert!(completion.tool_calls.is_empty());
    }
}
