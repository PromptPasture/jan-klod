//! The WebAssembly boundary (#181): turns a `config.yaml` into sandboxed extension components.
//! Loads config, wires host capabilities, compiles components and drives lifecycle; no agent
//! behaviour lives here. The only crate that names `wasmtime`.

// Generated Component-Model bindings; lint exemptions scoped to the macro output.
#[allow(missing_docs, clippy::all, clippy::pedantic, clippy::nursery)]
mod bindings;
pub mod egress;
pub mod ext;
pub mod ext_index;
pub mod guest_storage;
mod host;
pub mod host_fs;
pub mod host_process;
pub mod http;
pub mod interceptor_host;
pub mod manifest;
pub mod native_tools;
pub mod registry_host;
pub mod route;
pub mod sandbox;
pub mod sandbox_landlock;
pub mod sandbox_seatbelt;
pub mod tool_host;
mod wasm_cache;

use jk_protocol::turn;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use wasmtime::component::{Component, HasSelf, Linker};
use wasmtime::{Engine, Store};

use bindings::jan_klod::interfaces::{host_config, host_http, host_log, host_secrets};
use bindings::ExtensionWorld;
use jk_config::{Config, ExtensionInstance};

pub use host::{ConfigSection, HostState};

/// The user's home directory, if the environment names one.
fn dirs_home() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .filter(|p| !p.as_os_str().is_empty())
}

/// Whether `cwd` may be adopted as the workspace unprompted: declines `/` and `$HOME`.
/// Guards against an accidental `cd`, not a determined operator.
fn adoptable_workspace(cwd: &Path, home: Option<&Path>) -> bool {
    if cwd.parent().is_none() {
        return false;
    }
    if home.is_some_and(|home| home == cwd) {
        return false;
    }
    true
}

/// Deterministic boot tier for a category. Dependencies boot before dependents:
/// registries, providers, managers that consume them, leaf surfaces (tools, agents, api, chat).
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

/// Whether `category` is instantiated on first use rather than eagerly (#59).
///
/// Providers and interceptors are never lazy: a provider must fail at boot rather than
/// mid-turn, and deferring an interceptor would run turn one without configured policy.
fn is_lazy_category(category: &str) -> bool {
    matches!(category, "tool" | "registry" | "agent")
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
    /// `host-*` interfaces the component imports, sorted. `None` when no component was
    /// loaded; `Some(&[])` means it imports none, which a manifest check treats differently.
    pub capabilities: Option<Vec<String>>,
}

/// The `host-*` interfaces `component` imports, sorted and deduplicated.
///
/// Excludes exports and type-only imports (`llm-types`, `store-types`), matching
/// `scripts/manifests.sh` so the two can be cross-checked.
fn host_capabilities(component: &Component, engine: &Engine) -> Vec<String> {
    let mut found: Vec<String> = component
        .component_type()
        .imports(engine)
        // Import names look like `jan-klod:interfaces/host-fs@0.1.0`.
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
/// Boot and `ext install` each map this to their own refusal.
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
/// Shared by boot and `ext install`: two copies of "consistent" could diverge silently on a
/// security-relevant comparison.
fn inspect(
    component: &Component,
    engine: &Engine,
    path: &Path,
) -> Result<Inspected, manifest::ManifestError> {
    let capabilities = host_capabilities(component, engine);
    let verdict = match manifest::Manifest::beside(path)? {
        Some(declared) => {
            // Contract version first, so a mismatch names both versions instead of failing in the linker.
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

/// The categories this runtime dispatches, and so the only ones a component stem may name (#222).
pub const CATEGORIES: [&str; 4] = ["provider", "interceptor", "registry", "tool"];

/// Split a component stem into its category and the rest: `tool-greet` -> `("tool", "greet")`.
///
/// An undispatched category is an error: the component would install and never be callable.
/// The error is a sentence because both callers ([`Runtime::adopt_installed`] and the install
/// tool) wrap it in their own.
///
/// # Errors
/// A stem with no `-`, or one whose prefix is not in [`CATEGORIES`].
pub fn categorise(stem: &str) -> Result<(&str, &str), String> {
    let (category, kind) = stem.split_once('-').ok_or_else(|| {
        format!("a component stem is `<category>-<kind>`, e.g. `tool-greet`; `{stem}` has no `-`")
    })?;
    if !CATEGORIES.contains(&category) {
        return Err(format!(
            "`{stem}` names the category `{category}`, which this runtime does not \
             dispatch — a component under it would load and never be callable. \
             The categories are: {}",
            CATEGORIES.join(", ")
        ));
    }
    if kind.is_empty() {
        return Err(format!("`{stem}` names no kind after `{category}-`"));
    }
    Ok((category, kind))
}

/// A booted core: engine, capability linker, and every enabled instance resolved against `ext/`.
pub struct Runtime {
    engine: Engine,
    linker: Linker<HostState>,
    extensions: Vec<LoadedExtension>,
    /// Top-level agent-behaviour config (`routing`, `providers`, …), verbatim; always a JSON object.
    agent: serde_json::Value,
    /// Where components live; the host's install tool writes here (#213).
    ext_dir: PathBuf,
    /// Wasmtime compile cache wired into `engine` (#60); kept to report hit/miss stats.
    compile_cache: wasmtime::Cache,
    /// Binary [`sandbox_landlock::LandlockBackend`] re-executes to confine a command;
    /// see [`Self::with_sandbox_wrapper`].
    sandbox_wrapper: Option<PathBuf>,
}

/// Pass 1 output of [`Runtime::instantiate`]: instantiated providers plus compiled but pending
/// tool and registry instances (#59).
struct ProvidersAndTools {
    providers: Vec<Box<dyn jk_protocol::intercept::Completer>>,
    provider_ids: Vec<String>,
    tool_fleet: tool_host::LazyToolFleet,
    registry_fleet: registry_host::LazyRegistryFleet,
}

/// What [`Runtime::instantiate`] hands back: the guests one agent runs on.
pub struct Instantiated {
    /// Every enabled interceptor, in the order the dispatcher consults them.
    pub interceptors: Vec<Box<dyn jk_protocol::intercept::Interceptor>>,
    /// The provider fallback chain, in the order `providers:` configures.
    pub providers: Vec<Box<dyn jk_protocol::intercept::Completer>>,
    /// The tool, registry and host-native fleet, lazily instantiated (#59).
    pub tools: CombinedFleet,
}

/// Compile one instance's component and cross-check what it declares.
///
/// Shared with [`Runtime::boot`] so a component adopted after boot (#214) takes the same path.
fn load_instance(
    engine: &Engine,
    ext_dir: &Path,
    instance: &ExtensionInstance,
    allow_unmanifested: bool,
) -> Result<(LoadState, Option<Vec<String>>), CoreError> {
    let path = ext_dir.join(instance.component_file());
    if !path.exists() {
        return Ok((LoadState::Missing(path), None));
    }
    let component = Component::from_file(engine, &path).map_err(|source| CoreError::Load {
        id: instance.id.clone(),
        path: path.display().to_string(),
        source: source.into(),
    })?;
    // Cross-check against the manifest through the same `inspect` as `ext install`.
    let inspected = inspect(&component, engine, &path).map_err(|source| CoreError::Manifest {
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
        // Needing more than it admits to is mislabelled or lying.
        Verdict::UnderDeclared { interfaces } => {
            return Err(CoreError::Undeclared {
                id: instance.id.clone(),
                component: instance.component_file(),
                interfaces: interfaces.join(", "),
            });
        }
        // No manifest: refused, since an undeclared component cannot be inspected before running.
        // `allow-unmanifested` is the named widening for local development.
        Verdict::NoManifest if !allow_unmanifested => {
            return Err(CoreError::NoManifest {
                id: instance.id.clone(),
                component: instance.component_file(),
            });
        }
        // Consistent, or unmanifested where allowed (a deliberate widening).
        Verdict::Consistent | Verdict::NoManifest => {}
    }
    Ok((LoadState::Compiled(component), Some(inspected.capabilities)))
}

impl Runtime {
    /// Load `config.yaml`, wire host capabilities, and resolve every enabled instance against
    /// `ext_dir`; missing components are recorded so a partial deployment still boots.
    ///
    /// # Errors
    /// Returns [`CoreError::Config`] if the config fails to load,
    /// [`CoreError::Cache`] if the Wasmtime compile cache directory cannot be
    /// prepared, [`CoreError::Linker`] if a host capability cannot be wired, or
    /// [`CoreError::Load`] if a present component fails to compile.
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
        // Wasmtime's built-in compile cache (#60): a second boot skips Cranelift for every guest.
        let (engine, compile_cache) = wasm_cache::build_engine(&config_dir, &agent)?;
        let linker = build_linker(&engine)?;

        // Enabled instances in deterministic dependency order.
        let mut order: Vec<&ExtensionInstance> = config.enabled().collect();
        order.sort_by(|a, b| {
            boot_rank(&a.category)
                .cmp(&boot_rank(&b.category))
                .then_with(|| a.id.cmp(&b.id))
        });

        let ext_dir = ext_dir.as_ref();
        // Top-level, not under `extensions:`, whose keys must all be instance mappings
        // (`ConfigError::CategoryNotMap`).
        let allow_unmanifested = agent
            .get("allow-unmanifested")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false);
        let mut extensions = Vec::with_capacity(order.len());
        for instance in order {
            let (state, capabilities) =
                load_instance(&engine, ext_dir, instance, allow_unmanifested)?;
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
            ext_dir: ext_dir.to_path_buf(),
            compile_cache,
            sandbox_wrapper: None,
        })
    }

    /// Name the binary the Landlock backend re-executes (instead of discovery).
    ///
    /// For tests only: under `cargo test`, `current_exe()` is the test binary, which lacks the
    /// `confine` subcommand ([#124](https://github.com/PromptPasture/jan-klod/issues/124)).
    /// Deliberately not a `config.yaml` key: an operator who could name the wrapper could name
    /// one that confines nothing.
    #[must_use]
    pub fn with_sandbox_wrapper(mut self, wrapper: impl Into<PathBuf>) -> Self {
        self.sandbox_wrapper = Some(wrapper.into());
        self
    }

    /// Wasmtime compile-cache `(hits, misses)` for this boot's `Engine`; a hit skipped Cranelift.
    #[must_use]
    pub fn compile_cache_stats(&self) -> (usize, usize) {
        (
            self.compile_cache.cache_hits(),
            self.compile_cache.cache_misses(),
        )
    }

    /// Where compiled artefacts are cached on disk for this boot.
    #[must_use]
    pub fn compile_cache_dir(&self) -> &Path {
        self.compile_cache.directory()
    }

    /// The resolved extension set, in boot order.
    #[must_use]
    pub fn extensions(&self) -> &[LoadedExtension] {
        &self.extensions
    }

    /// Instantiate every compiled non-lazy component (#59) and run its lifecycle (`init` →
    /// `start`); missing components are skipped. Returns the started ids.
    ///
    /// The boot-plan path: checks that turn-one components (providers, interceptors) work without
    /// instantiating tools. [`Self::start_all_eager`] skips nothing; `verify` uses it.
    ///
    /// # Errors
    /// Returns [`CoreError::Instantiate`] if a component cannot be instantiated,
    /// [`CoreError::Lifecycle`] if a lifecycle call traps, or
    /// [`CoreError::LifecycleRejected`] if an extension refuses to start.
    pub fn start_all(&self) -> Result<Vec<String>, CoreError> {
        self.start_selected(true)
    }

    /// Like [`Self::start_all`], but instantiates every category, lazy ones included;
    /// `verify` needs this to prove a component starts before a real turn finds out.
    ///
    /// # Errors
    /// Returns [`CoreError::Instantiate`] if a component cannot be instantiated,
    /// [`CoreError::Lifecycle`] if a lifecycle call traps, or
    /// [`CoreError::LifecycleRejected`] if an extension refuses to start.
    pub fn start_all_eager(&self) -> Result<Vec<String>, CoreError> {
        self.start_selected(false)
    }

    /// Shared body of [`Self::start_all`]/[`Self::start_all_eager`]. Richer-world categories use
    /// the same seams and default-deny substrates as [`Self::instantiate`].
    fn start_selected(&self, skip_lazy: bool) -> Result<Vec<String>, CoreError> {
        let mut started = Vec::new();
        let workspace = self.open_workspace();
        let process = self.open_process_runner(workspace.as_ref());
        for ext in &self.extensions {
            let LoadState::Compiled(component) = &ext.state else {
                continue;
            };
            if skip_lazy && is_lazy_category(&ext.instance.category) {
                continue;
            }
            started.push(self.start_one(ext, component, workspace.as_ref(), &process)?);
        }
        Ok(started)
    }

    /// Instantiate and start one compiled extension, returning its id. `tool-*`, `registry-*` and
    /// `interceptor-*` use their own seams (as [`Self::instantiate`]); the rest use the shared linker.
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
                    self.open_process_runner(workspace),
                )?;
            }
            ("interceptor", _) => {
                // Constant classifier: this path must not make a model call; `instantiate` supplies the real one.
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

    /// Instantiate through the shared linker and run lifecycle; for categories not handled in
    /// [`Self::start_one`].
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

    /// Top-level agent-behaviour config (`routing`, `providers`, …), verbatim; a JSON object.
    #[must_use]
    pub const fn agent_config(&self) -> &serde_json::Value {
        &self.agent
    }

    /// Every loaded instance's id.
    #[must_use]
    pub fn extension_ids(&self) -> Vec<String> {
        self.extensions
            .iter()
            .map(|ext| ext.instance.id.clone())
            .collect()
    }

    /// Adopt a component installed after boot so the next [`Self::instantiate`] can serve it (#214).
    ///
    /// `stem` is the file stem: `tool-greet` becomes instance `tool.greet`. It goes through
    /// [`load_instance`], the boot path, so it gets the same manifest refusals.
    ///
    /// It does not write `config.yaml`: installing enables the component for this process only,
    /// since the approval was for one install, not every boot. Running agents are untouched;
    /// the caller rebuilds at a turn boundary.
    ///
    /// # Errors
    /// [`CoreError`] when the component is absent, is not a component, or fails the manifest
    /// cross-check; `self` is unchanged on failure.
    pub fn adopt_installed(&mut self, stem: &str) -> Result<String, CoreError> {
        let (category, kind) = categorise(stem).map_err(|reason| CoreError::Adopt {
            stem: stem.to_owned(),
            reason,
        })?;
        let id = format!("{category}.{kind}");
        if self.extensions.iter().any(|ext| ext.instance.id == id) {
            return Err(CoreError::Adopt {
                stem: stem.to_owned(),
                reason: format!("`{id}` is already loaded; nothing to adopt"),
            });
        }
        let instance = ExtensionInstance {
            id: id.clone(),
            category: category.to_owned(),
            name: kind.to_owned(),
            kind: kind.to_owned(),
            component: stem.to_owned(),
            enabled: true,
            config: serde_json::Value::Object(serde_json::Map::new()),
        };
        let allow_unmanifested = self
            .agent
            .get("allow-unmanifested")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false);
        // Load before recording: a half-adopted runtime is worse than a refused adoption.
        let (state, capabilities) =
            load_instance(&self.engine, &self.ext_dir, &instance, allow_unmanifested)?;
        if let LoadState::Missing(path) = &state {
            return Err(CoreError::Adopt {
                stem: stem.to_owned(),
                reason: format!("nothing at {}", path.display()),
            });
        }
        self.extensions.push(LoadedExtension {
            instance,
            state,
            capabilities,
        });
        Ok(id)
    }

    /// Instantiate the guests one agent runs on: enabled interceptors, providers (as the
    /// configured fallback chain), and tools and registries as a lazy fleet.
    ///
    /// `http_factory` mints a `host-http` backend per provider; `storage` is what `host-storage`
    /// writes to, for instances that opt in with `persist: true`. Both are factories because this
    /// crate holds no session store (#181).
    ///
    /// # Errors
    /// Returns a [`CoreError`] if any interceptor or provider fails to instantiate
    /// or start.
    pub fn instantiate(
        &self,
        http_factory: &dyn Fn() -> route::HttpFn,
        storage: &dyn Fn() -> Arc<dyn guest_storage::Entries>,
    ) -> Result<Instantiated, CoreError> {
        // Shared, default-deny substrates for tools (opt-in via config).
        let workspace = self.open_workspace();
        let project_instructions = Self::project_instructions(workspace.as_ref());
        let process = self.open_process_runner(workspace.as_ref());

        // Pass 1: providers (eager) and pending tools/registries, before interceptors so
        // tool-selector sees combined advertised metadata.
        let ProvidersAndTools {
            providers,
            provider_ids,
            tool_fleet,
            registry_fleet,
        } = self.instantiate_providers_and_tools(
            http_factory,
            workspace.as_ref(),
            &process,
            storage,
        )?;
        let mut tools = CombinedFleet {
            tools: tool_fleet,
            registry: registry_fleet,
            native: self.native_tools(),
        };
        // Only if an interceptor is enabled: resolving instantiates every pending tool/registry.
        // Counted from the resolved instances, not by asking what an interceptor wants: core holds
        // mechanism, not policy (#59).
        let any_interceptor_enabled = self.extensions.iter().any(|ext| {
            ext.instance.category == "interceptor" && matches!(ext.state, LoadState::Compiled(_))
        });
        let tools_advert = if any_interceptor_enabled {
            tools.all_metas_json()?
        } else {
            serde_json::Value::Array(Vec::new())
        };

        // An interceptor consulting a model gets its own provider instance: the conductor holds
        // the chain mutably for the whole turn, so sharing would alias it.
        let classifier = self.open_classifier(http_factory)?;

        // Pass 2: interceptors, each served the tool set at `select-tools`.
        let interceptors = self.instantiate_interceptors(
            &tools_advert,
            project_instructions.as_deref(),
            classifier.as_ref(),
            storage,
        )?;

        // Order by the `providers:` config, not boot order, so the documented top-to-bottom list applies.
        let ordered = order_chain(self.agent.get("providers"), &provider_ids);
        let providers = reorder(providers, &ordered);

        Ok(Instantiated {
            interceptors,
            providers,
            tools,
        })
    }

    /// Pass 1 of [`Self::instantiate`]: instantiate enabled providers (eager) and register
    /// tools and registries as pending (#59).
    ///
    /// Host-native tools are off unless `registry.install-tool` says otherwise; it lives in the
    /// same `registry:` block as `trusted-keys`.
    fn native_tools(&self) -> native_tools::NativeTools {
        let registry = self.agent.get("registry");
        let enabled = registry
            .and_then(|r| r.get("install-tool"))
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false);
        if !enabled {
            return native_tools::NativeTools::disabled();
        }
        let trusted_keys = registry
            .and_then(|r| r.get("trusted-keys"))
            .and_then(serde_json::Value::as_array)
            .map(|keys| {
                keys.iter()
                    .filter_map(|k| k.as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default();
        // The index `ext search` reads; `None` makes it say no registry is configured rather than
        // report empty matches.
        let index_source = registry
            .and_then(|r| r.get("url"))
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned);
        native_tools::NativeTools::installing_into(self.ext_dir.clone(), trusted_keys, index_source)
    }

    fn instantiate_providers_and_tools(
        &self,
        http_factory: &dyn Fn() -> route::HttpFn,
        workspace: Option<&host_fs::Workspace>,
        process: &host_process::ProcessRunner,
        storage: &dyn Fn() -> Arc<dyn guest_storage::Entries>,
    ) -> Result<ProvidersAndTools, CoreError> {
        let mut providers: Vec<Box<dyn jk_protocol::intercept::Completer>> = Vec::new();
        // Instance ids parallel to `providers`, to match the chain by name without downcasting.
        let mut provider_ids: Vec<String> = Vec::new();
        let mut tool_fleet = tool_host::LazyToolFleet::new(self.engine.clone());
        let mut registry_fleet = registry_host::LazyRegistryFleet::new(self.engine.clone());

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
                    // Egress is per instance, never default (like `host-fs` needing a workspace).
                    // Resolved now: the factory borrows this call's stack frame.
                    let network = ext
                        .instance
                        .config
                        .get("network")
                        .and_then(serde_json::Value::as_bool)
                        .unwrap_or(false);
                    // Durability is opt-in per instance; see `guest_storage::GuestStorage`.
                    let persist = ext
                        .instance
                        .config
                        .get("persist")
                        .and_then(serde_json::Value::as_bool)
                        .unwrap_or(false);
                    // `scope: session` also keys namespaces by session (#215); the default is
                    // run-scoped, which a permission gate's standing grants need.
                    let per_session = ext
                        .instance
                        .config
                        .get("scope")
                        .and_then(serde_json::Value::as_str)
                        == Some("session");
                    tool_fleet.push(
                        &ext.instance.id,
                        component.clone(),
                        workspace.cloned(),
                        process.clone(),
                        network.then(|| http_factory()),
                        persist.then(storage),
                        per_session,
                    );
                }
                "registry" if ext.instance.kind == "skills" => {
                    registry_fleet.push_skills(
                        &ext.instance.id,
                        component.clone(),
                        config_json,
                        workspace.cloned(),
                    );
                }
                "registry" if ext.instance.kind == "mcp" => {
                    registry_fleet.push_mcp(
                        &ext.instance.id,
                        component.clone(),
                        config_json,
                        self.egress_policy(),
                        process.clone(),
                    );
                }
                _ => {}
            }
        }
        Ok(ProvidersAndTools {
            providers,
            provider_ids,
            tool_fleet,
            registry_fleet,
        })
    }

    /// Pass 2 of [`Self::instantiate`]: instantiate every enabled interceptor,
    /// each served the tool set from pass 1 at `select-tools`.
    fn instantiate_interceptors(
        &self,
        tools_advert: &serde_json::Value,
        project_instructions: Option<&str>,
        classifier: Option<&std::sync::Arc<std::sync::Mutex<route::ProviderCompleter>>>,
        storage: &dyn Fn() -> Arc<dyn guest_storage::Entries>,
    ) -> Result<Vec<Box<dyn jk_protocol::intercept::Interceptor>>, CoreError> {
        let mut interceptors: Vec<Box<dyn jk_protocol::intercept::Interceptor>> = Vec::new();
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
                // The host does the permitted reading; the guest receives data, not a capability.
                if let Some(project) = project_instructions {
                    map.entry("project-instructions")
                        .or_insert_with(|| serde_json::Value::String(project.to_string()));
                }
            }
            // Durability opt-in, off by default: `interceptor-permission` grants are run-scoped,
            // and a session store would make them survive restart.
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
                    persist.then(storage),
                )?,
            ));
        }
        Ok(interceptors)
    }

    /// The project's `AGENTS.md` from the workspace root, if any, read host-side so interceptors
    /// need no `host-fs`. Root only (ancestors would leak another project's instructions);
    /// truncated at a cap since a system prompt is paid for every turn.
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

    /// Destinations guests may reach: every enabled instance's `base-url`/`endpoint`/`url` (even
    /// loopback), otherwise public only. Closed over by the HTTP backend, so guests cannot widen it.
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
        // Escape hatch for anything config doesn't name (sidecar, proxy), one origin at a time.
        if let Some(list) = self.agent.get("network").and_then(|n| n.get("allow")) {
            for url in list.as_array().into_iter().flatten() {
                if let Some(url) = url.as_str() {
                    policy = policy.allowing(url);
                }
            }
        }
        policy
    }

    /// Open the host-side workspace for `host-fs` from `workspace:` (else `$PWD`); `None` is default-deny.
    fn open_workspace(&self) -> Option<host_fs::Workspace> {
        let root_owned;
        let root: &str = if let Some(r) = self
            .agent
            .get("workspace")
            .and_then(serde_json::Value::as_str)
        {
            // Explicit means explicit, even for a root this would not adopt itself.
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
            // The grant is implicit; the notice should not be.
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

    /// Build the `host-process` runner from `execution:` config. Disabled unless enabled *and* a
    /// workspace is configured (cwd is jailed to it), or when `sandbox:` is unreadable.
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
                // Named env vars are a one-at-a-time grant; children otherwise get `host_process::BASE_ENV`.
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
                // Long-lived children the operator named: a narrower grant than `enabled`, so a guest's
                // string never becomes a program (#109). Entries missing `name`/`command` are skipped:
                // a half-written grant reads as none.
                let long_lived: Vec<host_process::LongLived> = exec
                    .and_then(|e| e.get("long-lived"))
                    .and_then(serde_json::Value::as_array)
                    .map(|entries| {
                        entries
                            .iter()
                            .filter_map(|entry| {
                                let name = entry.get("name")?.as_str()?.to_owned();
                                let command = entry.get("command")?.as_str()?.to_owned();
                                let args = entry
                                    .get("args")
                                    .and_then(serde_json::Value::as_array)
                                    .map(|items| {
                                        items
                                            .iter()
                                            .filter_map(|a| a.as_str().map(str::to_owned))
                                            .collect()
                                    })
                                    .unwrap_or_default();
                                Some(host_process::LongLived {
                                    name,
                                    command,
                                    args,
                                })
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                // What a command may do once running. An unreadable policy denies execution
                // rather than running unconfined.
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
                // One `host_backend()` for report and wiring, so the boot line cannot describe
                // confinement the runner lacks. `with_sandbox_wrapper` overrides only which binary
                // Landlock re-executes; an unresolved override leaves `backend` `None` and downgrades the mode.
                let backend = self
                    .sandbox_wrapper
                    .as_ref()
                    .map_or_else(sandbox::host_backend, |wrapper| {
                        sandbox::host_backend_at(wrapper)
                    });
                let effective = policy.resolve(backend.as_deref());
                // `require: true` asked for no command, not an unconfined one: refusal is the config working.
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
                .with_env_passthrough(passthrough)
                .with_long_lived(long_lived);
                // Same pair for mode and confinement, so `Os` is never reported while unconfined.
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

    /// Instantiate the provider that answers interceptors' `llm-provider` calls: the instance
    /// named by `classifier:`, else the head of the chain (so a small local model can classify).
    /// `None` when no provider is enabled; callers use the conservative default.
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

    /// An interceptor's `host-config` section: its own config plus top-level agent keys
    /// (`routing`, `providers`); an instance's own key wins.
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

/// Combined tool + registry fleet implementing [`jk_protocol::intercept::ToolInvoker`]:
/// dispatches to `tool-*` first, then registries. Both are lazy (#59).
pub struct CombinedFleet {
    tools: tool_host::LazyToolFleet,
    registry: registry_host::LazyRegistryFleet,
    /// The host's own tools (#213), last so they never shadow an installed extension.
    native: native_tools::NativeTools,
}

impl CombinedFleet {
    /// Stems installed during turns run on this fleet, drained.
    #[must_use]
    pub fn take_installed(&self) -> Vec<String> {
        self.native.take_installed()
    }
}

impl jk_protocol::intercept::ToolInvoker for CombinedFleet {
    fn invoke(&mut self, call: &turn::ToolCall) -> Option<jk_protocol::intercept::ToolInvocation> {
        self.tools
            .invoke(call)
            .or_else(|| self.registry.invoke(call))
            .or_else(|| self.native.invoke(call))
    }
    fn invoke_asking(
        &mut self,
        call: &turn::ToolCall,
        driver: &mut dyn turn::Driver,
    ) -> Option<jk_protocol::intercept::ToolInvocation> {
        // Only tools may ask; order matches `invoke` so a name resolves the same either way.
        self.tools
            .invoke_asking(call, driver)
            .or_else(|| self.registry.invoke(call))
            .or_else(|| self.native.invoke(call))
    }

    fn bind_session(&mut self, session: &str) {
        self.tools.bind_session(session);
        self.registry.bind_session(session);
    }
}

impl CombinedFleet {
    /// Whether the tool fleet has instantiated its pending guests yet (a test hook onto #59).
    #[must_use]
    pub const fn tools_instantiated(&self) -> bool {
        self.tools.is_instantiated()
    }

    /// The advertised tool names. A lazy instantiation failure is reported on stderr and
    /// degrades to no tools.
    pub fn tool_names(&mut self) -> Vec<String> {
        self.tools.tool_names().unwrap_or_else(|err| {
            eprintln!("WARN [core] tool fleet failed to instantiate: {err}");
            Vec::new()
        })
    }

    /// # Errors
    /// instantiate or start.
    pub fn all_metas_json(&mut self) -> Result<serde_json::Value, CoreError> {
        let mut metas: Vec<serde_json::Value> = self
            .tools
            .metas()?
            .into_iter()
            .map(|m| {
                serde_json::json!({
                    "name": m.name,
                    "description": m.description,
                    "parameters-schema": m.arguments_schema,
                })
            })
            .collect();
        for (name, description, schema) in self.registry.all_metas()? {
            metas.push(serde_json::json!({
                "name": name,
                "description": description,
                "parameters-schema": schema,
            }));
        }
        metas.extend(self.native.metas());
        Ok(serde_json::Value::Array(metas))
    }
}

/// The closure interceptors' `llm-provider` resolves to. Any failure defaults to `"agentic"`,
/// the conservative label that routes through the full loop. Shared by `Arc` since the closure
/// outlives the call that builds it.
fn classifier_fn(
    classifier: Option<std::sync::Arc<std::sync::Mutex<route::ProviderCompleter>>>,
) -> interceptor_host::ProviderFn {
    use jk_protocol::intercept::Completer;
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

/// Resolve the configured fallback chain (`providers:` list) into indices into `ids`, in try order.
/// Entries naming a disabled provider are skipped with a warning; enabled providers the list
/// omits run last.
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

/// Capability linker every extension store shares: WASI plus `host-log`/`host-config`/`host-http`/`host-secrets`.
fn build_linker(engine: &Engine) -> Result<Linker<HostState>, CoreError> {
    let mut linker: Linker<HostState> = Linker::new(engine);
    wasmtime_wasi::p2::add_to_linker_sync(&mut linker).map_err(CoreError::linker)?;
    host_log::add_to_linker::<_, HasSelf<_>>(&mut linker, |s| s).map_err(CoreError::linker)?;
    host_config::add_to_linker::<_, HasSelf<_>>(&mut linker, |s| s).map_err(CoreError::linker)?;
    host_http::add_to_linker::<_, HasSelf<_>>(&mut linker, |s| s).map_err(CoreError::linker)?;
    host_secrets::add_to_linker::<_, HasSelf<_>>(&mut linker, |s| s).map_err(CoreError::linker)?;
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
                    // Name the lazy category so an operator can tell it from eagerly started ones;
                    // a static fact (`is_lazy_category`), not live state.
                    let note = if is_lazy_category(&ext.instance.category) {
                        "  (lazy: instantiated on first use, not at boot)"
                    } else {
                        ""
                    };
                    writeln!(f, "  loaded   {:<22} -> {file}{note}", ext.instance.id)?;
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
        writeln!(f, "{compiled} loaded, {} missing", exts.len() - compiled)?;
        // Proof the compile cache works, from Wasmtime's counters: a second boot against the same
        // `storage.cache-dir` reports one hit per compiled component.
        let (hits, misses) = self.0.compile_cache_stats();
        write!(
            f,
            "wasmtime compile cache: {hits} hit(s), {misses} miss(es) (dir: {})",
            self.0.compile_cache_dir().display()
        )
    }
}

/// Errors surfaced while booting the core.
#[derive(Debug, thiserror::Error)]
pub enum CoreError {
    /// Loading or parsing `config.yaml` failed.
    #[error(transparent)]
    Config(#[from] jk_config::ConfigError),
    /// A component installed after boot could not be adopted (#214).
    #[error("cannot adopt `{stem}`: {reason}")]
    Adopt {
        /// The component stem the caller named.
        stem: String,
        /// Why it was refused, in terms the caller can act on.
        reason: String,
    },
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
    /// A component's manifest could not be read; unreadable or malformed is refused, not treated as absent.
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
    /// Wasmtime's compile cache could not be set up at `storage.cache-dir` (or
    /// its default, beside `config.yaml`).
    #[error("setting up the wasmtime compile cache at {path}: {message}")]
    Cache {
        /// The cache directory that could not be prepared.
        path: String,
        /// What went wrong — an I/O error creating/chmod-ing the directory,
        /// or Wasmtime refusing the cache configuration or engine.
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

    /// A stem naming an undispatched category is refused, and the refusal lists the real ones (#222).
    #[test]
    fn a_stem_naming_no_category_is_refused_and_the_refusal_names_the_ones_that_exist() {
        let err = categorise("self-built").expect_err("`self` is not a category");
        for category in CATEGORIES {
            assert!(
                err.contains(category),
                "the refusal must name `{category}`, or the agent cannot correct itself: {err}"
            );
        }
        assert!(err.contains("self-built"), "and name what was asked: {err}");
    }

    /// Control: dispatched categories still split.
    #[test]
    fn every_dispatched_category_splits_from_a_stem() {
        for category in CATEGORIES {
            let stem = format!("{category}-thing");
            assert_eq!(
                categorise(&stem),
                Ok((category, "thing")),
                "`{stem}` must categorise"
            );
        }
    }

    /// The two shapes that are not a category error but are still not a
    /// stem: nothing to split on, and nothing after the category.
    #[test]
    fn a_stem_needs_both_halves() {
        assert!(categorise("toolgreet").is_err(), "no separator");
        assert!(categorise("tool-").is_err(), "no kind");
    }

    /// `Runtime` is shareable across threads so each session can build its own agent (#228).
    /// Compile-time check: an agent is `!Send`, so only the builder can cross threads.
    #[test]
    fn a_runtime_can_be_shared_across_threads() {
        const fn needs<T: Send + Sync>() {}
        needs::<Runtime>();
    }

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
        assert_eq!(runtime.start_all().unwrap(), [] as [std::string::String; 0]);

        std::fs::remove_dir_all(&dir).ok();
    }
}
