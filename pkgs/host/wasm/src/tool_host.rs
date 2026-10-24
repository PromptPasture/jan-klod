//! Host adapter for `tool-*` extensions; `host-fs` is default-deny without a [`Workspace`].

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

/// Max asks per tool call; enforced by the host so a guest cannot raise it.
const MAX_ASKS: usize = 8;

/// Host view of a tool that may also ask the user; a component without
/// `tool-askable` fails construction here (#175, #216).
mod ask_bind {
    wasmtime::component::bindgen!({
        path: "../../../wit",
        world: "tool-askable-view",
    });
}

use bind::jan_klod::interfaces::host_config as g_config;
use bind::jan_klod::interfaces::host_fs as g_fs;
use bind::jan_klod::interfaces::host_http as g_http;
use bind::jan_klod::interfaces::host_log as g_log;
use bind::jan_klod::interfaces::host_process as g_proc;
use bind::jan_klod::interfaces::host_secrets as g_secrets;
use bind::jan_klod::interfaces::host_storage as g_storage;

/// Host state for a tool guest.
struct ToolHost {
    wasi: WasiCtx,
    table: ResourceTable,
    component_id: String,
    /// Path-jailed workspace; `None` is default-deny.
    workspace: Option<Workspace>,
    /// Bounded command runner (default-deny unless enabled).
    process: ProcessRunner,
    /// Outbound HTTP; `None` is default-deny (importing the interface is not egress).
    http: Option<crate::route::HttpFn>,
    /// Session working memory, namespace-scoped like the interceptor host's (#215).
    storage: crate::guest_storage::GuestStorage,
    /// Long-lived children (#109). Owned here, not by `ProcessRunner`, so they die
    /// with this instance (`LiveChild::Drop`); a child in a runner clone belongs to nothing.
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

impl g_secrets::Host for ToolHost {
    fn get(&mut self, name: String) -> Result<String, g_secrets::SecretsError> {
        std::env::var(&name).map_err(|_| g_secrets::SecretsError::NotFound)
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

impl g_storage::Host for ToolHost {
    fn set(
        &mut self,
        namespace: String,
        key: String,
        value: String,
    ) -> Result<g_storage::Entry, g_storage::StoreError> {
        self.storage
            .set(namespace, key, value)
            .map(entry)
            .map_err(|err| fault(&err))
    }

    fn get(
        &mut self,
        namespace: String,
        key: String,
    ) -> Result<g_storage::Entry, g_storage::StoreError> {
        self.storage
            .get(&namespace, &key)
            .map(entry)
            .map_err(|err| fault(&err))
    }

    fn delete(&mut self, namespace: String, key: String) -> Result<(), g_storage::StoreError> {
        self.storage
            .delete(&namespace, &key)
            .map_err(|err| fault(&err))
    }

    fn list_keys(
        &mut self,
        namespace: String,
    ) -> Result<Vec<g_storage::Entry>, g_storage::StoreError> {
        self.storage
            .entries_in(&namespace)
            .map(|rows| rows.into_iter().map(entry).collect())
            .map_err(|err| fault(&err))
    }

    fn recent(
        &mut self,
        namespace: String,
        limit: u32,
    ) -> Result<Vec<g_storage::Entry>, g_storage::StoreError> {
        self.storage
            .recent(&namespace, limit)
            .map(|rows| rows.into_iter().map(entry).collect())
            .map_err(|err| fault(&err))
    }
}

/// This world's `entry`.
fn entry(stored: crate::guest_storage::StoredEntry) -> g_storage::Entry {
    g_storage::Entry {
        id: stored.id,
        namespace: stored.namespace,
        key: stored.key,
        value: stored.value,
        created_at: stored.created_at,
        updated_at: stored.updated_at,
    }
}

/// This world's `store-error`.
const fn fault(err: &crate::guest_storage::StorageFault) -> g_storage::StoreError {
    match err {
        crate::guest_storage::StorageFault::NotFound => g_storage::StoreError::NotFound,
        crate::guest_storage::StorageFault::Backend => g_storage::StoreError::Backend,
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

    // Unknown handle is `Denied`: a guest can pass any integer.

    fn granted(&mut self) -> Vec<String> {
        self.process.long_lived_names()
    }

    fn spawn(&mut self, name: String) -> Result<u32, g_proc::ProcError> {
        // Admit before spawn: a spawn-then-check window breaks default-deny.
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
    /// Present only when the component exports `tool-askable` (#216).
    asking: Option<ask_bind::ToolAskableView>,
}

impl ToolExtension {
    /// Bind the session for `host-storage`; per turn, since an instance outlives sessions (#215).
    pub fn bind_session(&mut self, session: &str) {
        self.store.data_mut().storage.bind_session(session);
    }
}

impl ToolExtension {
    /// Instantiate `component` as a tool; `workspace` backs `host-fs` (`None` = default-deny).
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
        Self::instantiate_with_http(engine, id, component, workspace, process, None, None, false)
    }

    /// Instantiate a tool with outbound HTTP granted (opt-in per call site).
    ///
    /// # Errors
    /// Returns a [`CoreError`] if wiring, instantiation, or lifecycle fails.
    // Eight independent operator-granted bounds; a struct would hide which one a call site forgot.
    #[allow(clippy::too_many_arguments)]
    pub fn instantiate_with_http(
        engine: &Engine,
        id: &str,
        component: &Component,
        workspace: Option<Workspace>,
        process: ProcessRunner,
        http: Option<crate::route::HttpFn>,
        storage: Option<std::sync::Arc<dyn crate::guest_storage::Entries>>,
        per_session: bool,
    ) -> Result<Self, CoreError> {
        let mut linker: Linker<ToolHost> = Linker::new(engine);
        wasmtime_wasi::p2::add_to_linker_sync(&mut linker).map_err(CoreError::linker)?;
        g_log::add_to_linker::<_, HasSelf<_>>(&mut linker, |s| s).map_err(CoreError::linker)?;
        g_config::add_to_linker::<_, HasSelf<_>>(&mut linker, |s| s).map_err(CoreError::linker)?;
        g_http::add_to_linker::<_, HasSelf<_>>(&mut linker, |s| s).map_err(CoreError::linker)?;
        g_fs::add_to_linker::<_, HasSelf<_>>(&mut linker, |s| s).map_err(CoreError::linker)?;
        g_proc::add_to_linker::<_, HasSelf<_>>(&mut linker, |s| s).map_err(CoreError::linker)?;
        g_secrets::add_to_linker::<_, HasSelf<_>>(&mut linker, |s| s).map_err(CoreError::linker)?;
        g_storage::add_to_linker::<_, HasSelf<_>>(&mut linker, |s| s).map_err(CoreError::linker)?;

        let host = ToolHost {
            // Not full stdio: stdin would let guests read permission prompt answers.
            wasi: WasiCtxBuilder::new().inherit_stderr().build(),
            table: ResourceTable::new(),
            component_id: id.to_string(),
            workspace,
            process,
            http,
            storage: {
                let backing = storage.map_or_else(
                    || crate::guest_storage::Backing::Ephemeral {
                        entries: std::collections::HashMap::new(),
                        clock: 0,
                    },
                    |rows| crate::guest_storage::Backing::Durable {
                        rows,
                        owner: id.to_string(),
                    },
                );
                if per_session {
                    crate::guest_storage::GuestStorage::per_session(backing)
                } else {
                    crate::guest_storage::GuestStorage::shared(backing)
                }
            },
            children: crate::host_process::Children::default(),
        };
        let mut store = Store::new(engine, host);
        let instance = linker
            .instantiate(&mut store, component)
            .map_err(|source| CoreError::instantiate(id, source))?;
        let world = bind::ToolWorld::new(&mut store, &instance)
            .map_err(|source| CoreError::instantiate(id, source))?;
        let asking = ask_bind::ToolAskableView::new(&mut store, &instance).ok();

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
            asking,
        })
    }

    /// Invoke, letting the tool ask the user first (#216); `None` if the component
    /// is not `tool-askable`.
    ///
    /// Re-invokes with each answer, capped at `MAX_ASKS`: the bound is the host's,
    /// not the guest's promise.
    fn invoke_asking(
        &mut self,
        arguments: &str,
        driver: &mut dyn jk_protocol::turn::Driver,
    ) -> Option<Result<String, String>> {
        let asking = self.asking.as_ref()?.jan_klod_interfaces_tool_askable();
        let mut answer: Option<String> = None;
        for _ in 0..MAX_ASKS {
            let step =
                match asking.call_invoke_asking(&mut self.store, arguments, answer.as_deref()) {
                    Ok(Ok(step)) => step,
                    Ok(Err(err)) => return Some(Err(format!("tool error: {err:?}"))),
                    Err(err) => return Some(Err(format!("tool trapped: {err}"))),
                };
            match step {
                ask_bind::exports::jan_klod::interfaces::tool_askable::Step::Done(result) => {
                    return Some(Ok(result));
                }
                ask_bind::exports::jan_klod::interfaces::tool_askable::Step::Asking(ask) => {
                    answer = Some(driver.ask(&jk_protocol::turn::UserPrompt {
                        question: ask.question,
                        options: ask.options,
                        default_answer: ask.default_answer,
                    }));
                }
            }
        }
        Some(Err(format!(
            "tool `{}` asked more than {MAX_ASKS} times without answering; \
             the turn was not left waiting on it",
            self.id
        )))
    }

    /// This extension's instance id.
    #[must_use]
    pub fn id(&self) -> &str {
        &self.id
    }

    /// The tool's advertised metadata.
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

/// Instantiated `tool-*` extensions, dispatched by name; unknown names return `None`.
pub struct ToolFleet {
    /// (advertised metadata, extension), metadata resolved once via `meta`.
    tools: Vec<(ToolMeta, ToolExtension)>,
}

impl ToolFleet {
    /// Bind `session` on every instance in the fleet.
    pub fn bind_session(&mut self, session: &str) {
        for (_, tool) in &mut self.tools {
            tool.bind_session(session);
        }
    }
}

impl ToolFleet {
    /// Build fleet from extensions, resolving each tool's metadata (fallback to id).
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

impl jk_protocol::intercept::ToolInvoker for ToolFleet {
    fn invoke_asking(
        &mut self,
        call: &jk_protocol::turn::ToolCall,
        driver: &mut dyn jk_protocol::turn::Driver,
    ) -> Option<jk_protocol::intercept::ToolInvocation> {
        let entry = self
            .tools
            .iter_mut()
            .find(|(meta, _)| meta.name == call.name)?;
        // Non-asking tool: fall through to `invoke`; `None` would mean no such tool.
        let Some(outcome) = entry.1.invoke_asking(&call.arguments, driver) else {
            return self.invoke(call);
        };
        Some(match outcome {
            Ok(content) => jk_protocol::intercept::ToolInvocation {
                content,
                failed: false,
            },
            Err(content) => jk_protocol::intercept::ToolInvocation {
                content,
                failed: true,
            },
        })
    }

    fn invoke(
        &mut self,
        call: &jk_protocol::turn::ToolCall,
    ) -> Option<jk_protocol::intercept::ToolInvocation> {
        let entry = self
            .tools
            .iter_mut()
            .find(|(meta, _)| meta.name == call.name)?;
        // Tool errors go back to the model as the result (#162), flagged failed.
        Some(match entry.1.invoke(&call.arguments) {
            Ok(content) => jk_protocol::intercept::ToolInvocation {
                content,
                failed: false,
            },
            Err(content) => jk_protocol::intercept::ToolInvocation {
                content,
                failed: true,
            },
        })
    }
}

/// One `tool-*` instance's compiled component and wiring, held uninstantiated.
/// One `tool-*` instance's compiled component and wiring, held uninstantiated.
struct PendingTool {
    id: String,
    component: Component,
    workspace: Option<Workspace>,
    process: ProcessRunner,
    http: Option<crate::route::HttpFn>,
    storage: Option<std::sync::Arc<dyn crate::guest_storage::Entries>>,
    per_session: bool,
}

/// Tool fleet with guests compiled but not instantiated until first use (#59).
///
/// The whole pending set resolves together: `select-tools` advertises every tool,
/// and `meta` is guest-authored, so the host cannot derive it cheaper.
pub struct LazyToolFleet {
    engine: Engine,
    pending: Vec<PendingTool>,
    live: Option<ToolFleet>,
    /// Session bound before instantiation, replayed onto each instance (#215).
    session: Option<String>,
}

impl LazyToolFleet {
    /// An empty fleet, ready to receive pending tools via [`Self::push`].
    #[must_use]
    pub const fn new(engine: Engine) -> Self {
        Self {
            engine,
            pending: Vec::new(),
            live: None,
            session: None,
        }
    }

    /// Register a compiled tool to be instantiated on first use.
    // Mirrors `instantiate_with_http`'s list; the two diverging would be the bug.
    #[allow(clippy::too_many_arguments)]
    pub fn push(
        &mut self,
        id: impl Into<String>,
        component: Component,
        workspace: Option<Workspace>,
        process: ProcessRunner,
        http: Option<crate::route::HttpFn>,
        storage: Option<std::sync::Arc<dyn crate::guest_storage::Entries>>,
        per_session: bool,
    ) {
        self.pending.push(PendingTool {
            id: id.into(),
            component,
            workspace,
            process,
            http,
            storage,
            per_session,
        });
    }

    /// Whether every pending tool has been instantiated. `false` until first
    /// [`Self::metas`]/[`Self::tool_names`]/`invoke` call.
    #[must_use]
    pub const fn is_instantiated(&self) -> bool {
        self.live.is_some()
    }

    /// Instantiate every pending tool once (memoized). Failures reported like
    /// the eager path: [`CoreError`], just here instead of `build_agent`.
    fn ensure(&mut self) -> Result<&mut ToolFleet, CoreError> {
        if self.live.is_none() {
            let mut extensions = Vec::with_capacity(self.pending.len());
            for pending in self.pending.drain(..) {
                extensions.push(Self::instantiate_pending(&self.engine, pending)?);
            }
            let mut fleet = ToolFleet::new(extensions);
            if let Some(session) = &self.session {
                fleet.bind_session(session);
            }
            self.live = Some(fleet);
        }
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
            pending.storage,
            pending.per_session,
        )
    }

    /// Advertised metadata for every tool, instantiating pending set on first call.
    ///
    /// # Errors
    /// Returns a [`CoreError`] if a pending tool fails to instantiate or start.
    pub fn metas(&mut self) -> Result<Vec<ToolMeta>, CoreError> {
        Ok(self.ensure()?.metas())
    }

    /// Advertised tool names, instantiating pending set on first call.
    ///
    /// # Errors
    /// Returns a [`CoreError`] if a pending tool fails to instantiate or start.
    pub fn tool_names(&mut self) -> Result<Vec<String>, CoreError> {
        Ok(self.ensure()?.tool_names())
    }
}

impl jk_protocol::intercept::ToolInvoker for LazyToolFleet {
    fn invoke_asking(
        &mut self,
        call: &jk_protocol::turn::ToolCall,
        driver: &mut dyn jk_protocol::turn::Driver,
    ) -> Option<jk_protocol::intercept::ToolInvocation> {
        self.ensure().ok()?.invoke_asking(call, driver)
    }

    fn invoke(
        &mut self,
        call: &jk_protocol::turn::ToolCall,
    ) -> Option<jk_protocol::intercept::ToolInvocation> {
        match self.ensure() {
            Ok(fleet) => fleet.invoke(call),
            // Like live tools: error becomes the result; a lazy failure costs one call.
            Err(err) => Some(jk_protocol::intercept::ToolInvocation {
                content: format!("tool fleet failed to instantiate: {err}"),
                failed: true,
            }),
        }
    }
    fn bind_session(&mut self, session: &str) {
        self.session = Some(session.to_string());
        if let Some(live) = self.live.as_mut() {
            live.bind_session(session);
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
