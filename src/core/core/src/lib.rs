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
pub mod acp;
#[allow(missing_docs, clippy::all, clippy::pedantic, clippy::nursery)]
mod bindings;
pub mod conductor;
pub mod delegate;
pub mod egress;
pub mod event_log;
pub mod ext;
mod host;
pub mod host_fs;
pub mod host_process;
pub mod http;
pub mod intercept;
pub mod interceptor_host;
pub mod manifest;
pub mod mcp;
pub mod projection;
pub mod registry_host;
pub mod route;
pub mod rpc;
pub mod sandbox;
pub mod sandbox_landlock;
pub mod sandbox_seatbelt;
pub mod serve;
pub mod store;
pub mod telegram;
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

/// The user's home directory, if the environment names one.
fn dirs_home() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .filter(|p| !p.as_os_str().is_empty())
}

/// Whether `cwd` may be adopted as the workspace with nobody having said so.
///
/// `host-fs` defaults to `$PWD` when `workspace:` is unset, so the jail is only
/// meaningful if the root is narrower than the machine. Declines `/` (jail = the
/// filesystem) and `$HOME` (jail = every file the user owns). Not a defense
/// against a determined operator — just a guard against an accidental `cd`.
fn adoptable_workspace(cwd: &Path, home: Option<&Path>) -> bool {
    // A filesystem root has no parent.
    if cwd.parent().is_none() {
        return false;
    }
    if home.is_some_and(|home| home == cwd) {
        return false;
    }
    true
}

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
    /// The `host-*` interfaces this component actually imports, sorted, read
    /// from the compiled component rather than from anything that describes it.
    ///
    /// `None` when there was no component to read ([`LoadState::Missing`]).
    /// `Some(&[])` is a different thing — a component that imports no host
    /// capability at all, which `tool-escape-probe` nearly is. Keeping those
    /// apart matters: one is "nothing to ask" and the other is "asks for
    /// nothing", and a manifest check has to treat them differently.
    pub capabilities: Option<Vec<String>>,
}

/// The `host-*` interfaces `component` imports, sorted and deduplicated.
///
/// Read from the component's own type, so it describes the artifact rather than
/// any claim about it. Two things are deliberately not in the result, and both
/// would otherwise be here:
///
/// * **Exports.** `tool-callable` and `extension-lifecycle` are what a guest
///   *implements*. Only imports are asked for.
/// * **Type-only imports.** `llm-types` and `store-types` are shapes; importing
///   one grants nothing, so calling it a capability would tell an operator to
///   allow something that does not exist to allow.
///
/// The same two exclusions the manifest generator makes
/// (`scripts/manifests.sh`), for the same reasons — which is what lets the two
/// be cross-checked against each other at all.
fn host_capabilities(component: &Component, engine: &Engine) -> Vec<String> {
    let mut found: Vec<String> = component
        .component_type()
        .imports(engine)
        // An import is named for the interface, e.g.
        // `jan-klod:interfaces/host-fs@0.1.0`.
        .filter_map(|(name, _)| {
            name.strip_prefix("jan-klod:interfaces/")
                .and_then(|rest| rest.split('@').next())
                .filter(|interface| interface.starts_with("host-"))
                .map(str::to_owned)
        })
        .collect();
    found.sort_unstable();
    found.dedup();
    found
}

/// Whether a component and the manifest beside it agree.
///
/// Neutral about what to *do*: boot refuses on all but `Consistent` (unless
/// `allow-unmanifested` covers the absent case) and `ext install` refuses on
/// all of them, and each maps this to its own error with its own wording.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Verdict {
    /// A manifest is present, built against a compatible interface package,
    /// and declaring everything the component imports.
    Consistent,
    /// No manifest file beside the component.
    NoManifest,
    /// Built against an interface package this host does not speak.
    ApiMismatch {
        /// The version the manifest declares.
        theirs: String,
    },
    /// The component imports host interfaces its manifest does not admit to.
    UnderDeclared {
        /// Those interfaces, in the order `undeclared` reports them.
        interfaces: Vec<String>,
    },
}

/// A component's real imports, and the verdict on its manifest.
pub(crate) struct Inspected {
    /// The `host-*` interfaces the component actually imports.
    pub capabilities: Vec<String>,
    /// What the manifest beside it turned out to say.
    pub verdict: Verdict,
}

/// Read a compiled component's imports and check the manifest beside it.
///
/// **One implementation on purpose.** Boot does this at every load and
/// `ext install` must do exactly the same thing before a component lands, and
/// two copies of "consistent" is how an install comes to accept what boot
/// refuses — a divergence that would be silent and security-relevant, since
/// the thing being compared is what a component may ask the host for.
fn inspect(
    component: &Component,
    engine: &Engine,
    path: &Path,
) -> Result<Inspected, manifest::ManifestError> {
    // Read once, while the component is compiled and in hand.
    let capabilities = host_capabilities(component, engine);
    let verdict = match manifest::Manifest::beside(path)? {
        Some(declared) => {
            // The contract version first: a component built against a different
            // interface package is refused with both versions named, rather
            // than later as an obscure "no such import" from the linker.
            if manifest::api_compatible(manifest::API_VERSION, &declared.api_version) {
                let undeclared = declared.undeclared(&capabilities);
                if undeclared.is_empty() {
                    Verdict::Consistent
                } else {
                    Verdict::UnderDeclared {
                        interfaces: undeclared.iter().map(|i| (*i).to_owned()).collect(),
                    }
                }
            } else {
                Verdict::ApiMismatch {
                    theirs: declared.api_version,
                }
            }
        }
        None => Verdict::NoManifest,
    };
    Ok(Inspected {
        capabilities,
        verdict,
    })
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

/// Pass 1 output of [`Runtime::build_agent`]: every instantiated provider, tool
/// and registry instance, ready to fold into the fleet and fallback chain.
struct ProvidersAndTools {
    providers: Vec<Box<dyn conductor::Completer>>,
    provider_ids: Vec<String>,
    tool_extensions: Vec<tool_host::ToolExtension>,
    skills_extensions: Vec<registry_host::SkillsExtension>,
    mcp_extensions: Vec<registry_host::McpExtension>,
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
    pub fn boot(
        config_path: impl AsRef<Path>,
        ext_dir: impl AsRef<Path>,
    ) -> Result<Self, CoreError> {
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
        // Top-level, not `extensions.allow-unmanifested` as first sketched:
        // every key under `extensions:` must be a mapping of named instances
        // (`ConfigError::CategoryNotMap`), so a boolean there is a hard config
        // error rather than a flag. Verified before moving it.
        let allow_unmanifested = agent
            .get("allow-unmanifested")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false);
        let mut extensions = Vec::with_capacity(order.len());
        for instance in order {
            let path = ext_dir.join(instance.component_file());
            let (state, capabilities) = if path.exists() {
                let component =
                    Component::from_file(&engine, &path).map_err(|source| CoreError::Load {
                        id: instance.id.clone(),
                        path: path.display().to_string(),
                        source: source.into(),
                    })?;
                // Cross-check the component against what it declares, through
                // the same `inspect` that `ext install` uses.
                let inspected =
                    inspect(&component, &engine, &path).map_err(|source| CoreError::Manifest {
                        id: instance.id.clone(),
                        source,
                    })?;
                match inspected.verdict {
                    Verdict::ApiMismatch { theirs } => {
                        return Err(CoreError::ApiVersion {
                            id: instance.id.clone(),
                            component: instance.component_file(),
                            theirs,
                            ours: manifest::API_VERSION.to_owned(),
                        });
                    }
                    // A component needing more than it admits to is either
                    // mislabelled or lying.
                    Verdict::UnderDeclared { interfaces } => {
                        return Err(CoreError::Undeclared {
                            id: instance.id.clone(),
                            component: instance.component_file(),
                            interfaces: interfaces.join(", "),
                        });
                    }
                    // No manifest at all: refused, because an undeclared
                    // component is one nobody can inspect before running it,
                    // and the whole point of a declaration is to be checkable
                    // ahead of time. `allow-unmanifested` is the named
                    // widening for local development.
                    Verdict::NoManifest if !allow_unmanifested => {
                        return Err(CoreError::NoManifest {
                            id: instance.id.clone(),
                            component: instance.component_file(),
                        });
                    }
                    // Consistent, or unmanifested where that is allowed. The
                    // guarded arm above is what makes the second case a
                    // deliberate widening rather than a gap.
                    Verdict::Consistent | Verdict::NoManifest => {}
                }
                (LoadState::Compiled(component), Some(inspected.capabilities))
            } else {
                (LoadState::Missing(path), None)
            };
            extensions.push(LoadedExtension {
                instance: instance.clone(),
                state,
                capabilities,
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
    /// grants (`tool-*` needs `host-fs`/`host-process`, `registry-*` needs
    /// `host-fs`/`host-event`) go through their own seam — the same one
    /// [`Self::build_agent`] uses — with the same default-deny substrates, so
    /// this boot-plan path proves exactly what the agent path will do.
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
            started.push(self.start_one(ext, component, workspace.as_ref(), &process)?);
        }
        Ok(started)
    }

    /// Instantiate and start one compiled extension. A `tool-*`, `registry-*` or
    /// `interceptor-*` category is instantiated through its own seam (the same
    /// one [`Self::build_agent`] uses), because its world imports more than the
    /// neutral `extension-world` grants; everything else goes through the shared
    /// linker. Returns the id once its lifecycle has started.
    fn start_one(
        &self,
        ext: &LoadedExtension,
        component: &Component,
        workspace: Option<&host_fs::Workspace>,
        process: &host_process::ProcessRunner,
    ) -> Result<String, CoreError> {
        let id = &ext.instance.id;
        let config_json = ext.instance.config.to_string();
        match (ext.instance.category.as_str(), ext.instance.kind.as_str()) {
            ("tool", _) => {
                tool_host::ToolExtension::instantiate(
                    &self.engine,
                    id,
                    component,
                    workspace.cloned(),
                    process.clone(),
                )?;
            }
            ("registry", "skills") => {
                registry_host::SkillsExtension::instantiate(
                    &self.engine,
                    id,
                    component,
                    config_json,
                    workspace.cloned(),
                )?;
            }
            ("registry", "mcp") => {
                registry_host::McpExtension::instantiate(
                    &self.engine,
                    id,
                    component,
                    config_json,
                    self.egress_policy(),
                )?;
            }
            ("interceptor", _) => {
                // A constant classifier: this path only proves the guest
                // instantiates and starts, and must not make a model call to do
                // it. `build_agent` supplies the real one.
                let provider_fn: interceptor_host::ProviderFn =
                    Box::new(|_request| "agentic".to_string());
                interceptor_host::WasmInterceptor::instantiate(
                    &self.engine,
                    id,
                    component,
                    ConfigSection::new(self.interceptor_config(&ext.instance)),
                    provider_fn,
                )?;
            }
            _ => return self.start_via_shared_linker(ext, component),
        }
        Ok(id.clone())
    }

    /// Instantiate through the shared, capability-neutral linker and run its
    /// lifecycle (`init` → `start`). The path every category not listed in
    /// [`Self::start_one`] takes.
    fn start_via_shared_linker(
        &self,
        ext: &LoadedExtension,
        component: &Component,
    ) -> Result<String, CoreError> {
        let id = &ext.instance.id;
        let section = ConfigSection::new(ext.instance.config.clone());
        let mut store = Store::new(
            &self.engine,
            HostState::new(id.clone(), section).with_egress(self.egress_policy()),
        );

        let world =
            ExtensionWorld::instantiate(&mut store, component, &self.linker).map_err(|source| {
                CoreError::Instantiate {
                    id: id.clone(),
                    source: source.into(),
                }
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
        Ok(id.clone())
    }

    /// A human-readable boot plan (each instance → its component, loaded/missing).
    #[must_use]
    pub const fn report(&self) -> BootReport<'_> {
        BootReport(self)
    }

    /// Boot the thin-loop agent from config: instantiate every enabled+compiled
    /// `interceptor.*` as a dispatcher (in boot/load order) and every
    /// `provider.*` as a completer fallback chain, ready to run turns through the
    /// [`conductor`].
    ///
    /// `http_factory` mints a fresh `host-http` backend per provider (each provider
    /// instance owns its own store). An interceptor's `llm-provider` import is
    /// backed by a dedicated provider instance (see [`Self::open_classifier`]).
    ///
    /// # Errors
    /// Returns a [`CoreError`] if any interceptor or provider fails to instantiate
    /// or start.
    pub fn build_agent(
        &self,
        http_factory: &dyn Fn() -> route::HttpFn,
    ) -> Result<AgentSession, CoreError> {
        // Shared, default-deny substrates for tools (opt-in via config).
        let workspace = self.open_workspace();
        let project_instructions = Self::project_instructions(workspace.as_ref());
        let process = self.open_process_runner(workspace.as_ref());

        // Pass 1: providers + tools + registries. (Before interceptors so tool-selector
        // can be handed the combined advertised metadata.)
        let ProvidersAndTools {
            providers,
            provider_ids,
            tool_extensions,
            skills_extensions,
            mcp_extensions,
        } = self.instantiate_providers_and_tools(http_factory, workspace.as_ref(), &process)?;
        let tool_fleet = tool_host::ToolFleet::new(tool_extensions);
        let registry_fleet = registry_host::RegistryFleet::new(skills_extensions, mcp_extensions);
        let mut tools = CombinedFleet {
            tools: tool_fleet,
            registry: registry_fleet,
        };
        let tools_advert = tools.all_metas_json();

        // An interceptor that consults a model (intent routing) gets its own
        // provider instance rather than a handle into the chain above: the
        // conductor holds the chain mutably for the whole turn, so reaching into
        // it mid-dispatch would alias it.
        let classifier = self.open_classifier(http_factory)?;

        // Opened before the interceptors, because they share it: an interceptor's
        // `host-storage` writes land here, namespaced to the component.
        let store = self.open_store()?;

        // Pass 2: interceptors, each served the tool set at `select-tools`.
        let interceptors = self.instantiate_interceptors(
            &tools_advert,
            project_instructions.as_deref(),
            classifier.as_ref(),
            &store,
        )?;

        // The fallback chain's *order* is what `providers:` configures; without
        // this the chain was whatever boot order produced (alphabetical), so the
        // documented "tried top-to-bottom" list had no effect at all.
        let ordered = order_chain(self.agent.get("providers"), &provider_ids);
        let providers = reorder(providers, &ordered);

        Ok(AgentSession {
            dispatcher: intercept::Dispatcher::new(interceptors),
            providers,
            store,
            limits: self.limits(),
            tools,
        })
    }

    /// Pass 1 of [`Self::build_agent`]: instantiate every enabled provider, tool
    /// and registry instance.
    fn instantiate_providers_and_tools(
        &self,
        http_factory: &dyn Fn() -> route::HttpFn,
        workspace: Option<&host_fs::Workspace>,
        process: &host_process::ProcessRunner,
    ) -> Result<ProvidersAndTools, CoreError> {
        let mut providers: Vec<Box<dyn conductor::Completer>> = Vec::new();
        // Instance ids, parallel to `providers`, so the configured chain can be
        // matched by name without downcasting a `dyn Completer`.
        let mut provider_ids: Vec<String> = Vec::new();
        let mut tool_extensions: Vec<tool_host::ToolExtension> = Vec::new();
        let mut skills_extensions: Vec<registry_host::SkillsExtension> = Vec::new();
        let mut mcp_extensions: Vec<registry_host::McpExtension> = Vec::new();

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
                        workspace.cloned(),
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
                        workspace.cloned(),
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
        Ok(ProvidersAndTools {
            providers,
            provider_ids,
            tool_extensions,
            skills_extensions,
            mcp_extensions,
        })
    }

    /// Pass 2 of [`Self::build_agent`]: instantiate every enabled interceptor,
    /// each served the tool set advertised by pass 1 at `select-tools`.
    fn instantiate_interceptors(
        &self,
        tools_advert: &serde_json::Value,
        project_instructions: Option<&str>,
        classifier: Option<&std::sync::Arc<std::sync::Mutex<route::ProviderCompleter>>>,
        store: &Arc<Mutex<store::Store>>,
    ) -> Result<Vec<Box<dyn intercept::Interceptor>>, CoreError> {
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
                // Same shape as `tools`: the host does the reading it is allowed to
                // do, and the guest receives data rather than a capability.
                if let Some(project) = project_instructions {
                    map.entry("project-instructions")
                        .or_insert_with(|| serde_json::Value::String(project.to_string()));
                }
            }
            // Durability is opt-in per instance, off by default. `interceptor-permission`
            // stores standing grants ("always allow `fs:write`") as run-scoped;
            // handing every interceptor the session store would make grants
            // survive a restart, silently breaking that guarantee. Same
            // default-deny shape as `host-fs`/`host-process`.
            let persist = ext
                .instance
                .config
                .get("persist")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false);
            let provider_fn = classifier_fn(classifier.cloned());
            interceptors.push(Box::new(
                interceptor_host::WasmInterceptor::instantiate_with_storage(
                    &self.engine,
                    &ext.instance.id,
                    component,
                    ConfigSection::new(config),
                    provider_fn,
                    persist.then(|| Arc::clone(store)),
                )?,
            ));
        }
        Ok(interceptors)
    }

    /// The project's own instructions, if the workspace has an `AGENTS.md`.
    ///
    /// Read **host-side** and handed to `interceptor-system` as config, rather
    /// than granting interceptors `host-fs` just to read one file. Only the
    /// workspace root — climbing to an ancestor directory would let a nested
    /// checkout inherit another project's instructions. Truncated at a cap with a
    /// note, since a system prompt is paid for on every turn.
    fn project_instructions(workspace: Option<&host_fs::Workspace>) -> Option<String> {
        /// Generous for conventions, small next to a context window.
        const MAX_BYTES: usize = 16 * 1024;
        let text = workspace?.read("AGENTS.md").ok()?;
        if text.len() <= MAX_BYTES {
            return Some(text);
        }
        let mut cut = MAX_BYTES;
        while cut > 0 && !text.is_char_boundary(cut) {
            cut -= 1;
        }
        Some(format!(
            "{}\n\n[AGENTS.md truncated at {MAX_BYTES} bytes — it is sent with every \
             turn, so keep it short]",
            &text[..cut]
        ))
    }

    /// Bounds a turn runs under, from the top-level `limits:` block.
    ///
    /// The cycle cap exists so a model that keeps emitting tool calls cannot spin —
    /// or, on a metered endpoint, spend — forever. Eight is a real constraint for
    /// coding work, so it has to be raisable by whoever is paying.
    fn limits(&self) -> conductor::Limits {
        let configured = self
            .agent
            .get("limits")
            .and_then(|limits| limits.get("max-iterations"))
            .and_then(serde_json::Value::as_u64)
            .and_then(|n| u32::try_from(n).ok())
            .filter(|n| *n > 0);
        conductor::Limits {
            max_iterations: configured.unwrap_or(conductor::DEFAULT_MAX_ITERATIONS),
        }
    }

    /// The destinations guests may reach, derived from what the operator already
    /// wrote down.
    ///
    /// Every enabled instance's `base-url`/`endpoint`/`url` is allowed even when
    /// local (a self-hosted model lives on `127.0.0.1`); everything else is
    /// public-only. A guest cannot widen this — it's built here and closed over
    /// by the host's HTTP backend.
    #[must_use]
    pub fn egress_policy(&self) -> egress::EgressPolicy {
        let mut policy = egress::EgressPolicy::public_only();
        for ext in &self.extensions {
            for key in ["base-url", "endpoint", "url"] {
                if let Some(url) = ext
                    .instance
                    .config
                    .get(key)
                    .and_then(serde_json::Value::as_str)
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
        let root: &str = if let Some(r) = self
            .agent
            .get("workspace")
            .and_then(serde_json::Value::as_str)
        {
            // Explicit means explicit: an operator who names a root gets it,
            // including one this would not adopt on its own.
            r
        } else {
            let cwd = std::env::current_dir().ok()?;
            if !adoptable_workspace(&cwd, dirs_home().as_deref()) {
                eprintln!(
                    "WARN [core] not adopting `{}` as the workspace: it is your home \
                         directory or a filesystem root, where a path jail protects nothing. \
                         Start jan-klod in a project directory, or set `workspace:` \
                         explicitly. Until then file tools are denied.",
                    cwd.display()
                );
                return None;
            }
            root_owned = cwd.to_string_lossy().into_owned();
            // Say what the agent can reach. The grant is implicit; the notice
            // should not be.
            eprintln!("INFO [core] workspace: {root_owned} (file tools are jailed here)");
            &root_owned
        };
        host_fs::Workspace::open(root).map_or_else(
            |_| {
                eprintln!("WARN [core] workspace `{root}` could not be opened; host-fs is denied");
                None
            },
            Some,
        )
    }

    /// Build the `host-process` runner from the top-level `execution:` config
    /// (`{ enabled, timeout-secs?, output-cap?, sandbox? }`). Disabled unless
    /// enabled *and* a workspace is configured (the exec cwd is jailed to it),
    /// and also when `sandbox:` cannot be read.
    fn open_process_runner(
        &self,
        workspace: Option<&host_fs::Workspace>,
    ) -> host_process::ProcessRunner {
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
                // Named environment variables are a grant, one at a time, the
                // same shape as `network.allow`. A child otherwise gets only
                // `host_process::BASE_ENV`.
                let passthrough: Vec<String> = exec
                    .and_then(|e| e.get("env-passthrough"))
                    .and_then(serde_json::Value::as_array)
                    .map(|names| {
                        names
                            .iter()
                            .filter_map(|n| n.as_str().map(str::to_owned))
                            .collect()
                    })
                    .unwrap_or_default();
                // What a command may do once running, as distinct from what the
                // runner above bounds. A policy that cannot be read denies
                // execution outright rather than running unconfined: the
                // operator asked for something specific about a command's
                // effects, and guessing at it is the one response that could
                // silently grant more than they wrote.
                let policy = match sandbox::SandboxPolicy::from_config(exec, ws) {
                    Ok(policy) => policy,
                    Err(err) => {
                        eprintln!(
                            "WARN [core] `execution.sandbox` could not be read ({err:?}); \
                             host-process is denied"
                        );
                        return host_process::ProcessRunner::disabled();
                    }
                };
                // One `host_backend()` for both the report and the wiring: two
                // calls could disagree (the mechanism could vanish between
                // them), and then the boot line would describe a confinement
                // the runner does not have.
                let backend = sandbox::host_backend();
                let effective = policy.resolve(backend.as_deref());
                // `require: true` asked for no command rather than an unconfined
                // one, so a refusal here is the configuration working, not
                // failing.
                if let Some(refusal) = policy.refusal(&effective) {
                    eprintln!("WARN [core] {refusal}");
                    return host_process::ProcessRunner::disabled();
                }
                if let Some(reason) = &effective.downgrade {
                    eprintln!("WARN [core] {reason}");
                } else {
                    eprintln!(
                        "INFO [core] command sandbox: {:?} (as configured)",
                        effective.mode
                    );
                }
                let runner = host_process::ProcessRunner::new(
                    ws.clone(),
                    std::time::Duration::from_secs(timeout),
                    usize::try_from(cap).unwrap_or(64 * 1024),
                )
                .with_env_passthrough(passthrough);
                // The mode just printed and the confinement just wired come from
                // the same pair, so the runtime cannot report `Os` while running
                // commands unconfined.
                match (effective.mode, backend) {
                    (sandbox::SandboxMode::Os, Some(backend)) => {
                        runner.with_sandbox(std::sync::Arc::from(backend), policy)
                    }
                    _ => runner,
                }
            }
            _ => host_process::ProcessRunner::disabled(),
        }
    }

    /// Instantiate the provider that answers interceptors' `llm-provider` calls.
    ///
    /// Uses the instance named by top-level `classifier:`, else the head of the
    /// fallback chain — lets classification (a two-token question) run on a
    /// small local model instead of the turn's expensive one.
    ///
    /// Returns `None` when no provider is enabled; callers fall back to the
    /// conservative default rather than failing the boot.
    ///
    /// # Errors
    /// Returns a [`CoreError`] if the chosen provider cannot be instantiated.
    fn open_classifier(
        &self,
        http_factory: &dyn Fn() -> route::HttpFn,
    ) -> Result<Option<std::sync::Arc<std::sync::Mutex<route::ProviderCompleter>>>, CoreError> {
        let named = self
            .agent
            .get("classifier")
            .and_then(serde_json::Value::as_str);
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
        let LoadState::Compiled(component) = &ext.state else {
            return Ok(None);
        };
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
    /// Storage is **not** an extension: a store guest would need `host-fs`
    /// granted back to it, and the transcript is the most sensitive thing the
    /// runtime holds. Guests reach it only through `host-storage`, namespaced to
    /// themselves.
    ///
    /// A relative `path` resolves against the directory holding `config.yaml`,
    /// not the working directory — otherwise each directory jan-klod is started
    /// from would get its own `jan-klod.db` and conversation history.
    fn open_store(&self) -> Result<Arc<Mutex<store::Store>>, CoreError> {
        let sqlite_path = self
            .agent
            .get("storage")
            .and_then(|storage| storage.get("path"))
            .and_then(serde_json::Value::as_str)
            .map(|path| self.config_dir.join(path));
        let store = sqlite_path
            .map_or_else(store::Store::open_in_memory, store::Store::open)
            .map_err(|source| CoreError::Store {
                message: source.to_string(),
            })?;
        // Convert any transcript written before the event log existed. Called
        // here rather than inside `Store::open` so the store stays ignorant of
        // what a payload means: the envelope belongs to `event_log`, and a
        // store that had to build one would know the format of the thing it is
        // supposed to hold opaquely.
        //
        // A failure here does not fail the boot. Nothing is lost by deferring —
        // the `entries` rows are still there and the next open tries again —
        // whereas refusing to start would make an unreadable old session into
        // an unusable install.
        match event_log::migrate_transcripts(&store) {
            Ok(0) => {}
            Ok(sessions) => eprintln!(
                "INFO [core] converted {sessions} session(s) from the pre-event-log \
                 transcript into the event log"
            ),
            Err(err) => eprintln!(
                "WARN [core] converting old transcripts failed ({err}); those sessions \
                 will not be listed until it succeeds"
            ),
        }
        Ok(Arc::new(Mutex::new(store)))
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
        self.tools
            .invoke(call)
            .or_else(|| self.registry.invoke(call))
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
    /// Bounds this session's turns run under, from top-level `limits:`.
    limits: conductor::Limits,
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
            self.limits,
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
            self.limits,
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
            self.limits,
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
            self.limits,
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
            self.limits,
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
            self.limits,
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
            self.limits,
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
    pub fn transcript(&self, session: &str) -> Vec<intercept::Message> {
        // Returns messages, not `store::Entry`. An entry is a key/value row with
        // a namespace, a key and two timestamps; a projected turn has none of
        // those, so handing back entries would mean inventing fields that mean
        // nothing and inviting a caller to read them.
        //
        // Unbounded, unlike `replay`: this is "show me the session", and a
        // reader asking for a transcript wants the whole thing.
        let events = self
            .store
            .lock()
            .map(|store| store.session_events(session).unwrap_or_default())
            .unwrap_or_default();
        projection::transcript(&events)
    }

    /// Copy `from`'s log up to and including `at_seq` into `into`, so the new
    /// session continues from that point and then diverges.
    ///
    /// Nothing is written to the parent, and nothing links the two: a fork is a
    /// copy of a prefix, not a branch pointer. That is what makes "then
    /// independent" true without any bookkeeping to keep true — the parent
    /// cannot be affected by a session it has no reference to.
    ///
    /// # Errors
    /// Returns the store's error if `into` already has a log, or on a SQL
    /// failure. Copying **nothing** — `at_seq` of 0, or a parent with no log —
    /// is reported as `Ok(0)`; whether that is a mistake is the caller's
    /// question, and the REST surface answers it.
    pub fn fork_session(
        &self,
        from: &str,
        at_seq: u64,
        into: &str,
    ) -> Result<u64, store::StoreError> {
        self.store
            .lock()
            .map_err(|_| store::StoreError::Backend {
                detail: "the store lock is poisoned".to_owned(),
            })?
            .fork_events(from, at_seq, into)
    }

    /// All known session ids, newest first.
    #[must_use]
    pub fn list_sessions(&self) -> Vec<String> {
        // Sessions come from the log, not from `entries`. A namespace existed in
        // `entries` because a transcript had been written there, so reading it
        // for this would report nothing the moment the transcript write goes
        // away — silently, since an empty list is a legitimate answer.
        //
        // Consequence worth knowing: a database written before the log existed
        // has entries and no events, so its sessions are not listed here and
        // read as empty through `transcript`. That is what makes the migration
        // a requirement rather than an option.
        self.store
            .lock()
            .map(|store| {
                // Not `unwrap_or_default()`: swallowing this is what let a broken
                // query report "no sessions" for as long as nobody looked.
                store.event_sessions().unwrap_or_else(|err| {
                    eprintln!("WARN [core] listing sessions failed: {err}");
                    Vec::new()
                })
            })
            .unwrap_or_default()
            .into_iter()
            // Interceptors share this database, under `ext/<component>/…`. Those
            // are `entries` namespaces and cannot appear as event sessions, but
            // the filter stays: it costs nothing and the day something logs
            // events under a slashed id, a session picker should not show it.
            .filter(|session| !session.contains('/'))
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
    limits: conductor::Limits,
    tools: &mut dyn conductor::ToolInvoker,
    driver: &mut dyn intercept::Driver,
    sink: &mut dyn conductor::EventSink,
    session: &str,
    message: &str,
) -> conductor::RunResult {
    // Locked around each use, never across the turn: an interceptor writing its
    // own `host-storage` mid-dispatch takes the same lock, and holding it here
    // would deadlock the first guest that remembered anything.
    let history = store
        .lock()
        .map(|store| replay(&store, session))
        .unwrap_or_default();
    // The event log, wrapped around whatever sink and driver the caller passed.
    // Every entry point into a turn funnels through here, so one wrap covers
    // them all — `run`, `run_with`, `run_streaming` and the rest cannot acquire
    // a turn that goes unlogged by forgetting to opt in.
    let mut logged_sink = event_log::PersistingSink::new(sink, store, session);
    let mut logged_driver = event_log::PersistingDriver::new(driver, store, session);
    // Logs the message the model is actually about to receive, not `message`
    // itself: a `before-loop` interceptor may `replace` it before the
    // conductor resolves `effective_message` (see `conductor::run_turn`'s
    // `on_effective_message` hook), and the log exists to record what
    // happened, not what was asked for — #84. The conductor calls this
    // exactly once, at the point `effective_message` is resolved and before
    // it emits a single event, so this row still opens the session's log
    // ahead of everything the turn goes on to record.
    let mut log_effective_message =
        |effective: &str| event_log::log_user_message(store, session, effective);
    let result = conductor::run_turn(
        dispatcher,
        providers,
        tools,
        &mut logged_driver,
        &mut logged_sink,
        session,
        message,
        history,
        limits,
        &mut log_effective_message,
    );
    // No transcript append. The turn recorded itself as it ran — the user
    // message before `run_turn`, every event through the sink — so writing a
    // `{user, answer}` row here as well would be a second copy of the same
    // session in a second format, which is the thing this phase removes. A
    // migration converts the rows written before that was true.
    result
}

/// How many past turns are replayed into a new one, bounded here as well as in
/// `select-context`; token-aware trimming on top is the context interceptor's
/// job.
///
/// This used to bound the store read itself (`recent(session, 20)` over the
/// transcript), then stopped doing that when #45's box 4 moved the log whole
/// into memory and trimmed it there with `projection::last_turns`, because
/// bounding an event log by *turns* is not something a plain `LIMIT` can
/// express — a turn is a variable number of rows. That made every turn read
/// and decode a session's entire log to keep the last 20 turns of it, a cost
/// that grew without bound in session length (#85). `Store::recent_turns` puts
/// the same rule back in SQL — the seq of the bound is itself a query rather
/// than a `LIMIT` — so the read is bounded again, in the store rather than
/// after it.
const REPLAYED_TURNS: u32 = 20;

/// The conversation so far, oldest-first, as loop messages.
///
/// Each stored turn is `{"user":…,"answer":…}`; a malformed or unreadable entry
/// is skipped rather than failing the turn — a corrupt transcript row should cost
/// context, not the ability to talk.
fn replay(store: &store::Store, session: &str) -> Vec<intercept::Message> {
    // Read from the event log, not the `entries` transcript. Both are written
    // today; the transcript write goes away once every read path is off it.
    // `recent_turns` is oldest-first already, so nothing is reversed here —
    // the log's order *is* the conversation's. It is `session_events`'s
    // bounded sibling, reading only the tail this replay actually uses
    // instead of the whole log and trimming in memory afterward.
    let events = store
        .recent_turns(session, REPLAYED_TURNS)
        .unwrap_or_default();
    projection::transcript(&events)
}

/// The closure interceptors' `llm-provider` resolves to.
///
/// A classification failure is not a turn failure: no provider, a poisoned
/// lock, or an erroring call all default to `"agentic"`, the conservative label
/// that routes through the full loop rather than short-circuiting it.
///
/// Shared by `Arc` rather than borrowed, since the closure outlives the call
/// that builds it.
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
/// Tolerates a config that has drifted from the enabled instance set: a named
/// provider that isn't enabled is skipped with a warning (not a boot failure),
/// and an enabled provider the list omits still runs, at the end.
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
    let unlisted: Vec<usize> = (0..ids.len())
        .filter(|index| !order.contains(index))
        .collect();
    order.extend(unlisted);
    order
}

/// Reorder `items` by `order` (a permutation of its indices).
fn reorder<T>(items: Vec<T>, order: &[usize]) -> Vec<T> {
    let mut slots: Vec<Option<T>> = items.into_iter().map(Some).collect();
    order
        .iter()
        .filter_map(|index| slots.get_mut(*index).and_then(Option::take))
        .collect()
}

/// Headless driver: no interactive surface, so an `ask` takes the prompt's
/// `default-answer`.
///
/// Public because `mcp` needs it and must not have any other kind. An MCP
/// server owns **stdin for protocol frames**, so a driver that prompted on the
/// terminal would read a frame as an answer and the client's next request would
/// vanish into a confirmation. The default answer is a refusal, which is also
/// the right policy there — an editor cannot answer a confirmation prompt.
pub struct HeadlessDriver;
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
        write!(f, "{compiled} loaded, {} missing", exts.len() - compiled)
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
    /// A component's manifest could not be read.
    ///
    /// Distinct from a manifest that is simply absent: unreadable or malformed
    /// is refused, because treating a typo as "no manifest" would turn it into
    /// a silent widening of what the component may ask for.
    #[error("{id}: its manifest cannot be read")]
    Manifest {
        /// Instance id whose manifest is unusable.
        id: String,
        /// Why.
        #[source]
        source: manifest::ManifestError,
    },
    /// A component imports a host capability its manifest does not declare.
    #[error(
        "{id}: `{component}` imports {interfaces}, which its manifest does not \
         declare — regenerate it with `make ext`, or the component is not the one \
         the manifest describes"
    )]
    Undeclared {
        /// Instance id that was refused.
        id: String,
        /// The component file, which is also how its manifest is named.
        component: String,
        /// The undeclared interfaces, comma-separated.
        interfaces: String,
    },
    /// A component was built against an incompatible interface package.
    #[error(
        "{id}: `{component}` was built against jan-klod:interfaces@{theirs}, and this \
         build speaks {ours}. Rebuild the component against this host's `wit/`"
    )]
    ApiVersion {
        /// Instance id that was refused.
        id: String,
        /// The component file.
        component: String,
        /// The version the component declares.
        theirs: String,
        /// The version this host speaks.
        ours: String,
    },
    /// A component ships no manifest, and none is permitted.
    #[error(
        "{id}: `{component}` has no manifest beside it. Run `make ext` to generate \
         one, or set top-level `allow-unmanifested: true` to load components that \
         declare nothing"
    )]
    NoManifest {
        /// Instance id that was refused.
        id: String,
        /// The component file whose manifest is absent.
        component: String,
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
        assert_eq!(
            order_chain(Some(&serde_json::json!("nonsense")), &ids),
            vec![0, 1]
        );
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
        let chain = serde_json::json!([{ "provider": "openai" }, { "provider": "openai" }]);
        assert_eq!(order_chain(Some(&chain), &ids), vec![1, 0]);
    }

    #[test]
    fn reorder_applies_the_permutation() {
        assert_eq!(
            reorder(vec!["a", "b", "c"], &[2, 0, 1]),
            vec!["c", "a", "b"]
        );
        // Out-of-range indices cannot panic or duplicate an item.
        assert_eq!(reorder(vec!["a", "b"], &[1, 9, 0]), vec!["b", "a"]);
    }

    /// The accident this guards against is `cd`, not malice.
    #[test]
    fn a_workspace_is_not_adopted_from_home_or_a_root() {
        let home = PathBuf::from("/Users/someone");
        // A project directory is adopted: this is what makes the runtime usable
        // with no configuration at all.
        assert!(adoptable_workspace(&home.join("code/project"), Some(&home)));
        assert!(adoptable_workspace(&PathBuf::from("/srv/app"), Some(&home)));

        // The home directory is every document, key and dotfile the user owns,
        // and it is where a shell starts.
        assert!(!adoptable_workspace(&home, Some(&home)));
        // A filesystem root makes the "jail" the machine.
        assert!(!adoptable_workspace(&PathBuf::from("/"), Some(&home)));

        // With no HOME in the environment, only the root check applies — refusing
        // everything would break every container that does not set it.
        assert!(adoptable_workspace(&PathBuf::from("/work"), None));
        assert!(!adoptable_workspace(&PathBuf::from("/"), None));
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

    /// A `before-loop` interceptor that unconditionally `replace`s the user
    /// message — the in-process stub `core/tests/intercept.rs` already uses for
    /// exactly this phase (`Behavior::ReplaceUserMessage`), reproduced here
    /// because that file only sees `jan_klod_core`'s public API and cannot
    /// reach `run_and_persist`, which is private to this crate.
    struct RewriteBeforeLoop {
        replacement: String,
    }
    impl intercept::Interceptor for RewriteBeforeLoop {
        fn id(&self) -> &'static str {
            "rewrite"
        }
        fn subscribed_phases(&self) -> Vec<intercept::Phase> {
            vec![intercept::Phase::BeforeLoop]
        }
        fn intercept(
            &mut self,
            _input: &intercept::InterceptInput,
        ) -> Result<intercept::Decision, intercept::InterceptorError> {
            Ok(intercept::Decision::Replace(
                intercept::HookState::BeforeLoop(intercept::UserTurn {
                    session: "s".to_string(),
                    user_message: self.replacement.clone(),
                }),
            ))
        }
    }

    /// A provider whose answer plays no part in what this test checks —
    /// present only so the turn has something to complete with.
    struct FixedAnswer;
    impl conductor::Completer for FixedAnswer {
        fn id(&self) -> &'static str {
            "p"
        }
        fn complete(
            &mut self,
            _request: &intercept::PendingRequest,
        ) -> Result<conductor::Completion, String> {
            Ok(conductor::Completion {
                text: "hi".to_string(),
                tool_calls: vec![],
                finish_reason: "stop".to_string(),
            })
        }
    }

    /// #84: `event_log::log_user_message` used to record `run_and_persist`'s
    /// own `message` argument — what the caller asked to send — even though a
    /// `before-loop` interceptor may `replace` it before the conductor builds
    /// the request the model actually sees. The log is supposed to be the
    /// record of what happened; on this field it recorded what was asked for.
    /// A resumed session (the projection in `projection.rs`) would then replay
    /// a history the model never had.
    ///
    /// This proves the fix by driving `run_and_persist` (private to this
    /// crate, hence a test here rather than in `core/tests/`) with a
    /// `before-loop` interceptor that rewrites the message, and reading back
    /// the very first row of the session's log.
    #[test]
    fn the_log_holds_the_message_a_before_loop_rewrite_produced() {
        let store = Mutex::new(store::Store::open_in_memory().unwrap());
        let mut dispatcher = intercept::Dispatcher::new(vec![Box::new(RewriteBeforeLoop {
            replacement: "rewritten by the interceptor".to_string(),
        })]);
        let mut providers: Vec<Box<dyn conductor::Completer>> = vec![Box::new(FixedAnswer)];
        let mut tools = conductor::NoTools;
        let mut driver = HeadlessDriver;
        let mut sink = conductor::NoSink;

        run_and_persist(
            &mut dispatcher,
            &mut providers,
            &store,
            conductor::Limits::default(),
            &mut tools,
            &mut driver,
            &mut sink,
            "s",
            "the message the caller actually sent",
        );

        let events = store.lock().unwrap().session_events("s").unwrap();
        let first = event_log::decode_record(&events[0].kind, &events[0].payload)
            .expect("the first row of a fresh session's log decodes");
        assert_eq!(
            first,
            event_log::Record::UserMessage("rewritten by the interceptor".to_string()),
            "the log must hold what the model received, not what the caller asked to send"
        );
    }
}
