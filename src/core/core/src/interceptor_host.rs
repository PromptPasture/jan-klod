//! Wasm-guest adapter for the interceptor dispatch engine.
//!
//! [`crate::intercept::Dispatcher`] drives anything implementing the
//! [`Interceptor`](crate::intercept::Interceptor) trait. This module is the
//! adapter that makes a *sandboxed component* one such implementor: it
//! `bindgen!`s the `interceptor-world`, instantiates a guest, satisfies the five
//! imports that world declares (`host-log`, `host-config`, `host-event`,
//! `host-storage`, `llm-provider`), and maps the host-side loop-state types
//! (`intercept::*`) to and from the generated component types.
//!
//! Two imports are backed by real host state so an interceptor is exercisable
//! offline: `host-storage` is an in-memory map, and `llm-provider` is an injected
//! closure (a canned completion) — the same pattern as the routed provider's
//! injected `host-http`. `host-event` is observation-only (publish logs; there is
//! no in-loop subscriber yet).

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};

use wasmtime::component::{Component, HasSelf, Linker};
use wasmtime::{Engine, Store};
use wasmtime_wasi::{ResourceTable, WasiCtx, WasiCtxBuilder, WasiCtxView, WasiView};

use crate::host::ConfigSection;
// `Store` is wasmtime's here, so the core's persistent store needs a distinct name.
use crate::store::Store as PersistentStore;
use crate::intercept::{
    self, BlockReason, Decision, HookState, InterceptInput, Interceptor, InterceptorError, Phase,
    UserPrompt,
};
use crate::CoreError;

// Generated Component-Model bindings for the interceptor world; lint exemptions
// scoped to the macro output.
#[allow(missing_docs, clippy::all, clippy::pedantic, clippy::nursery)]
mod bind {
    wasmtime::component::bindgen!({
        path: "../../../wit",
        world: "interceptor-world",
    });
}

use bind::jan_klod::interfaces::host_config as g_config;
use bind::jan_klod::interfaces::host_event as g_event;
use bind::jan_klod::interfaces::host_log as g_log;
use bind::jan_klod::interfaces::host_storage as g_storage;
use bind::jan_klod::interfaces::llm_provider as g_llm;
use bind::jan_klod::interfaces::llm_types as g_types;
use bind::exports::jan_klod::interfaces::interceptor as g_icept;

/// The completion backend an interceptor's `llm-provider` import resolves to:
/// given the request the guest assembled, return the assistant text.
///
/// It takes the **whole request**, not just the prompt. An interceptor that
/// consults a model constrains it — `interceptor-intent-router` sets a grammar
/// admitting exactly two labels — and a seam that passed only the last user
/// message would silently drop that constraint, turning a two-token classifier
/// into free-form generation the guest then has to parse.
///
/// Injected so the tier runs offline in tests; `Runtime::build_agent` backs it
/// with a real provider instance.
pub type ProviderFn =
    Box<dyn Fn(&crate::intercept::PendingRequest) -> String + Send + Sync>;

/// Host state for one interceptor guest.
struct InterceptorHost {
    wasi: WasiCtx,
    table: ResourceTable,
    component_id: String,
    section: ConfigSection,
    provider: ProviderFn,
    /// Open completion streams: handle -> the chunks left to drain.
    streams: HashMap<u32, VecDeque<g_llm::CompletionChunk>>,
    next_handle: u32,
    /// Where this guest's `host-storage` calls land — the core's durable store
    /// when one is open, an ephemeral map otherwise. See [`Storage`].
    storage: Storage,
}

/// The backing for one interceptor's `host-storage`.
///
/// This used to be unconditionally a private `HashMap`, which made every write
/// through the contract a lie by omission: `interceptor-permission` records
/// standing grants ("always allow writes under `src/`") through `host-storage`,
/// and those grants vanish when the process does — while the core holds an open
/// `SQLite` store, used for session transcripts, three fields away. An
/// interceptor that summarises context, or learns a routing preference, has
/// nowhere to put it.
///
/// So a durable store is available — but only to an instance whose config sets
/// `persist: true`, because the ephemeral default is itself load-bearing: the
/// permission gate's standing grants are documented as dying with the process,
/// and making every interceptor durable would quietly turn "always allow" into
/// "allow forever". Durability is a grant, like a workspace or a subprocess.
///
/// Sharing one database needs a second rule,
/// because until now isolation came for free from the map being private: with one
/// database behind every guest, an interceptor could name `session-abc` and read
/// the transcript, or name a peer's namespace and read its decisions. **Every
/// namespace is therefore prefixed with the component's own id** — a guest cannot
/// express a namespace outside its own subtree, because it never gets to write the
/// prefix. The core's own namespaces contain no `/`, so nothing a guest can ask
/// for collides with them.
enum Storage {
    /// A private map, for when no store is open (unit tests, offline harnesses).
    Ephemeral {
        /// (namespace, key) -> (value, created, updated).
        entries: HashMap<(String, String), (String, u64, u64)>,
        /// Monotonic clock, so timestamps order without a real clock.
        clock: u64,
    },
    /// The core's store, namespaced to the owning component.
    Durable {
        /// Shared with the [`AgentSession`](crate::AgentSession) that opened it.
        store: Arc<Mutex<PersistentStore>>,
        /// The component id every namespace is prefixed with.
        owner: String,
    },
}

impl Storage {
    /// The namespace a guest request actually reaches.
    fn scope(&self, namespace: &str) -> String {
        match self {
            Self::Ephemeral { .. } => namespace.to_string(),
            Self::Durable { owner, .. } => format!("ext/{owner}/{namespace}"),
        }
    }

    /// Undo [`Self::scope`], so returned entries name the namespace the guest
    /// asked for rather than the one the host stored under.
    fn unscope(&self, namespace: &str) -> String {
        match self {
            Self::Ephemeral { .. } => namespace.to_string(),
            Self::Durable { owner, .. } => namespace
                .strip_prefix(&format!("ext/{owner}/"))
                .unwrap_or(namespace)
                .to_string(),
        }
    }
}

/// Present an entry to the guest under the namespace it asked for.
fn present(storage: &Storage, entry: crate::store::Entry) -> g_storage::Entry {
    let namespace = storage.unscope(&entry.namespace);
    g_storage::Entry {
        id: format!("{namespace}/{}", entry.key),
        namespace,
        key: entry.key,
        value: entry.value,
        created_at: entry.created_at,
        updated_at: entry.updated_at,
    }
}

/// Map a store failure onto the contract's error.
fn as_store_error(err: &crate::store::StoreError) -> g_storage::StoreError {
    match err {
        crate::store::StoreError::NotFound => g_storage::StoreError::NotFound,
        crate::store::StoreError::Backend { detail } => {
            // The contract's error carries no detail, so log it rather than drop it.
            eprintln!("WARN [core] host-storage backend error: {detail}");
            g_storage::StoreError::Backend
        }
    }
}

impl WasiView for InterceptorHost {
    fn ctx(&mut self) -> WasiCtxView<'_> {
        WasiCtxView {
            ctx: &mut self.wasi,
            table: &mut self.table,
        }
    }
}

impl g_log::Host for InterceptorHost {
    fn log(
        &mut self,
        level: g_log::LogLevel,
        component: String,
        message: String,
        _fields: Vec<g_log::LogField>,
    ) {
        let level = match level {
            g_log::LogLevel::Debug => "DEBUG",
            g_log::LogLevel::Info => "INFO",
            g_log::LogLevel::Warn => "WARN",
            g_log::LogLevel::Error => "ERROR",
        };
        eprintln!("{level} [{}] {component}: {message}", self.component_id);
    }
}

impl g_config::Host for InterceptorHost {
    fn get(&mut self, key: String) -> Result<String, g_config::ConfigError> {
        self.section
            .get(&key)
            .ok_or(g_config::ConfigError::KeyNotFound)
    }
    fn has(&mut self, key: String) -> bool {
        self.section.has(&key)
    }
    fn all(&mut self) -> Result<String, g_config::ConfigError> {
        Ok(self.section.all())
    }
}

impl g_event::Host for InterceptorHost {
    /// One-way: the host records what an extension reports. There is no delivery
    /// side — see `wit/host-event.wit` for why the polling half was removed
    /// rather than left as a queue nobody fills.
    fn publish(&mut self, topic: String, payload: String) {
        eprintln!("EVENT [{}] {topic}: {payload}", self.component_id);
    }
}

impl g_storage::Host for InterceptorHost {
    fn set(
        &mut self,
        namespace: String,
        key: String,
        value: String,
    ) -> Result<g_storage::Entry, g_storage::StoreError> {
        let scoped = self.storage.scope(&namespace);
        match &mut self.storage {
            Storage::Ephemeral { entries, clock } => {
                *clock += 1;
                let now = *clock;
                let created = entries
                    .get(&(scoped.clone(), key.clone()))
                    .map_or(now, |(_, created, _)| *created);
                entries.insert((scoped, key.clone()), (value.clone(), created, now));
                Ok(g_storage::Entry {
                    id: format!("{namespace}/{key}"),
                    namespace,
                    key,
                    value,
                    created_at: created,
                    updated_at: now,
                })
            }
            Storage::Durable { store, .. } => {
                let entry = store
                    .lock()
                    .map_err(|_| g_storage::StoreError::Backend)?
                    .set(&scoped, &key, &value)
                    .map_err(|err| as_store_error(&err))?;
                Ok(present(&self.storage, entry))
            }
        }
    }

    fn get(
        &mut self,
        namespace: String,
        key: String,
    ) -> Result<g_storage::Entry, g_storage::StoreError> {
        let scoped = self.storage.scope(&namespace);
        match &self.storage {
            Storage::Ephemeral { entries, .. } => entries
                .get(&(scoped, key.clone()))
                .map(|(value, created, updated)| g_storage::Entry {
                    id: format!("{namespace}/{key}"),
                    namespace: namespace.clone(),
                    key: key.clone(),
                    value: value.clone(),
                    created_at: *created,
                    updated_at: *updated,
                })
                .ok_or(g_storage::StoreError::NotFound),
            Storage::Durable { store, .. } => {
                let entry = store
                    .lock()
                    .map_err(|_| g_storage::StoreError::Backend)?
                    .get(&scoped, &key)
                    .map_err(|err| as_store_error(&err))?;
                Ok(present(&self.storage, entry))
            }
        }
    }

    fn delete(&mut self, namespace: String, key: String) -> Result<(), g_storage::StoreError> {
        let scoped = self.storage.scope(&namespace);
        match &mut self.storage {
            Storage::Ephemeral { entries, .. } => {
                entries.remove(&(scoped, key)).map(|_| ()).ok_or(g_storage::StoreError::NotFound)
            }
            Storage::Durable { store, .. } => store
                .lock()
                .map_err(|_| g_storage::StoreError::Backend)?
                .delete(&scoped, &key)
                .map_err(|err| as_store_error(&err)),
        }
    }

    fn list_keys(
        &mut self,
        namespace: String,
    ) -> Result<Vec<g_storage::Entry>, g_storage::StoreError> {
        self.entries_in(&namespace)
    }

    fn recent(
        &mut self,
        namespace: String,
        limit: u32,
    ) -> Result<Vec<g_storage::Entry>, g_storage::StoreError> {
        let mut entries = self.entries_in(&namespace)?;
        entries.sort_by_key(|e| std::cmp::Reverse(e.updated_at));
        entries.truncate(limit as usize);
        Ok(entries)
    }
}

impl InterceptorHost {
    /// All stored entries in `namespace` as generated `entry` records.
    fn entries_in(&self, namespace: &str) -> Result<Vec<g_storage::Entry>, g_storage::StoreError> {
        let scoped = self.storage.scope(namespace);
        match &self.storage {
            Storage::Ephemeral { entries, .. } => Ok(entries
                .iter()
                .filter(|((ns, _), _)| *ns == scoped)
                .map(|((_, key), (value, created, updated))| g_storage::Entry {
                    id: format!("{namespace}/{key}"),
                    namespace: namespace.to_string(),
                    key: key.clone(),
                    value: value.clone(),
                    created_at: *created,
                    updated_at: *updated,
                })
                .collect()),
            Storage::Durable { store, .. } => {
                let rows = store
                    .lock()
                    .map_err(|_| g_storage::StoreError::Backend)?
                    .recent(&scoped, u32::MAX)
                    .map_err(|err| as_store_error(&err))?;
                Ok(rows.into_iter().map(|row| present(&self.storage, row)).collect())
            }
        }
    }
}

/// Convert the guest's generated request into the core's neutral shape, so the
/// backing provider sees the model, grammar, and full message list the
/// interceptor actually asked for.
fn to_pending_request(request: &g_llm::CompletionRequest) -> crate::intercept::PendingRequest {
    use crate::intercept::{Message, PendingRequest, Role};
    PendingRequest {
        model: Some(request.model.clone()).filter(|m| !m.is_empty()),
        messages: request
            .messages
            .iter()
            .map(|m| Message {
                role: match m.role {
                    g_types::Role::System => Role::System,
                    g_types::Role::User => Role::User,
                    g_types::Role::Assistant => Role::Assistant,
                    g_types::Role::Tool => Role::Tool,
                },
                content: m.content.clone(),
                tool_call_id: m.tool_call_id.clone(),
            })
            .collect(),
        // An interceptor's classification call offers no tools: it is asking a
        // question, not running a turn.
        tools: Vec::new(),
        grammar: request.grammar.clone(),
        max_tokens: request.max_tokens,
        temperature: request.temperature,
    }
}

impl g_llm::Host for InterceptorHost {
    fn complete(
        &mut self,
        request: g_llm::CompletionRequest,
    ) -> Result<u32, g_llm::ProviderError> {
        let text = (self.provider)(&to_pending_request(&request));
        let handle = self.next_handle;
        self.next_handle += 1;
        let chunks = VecDeque::from(vec![
            g_llm::CompletionChunk::TextDelta(text),
            g_llm::CompletionChunk::Done("stop".to_string()),
        ]);
        self.streams.insert(handle, chunks);
        Ok(handle)
    }

    fn next_chunk(&mut self, handle: u32) -> Option<g_llm::CompletionChunk> {
        self.streams.get_mut(&handle).and_then(VecDeque::pop_front)
    }

    fn close_stream(&mut self, handle: u32) {
        self.streams.remove(&handle);
    }

    fn info(&mut self) -> g_llm::ProviderInfo {
        g_llm::ProviderInfo {
            id: "canned".to_string(),
            supported_models: vec![],
        }
    }
}

/// A sandboxed interceptor component, adapted to the [`Interceptor`] trait.
pub struct WasmInterceptor {
    id: String,
    store: Store<InterceptorHost>,
    world: bind::InterceptorWorld,
    phases: Vec<Phase>,
}

impl WasmInterceptor {
    /// Instantiate `component` as an interceptor, run its lifecycle to `start`,
    /// and cache the phases it subscribes to. `provider` backs its `llm-provider`
    /// import (a canned completion for offline use).
    ///
    /// # Errors
    /// Returns a [`CoreError`] if a capability cannot be wired, the component
    /// cannot be instantiated, or a lifecycle call traps or is rejected.
    pub fn instantiate(
        engine: &Engine,
        id: &str,
        component: &Component,
        section: ConfigSection,
        provider: ProviderFn,
    ) -> Result<Self, CoreError> {
        Self::instantiate_with_storage(engine, id, component, section, provider, None)
    }

    /// As [`Self::instantiate`], with the core's store backing `host-storage`.
    ///
    /// Passing the store is what makes a standing permission grant outlive the
    /// process. Passing `None` keeps the private-map behaviour, which is what an
    /// offline unit test wants.
    ///
    /// # Errors
    /// As [`Self::instantiate`].
    pub fn instantiate_with_storage(
        engine: &Engine,
        id: &str,
        component: &Component,
        section: ConfigSection,
        provider: ProviderFn,
        storage: Option<Arc<Mutex<PersistentStore>>>,
    ) -> Result<Self, CoreError> {
        let mut linker: Linker<InterceptorHost> = Linker::new(engine);
        wasmtime_wasi::p2::add_to_linker_sync(&mut linker).map_err(CoreError::linker)?;
        g_log::add_to_linker::<_, HasSelf<_>>(&mut linker, |s| s).map_err(CoreError::linker)?;
        g_config::add_to_linker::<_, HasSelf<_>>(&mut linker, |s| s).map_err(CoreError::linker)?;
        g_event::add_to_linker::<_, HasSelf<_>>(&mut linker, |s| s).map_err(CoreError::linker)?;
        g_storage::add_to_linker::<_, HasSelf<_>>(&mut linker, |s| s).map_err(CoreError::linker)?;
        g_llm::add_to_linker::<_, HasSelf<_>>(&mut linker, |s| s).map_err(CoreError::linker)?;

        let host = InterceptorHost {
            wasi: WasiCtxBuilder::new().inherit_stdio().build(),
            table: ResourceTable::new(),
            component_id: id.to_string(),
            section,
            provider,
            streams: HashMap::new(),
            next_handle: 1,
            storage: storage.map_or_else(
                || Storage::Ephemeral { entries: HashMap::new(), clock: 0 },
                |store| Storage::Durable { store, owner: id.to_string() },
            ),
        };
        let mut store = Store::new(engine, host);
        let world = bind::InterceptorWorld::instantiate(&mut store, component, &linker)
            .map_err(|source| CoreError::instantiate(id, source))?;

        // Lifecycle init -> start.
        let lifecycle = world.jan_klod_interfaces_extension_lifecycle();
        let ctx = bind::exports::jan_klod::interfaces::extension_lifecycle::ExtensionContext {
            id: id.to_string(),
            version: "0.0.0".to_string(),
        };
        drive(id, lifecycle.call_init(&mut store, &ctx), "init")?;
        drive(id, lifecycle.call_start(&mut store), "start")?;

        // Resolve subscribed phases once, at boot.
        let phases = world
            .jan_klod_interfaces_interceptor()
            .call_subscribed_phases(&mut store)
            .map_err(|source| CoreError::Lifecycle {
                id: id.to_string(),
                phase: "subscribed-phases",
                source: source.into(),
            })?
            .into_iter()
            .map(from_gen_phase)
            .collect();

        Ok(Self {
            id: id.to_string(),
            store,
            world,
            phases,
        })
    }
}

impl Interceptor for WasmInterceptor {
    fn id(&self) -> &str {
        &self.id
    }

    fn subscribed_phases(&self) -> Vec<Phase> {
        self.phases.clone()
    }

    fn intercept(&mut self, input: &InterceptInput) -> Result<Decision, InterceptorError> {
        let gen_input = g_icept::InterceptInput {
            phase: to_gen_phase(input.phase),
            state: to_gen_state(&input.state),
            answer: input.answer.clone(),
        };
        // A guest trap folds to `internal` — the dispatcher applies the
        // fail-closed-at-tool-call policy on any `Err`.
        match self
            .world
            .jan_klod_interfaces_interceptor()
            .call_intercept(&mut self.store, &gen_input)
        {
            Ok(Ok(decision)) => Ok(from_gen_decision(decision)),
            Ok(Err(err)) => Err(from_gen_error(err)),
            Err(_) => Err(InterceptorError::Internal),
        }
    }
}

/// Collapse a lifecycle call's nested result into a [`CoreError`].
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
// Type mapping: host `intercept::*` <-> generated `g_icept::*`.
// ---------------------------------------------------------------------------

const fn to_gen_phase(phase: Phase) -> g_icept::Phase {
    match phase {
        Phase::BeforeLoop => g_icept::Phase::BeforeLoop,
        Phase::SelectModel => g_icept::Phase::SelectModel,
        Phase::SelectContext => g_icept::Phase::SelectContext,
        Phase::SelectTools => g_icept::Phase::SelectTools,
        Phase::AfterResponse => g_icept::Phase::AfterResponse,
        Phase::ToolCall => g_icept::Phase::ToolCall,
        Phase::ToolResult => g_icept::Phase::ToolResult,
        Phase::Finalize => g_icept::Phase::Finalize,
        Phase::PrepareNextTurn => g_icept::Phase::PrepareNextTurn,
    }
}

const fn from_gen_phase(phase: g_icept::Phase) -> Phase {
    match phase {
        g_icept::Phase::BeforeLoop => Phase::BeforeLoop,
        g_icept::Phase::SelectModel => Phase::SelectModel,
        g_icept::Phase::SelectContext => Phase::SelectContext,
        g_icept::Phase::SelectTools => Phase::SelectTools,
        g_icept::Phase::AfterResponse => Phase::AfterResponse,
        g_icept::Phase::ToolCall => Phase::ToolCall,
        g_icept::Phase::ToolResult => Phase::ToolResult,
        g_icept::Phase::Finalize => Phase::Finalize,
        g_icept::Phase::PrepareNextTurn => Phase::PrepareNextTurn,
    }
}

const fn to_gen_role(role: intercept::Role) -> g_types::Role {
    match role {
        intercept::Role::System => g_types::Role::System,
        intercept::Role::User => g_types::Role::User,
        intercept::Role::Assistant => g_types::Role::Assistant,
        intercept::Role::Tool => g_types::Role::Tool,
    }
}

const fn from_gen_role(role: g_types::Role) -> intercept::Role {
    match role {
        g_types::Role::System => intercept::Role::System,
        g_types::Role::User => intercept::Role::User,
        g_types::Role::Assistant => intercept::Role::Assistant,
        g_types::Role::Tool => intercept::Role::Tool,
    }
}

fn to_gen_message(m: &intercept::Message) -> g_types::Message {
    g_types::Message {
        role: to_gen_role(m.role),
        content: m.content.clone(),
        tool_call_id: m.tool_call_id.clone(),
    }
}

fn from_gen_message(m: g_types::Message) -> intercept::Message {
    intercept::Message {
        role: from_gen_role(m.role),
        content: m.content,
        tool_call_id: m.tool_call_id,
    }
}

fn to_gen_tool(t: &intercept::ToolDefinition) -> g_types::ToolDefinition {
    g_types::ToolDefinition {
        name: t.name.clone(),
        description: t.description.clone(),
        parameters_schema: t.parameters_schema.clone(),
    }
}

fn from_gen_tool(t: g_types::ToolDefinition) -> intercept::ToolDefinition {
    intercept::ToolDefinition {
        name: t.name,
        description: t.description,
        parameters_schema: t.parameters_schema,
    }
}

fn to_gen_call(c: &intercept::ToolCall) -> g_types::ToolCall {
    g_types::ToolCall {
        id: c.id.clone(),
        name: c.name.clone(),
        arguments: c.arguments.clone(),
    }
}

fn from_gen_call(c: g_types::ToolCall) -> intercept::ToolCall {
    intercept::ToolCall {
        id: c.id,
        name: c.name,
        arguments: c.arguments,
    }
}

fn to_gen_request(r: &intercept::PendingRequest) -> g_icept::PendingRequest {
    g_icept::PendingRequest {
        model: r.model.clone(),
        messages: r.messages.iter().map(to_gen_message).collect(),
        tools: r.tools.iter().map(to_gen_tool).collect(),
        grammar: r.grammar.clone(),
        max_tokens: r.max_tokens,
        temperature: r.temperature,
    }
}

fn from_gen_request(r: g_icept::PendingRequest) -> intercept::PendingRequest {
    intercept::PendingRequest {
        model: r.model,
        messages: r.messages.into_iter().map(from_gen_message).collect(),
        tools: r.tools.into_iter().map(from_gen_tool).collect(),
        grammar: r.grammar,
        max_tokens: r.max_tokens,
        temperature: r.temperature,
    }
}

fn to_gen_state(state: &HookState) -> g_icept::HookState {
    match state {
        HookState::BeforeLoop(t) => g_icept::HookState::BeforeLoop(g_icept::UserTurn {
            session: t.session.clone(),
            user_message: t.user_message.clone(),
        }),
        HookState::SelectModel(r) => g_icept::HookState::SelectModel(to_gen_request(r)),
        HookState::SelectContext(r) => g_icept::HookState::SelectContext(to_gen_request(r)),
        HookState::SelectTools(r) => g_icept::HookState::SelectTools(to_gen_request(r)),
        HookState::AfterResponse(r) => g_icept::HookState::AfterResponse(g_icept::RawResponse {
            text: r.text.clone(),
            finish_reason: r.finish_reason.clone(),
        }),
        HookState::ToolCall(c) => g_icept::HookState::ToolCall(to_gen_call(c)),
        HookState::ToolResult(o) => g_icept::HookState::ToolResult(g_icept::ToolOutcome {
            tool_call_id: o.tool_call_id.clone(),
            content: o.content.clone(),
        }),
        HookState::Finalize(a) => g_icept::HookState::Finalize(g_icept::FinalAnswer {
            text: a.text.clone(),
        }),
        HookState::PrepareNextTurn(r) => g_icept::HookState::PrepareNextTurn(to_gen_request(r)),
    }
}

fn from_gen_state(state: g_icept::HookState) -> HookState {
    match state {
        g_icept::HookState::BeforeLoop(t) => HookState::BeforeLoop(intercept::UserTurn {
            session: t.session,
            user_message: t.user_message,
        }),
        g_icept::HookState::SelectModel(r) => HookState::SelectModel(from_gen_request(r)),
        g_icept::HookState::SelectContext(r) => HookState::SelectContext(from_gen_request(r)),
        g_icept::HookState::SelectTools(r) => HookState::SelectTools(from_gen_request(r)),
        g_icept::HookState::AfterResponse(r) => HookState::AfterResponse(intercept::RawResponse {
            text: r.text,
            finish_reason: r.finish_reason,
        }),
        g_icept::HookState::ToolCall(c) => HookState::ToolCall(from_gen_call(c)),
        g_icept::HookState::ToolResult(o) => HookState::ToolResult(intercept::ToolOutcome {
            tool_call_id: o.tool_call_id,
            content: o.content,
        }),
        g_icept::HookState::Finalize(a) => HookState::Finalize(intercept::FinalAnswer { text: a.text }),
        g_icept::HookState::PrepareNextTurn(r) => HookState::PrepareNextTurn(from_gen_request(r)),
    }
}

fn from_gen_decision(decision: g_icept::Decision) -> Decision {
    match decision {
        g_icept::Decision::Proceed => Decision::Proceed,
        g_icept::Decision::Replace(state) => Decision::Replace(from_gen_state(state)),
        g_icept::Decision::Block(reason) => Decision::Block(BlockReason {
            message: reason.message,
        }),
        g_icept::Decision::Ask(prompt) => Decision::Ask(UserPrompt {
            question: prompt.question,
            options: prompt.options,
            default_answer: prompt.default_answer,
        }),
    }
}

const fn from_gen_error(err: g_icept::InterceptorError) -> InterceptorError {
    match err {
        g_icept::InterceptorError::Internal => InterceptorError::Internal,
        g_icept::InterceptorError::InvalidState => InterceptorError::InvalidState,
        g_icept::InterceptorError::DependencyFailed => InterceptorError::DependencyFailed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::intercept::{
        Dispatcher, Driver, HookState, Message, Outcome, PendingRequest, Role, UserTurn,
    };
    use serde_json::json;
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};

    fn repo_root() -> PathBuf {
        [env!("CARGO_MANIFEST_DIR"), "..", "..", ".."].iter().collect()
    }

    /// A driver that never expects to be asked (the intent router does not `ask`).
    struct NoDriver;
    impl Driver for NoDriver {
        fn ask(&mut self, _prompt: &UserPrompt) -> String {
            panic!("intent router must not ask");
        }
    }

    fn load_intent_router(provider: ProviderFn) -> Option<(Engine, WasmInterceptor)> {
        let path = repo_root().join("ext").join("interceptor-intent-router.wasm");
        if !path.exists() {
            eprintln!("skipping: interceptor-intent-router.wasm not staged — run `make ext`");
            return None;
        }
        let engine = Engine::default();
        let component = Component::from_file(&engine, &path).expect("component compiles");
        let interceptor = WasmInterceptor::instantiate(
            &engine,
            "interceptor.intent-router",
            &component,
            ConfigSection::new(json!({})),
            provider,
        )
        .expect("interceptor instantiates");
        Some((engine, interceptor))
    }

    fn before_loop(msg: &str) -> HookState {
        HookState::BeforeLoop(UserTurn {
            session: "s".into(),
            user_message: msg.into(),
        })
    }

    /// A driver that answers every `ask` with a fixed response.
    struct FixedDriver(&'static str);
    impl Driver for FixedDriver {
        fn ask(&mut self, _prompt: &UserPrompt) -> String {
            self.0.to_string()
        }
    }

    fn load_permission() -> Option<WasmInterceptor> {
        let path = repo_root().join("ext").join("interceptor-permission.wasm");
        if !path.exists() {
            eprintln!("skipping: interceptor-permission.wasm not staged — run `make ext`");
            return None;
        }
        let engine = Engine::default();
        let component = Component::from_file(&engine, &path).expect("component compiles");
        Some(
            WasmInterceptor::instantiate(
                &engine,
                "interceptor.permission",
                &component,
                ConfigSection::new(json!({})),
                Box::new(|_| String::new()),
            )
            .expect("interceptor instantiates"),
        )
    }

    fn tool_call(name: &str) -> HookState {
        HookState::ToolCall(crate::intercept::ToolCall {
            id: "1".into(),
            name: name.into(),
            arguments: "{}".into(),
        })
    }

    #[test]
    fn permission_subscribes_only_to_tool_call() {
        let Some(p) = load_permission() else { return };
        assert_eq!(p.subscribed_phases(), vec![Phase::ToolCall]);
    }

    #[test]
    fn permission_blocks_a_dangerous_tool_when_denied() {
        let Some(p) = load_permission() else { return };
        let mut d = Dispatcher::new(vec![Box::new(p)]);
        let mut state = tool_call("bash");
        assert!(matches!(
            d.dispatch(Phase::ToolCall, &mut state, &mut FixedDriver("no")),
            Outcome::Blocked(_)
        ));
    }

    #[test]
    fn permission_allows_a_dangerous_tool_when_approved() {
        let Some(p) = load_permission() else { return };
        let mut d = Dispatcher::new(vec![Box::new(p)]);
        let mut state = tool_call("bash");
        assert!(matches!(
            d.dispatch(Phase::ToolCall, &mut state, &mut FixedDriver("yes")),
            Outcome::Proceeded
        ));
    }

    /// Counts how many times it was asked, so a test can prove a second call did
    /// *not* reach the user.
    struct CountingDriver {
        answer: &'static str,
        asked: std::cell::Cell<usize>,
    }
    impl CountingDriver {
        fn new(answer: &'static str) -> Self {
            Self { answer, asked: std::cell::Cell::new(0) }
        }
    }
    impl Driver for CountingDriver {
        fn ask(&mut self, _prompt: &UserPrompt) -> String {
            self.asked.set(self.asked.get() + 1);
            self.answer.to_string()
        }
    }

    fn tool_call_with(name: &str, arguments: &str) -> HookState {
        HookState::ToolCall(crate::intercept::ToolCall {
            id: "1".into(),
            name: name.into(),
            arguments: arguments.into(),
        })
    }

    #[test]
    fn permission_offers_a_standing_decision_and_then_stops_asking() {
        let Some(p) = load_permission() else { return };
        let mut d = Dispatcher::new(vec![Box::new(p)]);
        let mut driver = CountingDriver::new("always");

        let mut first = tool_call_with("fs", r#"{"op":"write","path":"src/a.rs"}"#);
        assert!(matches!(d.dispatch(Phase::ToolCall, &mut first, &mut driver), Outcome::Proceeded));
        assert_eq!(driver.asked.get(), 1, "the first call asks");

        // Same kind of action, different argument: covered by the standing decision.
        let mut second = tool_call_with("fs", r#"{"op":"write","path":"src/b.rs"}"#);
        assert!(matches!(d.dispatch(Phase::ToolCall, &mut second, &mut driver), Outcome::Proceeded));
        assert_eq!(driver.asked.get(), 1, "the second call must not ask again");

        // A different kind of action is a different scope — it still asks.
        let mut other = tool_call_with("fs", r#"{"op":"delete","path":"src/a.rs"}"#);
        let _ = d.dispatch(Phase::ToolCall, &mut other, &mut driver);
        assert_eq!(driver.asked.get(), 2, "`fs:delete` is not covered by `fs:write`");
    }

    #[test]
    fn a_standing_allow_does_not_cover_a_path_outside_the_workspace() {
        let Some(p) = load_permission() else { return };
        let mut d = Dispatcher::new(vec![Box::new(p)]);
        let mut driver = CountingDriver::new("always");

        let mut inside = tool_call_with("fs", r#"{"op":"write","path":"src/a.rs"}"#);
        assert!(matches!(d.dispatch(Phase::ToolCall, &mut inside, &mut driver), Outcome::Proceeded));
        assert_eq!(driver.asked.get(), 1);

        // "Always allow writes" is about writing files, not about writing outside
        // the workspace — the escape must still be put to the user.
        let mut escaping = tool_call_with("fs", r#"{"op":"write","path":"/etc/passwd"}"#);
        let _ = d.dispatch(Phase::ToolCall, &mut escaping, &mut driver);
        assert_eq!(driver.asked.get(), 2, "a scope escape is asked every time");
    }

    #[test]
    fn permission_remembers_a_refusal_too() {
        let Some(p) = load_permission() else { return };
        let mut d = Dispatcher::new(vec![Box::new(p)]);
        let mut driver = CountingDriver::new("never");

        let mut first = tool_call_with("shell", r#"{"command":"curl evil.example"}"#);
        assert!(matches!(d.dispatch(Phase::ToolCall, &mut first, &mut driver), Outcome::Blocked(_)));
        assert_eq!(driver.asked.get(), 1);

        let mut second = tool_call_with("shell", r#"{"command":"curl other.example"}"#);
        assert!(matches!(
            d.dispatch(Phase::ToolCall, &mut second, &mut driver),
            Outcome::Blocked(_)
        ));
        assert_eq!(driver.asked.get(), 1, "a standing `never` blocks without asking");

        // A different program is a different scope, so it is still asked about.
        let mut cargo = tool_call_with("shell", r#"{"command":"cargo test"}"#);
        let _ = d.dispatch(Phase::ToolCall, &mut cargo, &mut driver);
        assert_eq!(driver.asked.get(), 2, "`shell:cargo` is not covered by `shell:curl`");
    }

    #[test]
    fn a_one_off_answer_leaves_no_standing_decision() {
        let Some(p) = load_permission() else { return };
        let mut d = Dispatcher::new(vec![Box::new(p)]);
        let mut driver = CountingDriver::new("yes");

        for _ in 0..3 {
            let mut state = tool_call_with("fs", r#"{"op":"write","path":"src/a.rs"}"#);
            assert!(matches!(
                d.dispatch(Phase::ToolCall, &mut state, &mut driver),
                Outcome::Proceeded
            ));
        }
        assert_eq!(driver.asked.get(), 3, "plain `yes` approves once, every time");
    }

    /// An allowlisted read runs untouched.
    #[test]
    fn permission_ignores_a_known_read_only_call() {
        let Some(p) = load_permission() else { return };
        let mut d = Dispatcher::new(vec![Box::new(p)]);
        let mut state = tool_call_with("fs", r#"{"op":"read","path":"src/main.rs"}"#);
        // NoDriver panics if asked — a classified read must never trigger an ask.
        assert!(matches!(
            d.dispatch(Phase::ToolCall, &mut state, &mut NoDriver),
            Outcome::Proceeded
        ));
    }

    /// And a tool nobody classified does not.
    ///
    /// This test used to assert the opposite — that `web_search` proceeds
    /// untouched — which is what a denylist does with every name it has not heard
    /// of. That is the behaviour that let `tool-edit` write files unasked. Under
    /// an allowlist the unclassified call is exactly the one to stop.
    #[test]
    fn permission_asks_about_a_tool_nobody_has_classified() {
        let Some(p) = load_permission() else { return };
        let mut d = Dispatcher::new(vec![Box::new(p)]);
        let mut state = tool_call("web_search");
        let driver = CountingDriver { answer: "no", asked: std::cell::Cell::new(0) };
        let outcome = d.dispatch(Phase::ToolCall, &mut state, &mut { driver });
        assert!(matches!(outcome, Outcome::Blocked { .. }), "refused: {outcome:?}");
    }

    fn load_tool_selector() -> Option<WasmInterceptor> {
        let path = repo_root().join("ext").join("interceptor-tool-selector.wasm");
        if !path.exists() {
            eprintln!("skipping: interceptor-tool-selector.wasm not staged — run `make ext`");
            return None;
        }
        let engine = Engine::default();
        let component = Component::from_file(&engine, &path).expect("component compiles");
        Some(
            WasmInterceptor::instantiate(
                &engine,
                "interceptor.tool-selector",
                &component,
                ConfigSection::new(json!({})),
                Box::new(|_| String::new()),
            )
            .expect("interceptor instantiates"),
        )
    }

    fn select_tools() -> HookState {
        HookState::SelectTools(crate::intercept::PendingRequest {
            model: Some("m".into()),
            messages: vec![],
            tools: vec![],
            grammar: None,
            max_tokens: None,
            temperature: None,
        })
    }

    #[test]
    fn tool_selector_subscribes_only_to_select_tools() {
        let Some(t) = load_tool_selector() else { return };
        assert_eq!(t.subscribed_phases(), vec![Phase::SelectTools]);
    }

    #[test]
    fn tool_selector_passes_through() {
        let Some(t) = load_tool_selector() else { return };
        let mut d = Dispatcher::new(vec![Box::new(t)]);
        let mut state = select_tools();
        assert!(matches!(
            d.dispatch(Phase::SelectTools, &mut state, &mut NoDriver),
            Outcome::Proceeded
        ));
    }

    fn load_tool_selector_with(config: serde_json::Value) -> Option<WasmInterceptor> {
        let path = repo_root().join("ext").join("interceptor-tool-selector.wasm");
        if !path.exists() {
            return None;
        }
        let engine = Engine::default();
        let component = Component::from_file(&engine, &path).expect("component compiles");
        Some(
            WasmInterceptor::instantiate(
                &engine,
                "interceptor.tool-selector",
                &component,
                ConfigSection::new(config),
                Box::new(|_| String::new()),
            )
            .expect("interceptor instantiates"),
        )
    }

    #[test]
    fn tool_selector_advertises_configured_tools() {
        let config = json!({
            "tools": [
                { "name": "fs-read", "description": "read a file", "parameters-schema": "{}" }
            ]
        });
        let Some(t) = load_tool_selector_with(config) else { return };
        let mut d = Dispatcher::new(vec![Box::new(t)]);
        let mut state = select_tools();
        d.dispatch(Phase::SelectTools, &mut state, &mut NoDriver);
        let HookState::SelectTools(request) = state else { panic!("state case changed") };
        assert_eq!(request.tools.len(), 1, "the advertised tool is placed on the request");
        assert_eq!(request.tools[0].name, "fs-read");
    }

    fn load_context(config: serde_json::Value) -> Option<WasmInterceptor> {
        let path = repo_root().join("ext").join("interceptor-context.wasm");
        if !path.exists() {
            eprintln!("skipping: interceptor-context.wasm not staged — run `make ext`");
            return None;
        }
        let engine = Engine::default();
        let component = Component::from_file(&engine, &path).expect("component compiles");
        Some(
            WasmInterceptor::instantiate(
                &engine,
                "interceptor.context",
                &component,
                ConfigSection::new(config),
                Box::new(|_| String::new()),
            )
            .expect("interceptor instantiates"),
        )
    }

    fn msg(role: Role, content: &str) -> Message {
        Message {
            role,
            content: content.into(),
            tool_call_id: None,
        }
    }

    fn select_context(messages: Vec<Message>) -> HookState {
        HookState::SelectContext(PendingRequest {
            model: Some("m".into()),
            messages,
            tools: vec![],
            grammar: None,
            max_tokens: None,
            temperature: None,
        })
    }

    fn message_count(state: &HookState) -> usize {
        match state {
            HookState::SelectContext(r) => r.messages.len(),
            _ => panic!("expected select-context state"),
        }
    }

    #[test]
    fn context_subscribes_only_to_select_context() {
        let Some(c) = load_context(json!({})) else { return };
        assert_eq!(c.subscribed_phases(), vec![Phase::SelectContext]);
    }

    #[test]
    fn context_trims_over_budget_history() {
        // A tiny configured budget forces trimming.
        let Some(c) = load_context(json!({ "context-tokens": 5 })) else { return };
        let mut d = Dispatcher::new(vec![Box::new(c)]);
        let long = "x".repeat(200); // ~50 tokens each
        let mut state = select_context(vec![
            msg(Role::System, "sys"),
            msg(Role::User, &long),
            msg(Role::Assistant, &long),
            msg(Role::User, &long),
        ]);
        d.dispatch(Phase::SelectContext, &mut state, &mut NoDriver);
        let kept = message_count(&state);
        assert!(kept < 4, "over-budget history is trimmed (kept {kept})");
        assert!(kept >= 2, "system + current turn are always kept (kept {kept})");
    }

    #[test]
    fn context_leaves_small_history_untouched() {
        let Some(c) = load_context(json!({ "context-tokens": 100_000 })) else { return };
        let mut d = Dispatcher::new(vec![Box::new(c)]);
        let mut state = select_context(vec![msg(Role::System, "sys"), msg(Role::User, "hi")]);
        assert!(matches!(
            d.dispatch(Phase::SelectContext, &mut state, &mut NoDriver),
            Outcome::Proceeded
        ));
        assert_eq!(message_count(&state), 2, "nothing dropped under budget");
    }

    fn load_task_router(config: serde_json::Value, provider: ProviderFn) -> Option<WasmInterceptor> {
        let path = repo_root().join("ext").join("interceptor-task-router.wasm");
        if !path.exists() {
            eprintln!("skipping: interceptor-task-router.wasm not staged — run `make ext`");
            return None;
        }
        let engine = Engine::default();
        let component = Component::from_file(&engine, &path).expect("component compiles");
        Some(
            WasmInterceptor::instantiate(
                &engine,
                "interceptor.task-router",
                &component,
                ConfigSection::new(config),
                provider,
            )
            .expect("interceptor instantiates"),
        )
    }

    fn select_model(user_message: &str) -> HookState {
        HookState::SelectModel(PendingRequest {
            model: None,
            messages: vec![msg(Role::User, user_message)],
            tools: vec![],
            grammar: None,
            max_tokens: None,
            temperature: None,
        })
    }

    fn model_of(state: &HookState) -> Option<String> {
        match state {
            HookState::SelectModel(r) => r.model.clone(),
            _ => panic!("expected select-model state"),
        }
    }

    #[test]
    fn task_router_subscribes_only_to_select_model() {
        let Some(t) = load_task_router(json!({}), Box::new(|_| "chat".into())) else { return };
        assert_eq!(t.subscribed_phases(), vec![Phase::SelectModel]);
    }

    #[test]
    fn task_router_sets_model_from_routing_table() {
        // Classifier says "code-generation"; the routing table maps it to a model.
        let config = json!({ "routing": { "code-generation": "openai/gpt-4o" } });
        let Some(t) = load_task_router(config, Box::new(|_| "code-generation".into())) else {
            return;
        };
        let mut d = Dispatcher::new(vec![Box::new(t)]);
        let mut state = select_model("write a function");
        d.dispatch(Phase::SelectModel, &mut state, &mut NoDriver);
        assert_eq!(model_of(&state).as_deref(), Some("gpt-4o"));
    }

    #[test]
    fn task_router_proceeds_when_no_route_configured() {
        // A classified task with no routing entry leaves the model unset.
        let Some(t) = load_task_router(json!({}), Box::new(|_| "chat".into())) else { return };
        let mut d = Dispatcher::new(vec![Box::new(t)]);
        let mut state = select_model("hi");
        assert!(matches!(
            d.dispatch(Phase::SelectModel, &mut state, &mut NoDriver),
            Outcome::Proceeded
        ));
        assert_eq!(model_of(&state), None);
    }

    #[test]
    fn intent_router_subscribes_only_to_before_loop() {
        let Some((_engine, interceptor)) = load_intent_router(Box::new(|_| "agentic".into())) else {
            return;
        };
        assert_eq!(interceptor.subscribed_phases(), vec![Phase::BeforeLoop]);
    }

    #[test]
    fn heuristic_greeting_blocks_without_calling_the_provider() {
        // If the router touched the provider for "hello", this closure would flip
        // the verdict to agentic — so a Block proves the heuristic short-circuit.
        let Some((_engine, router)) = load_intent_router(Box::new(|_| "agentic".into())) else {
            return;
        };
        let mut d = Dispatcher::new(vec![Box::new(router)]);
        let mut state = before_loop("hello");
        assert!(matches!(
            d.dispatch(Phase::BeforeLoop, &mut state, &mut NoDriver),
            Outcome::Blocked(_)
        ));
    }

    #[test]
    fn multi_step_prompt_reaches_the_provider_and_proceeds() {
        // English multi-step prompt passes the heuristics to the LLM tier; the
        // canned provider says "agentic" -> the loop proceeds.
        let Some((_engine, router)) = load_intent_router(Box::new(|_| "agentic".into())) else {
            return;
        };
        let mut d = Dispatcher::new(vec![Box::new(router)]);
        let mut state = before_loop("Refactor the auth module and run the whole test suite");
        assert!(matches!(
            d.dispatch(Phase::BeforeLoop, &mut state, &mut NoDriver),
            Outcome::Proceeded
        ));
    }

    #[test]
    fn provider_verdict_simple_blocks() {
        // Same multi-step prompt, but the canned provider deems it simple -> Block.
        let Some((_engine, router)) = load_intent_router(Box::new(|_| "simple".into())) else {
            return;
        };
        let mut d = Dispatcher::new(vec![Box::new(router)]);
        let mut state = before_loop("Tell me a fact and then do three unrelated things please");
        assert!(matches!(
            d.dispatch(Phase::BeforeLoop, &mut state, &mut NoDriver),
            Outcome::Blocked(_)
        ));
    }

    #[test]
    fn the_seam_hands_the_provider_the_whole_request_not_just_a_prompt() {
        // The router constrains its classifier to two labels. A seam that passed
        // only the last user message would drop that grammar, and the "classifier"
        // would become free-form generation the guest then has to parse.
        let seen: Arc<Mutex<Option<crate::intercept::PendingRequest>>> =
            Arc::new(Mutex::new(None));
        let captured = Arc::clone(&seen);
        let Some((_engine, router)) = load_intent_router(Box::new(move |request| {
            *captured.lock().unwrap() = Some(request.clone());
            "agentic".into()
        })) else {
            return;
        };
        let mut d = Dispatcher::new(vec![Box::new(router)]);
        let mut state = before_loop("Tell me a fact and then do three unrelated things please");
        let _ = d.dispatch(Phase::BeforeLoop, &mut state, &mut NoDriver);

        let request = seen.lock().unwrap().clone().expect("the provider was consulted");
        let grammar = request.grammar.expect("the classifier's grammar survives the seam");
        assert!(grammar.contains("simple"), "grammar admits `simple`: {grammar}");
        assert!(grammar.contains("agentic"), "grammar admits `agentic`: {grammar}");
        assert!(
            request.messages.iter().any(|m| m.content.contains("three unrelated things")),
            "the prompt reaches the provider: {:?}",
            request.messages
        );
        assert!(request.tools.is_empty(), "a classification offers no tools");
    }
}
