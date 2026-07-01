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

use wasmtime::component::{Component, HasSelf, Linker};
use wasmtime::{Engine, Store};
use wasmtime_wasi::{ResourceTable, WasiCtx, WasiCtxBuilder, WasiCtxView, WasiView};

use crate::host::ConfigSection;
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

/// Canned completion backend: given the request's last user message, return the
/// assistant text. Injected so an interceptor's LLM tier runs offline.
pub type ProviderFn = Box<dyn Fn(&str) -> String + Send + Sync>;

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
    /// In-memory `host-storage`: (namespace, key) -> (value, created, updated).
    storage: HashMap<(String, String), (String, u64, u64)>,
    /// Monotonic clock for storage timestamps and recency ordering.
    clock: u64,
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
    // Observation-only: publish logs; there is no in-loop subscriber yet, so
    // subscribe/poll are inert but well-formed.
    fn publish(&mut self, topic: String, payload: String) {
        eprintln!("EVENT [{}] {topic}: {payload}", self.component_id);
    }
    fn subscribe(&mut self, _topic_prefix: String) -> u32 {
        0
    }
    fn next_event(&mut self, _handle: u32) -> Option<g_event::Event> {
        None
    }
    fn unsubscribe(&mut self, _handle: u32) {}
}

impl g_storage::Host for InterceptorHost {
    fn set(
        &mut self,
        namespace: String,
        key: String,
        value: String,
    ) -> Result<g_storage::Entry, g_storage::StoreError> {
        self.clock += 1;
        let now = self.clock;
        let created = self
            .storage
            .get(&(namespace.clone(), key.clone()))
            .map_or(now, |(_, created, _)| *created);
        self.storage
            .insert((namespace.clone(), key.clone()), (value.clone(), created, now));
        Ok(g_storage::Entry {
            id: format!("{namespace}/{key}"),
            namespace,
            key,
            value,
            created_at: created,
            updated_at: now,
        })
    }

    fn get(
        &mut self,
        namespace: String,
        key: String,
    ) -> Result<g_storage::Entry, g_storage::StoreError> {
        self.storage
            .get(&(namespace.clone(), key.clone()))
            .map(|(value, created, updated)| g_storage::Entry {
                id: format!("{namespace}/{key}"),
                namespace: namespace.clone(),
                key: key.clone(),
                value: value.clone(),
                created_at: *created,
                updated_at: *updated,
            })
            .ok_or(g_storage::StoreError::NotFound)
    }

    fn delete(&mut self, namespace: String, key: String) -> Result<(), g_storage::StoreError> {
        self.storage
            .remove(&(namespace, key))
            .map(|_| ())
            .ok_or(g_storage::StoreError::NotFound)
    }

    fn list_keys(
        &mut self,
        namespace: String,
    ) -> Result<Vec<g_storage::Entry>, g_storage::StoreError> {
        Ok(self.entries_in(&namespace))
    }

    fn recent(
        &mut self,
        namespace: String,
        limit: u32,
    ) -> Result<Vec<g_storage::Entry>, g_storage::StoreError> {
        let mut entries = self.entries_in(&namespace);
        entries.sort_by_key(|e| std::cmp::Reverse(e.updated_at));
        entries.truncate(limit as usize);
        Ok(entries)
    }
}

impl InterceptorHost {
    /// All stored entries in `namespace` as generated `entry` records.
    fn entries_in(&self, namespace: &str) -> Vec<g_storage::Entry> {
        self.storage
            .iter()
            .filter(|((ns, _), _)| ns == namespace)
            .map(|((ns, key), (value, created, updated))| g_storage::Entry {
                id: format!("{ns}/{key}"),
                namespace: ns.clone(),
                key: key.clone(),
                value: value.clone(),
                created_at: *created,
                updated_at: *updated,
            })
            .collect()
    }
}

impl g_llm::Host for InterceptorHost {
    fn complete(
        &mut self,
        request: g_llm::CompletionRequest,
    ) -> Result<u32, g_llm::ProviderError> {
        // The classifier's prompt is the last user message.
        let prompt = request
            .messages
            .iter()
            .rev()
            .find(|m| matches!(m.role, g_types::Role::User))
            .map_or_else(String::new, |m| m.content.clone());
        let text = (self.provider)(&prompt);
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
            storage: HashMap::new(),
            clock: 0,
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
        Phase::SessionStart => g_icept::Phase::SessionStart,
        Phase::BeforeLoop => g_icept::Phase::BeforeLoop,
        Phase::SelectModel => g_icept::Phase::SelectModel,
        Phase::SelectContext => g_icept::Phase::SelectContext,
        Phase::SelectTools => g_icept::Phase::SelectTools,
        Phase::OnError => g_icept::Phase::OnError,
        Phase::AfterResponse => g_icept::Phase::AfterResponse,
        Phase::ToolCall => g_icept::Phase::ToolCall,
        Phase::ToolResult => g_icept::Phase::ToolResult,
        Phase::Finalize => g_icept::Phase::Finalize,
        Phase::PrepareNextTurn => g_icept::Phase::PrepareNextTurn,
    }
}

const fn from_gen_phase(phase: g_icept::Phase) -> Phase {
    match phase {
        g_icept::Phase::SessionStart => Phase::SessionStart,
        g_icept::Phase::BeforeLoop => Phase::BeforeLoop,
        g_icept::Phase::SelectModel => Phase::SelectModel,
        g_icept::Phase::SelectContext => Phase::SelectContext,
        g_icept::Phase::SelectTools => Phase::SelectTools,
        g_icept::Phase::OnError => Phase::OnError,
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
        HookState::SessionStart(s) => g_icept::HookState::SessionStart(g_icept::SessionCtx {
            session: s.session.clone(),
        }),
        HookState::BeforeLoop(t) => g_icept::HookState::BeforeLoop(g_icept::UserTurn {
            session: t.session.clone(),
            user_message: t.user_message.clone(),
        }),
        HookState::SelectModel(r) => g_icept::HookState::SelectModel(to_gen_request(r)),
        HookState::SelectContext(r) => g_icept::HookState::SelectContext(to_gen_request(r)),
        HookState::SelectTools(r) => g_icept::HookState::SelectTools(to_gen_request(r)),
        HookState::OnError(e) => g_icept::HookState::OnError(g_icept::ErrorInfo {
            failed_phase: to_gen_phase(e.failed_phase),
            message: e.message.clone(),
        }),
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
        g_icept::HookState::SessionStart(s) => HookState::SessionStart(intercept::SessionCtx {
            session: s.session,
        }),
        g_icept::HookState::BeforeLoop(t) => HookState::BeforeLoop(intercept::UserTurn {
            session: t.session,
            user_message: t.user_message,
        }),
        g_icept::HookState::SelectModel(r) => HookState::SelectModel(from_gen_request(r)),
        g_icept::HookState::SelectContext(r) => HookState::SelectContext(from_gen_request(r)),
        g_icept::HookState::SelectTools(r) => HookState::SelectTools(from_gen_request(r)),
        g_icept::HookState::OnError(e) => HookState::OnError(intercept::ErrorInfo {
            failed_phase: from_gen_phase(e.failed_phase),
            message: e.message,
        }),
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
    use crate::intercept::{Dispatcher, Driver, HookState, Outcome, UserTurn};
    use serde_json::json;
    use std::path::PathBuf;

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

    #[test]
    fn permission_ignores_ordinary_tools() {
        let Some(p) = load_permission() else { return };
        let mut d = Dispatcher::new(vec![Box::new(p)]);
        let mut state = tool_call("web_search");
        // NoDriver panics if asked — an ordinary tool must never trigger an ask.
        assert!(matches!(
            d.dispatch(Phase::ToolCall, &mut state, &mut NoDriver),
            Outcome::Proceeded
        ));
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
}
