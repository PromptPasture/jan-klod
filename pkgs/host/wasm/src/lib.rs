//! The WebAssembly boundary: boots `config.yaml` into sandboxed extension components.

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

fn dirs_home() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .filter(|p| !p.as_os_str().is_empty())
}

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

/// Boot tier for a category: dependencies boot before dependents.
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

/// Providers and interceptors are never lazy: they must fail at boot, not mid-turn.
fn is_lazy_category(category: &str) -> bool {
    matches!(category, "tool" | "registry" | "agent")
}

/// Result of resolving an instance to a component on disk.
pub enum LoadState {
    /// Found and compiled.
    Compiled(Component),
    /// Not present yet; recorded, not fatal.
    Missing(PathBuf),
}

/// One enabled instance after boot resolution.
pub struct LoadedExtension {
    /// The resolved config instance.
    pub instance: ExtensionInstance,
    /// Compiled or missing.
    pub state: LoadState,
    /// Sorted `host-*` imports; `None` if not loaded, which a manifest check treats differently from empty.
    pub capabilities: Option<Vec<String>>,
}

/// Excludes type-only imports, matching `scripts/manifests.sh` so the two can be cross-checked.
fn host_capabilities(component: &Component, engine: &Engine) -> Vec<String> {
    let mut found: Vec<String> = component
        .component_type()
        .imports(engine)
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Verdict {
    Consistent,
    NoManifest,
    ApiMismatch { theirs: String },
    UnderDeclared { interfaces: Vec<String> },
}

pub(crate) struct Inspected {
    pub capabilities: Vec<String>,
    pub verdict: Verdict,
}

/// Shared by boot and `ext install` so "consistent" cannot diverge between them.
fn inspect(
    component: &Component,
    engine: &Engine,
    path: &Path,
) -> Result<Inspected, manifest::ManifestError> {
    let capabilities = host_capabilities(component, engine);
    let verdict = match manifest::Manifest::beside(path)? {
        Some(declared) => {
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

/// Categories this runtime dispatches; the only ones a component stem may name.
pub const CATEGORIES: [&str; 4] = ["provider", "interceptor", "registry", "tool"];

/// Split a component stem into category and kind: `tool-greet` -> `("tool", "greet")`.
///
/// # Errors
/// A stem with no `-`, an empty kind, or a prefix not in [`CATEGORIES`].
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

/// A booted core: engine, capability linker and resolved instances.
pub struct Runtime {
    engine: Engine,
    linker: Linker<HostState>,
    extensions: Vec<LoadedExtension>,
    /// Top-level agent config, verbatim; always a JSON object.
    agent: serde_json::Value,
    ext_dir: PathBuf,
    compile_cache: wasmtime::Cache,
    sandbox_wrapper: Option<PathBuf>,
}

struct ProvidersAndTools {
    providers: Vec<Box<dyn jk_protocol::intercept::Completer>>,
    provider_ids: Vec<String>,
    tool_fleet: tool_host::LazyToolFleet,
    registry_fleet: registry_host::LazyRegistryFleet,
}

/// The guests one agent runs on.
pub struct Instantiated {
    /// Enabled interceptors, in dispatch order.
    pub interceptors: Vec<Box<dyn jk_protocol::intercept::Interceptor>>,
    /// Provider fallback chain, in configured order.
    pub providers: Vec<Box<dyn jk_protocol::intercept::Completer>>,
    /// Tool, registry and native fleet, lazily instantiated.
    pub tools: CombinedFleet,
}

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
        Verdict::UnderDeclared { interfaces } => {
            return Err(CoreError::Undeclared {
                id: instance.id.clone(),
                component: instance.component_file(),
                interfaces: interfaces.join(", "),
            });
        }
        Verdict::NoManifest if !allow_unmanifested => {
            return Err(CoreError::NoManifest {
                id: instance.id.clone(),
                component: instance.component_file(),
            });
        }
        Verdict::Consistent | Verdict::NoManifest => {}
    }
    Ok((LoadState::Compiled(component), Some(inspected.capabilities)))
}

impl Runtime {
    /// Load `config.yaml` and resolve enabled instances against `ext_dir`; missing components are recorded.
    ///
    /// # Errors
    /// [`CoreError`] if config, compile cache, linker wiring or component compilation fails.
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
        let (engine, compile_cache) = wasm_cache::build_engine(&config_dir, &agent)?;
        let linker = build_linker(&engine)?;

        let mut order: Vec<&ExtensionInstance> = config.enabled().collect();
        order.sort_by(|a, b| {
            boot_rank(&a.category)
                .cmp(&boot_rank(&b.category))
                .then_with(|| a.id.cmp(&b.id))
        });

        let ext_dir = ext_dir.as_ref();
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

    /// Test-only: name the Landlock re-exec binary; deliberately not a `config.yaml` key.
    #[must_use]
    pub fn with_sandbox_wrapper(mut self, wrapper: impl Into<PathBuf>) -> Self {
        self.sandbox_wrapper = Some(wrapper.into());
        self
    }

    /// Wasmtime compile-cache `(hits, misses)`; a hit skipped Cranelift.
    #[must_use]
    pub fn compile_cache_stats(&self) -> (usize, usize) {
        (
            self.compile_cache.cache_hits(),
            self.compile_cache.cache_misses(),
        )
    }

    /// Where compiled artefacts are cached on disk.
    #[must_use]
    pub fn compile_cache_dir(&self) -> &Path {
        self.compile_cache.directory()
    }

    /// The resolved extension set, in boot order.
    #[must_use]
    pub fn extensions(&self) -> &[LoadedExtension] {
        &self.extensions
    }

    /// Instantiate and start every compiled non-lazy component; returns the started ids.
    ///
    /// # Errors
    /// [`CoreError`] if a component fails to instantiate, traps or refuses to start.
    pub fn start_all(&self) -> Result<Vec<String>, CoreError> {
        self.start_selected(true)
    }

    /// Like [`Self::start_all`] but includes lazy categories, which `verify` needs.
    ///
    /// # Errors
    /// [`CoreError`] if a component fails to instantiate, traps or refuses to start.
    pub fn start_all_eager(&self) -> Result<Vec<String>, CoreError> {
        self.start_selected(false)
    }

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

    /// A human-readable boot plan.
    #[must_use]
    pub const fn report(&self) -> BootReport<'_> {
        BootReport(self)
    }

    /// Top-level agent config, verbatim; a JSON object.
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

    /// Adopt a post-boot install (`tool-greet` becomes `tool.greet`); not persisted to `config.yaml`.
    ///
    /// # Errors
    /// [`CoreError`] if absent, not a component or failing the manifest check; `self` is unchanged.
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

    /// Instantiate enabled interceptors, providers (fallback chain) and a lazy tool/registry fleet.
    ///
    /// # Errors
    /// [`CoreError`] if an interceptor or provider fails to instantiate or start.
    pub fn instantiate(
        &self,
        http_factory: &dyn Fn() -> route::HttpFn,
        storage: &dyn Fn() -> Arc<dyn guest_storage::Entries>,
    ) -> Result<Instantiated, CoreError> {
        let workspace = self.open_workspace();
        let project_instructions = Self::project_instructions(workspace.as_ref());
        let process = self.open_process_runner(workspace.as_ref());

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
        let any_interceptor_enabled = self.extensions.iter().any(|ext| {
            ext.instance.category == "interceptor" && matches!(ext.state, LoadState::Compiled(_))
        });
        let tools_advert = if any_interceptor_enabled {
            tools.all_metas_json()?
        } else {
            serde_json::Value::Array(Vec::new())
        };

        let classifier = self.open_classifier(http_factory)?;

        let interceptors = self.instantiate_interceptors(
            &tools_advert,
            project_instructions.as_deref(),
            classifier.as_ref(),
            storage,
        )?;

        let ordered = order_chain(self.agent.get("providers"), &provider_ids);
        let providers = reorder(providers, &ordered);

        Ok(Instantiated {
            interceptors,
            providers,
            tools,
        })
    }

    /// Host-native tools are off unless `registry.install-tool` is true.
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
                    let network = ext
                        .instance
                        .config
                        .get("network")
                        .and_then(serde_json::Value::as_bool)
                        .unwrap_or(false);
                    let persist = ext
                        .instance
                        .config
                        .get("persist")
                        .and_then(serde_json::Value::as_bool)
                        .unwrap_or(false);
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
                if let Some(project) = project_instructions {
                    map.entry("project-instructions")
                        .or_insert_with(|| serde_json::Value::String(project.to_string()));
                }
            }
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

    /// Root `AGENTS.md` only (ancestors would leak another project's); truncated as it is sent every turn.
    fn project_instructions(workspace: Option<&host_fs::Workspace>) -> Option<String> {
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

    /// Destinations guests may reach: configured instance URLs (even loopback), else public only.
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
        if let Some(list) = self.agent.get("network").and_then(|n| n.get("allow")) {
            for url in list.as_array().into_iter().flatten() {
                if let Some(url) = url.as_str() {
                    policy = policy.allowing(url);
                }
            }
        }
        policy
    }

    fn open_workspace(&self) -> Option<host_fs::Workspace> {
        let root_owned;
        let root: &str = if let Some(r) = self
            .agent
            .get("workspace")
            .and_then(serde_json::Value::as_str)
        {
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
                let backend = self
                    .sandbox_wrapper
                    .as_ref()
                    .map_or_else(sandbox::host_backend, |wrapper| {
                        sandbox::host_backend_at(wrapper)
                    });
                let effective = policy.resolve(backend.as_deref());
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

    /// The `classifier:` provider, else the head of the chain; `None` when no provider is enabled.
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

    /// An instance's own key wins over top-level agent keys.
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

/// Dispatches to `tool-*`, then registries, then host-native tools.
pub struct CombinedFleet {
    tools: tool_host::LazyToolFleet,
    registry: registry_host::LazyRegistryFleet,
    /// Consulted last so native tools never shadow an installed extension.
    native: native_tools::NativeTools,
}

impl CombinedFleet {
    /// Stems installed during turns on this fleet, drained.
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
    /// Whether the lazy tool fleet has instantiated its guests yet.
    #[must_use]
    pub const fn tools_instantiated(&self) -> bool {
        self.tools.is_instantiated()
    }

    /// The advertised tool names; an instantiation failure is logged and yields none.
    pub fn tool_names(&mut self) -> Vec<String> {
        self.tools.tool_names().unwrap_or_else(|err| {
            eprintln!("WARN [core] tool fleet failed to instantiate: {err}");
            Vec::new()
        })
    }

    /// Advertised tool metas as JSON.
    ///
    /// # Errors
    /// [`CoreError`] if a lazy guest fails to instantiate or start.
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

/// Any failure defaults to `"agentic"`, the conservative label that routes through the full loop.
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

/// Entries naming a disabled provider are skipped with a warning; unlisted providers run last.
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

fn reorder<T>(items: Vec<T>, order: &[usize]) -> Vec<T> {
    let mut slots: Vec<Option<T>> = items.into_iter().map(Some).collect();
    order
        .iter()
        .filter_map(|index| slots.get_mut(*index).and_then(Option::take))
        .collect()
}

fn build_linker(engine: &Engine) -> Result<Linker<HostState>, CoreError> {
    let mut linker: Linker<HostState> = Linker::new(engine);
    wasmtime_wasi::p2::add_to_linker_sync(&mut linker).map_err(CoreError::linker)?;
    host_log::add_to_linker::<_, HasSelf<_>>(&mut linker, |s| s).map_err(CoreError::linker)?;
    host_config::add_to_linker::<_, HasSelf<_>>(&mut linker, |s| s).map_err(CoreError::linker)?;
    host_http::add_to_linker::<_, HasSelf<_>>(&mut linker, |s| s).map_err(CoreError::linker)?;
    host_secrets::add_to_linker::<_, HasSelf<_>>(&mut linker, |s| s).map_err(CoreError::linker)?;
    Ok(linker)
}

/// Renders the boot plan.
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
    /// A component installed after boot could not be adopted.
    #[error("cannot adopt `{stem}`: {reason}")]
    Adopt {
        /// Component stem the caller named.
        stem: String,
        /// Why it was refused.
        reason: String,
    },
    /// Wiring a host capability into the linker failed.
    #[error("wiring host capabilities into the linker")]
    Linker {
        /// Underlying linker error.
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },
    /// Compiling a component from disk failed.
    #[error("loading component for {id} from {path}")]
    Load {
        /// Instance id.
        id: String,
        /// Component path.
        path: String,
        /// Compilation error.
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },
    /// Manifest unreadable or malformed; refused, not treated as absent.
    #[error("{id}: its manifest cannot be read")]
    Manifest {
        /// Instance id.
        id: String,
        /// Read or parse error.
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
        /// Instance id.
        id: String,
        /// Component file, which also names its manifest.
        component: String,
        /// Undeclared interfaces, comma-separated.
        interfaces: String,
    },
    /// Component built against an incompatible interface package.
    #[error(
        "{id}: `{component}` was built against jan-klod:interfaces@{theirs}, and this \
         build speaks {ours}. Rebuild the component against this host's `wit/`"
    )]
    ApiVersion {
        /// Instance id.
        id: String,
        /// Component file.
        component: String,
        /// Version the component declares.
        theirs: String,
        /// Version this host speaks.
        ours: String,
    },
    /// Component ships no manifest and none is permitted.
    #[error(
        "{id}: `{component}` has no manifest beside it. Run `make ext` to generate \
         one, or set top-level `allow-unmanifested: true` to load components that \
         declare nothing"
    )]
    NoManifest {
        /// Instance id.
        id: String,
        /// Component file whose manifest is absent.
        component: String,
    },
    /// Instantiating a compiled component failed.
    #[error("instantiating {id}")]
    Instantiate {
        /// Instance id.
        id: String,
        /// Underlying instantiation error.
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },
    /// A lifecycle call trapped.
    #[error("{id}: lifecycle `{phase}` trapped")]
    Lifecycle {
        /// Instance id.
        id: String,
        /// `init` or `start`.
        phase: &'static str,
        /// Underlying trap.
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },
    /// The persistent store could not be opened.
    #[error("opening the persistent store: {message}")]
    Store {
        /// Underlying error, stringified.
        message: String,
    },
    /// Wasmtime's compile cache could not be set up.
    #[error("setting up the wasmtime compile cache at {path}: {message}")]
    Cache {
        /// Cache directory.
        path: String,
        /// What went wrong.
        message: String,
    },
    /// A lifecycle call returned an error: the extension refused to load.
    #[error("{id}: lifecycle `{phase}` failed: {message}")]
    LifecycleRejected {
        /// Instance id.
        id: String,
        /// `init` or `start`.
        phase: &'static str,
        /// Message the extension returned.
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

    #[test]
    fn a_stem_needs_both_halves() {
        assert!(categorise("toolgreet").is_err(), "no separator");
        assert!(categorise("tool-").is_err(), "no kind");
    }

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
        let ids = ids(&["anthropic", "openai"]);
        let chain = serde_json::json!([{ "provider": "openai" }, { "provider": "anthropic" }]);
        assert_eq!(order_chain(Some(&chain), &ids), vec![1, 0]);
    }

    #[test]
    fn no_chain_keeps_boot_order() {
        let ids = ids(&["anthropic", "openai"]);
        assert_eq!(order_chain(None, &ids), vec![0, 1]);
        assert_eq!(
            order_chain(Some(&serde_json::json!("nonsense")), &ids),
            vec![0, 1]
        );
    }

    #[test]
    fn an_enabled_provider_the_chain_omits_still_runs_last() {
        let ids = ids(&["anthropic", "openai"]);
        let chain = serde_json::json!([{ "provider": "openai" }]);
        assert_eq!(order_chain(Some(&chain), &ids), vec![1, 0]);
    }

    #[test]
    fn a_named_provider_that_is_not_enabled_is_skipped() {
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
        assert_eq!(reorder(vec!["a", "b"], &[1, 9, 0]), vec!["b", "a"]);
    }

    #[test]
    fn a_workspace_is_not_adopted_from_home_or_a_root() {
        let home = PathBuf::from("/Users/someone");
        assert!(adoptable_workspace(&home.join("code/project"), Some(&home)));
        assert!(adoptable_workspace(&PathBuf::from("/srv/app"), Some(&home)));

        assert!(!adoptable_workspace(&home, Some(&home)));
        assert!(!adoptable_workspace(&PathBuf::from("/"), Some(&home)));

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

        let runtime = Runtime::boot(&cfg, dir.join("ext")).unwrap();
        let exts = runtime.extensions();
        assert_eq!(exts.len(), 1, "only the enabled instance is resolved");
        assert_eq!(exts[0].instance.id, "tool.fs");
        assert!(matches!(exts[0].state, LoadState::Missing(_)));

        assert_eq!(runtime.start_all().unwrap(), [] as [std::string::String; 0]);

        std::fs::remove_dir_all(&dir).ok();
    }
}
