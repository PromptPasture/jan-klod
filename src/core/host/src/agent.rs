//! The agent loop — the host-side seed of the future `manager.agent-loop`
//! extension, and the driver that closes Phase 1's exit gate.
//!
//! [`run_turn`] takes a `jan-klod.yaml` plus a prompt and drives **one turn**
//! through the Component Model, end to end:
//!
//! 1. **config-driven load** — resolve the enabled provider + store instances
//!    ([`jan_klod_config`]) and compile their staged components;
//! 2. **lifecycle** — instantiate each as its category world and run
//!    `init` → `start`;
//! 3. **completion** — issue `llm-provider.complete` against the provider and
//!    drain the stream into the response text;
//! 4. **persistence** — write the prompt+response into the `memory-store` and
//!    read it back, proving the round-trip.
//!
//! The provider's `host-http` is injected as an [`HttpFn`]: the `jan-klod`
//! binary passes the live blocking client ([`jan_klod_core::http::fetch`]); the
//! offline exit-gate test passes a canned reply, so the whole loop runs without
//! a network or an API key.
//!
//! This is deliberately the *minimal* driver — one provider, one store, one
//! turn — not a routing/fallback engine. That richer behaviour belongs in the
//! `manager.agent-loop` guest (fed by the top-level agent config the core
//! preserves verbatim); this host-side seed exists only until that guest does.

use std::fmt::Write as _;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use jan_klod_config::{Config, ExtensionInstance};
use jan_klod_core::http::{WireError, WireResponse};
use jan_klod_core::ConfigSection;
use serde_json::{json, Value};
use wasmtime::component::{Component, HasSelf, Linker};
use wasmtime::{Engine, Store};
use wasmtime_wasi::{ResourceTable, WasiCtx, WasiCtxBuilder, WasiCtxView, WasiView};

// Generated Component-Model bindings; lint exemptions scoped to the macro output
// (the hand-written driver below is held to the full workspace lint policy).
#[allow(missing_docs, clippy::all, clippy::pedantic, clippy::nursery)]
mod provider_bind {
    wasmtime::component::bindgen!({
        path: "../../../wit",
        world: "provider-world",
    });
}

#[allow(missing_docs, clippy::all, clippy::pedantic, clippy::nursery)]
mod store_bind {
    wasmtime::component::bindgen!({
        path: "../../../wit",
        world: "store-world",
    });
}

/// Host-side HTTP backend injected into the provider's `host-http` import.
///
/// The signature mirrors [`jan_klod_core::http::fetch`] exactly, so the live
/// binary passes that function directly (`Box::new(jan_klod_core::http::fetch)`)
/// while a test passes a canned closure.
pub type HttpFn = Box<
    dyn Fn(&str, &str, &[(String, String)], Option<&[u8]>, u32) -> Result<WireResponse, WireError>
        + Send
        + Sync,
>;

/// The outcome of one driven turn: what was asked, what the provider answered,
/// and where it landed in the store.
#[derive(Debug, Clone)]
pub struct Turn {
    /// The prompt that was sent to the provider.
    pub prompt: String,
    /// The assembled completion text (all `text-delta` chunks concatenated).
    pub response: String,
    /// The stream's `done` reason (`"stop"`, `"length"`, …).
    pub done_reason: String,
    /// The store namespace the turn was persisted under.
    pub namespace: String,
    /// The key the turn was persisted under.
    pub key: String,
    /// The row id the store assigned, read back after the write.
    pub stored_id: String,
    /// The JSON value read back from the store (proves the round-trip).
    pub stored_value: String,
}

/// Run one agent turn end to end over the Component Model.
///
/// Loads `config_path`, resolves the first enabled provider and the enabled
/// store, compiles their components from `ext_dir`, drives each lifecycle, then
/// completes `prompt` through the provider and persists the prompt+response in
/// the store (reading it back to confirm).
///
/// # Errors
/// Returns an [`AgentError`] if the config fails to load, no provider/store is
/// enabled, a component is missing or fails to compile/instantiate, a lifecycle
/// or interface call traps, or the provider/store returns a domain error.
pub fn run_turn(
    config_path: impl AsRef<Path>,
    ext_dir: impl AsRef<Path>,
    prompt: &str,
    http: HttpFn,
) -> Result<Turn, AgentError> {
    let config = Config::from_path(config_path)?;
    let provider = first_enabled(&config, "provider")
        .ok_or(AgentError::NoProvider)?
        .clone();
    let store = first_enabled(&config, "store")
        .ok_or(AgentError::NoStore)?
        .clone();

    let engine = Engine::default();
    let ext_dir = ext_dir.as_ref();
    let provider_component = compile(&engine, ext_dir, &provider)?;
    let store_component = compile(&engine, ext_dir, &store)?;

    // 3. Completion through the provider component.
    let completion = complete(&engine, &provider, &provider_component, prompt, http)?;

    // 4. Persist the turn into the store component, then read it back.
    let namespace = "agent.history";
    let key = turn_key();
    let value = json!({ "prompt": prompt, "response": completion.text }).to_string();
    let stored = persist(&engine, &store, &store_component, namespace, &key, &value)?;

    Ok(Turn {
        prompt: prompt.to_string(),
        response: completion.text,
        done_reason: completion.done_reason,
        namespace: namespace.to_string(),
        key,
        stored_id: stored.id,
        stored_value: stored.value,
    })
}

/// First enabled instance in a category, in declaration order.
fn first_enabled<'a>(config: &'a Config, category: &str) -> Option<&'a ExtensionInstance> {
    config.enabled().find(|i| i.category == category)
}

/// Compile an instance's staged `ext/<component>.wasm`.
fn compile(
    engine: &Engine,
    ext_dir: &Path,
    instance: &ExtensionInstance,
) -> Result<Component, AgentError> {
    let path = ext_dir.join(instance.component_file());
    Component::from_file(engine, &path).map_err(|source| AgentError::Load {
        id: instance.id.clone(),
        path: path.display().to_string(),
        source: source.into(),
    })
}

/// A unique key for this turn — millisecond timestamp keeps writes ordered.
fn turn_key() -> String {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    format!("turn-{millis}")
}

/// The provider leg's result: assembled text and the stream's done reason.
struct Completion {
    text: String,
    done_reason: String,
}

/// Instantiate the provider as `provider-world`, drive lifecycle, and run one
/// completion — draining the stream into text.
fn complete(
    engine: &Engine,
    instance: &ExtensionInstance,
    component: &Component,
    prompt: &str,
    http: HttpFn,
) -> Result<Completion, AgentError> {
    use provider_bind::exports::jan_klod::interfaces::extension_lifecycle::ExtensionContext;
    use provider_bind::exports::jan_klod::interfaces::llm_provider::{
        CompletionChunk, CompletionRequest, Message, Role,
    };
    use provider_bind::jan_klod::interfaces::{host_config, host_http, host_log};
    use provider_bind::ProviderWorld;

    let mut linker: Linker<AgentHost> = Linker::new(engine);
    wasmtime_wasi::p2::add_to_linker_sync(&mut linker).map_err(AgentError::linker)?;
    host_log::add_to_linker::<_, HasSelf<_>>(&mut linker, |s| s).map_err(AgentError::linker)?;
    host_config::add_to_linker::<_, HasSelf<_>>(&mut linker, |s| s).map_err(AgentError::linker)?;
    host_http::add_to_linker::<_, HasSelf<_>>(&mut linker, |s| s).map_err(AgentError::linker)?;

    let host = AgentHost::new(instance.id.clone(), instance.config.clone(), http);
    let mut store = Store::new(engine, host);
    let world = ProviderWorld::instantiate(&mut store, component, &linker)
        .map_err(|source| AgentError::instantiate(&instance.id, source))?;
    let lifecycle = world.jan_klod_interfaces_extension_lifecycle();
    let provider = world.jan_klod_interfaces_llm_provider();

    let ctx = ExtensionContext {
        id: instance.id.clone(),
        version: "0.0.0".to_string(),
    };
    lifecycle
        .call_init(&mut store, &ctx)
        .map_err(|source| AgentError::trap(&instance.id, "init", source))?
        .map_err(|message| AgentError::rejected(&instance.id, "init", message))?;
    lifecycle
        .call_start(&mut store)
        .map_err(|source| AgentError::trap(&instance.id, "start", source))?
        .map_err(|message| AgentError::rejected(&instance.id, "start", message))?;

    // An empty model lets the provider fall back to its configured `model`.
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
            content: prompt.to_string(),
            tool_call_id: None,
        }],
        tools: vec![],
        grammar: None,
        max_tokens: Some(512),
        temperature: Some(0.0),
    };

    let handle = provider
        .call_complete(&mut store, &request)
        .map_err(|source| AgentError::trap(&instance.id, "complete", source))?
        .map_err(|err| AgentError::Provider {
            id: instance.id.clone(),
            kind: format!("{err:?}"),
        })?;

    let mut text = String::new();
    let mut done_reason = String::new();
    loop {
        match provider
            .call_next_chunk(&mut store, handle)
            .map_err(|source| AgentError::trap(&instance.id, "next-chunk", source))?
        {
            Some(CompletionChunk::TextDelta(delta)) => text.push_str(&delta),
            // Tool calls are out of scope for the skeleton turn — the manager
            // guest will own the tool-execution loop.
            Some(CompletionChunk::ToolCallRequest(_)) => {}
            Some(CompletionChunk::Done(reason)) => {
                done_reason = reason;
                break;
            }
            None => break,
        }
    }
    provider
        .call_close_stream(&mut store, handle)
        .map_err(|source| AgentError::trap(&instance.id, "close-stream", source))?;

    Ok(Completion { text, done_reason })
}

/// The store leg's result: the row id and value as read back after the write.
struct Stored {
    id: String,
    value: String,
}

/// Instantiate the store as `store-world`, drive lifecycle, `set` the turn, then
/// `get` it back to confirm persistence.
fn persist(
    engine: &Engine,
    instance: &ExtensionInstance,
    component: &Component,
    namespace: &str,
    key: &str,
    value: &str,
) -> Result<Stored, AgentError> {
    use store_bind::exports::jan_klod::interfaces::extension_lifecycle::ExtensionContext;
    use store_bind::jan_klod::interfaces::{host_config, host_log};
    use store_bind::StoreWorld;

    let mut linker: Linker<AgentHost> = Linker::new(engine);
    wasmtime_wasi::p2::add_to_linker_sync(&mut linker).map_err(AgentError::linker)?;
    host_log::add_to_linker::<_, HasSelf<_>>(&mut linker, |s| s).map_err(AgentError::linker)?;
    host_config::add_to_linker::<_, HasSelf<_>>(&mut linker, |s| s).map_err(AgentError::linker)?;

    // The store world imports no `host-http`; a stub backend is never called.
    let host = AgentHost::new(instance.id.clone(), instance.config.clone(), noop_http());
    let mut store = Store::new(engine, host);
    let world = StoreWorld::instantiate(&mut store, component, &linker)
        .map_err(|source| AgentError::instantiate(&instance.id, source))?;
    let lifecycle = world.jan_klod_interfaces_extension_lifecycle();
    let mem = world.jan_klod_interfaces_memory_store();

    let ctx = ExtensionContext {
        id: instance.id.clone(),
        version: "0.0.0".to_string(),
    };
    lifecycle
        .call_init(&mut store, &ctx)
        .map_err(|source| AgentError::trap(&instance.id, "init", source))?
        .map_err(|message| AgentError::rejected(&instance.id, "init", message))?;
    lifecycle
        .call_start(&mut store)
        .map_err(|source| AgentError::trap(&instance.id, "start", source))?
        .map_err(|message| AgentError::rejected(&instance.id, "start", message))?;

    mem.call_set(&mut store, namespace, key, value)
        .map_err(|source| AgentError::trap(&instance.id, "set", source))?
        .map_err(|err| AgentError::Store {
            id: instance.id.clone(),
            kind: format!("{err:?}"),
        })?;
    let got = mem
        .call_get(&mut store, namespace, key)
        .map_err(|source| AgentError::trap(&instance.id, "get", source))?
        .map_err(|err| AgentError::Store {
            id: instance.id.clone(),
            kind: format!("{err:?}"),
        })?;

    Ok(Stored {
        id: got.id,
        value: got.value,
    })
}

/// A never-called HTTP backend for worlds (like `store-world`) that import no
/// `host-http`.
fn noop_http() -> HttpFn {
    Box::new(|_, _, _, _, _| Err(WireError::Backend))
}

/// Per-instance host state backing the category worlds' imports: WASI, the
/// instance's config section, and the injected HTTP backend. One struct serves
/// both generated worlds — `store-world` simply never touches `http`.
struct AgentHost {
    wasi: WasiCtx,
    table: ResourceTable,
    component_id: String,
    section: ConfigSection,
    http: HttpFn,
}

impl AgentHost {
    fn new(component_id: impl Into<String>, section: Value, http: HttpFn) -> Self {
        Self {
            wasi: WasiCtxBuilder::new().inherit_stdio().build(),
            table: ResourceTable::new(),
            component_id: component_id.into(),
            section: ConfigSection::new(section),
            http,
        }
    }
}

impl WasiView for AgentHost {
    fn ctx(&mut self) -> WasiCtxView<'_> {
        WasiCtxView {
            ctx: &mut self.wasi,
            table: &mut self.table,
        }
    }
}

/// Format and emit one `host-log` line to stderr (shared by both worlds).
fn emit_log(component_id: &str, level: &str, component: &str, message: &str, fields: &[(String, String)]) {
    let mut line = format!("{level} [{component_id}] {component}: {message}");
    for (key, value) in fields {
        // Infallible: writing into a String never errors.
        let _ = write!(line, " {key}={value}");
    }
    eprintln!("{line}");
}

impl store_bind::jan_klod::interfaces::host_log::Host for AgentHost {
    fn log(
        &mut self,
        level: store_bind::jan_klod::interfaces::host_log::LogLevel,
        component: String,
        message: String,
        fields: Vec<store_bind::jan_klod::interfaces::host_log::LogField>,
    ) {
        use store_bind::jan_klod::interfaces::host_log::LogLevel;
        let level = match level {
            LogLevel::Debug => "DEBUG",
            LogLevel::Info => "INFO",
            LogLevel::Warn => "WARN",
            LogLevel::Error => "ERROR",
        };
        let fields: Vec<(String, String)> = fields.into_iter().map(|f| (f.key, f.value)).collect();
        emit_log(&self.component_id, level, &component, &message, &fields);
    }
}

impl provider_bind::jan_klod::interfaces::host_log::Host for AgentHost {
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
        let fields: Vec<(String, String)> = fields.into_iter().map(|f| (f.key, f.value)).collect();
        emit_log(&self.component_id, level, &component, &message, &fields);
    }
}

impl store_bind::jan_klod::interfaces::host_config::Host for AgentHost {
    fn get(
        &mut self,
        key: String,
    ) -> Result<String, store_bind::jan_klod::interfaces::host_config::ConfigError> {
        self.section
            .get(&key)
            .ok_or(store_bind::jan_klod::interfaces::host_config::ConfigError::KeyNotFound)
    }
    fn has(&mut self, key: String) -> bool {
        self.section.has(&key)
    }
    fn all(
        &mut self,
    ) -> Result<String, store_bind::jan_klod::interfaces::host_config::ConfigError> {
        Ok(self.section.all())
    }
}

impl provider_bind::jan_klod::interfaces::host_config::Host for AgentHost {
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

impl provider_bind::jan_klod::interfaces::host_http::Host for AgentHost {
    fn fetch(
        &mut self,
        request: provider_bind::jan_klod::interfaces::host_http::HttpRequest,
    ) -> Result<
        provider_bind::jan_klod::interfaces::host_http::HttpResponse,
        provider_bind::jan_klod::interfaces::host_http::HttpError,
    > {
        use provider_bind::jan_klod::interfaces::host_http::{HttpHeader, HttpResponse};
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
            Err(err) => Err(wire_to_http(&err)),
        }
    }
}

/// Map a neutral [`WireError`] onto the provider world's generated `host-http`
/// error (the host boundary the live client and the canned test share).
const fn wire_to_http(err: &WireError) -> provider_bind::jan_klod::interfaces::host_http::HttpError {
    use provider_bind::jan_klod::interfaces::host_http::HttpError;
    match err {
        WireError::InvalidUrl => HttpError::InvalidUrl,
        WireError::ConnectionFailed => HttpError::ConnectionFailed,
        WireError::Timeout => HttpError::Timeout,
        WireError::TlsError => HttpError::TlsError,
        WireError::ClientError(code) => HttpError::ClientError(*code),
        WireError::ServerError(code) => HttpError::ServerError(*code),
        WireError::Backend => HttpError::Backend,
    }
}

/// Errors surfaced while driving one agent turn.
#[derive(Debug, thiserror::Error)]
pub enum AgentError {
    /// Loading or parsing `jan-klod.yaml` failed.
    #[error(transparent)]
    Config(#[from] jan_klod_config::ConfigError),
    /// No enabled provider instance to complete against.
    #[error("no enabled provider instance in config")]
    NoProvider,
    /// No enabled store instance to persist into.
    #[error("no enabled store instance in config")]
    NoStore,
    /// Compiling a component from disk failed (often: not staged in `ext/`).
    #[error("loading component for {id} from {path}")]
    Load {
        /// Instance id whose component failed to compile.
        id: String,
        /// Path the component was loaded from.
        path: String,
        /// The underlying compilation error.
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },
    /// Wiring a host capability into the linker failed.
    #[error("wiring host capabilities into the linker")]
    Linker {
        /// The underlying Wasmtime linker error.
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },
    /// Instantiating a compiled component failed.
    #[error("instantiating {id}")]
    Instantiate {
        /// Instance id that failed to instantiate.
        id: String,
        /// The underlying instantiation error.
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },
    /// A lifecycle or interface call trapped (guest crash, host-cap error).
    #[error("{id}: `{op}` trapped")]
    Trap {
        /// Instance id whose call trapped.
        id: String,
        /// The operation that trapped (`init`, `complete`, `set`, …).
        op: &'static str,
        /// The underlying trap.
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },
    /// A lifecycle call returned an error result (the extension refused).
    #[error("{id}: lifecycle `{phase}` failed: {message}")]
    LifecycleRejected {
        /// Instance id that refused.
        id: String,
        /// Lifecycle phase that was rejected (`init` / `start`).
        phase: &'static str,
        /// The message the extension returned.
        message: String,
    },
    /// The provider returned a `provider-error`.
    #[error("{id}: provider error: {kind}")]
    Provider {
        /// Provider instance id.
        id: String,
        /// The `provider-error` variant, debug-formatted.
        kind: String,
    },
    /// The store returned a `store-error`.
    #[error("{id}: store error: {kind}")]
    Store {
        /// Store instance id.
        id: String,
        /// The `store-error` variant, debug-formatted.
        kind: String,
    },
}

impl AgentError {
    fn linker(source: wasmtime::Error) -> Self {
        Self::Linker {
            source: source.into(),
        }
    }

    fn instantiate(id: &str, source: wasmtime::Error) -> Self {
        Self::Instantiate {
            id: id.to_string(),
            source: source.into(),
        }
    }

    fn trap(id: &str, op: &'static str, source: wasmtime::Error) -> Self {
        Self::Trap {
            id: id.to_string(),
            op,
            source: source.into(),
        }
    }

    fn rejected(id: &str, phase: &'static str, message: String) -> Self {
        Self::LifecycleRejected {
            id: id.to_string(),
            phase,
            message,
        }
    }
}
