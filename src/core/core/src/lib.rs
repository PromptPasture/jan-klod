//! Jan-Klod core runtime: the minimal container that turns a `config.yaml`
//! into a set of sandboxed extension components.
//!
//! The flow is: load config ([`jan_klod_config`]) → build a host [`Linker`] that
//! grants the Component-Model capabilities (`host-log`, `host-config`,
//! `host-http`) → resolve each enabled instance's `ext/<component>.wasm` and
//! compile it → drive the universal `extension-lifecycle` (`init` → `start`).
//!
//! There is **zero agent behaviour here** — the core only loads, wires
//! capabilities, and runs lifecycle. Everything domain-specific lives in the
//! extensions it hosts.

// Generated Component-Model bindings; lint exemptions scoped to the macro output.
#[allow(missing_docs, clippy::all, clippy::pedantic, clippy::nursery)]
mod bindings;
pub mod conductor;
pub mod delegate;
mod host;
pub mod egress;
pub mod host_fs;
pub mod host_process;
pub mod http;
pub mod intercept;
pub mod interceptor_host;
pub mod route;
pub mod serve;
pub mod store;
pub mod telegram;
pub mod registry_host;
pub mod tool_host;

use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use wasmtime::component::{Component, HasSelf, Linker};
use wasmtime::{Engine, Store};

use bindings::jan_klod::interfaces::{host_config, host_http, host_log};
use bindings::ExtensionWorld;
use jan_klod_config::{Config, ExtensionInstance};

pub use host::{ConfigSection, HostState};

/// Deterministic boot tier for a category. Dependencies boot before dependents:
/// registries first, then providers, then the managers that consume them, then
/// leaf surfaces (tools, agents, api, chat).
fn boot_rank(category: &str) -> u8 {
    match category {
        "registry" => 1,
        "provider" => 2,
        "manager" => 3,
        "tool" => 4,
        "agent" => 5,
        "api" => 6,
        "chat" => 7,
        _ => 8,
    }
}

/// Outcome of resolving one enabled instance to a component on disk.
pub enum LoadState {
    /// `ext/<component>.wasm` was found and compiled.
    Compiled(Component),
    /// No component file present yet — recorded, not fatal.
    Missing(PathBuf),
}

/// One enabled instance after the boot resolution pass.
pub struct LoadedExtension {
    /// The resolved config instance (`provider.openai`, …).
    pub instance: ExtensionInstance,
    /// Whether its component was found and compiled.
    pub state: LoadState,
}

/// A booted core: the engine, the capability linker, and every enabled instance
/// resolved against `ext/`. Holds compiled components ready to instantiate.
pub struct Runtime {
    engine: Engine,
    linker: Linker<HostState>,
    extensions: Vec<LoadedExtension>,
    /// Top-level agent-behaviour config (`routing`, `providers`, …), preserved
    /// verbatim and served to interceptors that need it (e.g. task-router) via
    /// `host-config`. Always a JSON object.
    agent: serde_json::Value,
    /// Directory holding `config.yaml`, so a relative `storage.path` resolves
    /// against the deployment rather than the working directory.
    config_dir: PathBuf,
}

impl Runtime {
    /// Load `config.yaml`, wire host capabilities, and resolve every enabled
    /// instance against `ext_dir`. Compiles present components; missing ones are
    /// recorded so a partial deployment still boots.
    ///
    /// # Errors
    /// Returns [`CoreError::Config`] if the config fails to load, [`CoreError::Linker`]
    /// if a host capability cannot be wired, or [`CoreError::Load`] if a present
    /// component fails to compile.
    pub fn boot(config_path: impl AsRef<Path>, ext_dir: impl AsRef<Path>) -> Result<Self, CoreError> {
        let config_dir = config_path
            .as_ref()
            .parent()
            .map_or_else(|| PathBuf::from("."), Path::to_path_buf);
        let config = Config::from_path(config_path)?;
        let agent = config.agent.clone();
        let engine = Engine::default();
        let linker = build_linker(&engine)?;

        // Enabled instances in deterministic dependency order.
        let mut order: Vec<&ExtensionInstance> = config.enabled().collect();
        order.sort_by(|a, b| {
            boot_rank(&a.category)
                .cmp(&boot_rank(&b.category))
                .then_with(|| a.id.cmp(&b.id))
        });

        let ext_dir = ext_dir.as_ref();
        let mut extensions = Vec::with_capacity(order.len());
        for instance in order {
            let path = ext_dir.join(instance.component_file());
            let state = if path.exists() {
                let component = Component::from_file(&engine, &path).map_err(|source| {
                    CoreError::Load {
                        id: instance.id.clone(),
                        path: path.display().to_string(),
                        source: source.into(),
                    }
                })?;
                LoadState::Compiled(component)
            } else {
                LoadState::Missing(path)
            };
            extensions.push(LoadedExtension {
                instance: instance.clone(),
                state,
            });
        }

        Ok(Self {
            engine,
            linker,
            extensions,
            agent,
            config_dir,
        })
    }

    /// The resolved extension set, in boot order.
    #[must_use]
    pub fn extensions(&self) -> &[LoadedExtension] {
        &self.extensions
    }

    /// Instantiate every compiled component in its own store and run its
    /// lifecycle (`init` → `start`). Missing components are skipped. Returns the
    /// ids that were started.
    ///
    /// Categories whose world imports more than the neutral `extension-world`
    /// grants are instantiated through **their own** seam — the same one
    /// [`Self::build_agent`] uses — rather than the shared linker: a `tool-*`
    /// guest imports `host-fs`/`host-process`, and `registry-*` imports
    /// `host-fs`/`host-event`, none of which the neutral linker can satisfy. They
    /// get the same default-deny substrates here, so this boot-plan path proves
    /// exactly what the agent path will do.
    ///
    /// # Errors
    /// Returns [`CoreError::Instantiate`] if a component cannot be instantiated,
    /// [`CoreError::Lifecycle`] if a lifecycle call traps, or
    /// [`CoreError::LifecycleRejected`] if an extension refuses to start.
    pub fn start_all(&self) -> Result<Vec<String>, CoreError> {
        let mut started = Vec::new();
        let workspace = self.open_workspace();
        let process = self.open_process_runner(workspace.as_ref());
        for ext in &self.extensions {
            let LoadState::Compiled(component) = &ext.state else {
                continue;
            };
            let id = &ext.instance.id;

            // Own-seam categories: instantiate + lifecycle happen inside the seam.
            let config_json = ext.instance.config.to_string();
            match (ext.instance.category.as_str(), ext.instance.kind.as_str()) {
                ("tool", _) => {
                    tool_host::ToolExtension::instantiate(
                        &self.engine,
                        id,
                        component,
                        workspace.clone(),
                        process.clone(),
                    )?;
                    started.push(id.clone());
                    continue;
                }
                ("registry", "skills") => {
                    registry_host::SkillsExtension::instantiate(
                        &self.engine,
                        id,
                        component,
                        config_json,
                        workspace.clone(),
                    )?;
                    started.push(id.clone());
                    continue;
                }
                ("registry", "mcp") => {
                    registry_host::McpExtension::instantiate(
                        &self.engine,
                        id,
                        component,
                        config_json,
                        self.egress_policy(),
                    )?;
                    started.push(id.clone());
                    continue;
                }
                ("interceptor", _) => {
                    // A constant classifier: this path only proves the guest
                    // instantiates and starts, and must not make a model call to
                    // do it. `build_agent` supplies the real one.
                    let provider_fn: interceptor_host::ProviderFn =
                        Box::new(|_request| "agentic".to_string());
                    interceptor_host::WasmInterceptor::instantiate(
                        &self.engine,
                        id,
                        component,
                        ConfigSection::new(self.interceptor_config(&ext.instance)),
                        provider_fn,
                    )?;
                    started.push(id.clone());
                    continue;
                }
                _ => {}
            }

            let section = ConfigSection::new(ext.instance.config.clone());
            let mut store = Store::new(
                &self.engine,
                HostState::new(id.clone(), section).with_egress(self.egress_policy()),
            );

            let world = ExtensionWorld::instantiate(&mut store, component, &self.linker)
                .map_err(|source| CoreError::Instantiate {
                    id: id.clone(),
                    source: source.into(),
                })?;
            let lifecycle = world.jan_klod_interfaces_extension_lifecycle();

            let ctx = bindings::exports::jan_klod::interfaces::extension_lifecycle::ExtensionContext {
                id: id.clone(),
                version: "0.0.0".to_string(),
            };
            lifecycle
                .call_init(&mut store, &ctx)
                .map_err(|source| CoreError::Lifecycle {
                    id: id.clone(),
                    phase: "init",
                    source: source.into(),
                })?
                .map_err(|message| CoreError::LifecycleRejected {
                    id: id.clone(),
                    phase: "init",
                    message,
                })?;
            lifecycle
                .call_start(&mut store)
                .map_err(|source| CoreError::Lifecycle {
                    id: id.clone(),
                    phase: "start",
                    source: source.into(),
                })?
                .map_err(|message| CoreError::LifecycleRejected {
                    id: id.clone(),
                    phase: "start",
                    message,
                })?;
            started.push(id.clone());
        }
        Ok(started)
    }

    /// A human-readable boot plan (each instance → its component, loaded/missing).
    #[must_use]
    pub const fn report(&self) -> BootReport<'_> {
        BootReport(self)
    }

    /// Boot the thin-loop agent from config: instantiate every enabled+compiled
    /// `interceptor.*` as a dispatcher (in boot/load order) and every
    /// `provider.*` as a completer fallback chain, ready to run turns through the
    /// [`conductor`]. This supersedes [`Self::route_agent_loop`] — the loop is now
    /// core mechanism, not the `manager-agent-loop` guest.
    ///
    /// `http_factory` mints a fresh `host-http` backend per provider (each provider
    /// instance owns its own store). In v1 an interceptor's `llm-provider` import is
    /// backed by a dedicated provider instance (see [`Self::open_classifier`]).
    ///
    /// # Errors
    /// Returns a [`CoreError`] if any interceptor or provider fails to instantiate
    /// or start.
    pub fn build_agent(
        &self,
        http_factory: &dyn Fn() -> route::HttpFn,
    ) -> Result<AgentSession, CoreError> {
        let mut providers: Vec<Box<dyn conductor::Completer>> = Vec::new();
        // Instance ids, parallel to `providers`, so the configured chain can be
        // matched by name without downcasting a `dyn Completer`.
        let mut provider_ids: Vec<String> = Vec::new();
        let mut tool_extensions: Vec<tool_host::ToolExtension> = Vec::new();
        let mut skills_extensions: Vec<registry_host::SkillsExtension> = Vec::new();
        let mut mcp_extensions: Vec<registry_host::McpExtension> = Vec::new();

        // Shared, default-deny substrates for tools (opt-in via config).
        let workspace = self.open_workspace();
        let process = self.open_process_runner(workspace.as_ref());

        // Pass 1: providers + tools + registries. (Before interceptors so tool-selector
        // can be handed the combined advertised metadata.)
        for ext in &self.extensions {
            let LoadState::Compiled(component) = &ext.state else {
                continue;
            };
            let config_json = ext.instance.config.to_string();
            match ext.instance.category.as_str() {
                "provider" => {
                    providers.push(Box::new(route::ProviderCompleter::instantiate(
                        &self.engine,
                        &ext.instance,
                        component,
                        http_factory(),
                    )?));
                    provider_ids.push(ext.instance.id.clone());
                }
                "tool" => {
                    // Egress is granted per instance, never by default: a tool
                    // that never asked for the network must not have it, the same
                    // way `host-fs` needs a workspace and `host-process` needs
                    // `execution:`.
                    let network = ext
                        .instance
                        .config
                        .get("network")
                        .and_then(serde_json::Value::as_bool)
                        .unwrap_or(false);
                    tool_extensions.push(tool_host::ToolExtension::instantiate_with_http(
                        &self.engine,
                        &ext.instance.id,
                        component,
                        workspace.clone(),
                        process.clone(),
                        network.then(|| http_factory()),
                    )?);
                }
                "registry" if ext.instance.kind == "skills" => {
                    skills_extensions.push(registry_host::SkillsExtension::instantiate(
                        &self.engine,
                        &ext.instance.id,
                        component,
                        config_json,
                        workspace.clone(),
                    )?);
                }
                "registry" if ext.instance.kind == "mcp" => {
                    mcp_extensions.push(registry_host::McpExtension::instantiate(
                        &self.engine,
                        &ext.instance.id,
                        component,
                        config_json,
                        self.egress_policy(),
                    )?);
                }
                _ => {}
            }
        }
        let tool_fleet = tool_host::ToolFleet::new(tool_extensions);
        let registry_fleet = registry_host::RegistryFleet::new(skills_extensions, mcp_extensions);
        let mut tools = CombinedFleet { tools: tool_fleet, registry: registry_fleet };
        let tools_advert = tools.all_metas_json();

        // An interceptor that consults a model (the intent router classifies simple
        // vs agentic) gets its **own** provider instance rather than a handle into
        // the chain above: the conductor holds the chain mutably for the whole
        // turn, so an interceptor reaching into it mid-dispatch would alias it. A
        // second instance costs one more component + client and keeps the seam
        // straightforward.
        let classifier = self.open_classifier(http_factory)?;

        // Opened before the interceptors, because they share it: an interceptor's
        // `host-storage` writes land here, namespaced to the component.
        let store = self.open_store()?;

        // Pass 2: interceptors, each served the tool set at `select-tools`.
        let mut interceptors: Vec<Box<dyn intercept::Interceptor>> = Vec::new();
        for ext in &self.extensions {
            let LoadState::Compiled(component) = &ext.state else {
                continue;
            };
            if ext.instance.category != "interceptor" {
                continue;
            }
            let mut config = self.interceptor_config(&ext.instance);
            if let serde_json::Value::Object(map) = &mut config {
                map.entry("tools").or_insert_with(|| tools_advert.clone());
            }
            // Durability is opt-in per instance, and off by default.
            //
            // `interceptor-permission` records standing grants ("always allow
            // `fs:write`") through `host-storage`, and documents them as
            // run-scoped — "a permission boundary should not quietly become
            // permanently open because of a click last week". That property was
            // enforced by nothing: it held because this host happened to back
            // `host-storage` with a private map. Handing every interceptor the
            // session store would have repealed it silently, which is how a
            // security property dies. So the store is granted only where the
            // config asks for it, the same default-deny shape as `host-fs` and
            // `host-process`, and `permission_grants_do_not_survive_a_restart`
            // fails if that default ever flips.
            let persist = ext
                .instance
                .config
                .get("persist")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false);
            let provider_fn = classifier_fn(classifier.clone());
            interceptors.push(Box::new(interceptor_host::WasmInterceptor::instantiate_with_storage(
                &self.engine,
                &ext.instance.id,
                component,
                ConfigSection::new(config),
                provider_fn,
                persist.then(|| Arc::clone(&store)),
            )?));
        }

        // The fallback chain's *order* is what `providers:` configures; without
        // this the chain was whatever boot order produced (alphabetical), so the
        // documented "tried top-to-bottom" list had no effect at all.
        let ordered = order_chain(self.agent.get("providers"), &provider_ids);
        let providers = reorder(providers, &ordered);

        Ok(AgentSession {
            dispatcher: intercept::Dispatcher::new(interceptors),
            providers,
            store,
            tools,
        })

    }

    /// The destinations guests may reach, derived from what the operator already
    /// wrote down.
    ///
    /// Every enabled instance's `base-url` (and an MCP server's `endpoint`) is an
    /// endpoint the operator chose, so it is allowed even when it is local — which
    /// is the whole point, because a self-hosted model lives on `127.0.0.1`.
    /// Everything else is public-only. A guest cannot widen this: the policy is
    /// built here and closed over by the host's HTTP backend.
    #[must_use]
    pub fn egress_policy(&self) -> egress::EgressPolicy {
        let mut policy = egress::EgressPolicy::public_only();
        for ext in &self.extensions {
            for key in ["base-url", "endpoint", "url"] {
                if let Some(url) = ext.instance.config.get(key).and_then(serde_json::Value::as_str)
                {
                    policy = policy.allowing(url);
                }
            }
        }
        // An explicit escape hatch for anything config does not already name — a
        // sidecar, a proxy — spelled out one origin at a time rather than as a
        // switch that opens the machine.
        if let Some(list) = self.agent.get("network").and_then(|n| n.get("allow")) {
            for url in list.as_array().into_iter().flatten() {
                if let Some(url) = url.as_str() {
                    policy = policy.allowing(url);
                }
            }
        }
        policy
    }

    /// Open the host-side workspace for `host-fs`. Uses the top-level `workspace:`
    /// config key, falling back to `$PWD` when the key is absent. Un-openable →
    /// `None` (default-deny).
    fn open_workspace(&self) -> Option<host_fs::Workspace> {
        let root_owned;
        let root: &str = if let Some(r) = self.agent.get("workspace").and_then(serde_json::Value::as_str) {
            r
        } else {
            root_owned = std::env::current_dir()
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_default();
            if root_owned.is_empty() {
                return None;
            }
            &root_owned
        };
        host_fs::Workspace::open(root).map_or_else(
            |_| {
                eprintln!("WARN [core] workspace `{root}` could not be opened; host-fs is default-deny");
                None
            },
            Some,
        )
    }

    /// Build the `host-process` runner from the top-level `execution:` config
    /// (`{ enabled, timeout-secs?, output-cap? }`). Disabled unless enabled *and* a
    /// workspace is configured (the exec cwd is jailed to it).
    fn open_process_runner(&self, workspace: Option<&host_fs::Workspace>) -> host_process::ProcessRunner {
        let exec = self.agent.get("execution");
        let enabled = exec
            .and_then(|e| e.get("enabled"))
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false);
        match (enabled, workspace) {
            (true, Some(ws)) => {
                let timeout = exec
                    .and_then(|e| e.get("timeout-secs"))
                    .and_then(serde_json::Value::as_u64)
                    .unwrap_or(30);
                let cap = exec
                    .and_then(|e| e.get("output-cap"))
                    .and_then(serde_json::Value::as_u64)
                    .unwrap_or(64 * 1024);
                host_process::ProcessRunner::new(
                    ws.clone(),
                    std::time::Duration::from_secs(timeout),
                    usize::try_from(cap).unwrap_or(64 * 1024),
                )
            }
            _ => host_process::ProcessRunner::disabled(),
        }
    }

    /// Instantiate the provider that answers interceptors' `llm-provider` calls.
    ///
    /// Which instance: the top-level `classifier:` key names one, otherwise the
    /// head of the fallback chain. Pointing it at a small local model is the
    /// reason the key exists — classification is a two-token question and does
    /// not want the expensive model the turn itself uses.
    ///
    /// Returns `None` when no provider is enabled; callers then fall back to the
    /// conservative default rather than failing the boot.
    ///
    /// # Errors
    /// Returns a [`CoreError`] if the chosen provider cannot be instantiated.
    fn open_classifier(
        &self,
        http_factory: &dyn Fn() -> route::HttpFn,
    ) -> Result<Option<std::sync::Arc<std::sync::Mutex<route::ProviderCompleter>>>, CoreError> {
        let named = self.agent.get("classifier").and_then(serde_json::Value::as_str);
        let chosen = self.extensions.iter().find(|ext| {
            ext.instance.category == "provider"
                && matches!(ext.state, LoadState::Compiled(_))
                && named.is_none_or(|name| ext.instance.id == format!("provider.{name}"))
        });
        let Some(ext) = chosen else {
            if let Some(name) = named {
                eprintln!(
                    "jan-klod: `classifier:` names `{name}`, which is not an enabled \
                     provider — interceptors will use the conservative default"
                );
            }
            return Ok(None);
        };
        let LoadState::Compiled(component) = &ext.state else { return Ok(None) };
        Ok(Some(std::sync::Arc::new(std::sync::Mutex::new(
            route::ProviderCompleter::instantiate(
                &self.engine,
                &ext.instance,
                component,
                http_factory(),
            )?,
        ))))
    }

    /// Open the host-side persistent store: top-level `storage.path` gives a
    /// durable `SQLite` file, and its absence an ephemeral in-memory one.
    ///
    /// Storage is **not** an extension. It was configured as one — an
    /// `extensions.store.sqlite` instance whose `path` this read — which
    /// advertised a swappable component family (`store-sqlite`,
    /// `store-postgres`, `store-supabase`) that never existed: the one component
    /// that did, `store-memory`, exported a `memory-store` interface the core
    /// never called once. The design was always host-side, for a reason the
    /// architecture notes record: the sandbox has no filesystem, so a store guest
    /// would need one granted back, and the transcript is the most sensitive
    /// thing the runtime holds. Guests reach it through `host-storage` only,
    /// namespaced to themselves.
    ///
    /// A relative `path` resolves against the directory holding `config.yaml`,
    /// **not** the working directory. An installed jan-klod is launched from
    /// whatever repository the user is in; resolving against the cwd would drop a
    /// `jan-klod.db` into each one and give a different conversation history per
    /// directory the agent happened to be started from.
    fn open_store(&self) -> Result<Arc<Mutex<store::Store>>, CoreError> {
        let sqlite_path = self
            .agent
            .get("storage")
            .and_then(|storage| storage.get("path"))
            .and_then(serde_json::Value::as_str)
            .map(|path| self.config_dir.join(path));
        sqlite_path
            .map_or_else(store::Store::open_in_memory, store::Store::open)
            .map(|store| Arc::new(Mutex::new(store)))
            .map_err(|source| CoreError::Store {
                message: source.to_string(),
            })
    }

    /// An interceptor's `host-config` section: its own config plus the top-level
    /// agent keys (`routing`, `providers`) served verbatim, so e.g. task-router can
    /// resolve `routing.<task>`. An instance's own key wins if it defines one.
    fn interceptor_config(&self, instance: &ExtensionInstance) -> serde_json::Value {
        let mut section = instance.config.clone();
        if let (serde_json::Value::Object(map), serde_json::Value::Object(agent)) =
            (&mut section, &self.agent)
        {
            for key in ["routing", "providers"] {
                if let Some(value) = agent.get(key) {
                    map.entry(key).or_insert_with(|| value.clone());
                }
            }
        }
        section
    }
}

/// Combined tool + registry fleet implementing [`conductor::ToolInvoker`].
///
/// Dispatches first to the `tool-*` fleet, then to the registry fleet (skills + MCP).
struct CombinedFleet {
    tools: tool_host::ToolFleet,
    registry: registry_host::RegistryFleet,
}

impl conductor::ToolInvoker for CombinedFleet {
    fn invoke(&mut self, call: &intercept::ToolCall) -> Option<String> {
        self.tools.invoke(call).or_else(|| self.registry.invoke(call))
    }
}

impl CombinedFleet {
    fn tool_names(&self) -> Vec<String> {
        self.tools.tool_names()
    }

    fn all_metas_json(&mut self) -> serde_json::Value {
        let mut metas: Vec<serde_json::Value> = self
            .tools
            .metas()
            .into_iter()
            .map(|m| {
                serde_json::json!({
                    "name": m.name,
                    "description": m.description,
                    "parameters-schema": m.arguments_schema,
                })
            })
            .collect();
        for (name, description, schema) in self.registry.all_metas() {
            metas.push(serde_json::json!({
                "name": name,
                "description": description,
                "parameters-schema": schema,
            }));
        }
        serde_json::Value::Array(metas)
    }
}

/// A booted thin-loop agent: the interceptor dispatcher and the provider fallback
/// chain, ready to run turns through the [`conductor`].
pub struct AgentSession {
    dispatcher: intercept::Dispatcher,
    providers: Vec<Box<dyn conductor::Completer>>,
    store: Arc<Mutex<store::Store>>,
    /// The enabled tool + registry extensions, dispatched by the loop as a `ToolInvoker`.
    tools: CombinedFleet,
}

impl AgentSession {
    /// Run one turn headless, using the session's tool fleet. An interceptor `ask`
    /// resolves to its `default-answer`.
    pub fn run(&mut self, session: &str, message: &str) -> conductor::RunResult {
        run_and_persist(
            &mut self.dispatcher,
            &mut self.providers,
            &self.store,
            &mut self.tools,
            &mut HeadlessDriver,
            &mut conductor::NoSink,
            session,
            message,
        )
    }

    /// Run one turn with an explicit `driver` and `tools` (overriding the fleet).
    /// On a completed turn the user message + answer are appended to the session's
    /// durable transcript. The seam a test injects stub tools through.
    pub fn run_with(
        &mut self,
        driver: &mut dyn intercept::Driver,
        tools: &mut dyn conductor::ToolInvoker,
        session: &str,
        message: &str,
    ) -> conductor::RunResult {
        run_and_persist(
            &mut self.dispatcher,
            &mut self.providers,
            &self.store,
            tools,
            driver,
            &mut conductor::NoSink,
            session,
            message,
        )
    }

    /// Run one turn with the session's tool fleet but an explicit `driver` (so a
    /// client can answer an interceptor `ask` — e.g. a permission confirmation).
    pub fn run_driven(
        &mut self,
        driver: &mut dyn intercept::Driver,
        session: &str,
        message: &str,
    ) -> conductor::RunResult {
        run_and_persist(
            &mut self.dispatcher,
            &mut self.providers,
            &self.store,
            &mut self.tools,
            driver,
            &mut conductor::NoSink,
            session,
            message,
        )
    }

    /// Like [`Self::run_with`], but streams incremental [`conductor::Event`]s to
    /// `sink` as the turn runs (for a live TUI transcript or SSE).
    pub fn run_streaming(
        &mut self,
        driver: &mut dyn intercept::Driver,
        tools: &mut dyn conductor::ToolInvoker,
        sink: &mut dyn conductor::EventSink,
        session: &str,
        message: &str,
    ) -> conductor::RunResult {
        run_and_persist(
            &mut self.dispatcher,
            &mut self.providers,
            &self.store,
            tools,
            driver,
            sink,
            session,
            message,
        )
    }

    /// A non-streaming turn using the session's own tool fleet, with `driver`
    /// answering any interceptor `ask` — the entry a chat surface uses, where
    /// there is no event stream but there *is* someone to ask.
    pub fn run_with_driver(
        &mut self,
        driver: &mut dyn intercept::Driver,
        session: &str,
        message: &str,
    ) -> conductor::RunResult {
        run_and_persist(
            &mut self.dispatcher,
            &mut self.providers,
            &self.store,
            &mut self.tools,
            driver,
            &mut conductor::NoSink,
            session,
            message,
        )
    }

    /// Streaming turn using the session's own tool fleet, with `driver` answering
    /// any interceptor `ask` — the entry the REST surface's SSE handler uses.
    ///
    /// [`Self::run_streaming`] exists for callers that bring their own fleet;
    /// this one borrows `self.tools`, which a caller cannot do while also holding
    /// `&mut self`.
    pub fn run_streaming_with_driver(
        &mut self,
        driver: &mut dyn intercept::Driver,
        sink: &mut dyn conductor::EventSink,
        session: &str,
        message: &str,
    ) -> conductor::RunResult {
        run_and_persist(
            &mut self.dispatcher,
            &mut self.providers,
            &self.store,
            &mut self.tools,
            driver,
            sink,
            session,
            message,
        )
    }

    /// Headless streaming turn using the session's tool fleet: an `ask` takes the
    /// prompt's default answer, which for the permission gate is a denial. Used by
    /// the non-interactive surfaces (Telegram, the blocking JSON turn).
    pub fn run_streaming_headless(
        &mut self,
        sink: &mut dyn conductor::EventSink,
        session: &str,
        message: &str,
    ) -> conductor::RunResult {
        run_and_persist(
            &mut self.dispatcher,
            &mut self.providers,
            &self.store,
            &mut self.tools,
            &mut HeadlessDriver,
            sink,
            session,
            message,
        )
    }

    /// The names of the tools the loop can call (advertised names from the fleet).
    #[must_use]
    pub fn tool_names(&self) -> Vec<String> {
        self.tools.tool_names()
    }

    /// All tool + registry metadata as JSON (used in tests).
    #[must_use]
    pub fn all_metas_json(&mut self) -> serde_json::Value {
        self.tools.all_metas_json()
    }

    /// The durable transcript for `session`, oldest turn first. Reads from the
    /// host-side store, so it survives a `Runtime` restart against the same DB.
    #[must_use]
    pub fn transcript(&self, session: &str) -> Vec<store::Entry> {
        let mut entries = self
            .store
            .lock()
            .map(|store| store.recent(session, u32::MAX).unwrap_or_default())
            .unwrap_or_default();
        entries.reverse(); // `recent` is newest-first; a transcript reads oldest-first
        entries
    }

    /// All known session ids, newest first.
    #[must_use]
    pub fn list_sessions(&self) -> Vec<String> {
        self.store
            .lock()
            .map(|store| {
                // Not `unwrap_or_default()`: swallowing this is what let a broken
                // query report "no sessions" for as long as nobody looked.
                store.list_namespaces().unwrap_or_else(|err| {
                    eprintln!("WARN [core] listing sessions failed: {err}");
                    Vec::new()
                })
            })
            .unwrap_or_default()
            .into_iter()
            // Interceptors share this database, under `ext/<component>/…`. Their
            // namespaces are not conversations, and listing them here would put
            // `ext/interceptor.permission/grants` in a session picker.
            .filter(|namespace| !namespace.contains('/'))
            .collect()
    }
}

/// Drive one turn through the conductor and persist a completed turn's transcript.
/// A free function (not a method) so `run`/streaming can pass disjoint `&mut` borrows
/// of the session's fields (dispatcher, providers, tools) in one call.
#[allow(clippy::too_many_arguments)]
fn run_and_persist(
    dispatcher: &mut intercept::Dispatcher,
    providers: &mut [Box<dyn conductor::Completer>],
    store: &Mutex<store::Store>,
    tools: &mut dyn conductor::ToolInvoker,
    driver: &mut dyn intercept::Driver,
    sink: &mut dyn conductor::EventSink,
    session: &str,
    message: &str,
) -> conductor::RunResult {
    // Locked around each use, never across the turn: an interceptor writing its
    // own `host-storage` mid-dispatch takes the same lock, and holding it here
    // would deadlock the first guest that remembered anything.
    let history = store.lock().map(|store| replay(&store, session)).unwrap_or_default();
    let result =
        conductor::run_turn(dispatcher, providers, tools, driver, sink, session, message, history);
    if let conductor::RunResult::Answered { text, .. } = &result {
        // Best-effort transcript append: a store failure never fails the answered turn.
        let Ok(store) = store.lock() else { return result };
        let turn = store.list_keys(session).map_or(0, |keys| keys.len()) + 1;
        let value = serde_json::json!({ "user": message, "answer": text }).to_string();
        if let Err(err) = store.set(session, &format!("turn-{turn}"), &value) {
            eprintln!("WARN [core] persisting turn for session {session} failed: {err}");
        }
    }
    result
}


/// How many past turns are replayed into a new one.
///
/// A bound belongs here, before the store read, as well as in `select-context`:
/// loading a thousand turns to then drop most of them costs a query and the
/// memory either way. The token-aware trimming on top is the context
/// interceptor's job — this is only "do not read the whole history of the world".
const REPLAYED_TURNS: u32 = 20;

/// The conversation so far, oldest-first, as loop messages.
///
/// Each stored turn is `{"user":…,"answer":…}`; a malformed or unreadable entry
/// is skipped rather than failing the turn — a corrupt transcript row should cost
/// context, not the ability to talk.
fn replay(store: &store::Store, session: &str) -> Vec<intercept::Message> {
    use intercept::{Message, Role};
    let mut entries = store.recent(session, REPLAYED_TURNS).unwrap_or_default();
    entries.reverse(); // `recent` is newest-first; a conversation reads oldest-first
    let mut messages = Vec::with_capacity(entries.len() * 2);
    for entry in entries {
        let Ok(turn) = serde_json::from_str::<serde_json::Value>(&entry.value) else { continue };
        if let Some(user) = turn.get("user").and_then(serde_json::Value::as_str) {
            messages.push(Message {
                role: Role::User,
                content: user.to_string(),
                tool_call_id: None,
            });
        }
        if let Some(answer) = turn.get("answer").and_then(serde_json::Value::as_str) {
            messages.push(Message {
                role: Role::Assistant,
                content: answer.to_string(),
                tool_call_id: None,
            });
        }
    }
    messages
}

/// The closure interceptors' `llm-provider` resolves to.
///
/// **A classification failure is not a turn failure.** With no provider
/// available, a poisoned lock, or a call that errors, the answer is `"agentic"`
/// — the conservative label, which routes the prompt through the full loop
/// rather than short-circuiting it. Defaulting the other way would silently
/// downgrade real work on any provider hiccup.
///
/// The classifier is shared by `Arc` rather than borrowed: the closure outlives
/// the call that builds it, and a lifetime cast to pretend otherwise is exactly
/// what this workspace's `unsafe_code = "deny"` exists to prevent.
fn classifier_fn(
    classifier: Option<std::sync::Arc<std::sync::Mutex<route::ProviderCompleter>>>,
) -> interceptor_host::ProviderFn {
    use crate::conductor::Completer;
    let Some(classifier) = classifier else {
        return Box::new(|_request| "agentic".to_string());
    };
    Box::new(move |request| {
        classifier.lock().map_or_else(
            |_| "agentic".to_string(),
            |mut provider| {
                provider
                    .complete(request)
                    .map_or_else(|_| "agentic".to_string(), |completion| completion.text)
            },
        )
    })
}

/// Resolve the configured fallback chain into an ordering of `ids`.
///
/// `chain` is the top-level `providers:` list — `[{provider: openai, …}, …]`.
/// Returns indices into `ids`, in the order the conductor should try them.
///
/// Two rules make this safe to apply to a config that has drifted from the
/// enabled instance set, which is the normal state of an example config someone
/// edited:
///
/// - **A named provider that is not enabled is skipped, with a warning.** The
///   shipped config lists `ollama` as a "last resort" nobody has enabled; that
///   should not be a boot failure, but silence would hide a typo.
/// - **An enabled provider the list does not mention is kept, at the end.** It is
///   enabled, so dropping it would be a worse surprise than ordering it last.
fn order_chain(chain: Option<&serde_json::Value>, ids: &[String]) -> Vec<usize> {
    let Some(entries) = chain.and_then(serde_json::Value::as_array) else {
        return (0..ids.len()).collect();
    };
    let mut order = Vec::with_capacity(ids.len());
    for entry in entries {
        let Some(name) = entry.get("provider").and_then(serde_json::Value::as_str) else {
            continue;
        };
        let wanted = format!("provider.{name}");
        match ids.iter().position(|id| *id == wanted) {
            Some(index) if !order.contains(&index) => order.push(index),
            Some(_) => {}
            None => eprintln!(
                "jan-klod: `providers:` names `{name}`, which is not an enabled \
                 provider — skipping it in the fallback chain"
            ),
        }
    }
    // Anything enabled but unlisted still runs, after the configured chain.
    let unlisted: Vec<usize> = (0..ids.len()).filter(|index| !order.contains(index)).collect();
    order.extend(unlisted);
    order
}

/// Reorder `items` by `order` (a permutation of its indices).
fn reorder<T>(items: Vec<T>, order: &[usize]) -> Vec<T> {
    let mut slots: Vec<Option<T>> = items.into_iter().map(Some).collect();
    order.iter().filter_map(|index| slots.get_mut(*index).and_then(Option::take)).collect()
}

/// Headless driver: no interactive surface, so an `ask` takes the prompt's
/// `default-answer`.
struct HeadlessDriver;
impl intercept::Driver for HeadlessDriver {
    fn ask(&mut self, prompt: &intercept::UserPrompt) -> String {
        prompt.default_answer.clone()
    }
}

/// Build the capability linker every extension store shares: WASI for the guest
/// runtime, plus the host-granted `host-log` / `host-config` / `host-http`.
fn build_linker(engine: &Engine) -> Result<Linker<HostState>, CoreError> {
    let mut linker: Linker<HostState> = Linker::new(engine);
    wasmtime_wasi::p2::add_to_linker_sync(&mut linker).map_err(CoreError::linker)?;
    host_log::add_to_linker::<_, HasSelf<_>>(&mut linker, |s| s).map_err(CoreError::linker)?;
    host_config::add_to_linker::<_, HasSelf<_>>(&mut linker, |s| s).map_err(CoreError::linker)?;
    host_http::add_to_linker::<_, HasSelf<_>>(&mut linker, |s| s).map_err(CoreError::linker)?;
    Ok(linker)
}

/// Renders the boot plan: one line per enabled instance, then a summary.
pub struct BootReport<'a>(&'a Runtime);

impl fmt::Display for BootReport<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let exts = &self.0.extensions;
        writeln!(f, "jan-klod core — {} enabled extension(s)", exts.len())?;
        let mut compiled = 0;
        for ext in exts {
            let file = ext.instance.component_file();
            match &ext.state {
                LoadState::Compiled(_) => {
                    compiled += 1;
                    writeln!(f, "  loaded   {:<22} -> {file}", ext.instance.id)?;
                }
                LoadState::Missing(path) => {
                    writeln!(
                        f,
                        "  missing  {:<22} -> {file} ({} not found)",
                        ext.instance.id,
                        path.display()
                    )?;
                }
            }
        }
        write!(
            f,
            "{compiled} loaded, {} missing",
            exts.len() - compiled
        )
    }
}

/// Errors surfaced while booting the core.
#[derive(Debug, thiserror::Error)]
pub enum CoreError {
    /// Loading or parsing `config.yaml` failed.
    #[error(transparent)]
    Config(#[from] jan_klod_config::ConfigError),
    /// Wiring a host capability into the linker failed.
    #[error("wiring host capabilities into the linker")]
    Linker {
        /// The underlying Wasmtime linker error.
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },
    /// Compiling a component from disk failed.
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
    /// Instantiating a compiled component failed.
    #[error("instantiating {id}")]
    Instantiate {
        /// Instance id that failed to instantiate.
        id: String,
        /// The underlying instantiation error.
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },
    /// A lifecycle call trapped (guest crash, host-cap error).
    #[error("{id}: lifecycle `{phase}` trapped")]
    Lifecycle {
        /// Instance id whose lifecycle call trapped.
        id: String,
        /// Lifecycle phase that trapped (`init` / `start`).
        phase: &'static str,
        /// The underlying trap.
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },
    /// The host-side persistent store could not be opened.
    #[error("opening the persistent store: {message}")]
    Store {
        /// The underlying store error, stringified.
        message: String,
    },
    /// A lifecycle call returned an error result (the extension refused to load).
    #[error("{id}: lifecycle `{phase}` failed: {message}")]
    LifecycleRejected {
        /// Instance id that refused to load.
        id: String,
        /// Lifecycle phase that was rejected (`init` / `start`).
        phase: &'static str,
        /// The message the extension returned.
        message: String,
    },
}

impl CoreError {
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn config_section_resolves_dot_paths() {
        let section = ConfigSection::new(json!({
            "base-url": "http://x/v1",
            "limits": { "max-tokens": 1024 }
        }));
        assert_eq!(section.get("base-url").unwrap(), "\"http://x/v1\"");
        assert_eq!(section.get("limits.max-tokens").unwrap(), "1024");
        assert!(section.has("limits.max-tokens"));
        assert!(!section.has("limits.missing"));
        assert!(section.get("nope").is_none());
    }

    fn ids(names: &[&str]) -> Vec<String> {
        names.iter().map(|n| format!("provider.{n}")).collect()
    }

    #[test]
    fn the_configured_chain_sets_the_fallback_order() {
        // Boot order is alphabetical; the config asks for the reverse.
        let ids = ids(&["anthropic", "openai"]);
        let chain = serde_json::json!([{ "provider": "openai" }, { "provider": "anthropic" }]);
        assert_eq!(order_chain(Some(&chain), &ids), vec![1, 0]);
    }

    #[test]
    fn no_chain_keeps_boot_order() {
        let ids = ids(&["anthropic", "openai"]);
        assert_eq!(order_chain(None, &ids), vec![0, 1]);
        // A malformed/empty list is the same as none, not "no providers".
        assert_eq!(order_chain(Some(&serde_json::json!("nonsense")), &ids), vec![0, 1]);
    }

    #[test]
    fn an_enabled_provider_the_chain_omits_still_runs_last() {
        // Dropping something the user enabled would be a worse surprise than
        // ordering it after the configured chain.
        let ids = ids(&["anthropic", "openai"]);
        let chain = serde_json::json!([{ "provider": "openai" }]);
        assert_eq!(order_chain(Some(&chain), &ids), vec![1, 0]);
    }

    #[test]
    fn a_named_provider_that_is_not_enabled_is_skipped() {
        // The shipped config lists `ollama` as a last resort nobody enabled.
        let ids = ids(&["openai"]);
        let chain = serde_json::json!([{ "provider": "openai" }, { "provider": "ollama" }]);
        assert_eq!(order_chain(Some(&chain), &ids), vec![0]);
    }

    #[test]
    fn a_provider_listed_twice_is_tried_once() {
        let ids = ids(&["anthropic", "openai"]);
        let chain =
            serde_json::json!([{ "provider": "openai" }, { "provider": "openai" }]);
        assert_eq!(order_chain(Some(&chain), &ids), vec![1, 0]);
    }

    #[test]
    fn reorder_applies_the_permutation() {
        assert_eq!(reorder(vec!["a", "b", "c"], &[2, 0, 1]), vec!["c", "a", "b"]);
        // Out-of-range indices cannot panic or duplicate an item.
        assert_eq!(reorder(vec!["a", "b"], &[1, 9, 0]), vec!["b", "a"]);
    }

    #[test]
    fn boot_rank_orders_dependencies_first() {
        assert!(boot_rank("registry") < boot_rank("provider"));
        assert!(boot_rank("provider") < boot_rank("manager"));
        assert!(boot_rank("manager") < boot_rank("chat"));
        assert_eq!(boot_rank("unknown"), 8);
    }

    #[test]
    fn boot_resolves_enabled_instances_and_marks_missing() {
        let dir = std::env::temp_dir().join(format!("jk-boot-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let cfg = dir.join("config.yaml");
        std::fs::write(
            &cfg,
            "
extensions:
  tool:
    fs:
      enabled: true
  provider:
    openai:
      enabled: false
",
        )
        .unwrap();

        // ext/ dir is empty, so the enabled tool resolves as missing.
        let runtime = Runtime::boot(&cfg, dir.join("ext")).unwrap();
        let exts = runtime.extensions();
        assert_eq!(exts.len(), 1, "only the enabled instance is resolved");
        assert_eq!(exts[0].instance.id, "tool.fs");
        assert!(matches!(exts[0].state, LoadState::Missing(_)));

        // No components compiled, so starting is a clean no-op.
        assert!(runtime.start_all().unwrap().is_empty());

        std::fs::remove_dir_all(&dir).ok();
    }
}
