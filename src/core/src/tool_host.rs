//! Host adapter for `tool-*` extensions.
//!
//! Instantiates a `tool-world` guest, satisfies its imports (`host-log`,
//! `host-config`, `host-http`, `host-fs`), and exposes its `tool-callable`
//! exports (`meta`/`invoke`). `host-fs` is backed by an optional [`Workspace`]:
//! default-deny — with no workspace configured every op returns `denied`.

use wasmtime::component::{Component, HasSelf, Linker};
use wasmtime::{Engine, Store};
use wasmtime_wasi::{ResourceTable, WasiCtx, WasiCtxBuilder, WasiCtxView, WasiView};

use crate::host_fs::{FsError, Workspace};
use crate::host_process::{ProcError, ProcessRunner};
use crate::CoreError;

#[allow(missing_docs, clippy::all, clippy::pedantic, clippy::nursery)]
mod bind {
    wasmtime::component::bindgen!({
        path: "../../wit",
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
    /// Having the interface imported and having egress granted are different
    /// things — a file tool suddenly making network calls is what the sandbox
    /// guards against. `None` unless the instance's config opts in.
    http: Option<crate::route::HttpFn>,
    /// Long-lived children this instance started, by handle (#109).
    ///
    /// **Owned here, not in `ProcessRunner`, because this is the lifetime.**
    /// `ToolHost` is per-instance and dies with it, so the children die with it
    /// too — `LiveChild`'s `Drop` kills what it holds, and that runs whether the
    /// instance stops cleanly, errors, or goes down with the runtime. Keeping
    /// them in the runner would not have that property: the runner is `Clone`,
    /// and a child in a clone belongs to nothing in particular.
    children: crate::host_process::Children,
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

    // ── Long-lived children (Slice 18c-1, #109) ──────────────────────────────
    //
    // Every call goes through `self.children`, which is keyed by a handle this
    // host issued. An unknown handle is `Denied` rather than a panic or a
    // silent success: a guest can pass any integer, and the answer to one it
    // was never given is the same answer it gets for a child it was never
    // granted.

    fn spawn(&mut self, name: String) -> Result<u32, g_proc::ProcError> {
        // The admission decision, and it happens **before** anything is
        // started. An implementation that spawned and then checked would have a
        // window in which it had done neither, and a window is all a capability
        // like this needs to stop being default-deny.
        //
        // `proc-error` carries no payload, so the reason goes to the host log —
        // the same answer `exec` gives, and the reason the interface says so.
        self.children
            .spawn(&self.process, &name)
            .map_err(to_gen_proc_error)
    }

    fn write_stdin(&mut self, child: u32, data: String) -> Result<(), g_proc::ProcError> {
        self.children
            .write_stdin(child, &data)
            .map_err(to_gen_proc_error)
    }

    fn read_stdout(
        &mut self,
        child: u32,
        max_bytes: u32,
        timeout_ms: u32,
    ) -> Result<String, g_proc::ProcError> {
        self.children
            .read_stdout(
                child,
                max_bytes as usize,
                std::time::Duration::from_millis(u64::from(timeout_ms)),
            )
            .map_err(to_gen_proc_error)
    }

    fn is_running(&mut self, child: u32) -> bool {
        self.children.is_running(child)
    }

    fn kill(&mut self, child: u32) {
        self.children.kill(child);
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

    /// Instantiate a tool with outbound HTTP granted (`http = Some(client)`).
    ///
    /// Separate from [`Self::instantiate`] so granting egress is opt-in per call
    /// site rather than a default.
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
            // `inherit_stderr`, not `inherit_stdio`: inheriting stdin would let a
            // guest read the terminal, including a human's answer to a permission
            // prompt. stderr stays for panic diagnostics; it grants no authority.
            wasi: WasiCtxBuilder::new().inherit_stderr().build(),
            table: ResourceTable::new(),
            component_id: id.to_string(),
            workspace,
            process,
            http,
            children: crate::host_process::Children::default(),
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
    fn invoke(
        &mut self,
        call: &crate::intercept::ToolCall,
    ) -> Option<crate::conductor::ToolInvocation> {
        let entry = self
            .tools
            .iter_mut()
            .find(|(meta, _)| meta.name == call.name)?;
        // A tool error is fed back to the model as the result, not an abort —
        // but it is still, per #162, a failure the wire is told about.
        Some(match entry.1.invoke(&call.arguments) {
            Ok(content) => crate::conductor::ToolInvocation {
                content,
                failed: false,
            },
            Err(content) => crate::conductor::ToolInvocation {
                content,
                failed: true,
            },
        })
    }
}

// ─── LazyToolFleet ───────────────────────────────────────────────────────────

/// One `tool-*` instance's compiled component and the wiring it will need to
/// instantiate — held, not yet used. Everything here is cheap: `component` is
/// a Wasmtime handle (an `Arc` under the hood, already produced by
/// `Runtime::boot`'s compile step), and the rest are values the caller already
/// built for the eager path (`workspace`, `process`, `http`).
struct PendingTool {
    id: String,
    component: Component,
    workspace: Option<Workspace>,
    process: ProcessRunner,
    http: Option<crate::route::HttpFn>,
}

/// A tool fleet whose guests are compiled but not yet instantiated (#59).
///
/// `Runtime::boot` still compiles every enabled `tool-*` component, same as
/// before — that part is unchanged and stays cheap (parses and validates the
/// module, does not run it). What moves is the next step: creating the guest's
/// `Store`, linking it, and driving `init`/`start` — the part that actually
/// allocates the guest's linear memory and runs its setup code. That now
/// happens the first time the fleet is asked for something, not unconditionally
/// during `Runtime::build_agent`.
///
/// **The whole pending set resolves together, not just the tool asked for.**
/// A model's `select-tools` advertisement is one JSON array describing every
/// enabled tool at once ([`ToolFleet::metas`]), so the first thing that needs
/// any of it needs all of it — there is no cheaper way to answer "what tools
/// exist" than asking every one of them, because that answer (`tool-callable`'s
/// `meta` export) is guest-authored data the host cannot derive any other way.
/// Finer-grained laziness — instantiating only the one tool a model actually
/// calls, never touching the rest even for `select-tools` — needs that catalog
/// to live somewhere static (the manifest, or `config.yaml`) instead of inside
/// the guest. That is a real, separate change with its own risk (the manifest
/// and the guest's own `meta` could then disagree), noted in #59's PR rather
/// than attempted here.
pub struct LazyToolFleet {
    engine: Engine,
    pending: Vec<PendingTool>,
    live: Option<ToolFleet>,
}

impl LazyToolFleet {
    /// An empty fleet, ready to receive pending tools via [`Self::push`].
    #[must_use]
    pub const fn new(engine: Engine) -> Self {
        Self {
            engine,
            pending: Vec::new(),
            live: None,
        }
    }

    /// Register a compiled tool to be instantiated on first use.
    pub fn push(
        &mut self,
        id: impl Into<String>,
        component: Component,
        workspace: Option<Workspace>,
        process: ProcessRunner,
        http: Option<crate::route::HttpFn>,
    ) {
        self.pending.push(PendingTool {
            id: id.into(),
            component,
            workspace,
            process,
            http,
        });
    }

    /// Whether every pending tool has already been instantiated. `false` until
    /// the first [`Self::metas`]/[`Self::tool_names`]/`invoke` call — the fact
    /// a test asserting laziness checks before touching the fleet.
    #[must_use]
    pub const fn is_instantiated(&self) -> bool {
        self.live.is_some()
    }

    /// Instantiate every pending tool (once; memoized), and hand back the live
    /// fleet. A component that fails to instantiate or refuses to start is
    /// reported the same way the eager path always was — a [`CoreError`] — just
    /// raised here instead of during `build_agent`.
    fn ensure(&mut self) -> Result<&mut ToolFleet, CoreError> {
        if self.live.is_none() {
            let mut extensions = Vec::with_capacity(self.pending.len());
            for pending in self.pending.drain(..) {
                extensions.push(Self::instantiate_pending(&self.engine, pending)?);
            }
            self.live = Some(ToolFleet::new(extensions));
        }
        // The `if` above guarantees this, but the borrow checker cannot see
        // through `Option::is_none()` followed by a fresh `.as_mut()`.
        Ok(self.live.as_mut().expect("just set above"))
    }

    fn instantiate_pending(
        engine: &Engine,
        pending: PendingTool,
    ) -> Result<ToolExtension, CoreError> {
        ToolExtension::instantiate_with_http(
            engine,
            &pending.id,
            &pending.component,
            pending.workspace,
            pending.process,
            pending.http,
        )
    }

    /// The advertised metadata for every tool, instantiating the whole pending
    /// set if this is the first call.
    ///
    /// # Errors
    /// Returns a [`CoreError`] if a pending tool fails to instantiate or start.
    pub fn metas(&mut self) -> Result<Vec<ToolMeta>, CoreError> {
        Ok(self.ensure()?.metas())
    }

    /// The advertised tool names, instantiating the whole pending set if this
    /// is the first call.
    ///
    /// # Errors
    /// Returns a [`CoreError`] if a pending tool fails to instantiate or start.
    pub fn tool_names(&mut self) -> Result<Vec<String>, CoreError> {
        Ok(self.ensure()?.tool_names())
    }
}

impl crate::conductor::ToolInvoker for LazyToolFleet {
    fn invoke(
        &mut self,
        call: &crate::intercept::ToolCall,
    ) -> Option<crate::conductor::ToolInvocation> {
        match self.ensure() {
            Ok(fleet) => fleet.invoke(call),
            // Consistent with a live tool's own error handling (`ToolFleet::invoke`,
            // `ToolExtension::invoke`): fed back to the model as the call's result,
            // not an abort — a lazily-failing guest costs this one tool call, not
            // the turn.
            Err(err) => Some(crate::conductor::ToolInvocation {
                content: format!("tool fleet failed to instantiate: {err}"),
                failed: true,
            }),
        }
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
