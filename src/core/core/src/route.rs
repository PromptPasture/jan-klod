//! Inter-component routing — the core capability that lets one extension consume
//! another's interface.
//!
//! Architecture rule: extensions "talk to other extensions … routed through
//! core" (see `docs/concepts/architecture.md`). A *manager* guest is the first
//! to need this: it imports `llm-provider` and `memory-store`, which are exported
//! by the provider and store extensions, not by the host. This module satisfies
//! those imports by instantiating the implementing extensions and **delegating
//! each imported call into the implementing instance** — a domain-neutral broker.
//! No agent behaviour lives here; the loop logic is in the `manager-agent-loop`
//! guest.
//!
//! v0 scope (the walking skeleton): one provider + one store, hand-wired. The
//! provider's `host-http` is injected as an [`HttpFn`] so a turn can run live
//! (real client) or offline (canned reply) without changing the broker.

use std::fmt::Write as _;

use wasmtime::component::{Component, HasSelf, Linker};
use wasmtime::{Engine, Store};
use wasmtime_wasi::{ResourceTable, WasiCtx, WasiCtxBuilder, WasiCtxView, WasiView};

use jan_klod_config::ExtensionInstance;

use crate::host::ConfigSection;
use crate::http::{WireError, WireResponse};
use crate::CoreError;

// Generated Component-Model bindings; lint exemptions scoped to the macro output.
// Three worlds: the two implementing extensions (to *call* their exports) and the
// manager (to *implement* its imported interfaces by routing).
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

#[allow(missing_docs, clippy::all, clippy::pedantic, clippy::nursery)]
mod manager_bind {
    wasmtime::component::bindgen!({
        path: "../../../wit",
        world: "agent-loop-world",
    });
}

// Short aliases for the type-bearing modules. `p_*` = provider export types,
// `s_*` = store export types, `m_*` = manager import types (what the routing Host
// traits speak).
use manager_bind::jan_klod::interfaces::llm_provider as m_llm;
use manager_bind::jan_klod::interfaces::memory_store as m_mem;
use provider_bind::exports::jan_klod::interfaces::llm_provider as p_llm;
use store_bind::exports::jan_klod::interfaces::memory_store as s_mem;

/// Host-side HTTP backend injected into the routed provider's `host-http` import.
/// The signature mirrors [`crate::http::fetch`], so a live caller passes that
/// function and a test passes a canned closure.
pub type HttpFn = Box<
    dyn Fn(&str, &str, &[(String, String)], Option<&[u8]>, u32) -> Result<WireResponse, WireError>
        + Send
        + Sync,
>;

/// A never-called HTTP backend for the store (its world imports no `host-http`).
fn noop_http() -> HttpFn {
    Box::new(|_, _, _, _, _| Err(WireError::Backend))
}

/// Format and emit one `host-log` line to stderr (shared across worlds).
fn emit_log(component_id: &str, level: &str, component: &str, message: &str, fields: &[(String, String)]) {
    let mut line = format!("{level} [{component_id}] {component}: {message}");
    for (key, value) in fields {
        let _ = write!(line, " {key}={value}");
    }
    eprintln!("{line}");
}

// ---------------------------------------------------------------------------
// CapHost — the host state for the two implementing extensions (provider, store).
// Identical to the core's HostState but with an *injected* HTTP backend so a
// routed turn can run offline. The store never calls `http`.
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
            wasi: WasiCtxBuilder::new().inherit_stdio().build(),
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
        let fields: Vec<(String, String)> = fields.into_iter().map(|f| (f.key, f.value)).collect();
        emit_log(&self.component_id, level, &component, &message, &fields);
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

impl store_bind::jan_klod::interfaces::host_log::Host for CapHost {
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

impl store_bind::jan_klod::interfaces::host_config::Host for CapHost {
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

// ---------------------------------------------------------------------------
// ManagerHost — the host state for the manager. It owns the live provider and
// store instances and implements the manager's *imported* interfaces by routing
// each call into them.
// ---------------------------------------------------------------------------

struct ManagerHost {
    wasi: WasiCtx,
    table: ResourceTable,
    component_id: String,
    provider: (Store<CapHost>, provider_bind::ProviderWorld),
    store: (Store<CapHost>, store_bind::StoreWorld),
}

impl WasiView for ManagerHost {
    fn ctx(&mut self) -> WasiCtxView<'_> {
        WasiCtxView {
            ctx: &mut self.wasi,
            table: &mut self.table,
        }
    }
}

impl manager_bind::jan_klod::interfaces::host_log::Host for ManagerHost {
    fn log(
        &mut self,
        level: manager_bind::jan_klod::interfaces::host_log::LogLevel,
        component: String,
        message: String,
        fields: Vec<manager_bind::jan_klod::interfaces::host_log::LogField>,
    ) {
        use manager_bind::jan_klod::interfaces::host_log::LogLevel;
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

impl m_llm::Host for ManagerHost {
    fn complete(
        &mut self,
        request: m_llm::CompletionRequest,
    ) -> Result<m_llm::StreamHandle, m_llm::ProviderError> {
        let preq = to_p_request(request);
        let (pstore, pworld) = &mut self.provider;
        let iface = pworld.jan_klod_interfaces_llm_provider();
        match iface.call_complete(pstore, &preq) {
            Ok(Ok(handle)) => Ok(handle),
            Ok(Err(err)) => Err(from_p_error(err)),
            Err(_) => Err(m_llm::ProviderError::Transient),
        }
    }

    fn next_chunk(&mut self, handle: m_llm::StreamHandle) -> Option<m_llm::CompletionChunk> {
        let (pstore, pworld) = &mut self.provider;
        let iface = pworld.jan_klod_interfaces_llm_provider();
        match iface.call_next_chunk(pstore, handle) {
            Ok(Some(chunk)) => Some(from_p_chunk(chunk)),
            _ => None,
        }
    }

    fn close_stream(&mut self, handle: m_llm::StreamHandle) {
        let (pstore, pworld) = &mut self.provider;
        let iface = pworld.jan_klod_interfaces_llm_provider();
        let _ = iface.call_close_stream(pstore, handle);
    }

    fn info(&mut self) -> m_llm::ProviderInfo {
        let (pstore, pworld) = &mut self.provider;
        let iface = pworld.jan_klod_interfaces_llm_provider();
        iface.call_info(pstore).map_or_else(
            |_| m_llm::ProviderInfo {
                id: String::new(),
                supported_models: Vec::new(),
            },
            from_p_info,
        )
    }
}

impl m_mem::Host for ManagerHost {
    fn set(
        &mut self,
        namespace: String,
        key: String,
        value: String,
    ) -> Result<m_mem::Entry, m_mem::StoreError> {
        let (sstore, sworld) = &mut self.store;
        let iface = sworld.jan_klod_interfaces_memory_store();
        match iface.call_set(sstore, &namespace, &key, &value) {
            Ok(Ok(entry)) => Ok(from_s_entry(entry)),
            Ok(Err(err)) => Err(from_s_error(err)),
            Err(_) => Err(m_mem::StoreError::Backend),
        }
    }

    fn get(&mut self, namespace: String, key: String) -> Result<m_mem::Entry, m_mem::StoreError> {
        let (sstore, sworld) = &mut self.store;
        let iface = sworld.jan_klod_interfaces_memory_store();
        match iface.call_get(sstore, &namespace, &key) {
            Ok(Ok(entry)) => Ok(from_s_entry(entry)),
            Ok(Err(err)) => Err(from_s_error(err)),
            Err(_) => Err(m_mem::StoreError::Backend),
        }
    }

    fn delete(&mut self, namespace: String, key: String) -> Result<(), m_mem::StoreError> {
        let (sstore, sworld) = &mut self.store;
        let iface = sworld.jan_klod_interfaces_memory_store();
        match iface.call_delete(sstore, &namespace, &key) {
            Ok(Ok(())) => Ok(()),
            Ok(Err(err)) => Err(from_s_error(err)),
            Err(_) => Err(m_mem::StoreError::Backend),
        }
    }

    fn list_keys(&mut self, namespace: String) -> Result<Vec<m_mem::Entry>, m_mem::StoreError> {
        let (sstore, sworld) = &mut self.store;
        let iface = sworld.jan_klod_interfaces_memory_store();
        match iface.call_list_keys(sstore, &namespace) {
            Ok(Ok(entries)) => Ok(entries.into_iter().map(from_s_entry).collect()),
            Ok(Err(err)) => Err(from_s_error(err)),
            Err(_) => Err(m_mem::StoreError::Backend),
        }
    }

    fn recent(
        &mut self,
        namespace: String,
        limit: u32,
    ) -> Result<Vec<m_mem::Entry>, m_mem::StoreError> {
        let (sstore, sworld) = &mut self.store;
        let iface = sworld.jan_klod_interfaces_memory_store();
        match iface.call_recent(sstore, &namespace, limit) {
            Ok(Ok(entries)) => Ok(entries.into_iter().map(from_s_entry).collect()),
            Ok(Err(err)) => Err(from_s_error(err)),
            Err(_) => Err(m_mem::StoreError::Backend),
        }
    }

    fn search(
        &mut self,
        namespace: String,
        query: String,
        limit: u32,
    ) -> Result<Vec<m_mem::Entry>, m_mem::StoreError> {
        let (sstore, sworld) = &mut self.store;
        let iface = sworld.jan_klod_interfaces_memory_store();
        match iface.call_search(sstore, &namespace, &query, limit) {
            Ok(Ok(entries)) => Ok(entries.into_iter().map(from_s_entry).collect()),
            Ok(Err(err)) => Err(from_s_error(err)),
            Err(_) => Err(m_mem::StoreError::Backend),
        }
    }

    fn purge_namespace(&mut self, namespace: String) -> Result<(), m_mem::StoreError> {
        let (sstore, sworld) = &mut self.store;
        let iface = sworld.jan_klod_interfaces_memory_store();
        match iface.call_purge_namespace(sstore, &namespace) {
            Ok(Ok(())) => Ok(()),
            Ok(Err(err)) => Err(from_s_error(err)),
            Err(_) => Err(m_mem::StoreError::Backend),
        }
    }
}

// ---- Type conversions between the manager's import types and the provider/
// ---- store export types (structurally identical, distinct generated types).

const fn to_p_role(role: m_llm::Role) -> p_llm::Role {
    match role {
        m_llm::Role::System => p_llm::Role::System,
        m_llm::Role::User => p_llm::Role::User,
        m_llm::Role::Assistant => p_llm::Role::Assistant,
        m_llm::Role::Tool => p_llm::Role::Tool,
    }
}

fn to_p_message(msg: m_llm::Message) -> p_llm::Message {
    p_llm::Message {
        role: to_p_role(msg.role),
        content: msg.content,
        tool_call_id: msg.tool_call_id,
    }
}

fn to_p_tool(tool: m_llm::ToolDefinition) -> p_llm::ToolDefinition {
    p_llm::ToolDefinition {
        name: tool.name,
        description: tool.description,
        parameters_schema: tool.parameters_schema,
    }
}

fn to_p_request(request: m_llm::CompletionRequest) -> p_llm::CompletionRequest {
    p_llm::CompletionRequest {
        model: request.model,
        messages: request.messages.into_iter().map(to_p_message).collect(),
        tools: request.tools.into_iter().map(to_p_tool).collect(),
        grammar: request.grammar,
        max_tokens: request.max_tokens,
        temperature: request.temperature,
    }
}

fn from_p_chunk(chunk: p_llm::CompletionChunk) -> m_llm::CompletionChunk {
    match chunk {
        p_llm::CompletionChunk::TextDelta(text) => m_llm::CompletionChunk::TextDelta(text),
        p_llm::CompletionChunk::ToolCallRequest(call) => {
            m_llm::CompletionChunk::ToolCallRequest(m_llm::ToolCall {
                id: call.id,
                name: call.name,
                arguments: call.arguments,
            })
        }
        p_llm::CompletionChunk::Done(reason) => m_llm::CompletionChunk::Done(reason),
    }
}

const fn from_p_error(err: p_llm::ProviderError) -> m_llm::ProviderError {
    match err {
        p_llm::ProviderError::AuthFailed => m_llm::ProviderError::AuthFailed,
        p_llm::ProviderError::ModelNotFound => m_llm::ProviderError::ModelNotFound,
        p_llm::ProviderError::RateLimited => m_llm::ProviderError::RateLimited,
        p_llm::ProviderError::OutOfMemory => m_llm::ProviderError::OutOfMemory,
        p_llm::ProviderError::ContextOverflow => m_llm::ProviderError::ContextOverflow,
        p_llm::ProviderError::Transient => m_llm::ProviderError::Transient,
    }
}

fn from_p_info(info: p_llm::ProviderInfo) -> m_llm::ProviderInfo {
    m_llm::ProviderInfo {
        id: info.id,
        supported_models: info.supported_models,
    }
}

fn from_s_entry(entry: s_mem::Entry) -> m_mem::Entry {
    m_mem::Entry {
        id: entry.id,
        namespace: entry.namespace,
        key: entry.key,
        value: entry.value,
        created_at: entry.created_at,
        updated_at: entry.updated_at,
    }
}

const fn from_s_error(err: s_mem::StoreError) -> m_mem::StoreError {
    match err {
        s_mem::StoreError::NotFound => m_mem::StoreError::NotFound,
        s_mem::StoreError::Conflict => m_mem::StoreError::Conflict,
        s_mem::StoreError::Serialization => m_mem::StoreError::Serialization,
        s_mem::StoreError::Backend => m_mem::StoreError::Backend,
    }
}

// ---------------------------------------------------------------------------
// Lifecycle drivers (one per world type — the ExtensionContext types differ).
// ---------------------------------------------------------------------------

fn start_provider(
    world: &provider_bind::ProviderWorld,
    store: &mut Store<CapHost>,
    id: &str,
) -> Result<(), CoreError> {
    use provider_bind::exports::jan_klod::interfaces::extension_lifecycle::ExtensionContext;
    let lifecycle = world.jan_klod_interfaces_extension_lifecycle();
    let ctx = ExtensionContext {
        id: id.to_string(),
        version: "0.0.0".to_string(),
    };
    drive(id, lifecycle.call_init(&mut *store, &ctx), "init")?;
    drive(id, lifecycle.call_start(&mut *store), "start")
}

fn start_store(
    world: &store_bind::StoreWorld,
    store: &mut Store<CapHost>,
    id: &str,
) -> Result<(), CoreError> {
    use store_bind::exports::jan_klod::interfaces::extension_lifecycle::ExtensionContext;
    let lifecycle = world.jan_klod_interfaces_extension_lifecycle();
    let ctx = ExtensionContext {
        id: id.to_string(),
        version: "0.0.0".to_string(),
    };
    drive(id, lifecycle.call_init(&mut *store, &ctx), "init")?;
    drive(id, lifecycle.call_start(&mut *store), "start")
}

fn start_manager(
    world: &manager_bind::AgentLoopWorld,
    store: &mut Store<ManagerHost>,
    id: &str,
) -> Result<(), CoreError> {
    use manager_bind::exports::jan_klod::interfaces::extension_lifecycle::ExtensionContext;
    let lifecycle = world.jan_klod_interfaces_extension_lifecycle();
    let ctx = ExtensionContext {
        id: id.to_string(),
        version: "0.0.0".to_string(),
    };
    drive(id, lifecycle.call_init(&mut *store, &ctx), "init")?;
    drive(id, lifecycle.call_start(&mut *store), "start")
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

// ---------------------------------------------------------------------------
// Public entry point.
// ---------------------------------------------------------------------------

/// A booted, routed agent loop: the manager instance with its `llm-provider` and
/// `memory-store` imports wired to the live provider and store instances. Call
/// [`RoutedAgentLoop::run`] to drive one turn.
pub struct RoutedAgentLoop {
    store: Store<ManagerHost>,
    world: manager_bind::AgentLoopWorld,
    manager_id: String,
}

impl RoutedAgentLoop {
    /// Run one turn: the manager completes `prompt` through the routed provider
    /// and persists it through the routed store, returning the completion text.
    ///
    /// # Errors
    /// Returns [`CoreError::Lifecycle`] if the guest traps, or
    /// [`CoreError::AgentLoop`] if the loop returns an `agent-error`.
    pub fn run(&mut self, prompt: &str) -> Result<String, CoreError> {
        let agent = self.world.jan_klod_interfaces_agent_loop();
        match agent.call_run(&mut self.store, prompt) {
            Ok(Ok(text)) => Ok(text),
            Ok(Err(err)) => Err(CoreError::AgentLoop {
                id: self.manager_id.clone(),
                kind: format!("{err:?}"),
            }),
            Err(source) => Err(CoreError::Lifecycle {
                id: self.manager_id.clone(),
                phase: "run",
                source: source.into(),
            }),
        }
    }
}

/// Instantiate the provider, store, and manager as one routed agent loop.
///
/// Wires the manager's imported `llm-provider` / `memory-store` to the provider
/// and store instances and drives every lifecycle to `start`; the returned loop
/// is ready to [`RoutedAgentLoop::run`]. `http` backs the provider's `host-http`
/// (live client or a canned test reply).
///
/// # Errors
/// Returns [`CoreError::Linker`] if a capability cannot be wired,
/// [`CoreError::Instantiate`] if a component cannot be instantiated, or a
/// lifecycle error ([`CoreError::Lifecycle`] / [`CoreError::LifecycleRejected`]).
pub fn build_routed_loop(
    engine: &Engine,
    manager: (&ExtensionInstance, &Component),
    provider: (&ExtensionInstance, &Component),
    store: (&ExtensionInstance, &Component),
    http: HttpFn,
) -> Result<RoutedAgentLoop, CoreError> {
    let (provider_inst, provider_component) = provider;
    let (store_inst, store_component) = store;
    let (manager_inst, manager_component) = manager;

    // Provider instance (real/injected host-http).
    let (provider_store, provider_world) =
        instantiate_provider(engine, provider_inst, provider_component, http)?;

    // Store instance (no host-http).
    let mut backend_linker: Linker<CapHost> = Linker::new(engine);
    wasmtime_wasi::p2::add_to_linker_sync(&mut backend_linker).map_err(CoreError::linker)?;
    store_bind::jan_klod::interfaces::host_log::add_to_linker::<_, HasSelf<_>>(&mut backend_linker, |s| s)
        .map_err(CoreError::linker)?;
    store_bind::jan_klod::interfaces::host_config::add_to_linker::<_, HasSelf<_>>(&mut backend_linker, |s| s)
        .map_err(CoreError::linker)?;
    let mut backend_store = Store::new(engine, CapHost::new(&store_inst.id, &store_inst.config, noop_http()));
    let backend_world = store_bind::StoreWorld::instantiate(&mut backend_store, store_component, &backend_linker)
        .map_err(|source| CoreError::instantiate(&store_inst.id, source))?;
    start_store(&backend_world, &mut backend_store, &store_inst.id)?;

    // Manager instance: its imports are satisfied by routing into the two above.
    let mut manager_linker: Linker<ManagerHost> = Linker::new(engine);
    wasmtime_wasi::p2::add_to_linker_sync(&mut manager_linker).map_err(CoreError::linker)?;
    manager_bind::jan_klod::interfaces::host_log::add_to_linker::<_, HasSelf<_>>(&mut manager_linker, |s| s)
        .map_err(CoreError::linker)?;
    m_llm::add_to_linker::<_, HasSelf<_>>(&mut manager_linker, |s| s).map_err(CoreError::linker)?;
    m_mem::add_to_linker::<_, HasSelf<_>>(&mut manager_linker, |s| s).map_err(CoreError::linker)?;
    let manager_host = ManagerHost {
        wasi: WasiCtxBuilder::new().inherit_stdio().build(),
        table: ResourceTable::new(),
        component_id: manager_inst.id.clone(),
        provider: (provider_store, provider_world),
        store: (backend_store, backend_world),
    };
    let mut manager_store = Store::new(engine, manager_host);
    let manager_world = manager_bind::AgentLoopWorld::instantiate(&mut manager_store, manager_component, &manager_linker)
        .map_err(|source| CoreError::instantiate(&manager_inst.id, source))?;
    start_manager(&manager_world, &mut manager_store, &manager_inst.id)?;

    Ok(RoutedAgentLoop {
        store: manager_store,
        world: manager_world,
        manager_id: manager_inst.id.clone(),
    })
}

/// Instantiate a provider extension in its own store with an injected `host-http`,
/// and drive its lifecycle to `start`. Shared by [`build_routed_loop`] and
/// [`ProviderCompleter`].
fn instantiate_provider(
    engine: &Engine,
    inst: &ExtensionInstance,
    component: &Component,
    http: HttpFn,
) -> Result<(Store<CapHost>, provider_bind::ProviderWorld), CoreError> {
    let mut linker: Linker<CapHost> = Linker::new(engine);
    wasmtime_wasi::p2::add_to_linker_sync(&mut linker).map_err(CoreError::linker)?;
    provider_bind::jan_klod::interfaces::host_log::add_to_linker::<_, HasSelf<_>>(&mut linker, |s| s)
        .map_err(CoreError::linker)?;
    provider_bind::jan_klod::interfaces::host_config::add_to_linker::<_, HasSelf<_>>(&mut linker, |s| s)
        .map_err(CoreError::linker)?;
    provider_bind::jan_klod::interfaces::host_http::add_to_linker::<_, HasSelf<_>>(&mut linker, |s| s)
        .map_err(CoreError::linker)?;
    let mut store = Store::new(engine, CapHost::new(&inst.id, &inst.config, http));
    let world = provider_bind::ProviderWorld::instantiate(&mut store, component, &linker)
        .map_err(|source| CoreError::instantiate(&inst.id, source))?;
    start_provider(&world, &mut store, &inst.id)?;
    Ok((store, world))
}

/// A provider extension adapted to the conductor's [`Completer`](crate::conductor::Completer)
/// trait: the routed provider becomes one link in the loop's fallback chain.
pub struct ProviderCompleter {
    id: String,
    store: Store<CapHost>,
    world: provider_bind::ProviderWorld,
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
            Ok(Err(err)) => return Err(format!("provider error: {err:?}")),
            Err(_) => return Err("provider trapped".to_string()),
        };

        // Drain the stream into text + tool calls.
        let mut text = String::new();
        let mut tool_calls = Vec::new();
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
                p_llm::CompletionChunk::Done(_) => break,
            }
        }
        let _ = iface.call_close_stream(&mut self.store, handle);
        Ok(crate::conductor::Completion { text, tool_calls })
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
    use crate::intercept::{Message, PendingRequest, Role};
    use crate::http::WireResponse;
    use std::path::PathBuf;

    fn repo_root() -> PathBuf {
        [env!("CARGO_MANIFEST_DIR"), "..", "..", ".."].iter().collect()
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
        let completion = completer.complete(&user_request("ping")).expect("completes");
        assert_eq!(completion.text, "pong");
        assert!(completion.tool_calls.is_empty());
    }
}
