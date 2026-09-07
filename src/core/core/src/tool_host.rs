//! Host adapter for `tool-*` extensions (Phase 7 / Phase 8).
//!
//! Instantiates a `tool-world` guest and satisfies its imports — `host-log`,
//! `host-config`, `host-http`, and the Phase 7 **`host-fs`** — then exposes its
//! `tool-callable` exports (`meta`/`invoke`). `host-fs` is backed by an optional
//! [`Workspace`]: **default-deny** — with no workspace configured every op returns
//! `denied`, so a tool cannot touch the filesystem unless the deployment opted in.

use wasmtime::component::{Component, HasSelf, Linker};
use wasmtime::{Engine, Store};
use wasmtime_wasi::{ResourceTable, WasiCtx, WasiCtxBuilder, WasiCtxView, WasiView};

use crate::host_fs::{FsError, Workspace};
use crate::host_process::{ProcError, ProcessRunner};
use crate::CoreError;

#[allow(missing_docs, clippy::all, clippy::pedantic, clippy::nursery)]
mod bind {
    wasmtime::component::bindgen!({
        path: "../../../wit",
        world: "tool-world",
    });
}

use bind::jan_klod::interfaces::host_config as g_config;
use bind::jan_klod::interfaces::host_fs as g_fs;
use bind::jan_klod::interfaces::host_http as g_http;
use bind::jan_klod::interfaces::host_log as g_log;
use bind::jan_klod::interfaces::host_process as g_proc;

/// Host state for a tool guest.
struct ToolHost {
    wasi: WasiCtx,
    table: ResourceTable,
    component_id: String,
    /// The path-jailed workspace, or `None` for default-deny (`host-fs`).
    workspace: Option<Workspace>,
    /// The bounded command runner (default-deny unless enabled) (`host-process`).
    process: ProcessRunner,
    /// Outbound HTTP, or `None` for default-deny (`host-http`).
    ///
    /// `tool-world` imports `host-http`, but a tool having *access to the
    /// interface* and a deployment having *granted egress* are different things:
    /// a file tool that suddenly makes network calls is exactly what a sandbox is
    /// for. So this is `None` unless the instance's config opts in, the same shape
    /// as `workspace` for `host-fs` and `process` for `host-process`.
    http: Option<crate::route::HttpFn>,
}

impl WasiView for ToolHost {
    fn ctx(&mut self) -> WasiCtxView<'_> {
        WasiCtxView {
            ctx: &mut self.wasi,
            table: &mut self.table,
        }
    }
}

impl g_log::Host for ToolHost {
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

impl g_config::Host for ToolHost {
    fn get(&mut self, _key: String) -> Result<String, g_config::ConfigError> {
        Err(g_config::ConfigError::KeyNotFound)
    }
    fn has(&mut self, _key: String) -> bool {
        false
    }
    fn all(&mut self) -> Result<String, g_config::ConfigError> {
        Ok("{}".to_string())
    }
}

impl g_http::Host for ToolHost {
    fn fetch(
        &mut self,
        request: g_http::HttpRequest,
    ) -> Result<g_http::HttpResponse, g_http::HttpError> {
        let Some(client) = &self.http else {
            // Default-deny: the tool was not granted egress.
            return Err(g_http::HttpError::Backend);
        };
        let headers: Vec<(String, String)> = request
            .headers
            .into_iter()
            .map(|h| (h.name, h.value))
            .collect();
        match client(
            &request.method,
            &request.url,
            &headers,
            request.body.as_deref(),
            request.timeout_ms,
        ) {
            Ok(response) => Ok(g_http::HttpResponse {
                status: response.status,
                headers: response
                    .headers
                    .into_iter()
                    .map(|(name, value)| g_http::HttpHeader { name, value })
                    .collect(),
                body: response.body,
            }),
            Err(_) => Err(g_http::HttpError::ConnectionFailed),
        }
    }
}

impl g_fs::Host for ToolHost {
    fn read(&mut self, path: String) -> Result<String, g_fs::FsError> {
        self.workspace
            .as_ref()
            .map_or(Err(g_fs::FsError::Denied), |ws| {
                ws.read(&path).map_err(to_gen_fs_error)
            })
    }
    fn write(&mut self, path: String, contents: String) -> Result<(), g_fs::FsError> {
        self.workspace
            .as_ref()
            .map_or(Err(g_fs::FsError::Denied), |ws| {
                ws.write(&path, &contents).map_err(to_gen_fs_error)
            })
    }
    fn list_dir(&mut self, path: String) -> Result<Vec<g_fs::Entry>, g_fs::FsError> {
        self.workspace
            .as_ref()
            .map_or(Err(g_fs::FsError::Denied), |ws| {
                ws.list_dir(&path)
                    .map(|entries| {
                        entries
                            .into_iter()
                            .map(|e| g_fs::Entry {
                                name: e.name,
                                is_dir: e.is_dir,
                            })
                            .collect()
                    })
                    .map_err(to_gen_fs_error)
            })
    }
    fn exists(&mut self, path: String) -> bool {
        self.workspace.as_ref().is_some_and(|ws| ws.exists(&path))
    }
}

const fn to_gen_fs_error(err: FsError) -> g_fs::FsError {
    match err {
        FsError::NotFound => g_fs::FsError::NotFound,
        FsError::Denied => g_fs::FsError::Denied,
        FsError::Io => g_fs::FsError::Io,
    }
}

impl g_proc::Host for ToolHost {
    fn exec(
        &mut self,
        command: String,
        args: Vec<String>,
        cwd: Option<String>,
        stdin: Option<String>,
    ) -> Result<g_proc::Exit, g_proc::ProcError> {
        match self
            .process
            .exec(&command, &args, cwd.as_deref(), stdin.as_deref())
        {
            Ok(exit) => Ok(g_proc::Exit {
                code: exit.code,
                stdout: exit.stdout,
                stderr: exit.stderr,
            }),
            Err(err) => Err(to_gen_proc_error(err)),
        }
    }
}

const fn to_gen_proc_error(err: ProcError) -> g_proc::ProcError {
    match err {
        ProcError::Denied => g_proc::ProcError::Denied,
        ProcError::Timeout => g_proc::ProcError::Timeout,
        ProcError::SpawnFailed => g_proc::ProcError::SpawnFailed,
    }
}

/// An instantiated, started `tool-*` extension, ready to `invoke`.
pub struct ToolExtension {
    id: String,
    store: Store<ToolHost>,
    world: bind::ToolWorld,
}

impl ToolExtension {
    /// Instantiate `component` as a tool, satisfying its imports. `workspace` backs
    /// `host-fs` (`None` = default-deny).
    ///
    /// # Errors
    /// Returns a [`CoreError`] if wiring, instantiation, or lifecycle fails.
    pub fn instantiate(
        engine: &Engine,
        id: &str,
        component: &Component,
        workspace: Option<Workspace>,
        process: ProcessRunner,
    ) -> Result<Self, CoreError> {
        Self::instantiate_with_http(engine, id, component, workspace, process, None)
    }

    /// Instantiate a tool **with outbound HTTP granted** (`http = Some(client)`).
    ///
    /// Separate from [`Self::instantiate`] so granting egress is something a
    /// caller does on purpose: the default stays no-network, and only an instance
    /// whose config asks for it gets a client.
    ///
    /// # Errors
    /// Returns a [`CoreError`] if wiring, instantiation, or lifecycle fails.
    pub fn instantiate_with_http(
        engine: &Engine,
        id: &str,
        component: &Component,
        workspace: Option<Workspace>,
        process: ProcessRunner,
        http: Option<crate::route::HttpFn>,
    ) -> Result<Self, CoreError> {
        let mut linker: Linker<ToolHost> = Linker::new(engine);
        wasmtime_wasi::p2::add_to_linker_sync(&mut linker).map_err(CoreError::linker)?;
        g_log::add_to_linker::<_, HasSelf<_>>(&mut linker, |s| s).map_err(CoreError::linker)?;
        g_config::add_to_linker::<_, HasSelf<_>>(&mut linker, |s| s).map_err(CoreError::linker)?;
        g_http::add_to_linker::<_, HasSelf<_>>(&mut linker, |s| s).map_err(CoreError::linker)?;
        g_fs::add_to_linker::<_, HasSelf<_>>(&mut linker, |s| s).map_err(CoreError::linker)?;
        g_proc::add_to_linker::<_, HasSelf<_>>(&mut linker, |s| s).map_err(CoreError::linker)?;

        let host = ToolHost {
            // `inherit_stderr`, not `inherit_stdio`.
            //
            // Inheriting all three handed every guest the host's **standard
            // input**, and `tool-escape-probe` reads 26 bytes of it straight off
            // the terminal. `jan-klod-gateway ask` runs in the user's shell, so a
            // component could read what is being typed — including the answer to
            // a permission prompt, which is the one input whose whole purpose is
            // to be a human's decision.
            //
            // stderr stays: a guest's panic message is the only thing that makes
            // a broken component diagnosable, and unlike stdin it grants no
            // authority. A guest can write misleading lines there, which is
            // cosmetic — host log lines are tagged by the host, not the guest.
            wasi: WasiCtxBuilder::new().inherit_stderr().build(),
            table: ResourceTable::new(),
            component_id: id.to_string(),
            workspace,
            process,
            http,
        };
        let mut store = Store::new(engine, host);
        let world = bind::ToolWorld::instantiate(&mut store, component, &linker)
            .map_err(|source| CoreError::instantiate(id, source))?;

        let lifecycle = world.jan_klod_interfaces_extension_lifecycle();
        let ctx = bind::exports::jan_klod::interfaces::extension_lifecycle::ExtensionContext {
            id: id.to_string(),
            version: "0.0.0".to_string(),
        };
        drive(id, lifecycle.call_init(&mut store, &ctx), "init")?;
        drive(id, lifecycle.call_start(&mut store), "start")?;

        Ok(Self {
            id: id.to_string(),
            store,
            world,
        })
    }

    /// This extension's instance id.
    #[must_use]
    pub fn id(&self) -> &str {
        &self.id
    }

    /// The tool's advertised metadata (name / description / arguments schema).
    ///
    /// # Errors
    /// Returns a trap description if the guest's `meta` call traps.
    pub fn meta(&mut self) -> Result<ToolMeta, String> {
        let tool = self.world.jan_klod_interfaces_tool_callable();
        match tool.call_meta(&mut self.store) {
            Ok(meta) => Ok(ToolMeta {
                name: meta.name,
                description: meta.description,
                arguments_schema: meta.arguments_schema,
            }),
            Err(_) => Err(format!("tool `{}` meta trapped", self.id)),
        }
    }

    /// Invoke the tool with JSON-encoded `arguments`, returning its JSON result.
    ///
    /// # Errors
    /// Returns a stringified `tool-error`, or a trap description.
    pub fn invoke(&mut self, arguments: &str) -> Result<String, String> {
        let tool = self.world.jan_klod_interfaces_tool_callable();
        match tool.call_invoke(&mut self.store, arguments) {
            Ok(Ok(result)) => Ok(result),
            Ok(Err(err)) => Err(format!("tool `{}` error: {err:?}", self.id)),
            Err(_) => Err(format!("tool `{}` trapped", self.id)),
        }
    }
}

/// A tool's advertised metadata (mirrors `tool-callable.tool-meta`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolMeta {
    /// Tool name — the `tool-call` name the model emits.
    pub name: String,
    /// Human-readable description.
    pub description: String,
    /// JSON Schema string for the arguments object.
    pub arguments_schema: String,
}

/// A set of instantiated `tool-*` extensions, dispatched by tool name.
///
/// Implements [`ToolInvoker`](crate::conductor::ToolInvoker) so the loop can call
/// tools: a model's `tool-call` name is matched to the extension that advertises it,
/// and its `invoke` runs. Unknown names return `None` (skip-if-absent).
pub struct ToolFleet {
    /// (advertised metadata, extension), metadata resolved once via `meta`.
    tools: Vec<(ToolMeta, ToolExtension)>,
}

impl ToolFleet {
    /// Build a fleet from instantiated extensions, resolving each tool's metadata
    /// (falling back to the instance id if `meta` traps).
    #[must_use]
    pub fn new(extensions: Vec<ToolExtension>) -> Self {
        let tools = extensions
            .into_iter()
            .map(|mut ext| {
                let meta = ext.meta().unwrap_or_else(|_| ToolMeta {
                    name: ext.id().to_string(),
                    description: String::new(),
                    arguments_schema: "{}".to_string(),
                });
                (meta, ext)
            })
            .collect();
        Self { tools }
    }

    /// Whether the fleet holds no tools.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.tools.is_empty()
    }

    /// The advertised tool names.
    #[must_use]
    pub fn tool_names(&self) -> Vec<String> {
        self.tools
            .iter()
            .map(|(meta, _)| meta.name.clone())
            .collect()
    }

    /// The advertised metadata for every tool (for `select-tools` advertising).
    #[must_use]
    pub fn metas(&self) -> Vec<ToolMeta> {
        self.tools.iter().map(|(meta, _)| meta.clone()).collect()
    }
}

impl crate::conductor::ToolInvoker for ToolFleet {
    fn invoke(&mut self, call: &crate::intercept::ToolCall) -> Option<String> {
        let entry = self
            .tools
            .iter_mut()
            .find(|(meta, _)| meta.name == call.name)?;
        // A tool error is fed back to the model as the result, not an abort.
        Some(entry.1.invoke(&call.arguments).unwrap_or_else(|err| err))
    }
}

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
