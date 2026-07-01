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
mod host;
pub mod http;
pub mod intercept;
pub mod interceptor_host;
pub mod route;
pub mod serve;
pub mod store;

use std::fmt;
use std::path::{Path, PathBuf};

use wasmtime::component::{Component, HasSelf, Linker};
use wasmtime::{Engine, Store};

use bindings::jan_klod::interfaces::{host_config, host_http, host_log};
use bindings::ExtensionWorld;
use jan_klod_config::{Config, ExtensionInstance};

pub use host::{ConfigSection, HostState};

/// Deterministic boot tier for a category. Dependencies boot before dependents:
/// stores and registries first, then providers, then the managers that consume
/// them, then leaf surfaces (tools, agents, api, chat).
fn boot_rank(category: &str) -> u8 {
    match category {
        "store" => 0,
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
    /// # Errors
    /// Returns [`CoreError::Instantiate`] if a component cannot be instantiated,
    /// [`CoreError::Lifecycle`] if a lifecycle call traps, or
    /// [`CoreError::LifecycleRejected`] if an extension refuses to start.
    pub fn start_all(&self) -> Result<Vec<String>, CoreError> {
        let mut started = Vec::new();
        for ext in &self.extensions {
            let LoadState::Compiled(component) = &ext.state else {
                continue;
            };
            let id = &ext.instance.id;
            let section = ConfigSection::new(ext.instance.config.clone());
            let mut store = Store::new(&self.engine, HostState::new(id.clone(), section));

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
    /// backed by a safe-default classifier (`"agentic"`); routing it to the real
    /// providers is a follow-up.
    ///
    /// # Errors
    /// Returns a [`CoreError`] if any interceptor or provider fails to instantiate
    /// or start.
    pub fn build_agent(
        &self,
        http_factory: &dyn Fn() -> route::HttpFn,
    ) -> Result<AgentSession, CoreError> {
        let mut providers: Vec<Box<dyn conductor::Completer>> = Vec::new();
        let mut interceptors: Vec<Box<dyn intercept::Interceptor>> = Vec::new();
        for ext in &self.extensions {
            let LoadState::Compiled(component) = &ext.state else {
                continue;
            };
            match ext.instance.category.as_str() {
                "provider" => providers.push(Box::new(route::ProviderCompleter::instantiate(
                    &self.engine,
                    &ext.instance,
                    component,
                    http_factory(),
                )?)),
                "interceptor" => {
                    let section = ConfigSection::new(self.interceptor_config(&ext.instance));
                    // v1: safe-default classifier; real routing is a follow-up.
                    let provider_fn: interceptor_host::ProviderFn =
                        Box::new(|_prompt| "agentic".to_string());
                    interceptors.push(Box::new(interceptor_host::WasmInterceptor::instantiate(
                        &self.engine,
                        &ext.instance.id,
                        component,
                        section,
                        provider_fn,
                    )?));
                }
                _ => {}
            }
        }
        Ok(AgentSession {
            dispatcher: intercept::Dispatcher::new(interceptors),
            providers,
            store: self.open_store()?,
        })
    }

    /// Open the host-side persistent store from config: the enabled `store.sqlite`
    /// instance's `path` gives a durable `SQLite` file; anything else (including
    /// `store.memory` or a missing `path`) is an ephemeral in-memory store.
    /// Persistence is host-side — the sandbox has no filesystem.
    fn open_store(&self) -> Result<store::Store, CoreError> {
        let sqlite_path = self.extensions.iter().find_map(|ext| {
            let inst = &ext.instance;
            (inst.category == "store" && inst.kind == "sqlite")
                .then(|| inst.config.get("path").and_then(serde_json::Value::as_str))
                .flatten()
        });
        sqlite_path
            .map_or_else(store::Store::open_in_memory, store::Store::open)
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

/// A booted thin-loop agent: the interceptor dispatcher and the provider fallback
/// chain, ready to run turns through the [`conductor`].
pub struct AgentSession {
    dispatcher: intercept::Dispatcher,
    providers: Vec<Box<dyn conductor::Completer>>,
    store: store::Store,
}

impl AgentSession {
    /// Run one turn headless with no tools — the convenience path. An interceptor
    /// `ask` resolves to its `default-answer`, and tool calls resolve to
    /// "no tool" (there is no first-party `tool-callable` extension in v1).
    pub fn run(&mut self, session: &str, message: &str) -> conductor::RunResult {
        self.run_with(&mut HeadlessDriver, &mut conductor::NoTools, session, message)
    }

    /// Run one turn driving the full loop (before-loop → shaping → `ReAct` →
    /// finalize) with an explicit `driver` (answers interceptor `ask`s) and
    /// `tools` (routes tool calls). On a completed turn the user message + answer
    /// are appended to the session's durable transcript in the host-side store.
    /// This is the seam a real client/driver and the `tool-callable` fleet wire into.
    pub fn run_with(
        &mut self,
        driver: &mut dyn intercept::Driver,
        tools: &mut dyn conductor::ToolInvoker,
        session: &str,
        message: &str,
    ) -> conductor::RunResult {
        let result = conductor::run_turn(
            &mut self.dispatcher,
            &mut self.providers,
            tools,
            driver,
            session,
            message,
        );
        if let conductor::RunResult::Answered { text, .. } = &result {
            self.persist_turn(session, message, text);
        }
        result
    }

    /// The durable transcript for `session`, oldest turn first. Reads from the
    /// host-side store, so it survives a `Runtime` restart against the same DB.
    #[must_use]
    pub fn transcript(&self, session: &str) -> Vec<store::Entry> {
        let mut entries = self.store.recent(session, u32::MAX).unwrap_or_default();
        entries.reverse(); // `recent` is newest-first; a transcript reads oldest-first
        entries
    }

    /// Append one `{user, answer}` turn to the session's transcript. Best-effort:
    /// a store failure is logged, never fatal to the turn that already succeeded.
    fn persist_turn(&self, session: &str, user: &str, answer: &str) {
        let turn = self.store.list_keys(session).map_or(0, |keys| keys.len()) + 1;
        let value = serde_json::json!({ "user": user, "answer": answer }).to_string();
        if let Err(err) = self.store.set(session, &format!("turn-{turn}"), &value) {
            eprintln!("WARN [core] persisting turn for session {session} failed: {err}");
        }
    }
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

    #[test]
    fn boot_rank_orders_dependencies_first() {
        assert!(boot_rank("store") < boot_rank("provider"));
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
  store:
    memory:
      enabled: true
  provider:
    openai:
      enabled: false
",
        )
        .unwrap();

        // ext/ dir is empty, so the enabled store resolves as missing.
        let runtime = Runtime::boot(&cfg, dir.join("ext")).unwrap();
        let exts = runtime.extensions();
        assert_eq!(exts.len(), 1, "only the enabled instance is resolved");
        assert_eq!(exts[0].instance.id, "store.memory");
        assert!(matches!(exts[0].state, LoadState::Missing(_)));

        // No components compiled, so starting is a clean no-op.
        assert!(runtime.start_all().unwrap().is_empty());

        std::fs::remove_dir_all(&dir).ok();
    }
}
