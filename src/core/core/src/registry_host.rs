//! Host adapter for `registry-skills` and `registry-mcp` extensions (Phase 10).
//!
//! Each registry world gets its own inline `bindgen!` block, its own host state,
//! and its own instantiation path — following the same pattern as `tool_host`.
//! The two registry types share a `RegistryFleet` that implements `ToolInvoker`
//! so the conductor dispatches skill and MCP tool calls through the same seam.


use wasmtime::component::{HasSelf, Linker};
use wasmtime::{Engine, Store};
use wasmtime_wasi::{ResourceTable, WasiCtx, WasiCtxBuilder, WasiCtxView, WasiView};

use crate::host_fs::{FsError, Workspace};
use crate::CoreError;

// ─── skills world bindgen ────────────────────────────────────────────────────

#[allow(missing_docs, clippy::all, clippy::pedantic, clippy::nursery)]
mod skills_bind {
    wasmtime::component::bindgen!({
        path: "../../../wit",
        world: "skill-registry-world",
    });
}

use skills_bind::jan_klod::interfaces::host_config as sk_config;
use skills_bind::jan_klod::interfaces::host_fs as sk_fs;
use skills_bind::jan_klod::interfaces::host_log as sk_log;

// ─── mcp world bindgen ───────────────────────────────────────────────────────

#[allow(missing_docs, clippy::all, clippy::pedantic, clippy::nursery)]
mod mcp_bind {
    wasmtime::component::bindgen!({
        path: "../../../wit",
        world: "mcp-registry-world",
    });
}

use mcp_bind::jan_klod::interfaces::host_config as mcp_config;
use mcp_bind::jan_klod::interfaces::host_event as mcp_event;
use mcp_bind::jan_klod::interfaces::host_http as mcp_http;
use mcp_bind::jan_klod::interfaces::host_log as mcp_log;

// ─── skills host state ───────────────────────────────────────────────────────

struct SkillsHost {
    wasi: WasiCtx,
    table: ResourceTable,
    component_id: String,
    config_json: String,
    workspace: Option<Workspace>,
}

impl WasiView for SkillsHost {
    fn ctx(&mut self) -> WasiCtxView<'_> {
        WasiCtxView { ctx: &mut self.wasi, table: &mut self.table }
    }
}

impl sk_log::Host for SkillsHost {
    fn log(&mut self, level: sk_log::LogLevel, component: String, message: String, _fields: Vec<sk_log::LogField>) {
        let level = match level {
            sk_log::LogLevel::Debug => "DEBUG",
            sk_log::LogLevel::Info => "INFO",
            sk_log::LogLevel::Warn => "WARN",
            sk_log::LogLevel::Error => "ERROR",
        };
        eprintln!("{level} [{}] {component}: {message}", self.component_id);
    }
}

impl sk_config::Host for SkillsHost {
    fn get(&mut self, key: String) -> Result<String, sk_config::ConfigError> {
        let v: serde_json::Value =
            serde_json::from_str(&self.config_json).unwrap_or(serde_json::Value::Null);
        v.get(&key)
            .map(|val| {
                if let serde_json::Value::String(s) = val { s.clone() } else { val.to_string() }
            })
            .ok_or(sk_config::ConfigError::KeyNotFound)
    }
    fn has(&mut self, key: String) -> bool {
        let v: serde_json::Value =
            serde_json::from_str(&self.config_json).unwrap_or(serde_json::Value::Null);
        v.get(&key).is_some()
    }
    fn all(&mut self) -> Result<String, sk_config::ConfigError> {
        Ok(self.config_json.clone())
    }
}

impl sk_fs::Host for SkillsHost {
    fn read(&mut self, path: String) -> Result<String, sk_fs::FsError> {
        self.workspace
            .as_ref()
            .map_or(Err(sk_fs::FsError::Denied), |ws| ws.read(&path).map_err(to_sk_fs_err))
    }
    fn write(&mut self, path: String, contents: String) -> Result<(), sk_fs::FsError> {
        self.workspace
            .as_ref()
            .map_or(Err(sk_fs::FsError::Denied), |ws| ws.write(&path, &contents).map_err(to_sk_fs_err))
    }
    fn list_dir(&mut self, path: String) -> Result<Vec<sk_fs::Entry>, sk_fs::FsError> {
        self.workspace.as_ref().map_or(Err(sk_fs::FsError::Denied), |ws| {
            ws.list_dir(&path)
                .map(|entries| {
                    entries.into_iter().map(|e| sk_fs::Entry { name: e.name, is_dir: e.is_dir }).collect()
                })
                .map_err(to_sk_fs_err)
        })
    }
    fn exists(&mut self, path: String) -> bool {
        self.workspace.as_ref().is_some_and(|ws| ws.exists(&path))
    }
}

const fn to_sk_fs_err(err: FsError) -> sk_fs::FsError {
    match err {
        FsError::NotFound => sk_fs::FsError::NotFound,
        FsError::Denied => sk_fs::FsError::Denied,
        FsError::Io => sk_fs::FsError::Io,
    }
}

// ─── mcp host state ──────────────────────────────────────────────────────────

struct McpHost {
    wasi: WasiCtx,
    table: ResourceTable,
    component_id: String,
    config_json: String,
}

impl WasiView for McpHost {
    fn ctx(&mut self) -> WasiCtxView<'_> {
        WasiCtxView { ctx: &mut self.wasi, table: &mut self.table }
    }
}

impl mcp_log::Host for McpHost {
    fn log(&mut self, level: mcp_log::LogLevel, component: String, message: String, _fields: Vec<mcp_log::LogField>) {
        let level = match level {
            mcp_log::LogLevel::Debug => "DEBUG",
            mcp_log::LogLevel::Info => "INFO",
            mcp_log::LogLevel::Warn => "WARN",
            mcp_log::LogLevel::Error => "ERROR",
        };
        eprintln!("{level} [{}] {component}: {message}", self.component_id);
    }
}

impl mcp_config::Host for McpHost {
    fn get(&mut self, key: String) -> Result<String, mcp_config::ConfigError> {
        let v: serde_json::Value =
            serde_json::from_str(&self.config_json).unwrap_or(serde_json::Value::Null);
        v.get(&key)
            .map(|val| {
                if let serde_json::Value::String(s) = val { s.clone() } else { val.to_string() }
            })
            .ok_or(mcp_config::ConfigError::KeyNotFound)
    }
    fn has(&mut self, key: String) -> bool {
        let v: serde_json::Value =
            serde_json::from_str(&self.config_json).unwrap_or(serde_json::Value::Null);
        v.get(&key).is_some()
    }
    fn all(&mut self) -> Result<String, mcp_config::ConfigError> {
        Ok(self.config_json.clone())
    }
}

impl mcp_http::Host for McpHost {
    fn fetch(&mut self, request: mcp_http::HttpRequest) -> Result<mcp_http::HttpResponse, mcp_http::HttpError> {
        let headers: Vec<(String, String)> =
            request.headers.into_iter().map(|h| (h.name, h.value)).collect();
        let result = crate::http::fetch(
            &request.method,
            &request.url,
            &headers,
            request.body.as_deref(),
            request.timeout_ms,
        );
        match result {
            Ok(r) => Ok(mcp_http::HttpResponse {
                status: r.status,
                headers: r.headers.into_iter().map(|(name, value)| mcp_http::HttpHeader { name, value }).collect(),
                body: r.body,
            }),
            Err(err) => Err(to_mcp_http_err(&err)),
        }
    }
}

const fn to_mcp_http_err(err: &crate::http::WireError) -> mcp_http::HttpError {
    use crate::http::WireError;
    match err {
        WireError::InvalidUrl => mcp_http::HttpError::InvalidUrl,
        WireError::ConnectionFailed => mcp_http::HttpError::ConnectionFailed,
        WireError::Timeout => mcp_http::HttpError::Timeout,
        WireError::TlsError => mcp_http::HttpError::TlsError,
        WireError::ClientError(c) => mcp_http::HttpError::ClientError(*c),
        WireError::ServerError(c) => mcp_http::HttpError::ServerError(*c),
        WireError::Backend => mcp_http::HttpError::Backend,
    }
}

impl mcp_event::Host for McpHost {
    /// One-way: the host records what the gateway reports. There is no delivery
    /// side — `host-event` narrowed to `publish` when it turned out the queue
    /// behind it was real here, stubbed for interceptors, and called by neither.
    /// See `wit/host-event.wit`.
    fn publish(&mut self, topic: String, payload: String) {
        eprintln!("EVENT [{}] {topic}: {payload}", self.component_id);
    }
}

// ─── SkillsExtension ─────────────────────────────────────────────────────────

/// An instantiated `registry-skills` extension.
pub struct SkillsExtension {
    id: String,
    store: Store<SkillsHost>,
    world: skills_bind::SkillRegistryWorld,
}

impl SkillsExtension {
    /// Instantiate and start a `registry-skills` guest.
    ///
    /// # Errors
    /// Returns [`CoreError`] if wiring, instantiation, or lifecycle fails.
    pub fn instantiate(
        engine: &Engine,
        id: &str,
        component: &wasmtime::component::Component,
        config_json: String,
        workspace: Option<Workspace>,
    ) -> Result<Self, CoreError> {
        let mut linker: Linker<SkillsHost> = Linker::new(engine);
        wasmtime_wasi::p2::add_to_linker_sync(&mut linker).map_err(CoreError::linker)?;
        sk_log::add_to_linker::<_, HasSelf<_>>(&mut linker, |s| s).map_err(CoreError::linker)?;
        sk_config::add_to_linker::<_, HasSelf<_>>(&mut linker, |s| s).map_err(CoreError::linker)?;
        sk_fs::add_to_linker::<_, HasSelf<_>>(&mut linker, |s| s).map_err(CoreError::linker)?;

        let host = SkillsHost {
            wasi: WasiCtxBuilder::new().inherit_stdio().build(),
            table: ResourceTable::new(),
            component_id: id.to_string(),
            config_json,
            workspace,
        };
        let mut store = Store::new(engine, host);
        let world = skills_bind::SkillRegistryWorld::instantiate(&mut store, component, &linker)
            .map_err(|source| CoreError::instantiate(id, source))?;

        let lifecycle = world.jan_klod_interfaces_extension_lifecycle();
        let ctx = skills_bind::exports::jan_klod::interfaces::extension_lifecycle::ExtensionContext {
            id: id.to_string(),
            version: "0.1.0".to_string(),
        };
        registry_drive(id, lifecycle.call_init(&mut store, &ctx), "init")?;
        registry_drive(id, lifecycle.call_start(&mut store), "start")?;

        Ok(Self { id: id.to_string(), store, world })
    }

    /// List all available skills.
    pub fn list_skills(&mut self) -> Vec<SkillMeta> {
        let reg = self.world.jan_klod_interfaces_skill_registry();
        if let Ok(Ok(skills)) = reg.call_list_skills(&mut self.store) {
            skills
                .into_iter()
                .map(|s| SkillMeta {
                    name: s.name,
                    description: s.description,
                    path: s.path,
                    arguments_schema: s.arguments_schema,
                })
                .collect()
        } else {
            eprintln!("WARN [{}] list-skills failed", self.id);
            Vec::new()
        }
    }

    /// Invoke a skill by name with JSON arguments.
    ///
    /// # Errors
    /// Returns an error string if the skill returns an error or the guest traps.
    pub fn invoke(&mut self, name: &str, arguments: &str) -> Result<String, String> {
        let reg = self.world.jan_klod_interfaces_skill_registry();
        match reg.call_invoke(&mut self.store, name, arguments) {
            Ok(Ok(result)) => Ok(result),
            Ok(Err(err)) => Err(format!("skill `{name}` error: {err:?}")),
            Err(_) => Err(format!("skill `{name}` trapped")),
        }
    }
}

// ─── McpExtension ────────────────────────────────────────────────────────────

/// An instantiated `registry-mcp` extension.
pub struct McpExtension {
    id: String,
    store: Store<McpHost>,
    world: mcp_bind::McpRegistryWorld,
}

impl McpExtension {
    /// Instantiate and start a `registry-mcp` guest.
    ///
    /// # Errors
    /// Returns [`CoreError`] if wiring, instantiation, or lifecycle fails.
    pub fn instantiate(
        engine: &Engine,
        id: &str,
        component: &wasmtime::component::Component,
        config_json: String,
    ) -> Result<Self, CoreError> {
        let mut linker: Linker<McpHost> = Linker::new(engine);
        wasmtime_wasi::p2::add_to_linker_sync(&mut linker).map_err(CoreError::linker)?;
        mcp_log::add_to_linker::<_, HasSelf<_>>(&mut linker, |s| s).map_err(CoreError::linker)?;
        mcp_config::add_to_linker::<_, HasSelf<_>>(&mut linker, |s| s).map_err(CoreError::linker)?;
        mcp_http::add_to_linker::<_, HasSelf<_>>(&mut linker, |s| s).map_err(CoreError::linker)?;
        mcp_event::add_to_linker::<_, HasSelf<_>>(&mut linker, |s| s).map_err(CoreError::linker)?;

        let host = McpHost {
            wasi: WasiCtxBuilder::new().inherit_stdio().build(),
            table: ResourceTable::new(),
            component_id: id.to_string(),
            config_json,
        };
        let mut store = Store::new(engine, host);
        let world = mcp_bind::McpRegistryWorld::instantiate(&mut store, component, &linker)
            .map_err(|source| CoreError::instantiate(id, source))?;

        let lifecycle = world.jan_klod_interfaces_extension_lifecycle();
        let ctx = mcp_bind::exports::jan_klod::interfaces::extension_lifecycle::ExtensionContext {
            id: id.to_string(),
            version: "0.1.0".to_string(),
        };
        registry_drive(id, lifecycle.call_init(&mut store, &ctx), "init")?;
        registry_drive(id, lifecycle.call_start(&mut store), "start")?;

        Ok(Self { id: id.to_string(), store, world })
    }

    /// List all tools exposed by connected MCP servers.
    pub fn list_tools(&mut self) -> Vec<McpToolMeta> {
        let reg = self.world.jan_klod_interfaces_mcp_registry();
        if let Ok(Ok(tools)) = reg.call_list_tools(&mut self.store) {
            tools
                .into_iter()
                .map(|t| McpToolMeta {
                    name: format!("{}::{}", t.server_id, t.name),
                    description: t.description,
                    input_schema: t.input_schema,
                })
                .collect()
        } else {
            eprintln!("WARN [{}] list-tools failed", self.id);
            Vec::new()
        }
    }

    /// Invoke an MCP tool (qualified name `server_id::tool_name`).
    ///
    /// # Errors
    /// Returns an error string if the tool returns an error or the guest traps.
    pub fn invoke_tool(&mut self, qualified_name: &str, arguments: &str) -> Result<String, String> {
        // Strip the server prefix; the MCP registry resolves by bare tool name per-server.
        let bare = qualified_name.split("::").last().unwrap_or(qualified_name);
        let reg = self.world.jan_klod_interfaces_mcp_registry();
        match reg.call_invoke_tool(&mut self.store, bare, arguments) {
            Ok(Ok(result)) => Ok(result),
            Ok(Err(err)) => Err(format!("mcp tool `{qualified_name}` error: {err:?}")),
            Err(_) => Err(format!("mcp tool `{qualified_name}` trapped")),
        }
    }
}

// ─── RegistryFleet ───────────────────────────────────────────────────────────

/// Skill metadata (mirrors `skill-info` WIT record).
#[derive(Debug, Clone)]
pub struct SkillMeta {
    /// Skill name (used as the tool name exposed to the model).
    pub name: String,
    /// Human-readable description.
    pub description: String,
    /// Workspace-relative path to the skill file.
    pub path: String,
    /// JSON Schema string for arguments.
    pub arguments_schema: String,
}

/// MCP tool metadata.
#[derive(Debug, Clone)]
pub struct McpToolMeta {
    /// Qualified tool name `server_id::tool_name`.
    pub name: String,
    /// Human-readable description.
    pub description: String,
    /// JSON Schema for inputs.
    pub input_schema: String,
}

/// All loaded registry extensions, exposing skills and MCP tools as `ToolInvoker`.
pub struct RegistryFleet {
    skills: Vec<SkillsExtension>,
    mcp: Vec<McpExtension>,
    /// Cached resolved tool metadata (name → which extension owns it).
    skill_names: Vec<(String, usize)>,
    mcp_names: Vec<(String, usize)>,
}

impl RegistryFleet {
    /// Build a fleet from the instantiated registry extensions, resolving metadata.
    #[must_use]
    pub fn new(skills: Vec<SkillsExtension>, mcp: Vec<McpExtension>) -> Self {
        let mut fleet = Self {
            skills,
            mcp,
            skill_names: Vec::new(),
            mcp_names: Vec::new(),
        };
        fleet.refresh_cache();
        fleet
    }

    /// Re-scan all registry extensions and rebuild the name→index cache.
    pub fn refresh_cache(&mut self) {
        self.skill_names.clear();
        for (idx, ext) in self.skills.iter_mut().enumerate() {
            for meta in ext.list_skills() {
                self.skill_names.push((meta.name, idx));
            }
        }
        self.mcp_names.clear();
        for (idx, ext) in self.mcp.iter_mut().enumerate() {
            for meta in ext.list_tools() {
                self.mcp_names.push((meta.name, idx));
            }
        }
    }

    /// Tool metadata for all skills (as `crate::tool_host::ToolMeta`-like tuples).
    #[must_use]
    pub fn skill_metas(&mut self) -> Vec<(String, String, String)> {
        let mut out = Vec::new();
        for (idx, ext) in self.skills.iter_mut().enumerate() {
            for meta in ext.list_skills() {
                // Re-sync cache entry
                if !self.skill_names.iter().any(|(n, _)| *n == meta.name) {
                    self.skill_names.push((meta.name.clone(), idx));
                }
                out.push((meta.name, meta.description, meta.arguments_schema));
            }
        }
        out
    }

    /// Tool metadata for all MCP tools.
    #[must_use]
    pub fn mcp_metas(&mut self) -> Vec<(String, String, String)> {
        let mut out = Vec::new();
        for (idx, ext) in self.mcp.iter_mut().enumerate() {
            for meta in ext.list_tools() {
                if !self.mcp_names.iter().any(|(n, _)| *n == meta.name) {
                    self.mcp_names.push((meta.name.clone(), idx));
                }
                out.push((meta.name, meta.description, meta.input_schema));
            }
        }
        out
    }

    /// Combined tool metadata (skills + MCP tools) in `(name, description, schema)` form.
    #[must_use]
    pub fn all_metas(&mut self) -> Vec<(String, String, String)> {
        let mut out = self.skill_metas();
        out.extend(self.mcp_metas());
        out
    }
}

impl crate::conductor::ToolInvoker for RegistryFleet {
    fn invoke(&mut self, call: &crate::intercept::ToolCall) -> Option<String> {
        // Check skills first.
        if let Some((_, idx)) = self.skill_names.iter().find(|(n, _)| n == &call.name) {
            let idx = *idx;
            return Some(
                self.skills[idx]
                    .invoke(&call.name, &call.arguments)
                    .unwrap_or_else(|err| err),
            );
        }
        // Then MCP tools.
        if let Some((_, idx)) = self.mcp_names.iter().find(|(n, _)| n == &call.name) {
            let idx = *idx;
            return Some(
                self.mcp[idx]
                    .invoke_tool(&call.name, &call.arguments)
                    .unwrap_or_else(|err| err),
            );
        }
        None
    }
}

fn registry_drive(
    id: &str,
    result: wasmtime::Result<Result<(), String>>,
    phase: &'static str,
) -> Result<(), CoreError> {
    match result {
        Ok(Ok(())) => Ok(()),
        Ok(Err(message)) => Err(CoreError::LifecycleRejected { id: id.to_string(), phase, message }),
        Err(source) => Err(CoreError::Lifecycle { id: id.to_string(), phase, source: source.into() }),
    }
}
