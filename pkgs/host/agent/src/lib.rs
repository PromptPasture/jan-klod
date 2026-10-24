//! Jan-Klod core: composes the component runtime ([`jk_wasm`]) with the session
//! store ([`jk_session`]) and runs turns on them. Nothing here names `wasmtime` (#181).

pub mod conductor;
pub mod delegate;
pub mod session;
mod session_entries;

use jk_protocol::turn;
use jk_session::{event_log, projection, store};
use jk_wasm::{egress, route, BootReport, CombinedFleet, CoreError, LoadedExtension};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// A booted core: the component runtime, and the one session store every
/// agent it builds shares.
pub struct Runtime {
    /// Engine, linker and every enabled instance resolved against `ext/`.
    components: jk_wasm::Runtime,
    /// One store per runtime, not per agent: sessions must agree on what was said (#233).
    store: Arc<Mutex<store::Store>>,
}

impl Runtime {
    /// Boots the components ([`jk_wasm::Runtime::boot`]) and opens the session
    /// store. A bad `storage.path` fails the boot here, not the first request.
    ///
    /// # Errors
    /// Whatever [`jk_wasm::Runtime::boot`] returns, or [`CoreError::Store`]
    /// if the store cannot be opened.
    pub fn boot(
        config_path: impl AsRef<Path>,
        ext_dir: impl AsRef<Path>,
    ) -> Result<Self, CoreError> {
        let config_dir = config_path
            .as_ref()
            .parent()
            .map_or_else(|| PathBuf::from("."), Path::to_path_buf);
        let components = jk_wasm::Runtime::boot(config_path, ext_dir)?;
        let store = Self::open_store(components.agent_config(), &config_dir)?;
        Ok(Self { components, store })
    }

    /// The component runtime this core was booted on.
    #[must_use]
    pub const fn components(&self) -> &jk_wasm::Runtime {
        &self.components
    }

    /// See [`jk_wasm::Runtime::with_sandbox_wrapper`] — for tests only.
    #[must_use]
    pub fn with_sandbox_wrapper(mut self, wrapper: impl Into<PathBuf>) -> Self {
        self.components = self.components.with_sandbox_wrapper(wrapper);
        self
    }

    /// See [`jk_wasm::Runtime::compile_cache_stats`].
    #[must_use]
    pub fn compile_cache_stats(&self) -> (usize, usize) {
        self.components.compile_cache_stats()
    }

    /// See [`jk_wasm::Runtime::extensions`].
    #[must_use]
    pub fn extensions(&self) -> &[LoadedExtension] {
        self.components.extensions()
    }

    /// See [`jk_wasm::Runtime::extension_ids`].
    #[must_use]
    pub fn extension_ids(&self) -> Vec<String> {
        self.components.extension_ids()
    }

    /// See [`jk_wasm::Runtime::report`].
    #[must_use]
    pub const fn report(&self) -> BootReport<'_> {
        self.components.report()
    }

    /// See [`jk_wasm::Runtime::start_all`].
    ///
    /// # Errors
    /// As [`jk_wasm::Runtime::start_all`].
    pub fn start_all(&self) -> Result<Vec<String>, CoreError> {
        self.components.start_all()
    }

    /// See [`jk_wasm::Runtime::start_all_eager`].
    ///
    /// # Errors
    /// As [`jk_wasm::Runtime::start_all_eager`].
    pub fn start_all_eager(&self) -> Result<Vec<String>, CoreError> {
        self.components.start_all_eager()
    }

    /// See [`jk_wasm::Runtime::adopt_installed`]. The next
    /// [`Self::build_agent`] serves what it adopted; a running
    /// [`AgentSession`] is not touched.
    ///
    /// # Errors
    /// As [`jk_wasm::Runtime::adopt_installed`].
    pub fn adopt_installed(&mut self, stem: &str) -> Result<String, CoreError> {
        self.components.adopt_installed(stem)
    }

    /// See [`jk_wasm::Runtime::egress_policy`].
    #[must_use]
    pub fn egress_policy(&self) -> egress::EgressPolicy {
        self.components.egress_policy()
    }

    /// Builds the thin-loop agent from config, ready for turns via [`conductor`].
    /// Guests with `persist: true` write to the same store as the log.
    ///
    /// # Errors
    /// Returns a [`CoreError`] if any interceptor or provider fails to instantiate
    /// or start.
    pub fn build_agent(
        &self,
        http_factory: &dyn Fn() -> route::HttpFn,
    ) -> Result<AgentSession, CoreError> {
        let store = Arc::clone(&self.store);
        let jk_wasm::Instantiated {
            interceptors,
            providers,
            tools,
        } = self.components.instantiate(http_factory, &|| {
            Arc::new(session_entries::SessionEntries::new(Arc::clone(&store)))
        })?;
        Ok(AgentSession {
            dispatcher: jk_protocol::intercept::Dispatcher::new(interceptors),
            providers,
            store,
            limits: self.limits(),
            tools,
        })
    }

    /// Bounds a turn runs under, from the top-level `limits:` block.
    ///
    /// The iteration cap stops a model that keeps calling tools forever. The
    /// default is a real limit for coding work, so `limits:` must be able to raise it.
    fn limits(&self) -> conductor::Limits {
        let configured = self
            .components
            .agent_config()
            .get("limits")
            .and_then(|limits| limits.get("max-iterations"))
            .and_then(serde_json::Value::as_u64)
            .and_then(|n| u32::try_from(n).ok())
            .filter(|n| *n > 0);
        conductor::Limits {
            max_iterations: configured.unwrap_or(conductor::DEFAULT_MAX_ITERATIONS),
        }
    }

    /// Opens the host-side store: a durable `SQLite` file at `storage.path`, or
    /// in-memory when unset.
    ///
    /// Storage is not an extension: a store guest would need `host-fs`, and the
    /// transcript is the most sensitive thing the runtime holds. Guests reach it
    /// only through `host-storage`, namespaced to themselves.
    ///
    /// A relative `path` resolves against the config directory, not the working
    /// directory. Called once at boot and shared by every agent (#233).
    fn open_store(
        agent: &serde_json::Value,
        config_dir: &Path,
    ) -> Result<Arc<Mutex<store::Store>>, CoreError> {
        let sqlite_path = agent
            .get("storage")
            .and_then(|storage| storage.get("path"))
            .and_then(serde_json::Value::as_str)
            .map(|path| config_dir.join(path));
        let store = sqlite_path
            .map_or_else(store::Store::open_in_memory, store::Store::open)
            .map_err(|source| CoreError::Store {
                message: source.to_string(),
            })?;
        // Converts transcripts written before the event log. Done here, not in
        // `Store::open`, so the store stays ignorant of payload format.
        //
        // A failure does not fail the boot: the `entries` rows survive and the
        // next open retries. Refusing to start would make one old session an unusable install.
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
}

/// A booted thin-loop agent: the interceptor dispatcher and provider fallback
/// chain, ready to run turns via [`conductor`].
pub struct AgentSession {
    dispatcher: jk_protocol::intercept::Dispatcher,
    providers: Vec<Box<dyn jk_protocol::intercept::Completer>>,
    store: Arc<Mutex<store::Store>>,
    /// Bounds this session's turns run under, from top-level `limits:`.
    limits: conductor::Limits,
    /// The enabled tool + registry extensions, dispatched by the loop as a `ToolInvoker`.
    tools: CombinedFleet,
}

impl AgentSession {
    /// Records whether a component adopted from [`Self::take_installed`] loaded.
    ///
    /// Written as a record, not a turn event, since loads happen between turns
    /// (see [`event_log::KIND_EXTENSION_LOADED`]). Best-effort: an unwritable
    /// store must not stop a session.
    pub fn record_load(&self, session: &str, stem: &str, outcome: Result<&str, &str>) {
        let (loaded, detail) = match outcome {
            Ok(id) => (true, id),
            Err(reason) => (false, reason),
        };
        let payload = event_log::envelope(&serde_json::json!({
            "stem": stem,
            "loaded": loaded,
            "detail": detail,
        }));
        if let Ok(store) = self.store.lock() {
            let _ = store.append_event(session, event_log::KIND_EXTENSION_LOADED, &payload);
        }
    }

    /// Drains the components installed by turns on this session.
    ///
    /// The seam between installing and loading (#214). `ext-install` runs inside
    /// a turn and cannot reach the [`Runtime`], so the driver adopts the stems
    /// between turns:
    ///
    /// ```ignore
    /// for stem in agent.take_installed() {
    ///     runtime.adopt_installed(&stem)?;
    ///     agent = runtime.build_agent(&factory)?;
    /// }
    /// ```
    ///
    /// Between turns, not during one, so a turn never runs half-reconfigured.
    /// Draining, so a caller that ignores this does not re-adopt on every turn.
    #[must_use]
    pub fn take_installed(&mut self) -> Vec<String> {
        self.tools.take_installed()
    }

    /// Run one turn headless, using the session's tool fleet. An interceptor
    /// `ask` resolves to its `default-answer`.
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
            None,
        )
    }

    /// Runs one turn with an explicit `driver` and `tools`, overriding the fleet.
    /// The seam a test injects stub tools through.
    pub fn run_with(
        &mut self,
        driver: &mut dyn turn::Driver,
        tools: &mut dyn jk_protocol::intercept::ToolInvoker,
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
            None,
        )
    }

    /// Runs one turn on the session's tool fleet, with `driver` answering interceptor `ask`s.
    pub fn run_driven(
        &mut self,
        driver: &mut dyn turn::Driver,
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
            None,
        )
    }

    /// Like [`Self::run_with`], but streams [`jk_protocol::turn::Event`]s to `sink` as the turn runs.
    pub fn run_streaming(
        &mut self,
        driver: &mut dyn turn::Driver,
        tools: &mut dyn jk_protocol::intercept::ToolInvoker,
        sink: &mut dyn turn::EventSink,
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
            None,
        )
    }

    /// Non-streaming turn on the session's tool fleet, with `driver` answering interceptor `ask`s.
    pub fn run_with_driver(
        &mut self,
        driver: &mut dyn turn::Driver,
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
            None,
        )
    }

    /// Like [`Self::run_with_driver`], with an authenticated request principal.
    pub fn run_with_driver_principal(
        &mut self,
        driver: &mut dyn turn::Driver,
        session: &str,
        message: &str,
        principal: Option<String>,
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
            principal,
        )
    }

    /// Streaming turn on the session's tool fleet, with `driver` answering interceptor `ask`s.
    /// Borrows `self.tools`, which [`Self::run_streaming`] cannot do under `&mut self`.
    pub fn run_streaming_with_driver(
        &mut self,
        driver: &mut dyn turn::Driver,
        sink: &mut dyn turn::EventSink,
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
            None,
        )
    }

    /// Like [`Self::run_streaming_with_driver`], with an authenticated request principal.
    pub fn run_streaming_with_driver_principal(
        &mut self,
        driver: &mut dyn turn::Driver,
        sink: &mut dyn turn::EventSink,
        session: &str,
        message: &str,
        principal: Option<String>,
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
            principal,
        )
    }

    /// Headless streaming turn on the session's tool fleet; an `ask` takes the
    /// prompt's default answer. Used by non-interactive surfaces.
    pub fn run_streaming_headless(
        &mut self,
        sink: &mut dyn turn::EventSink,
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
            None,
        )
    }

    /// Advertised names of the tools the loop can call. `&mut self` since #59:
    /// a pending fleet instantiates on this call.
    #[must_use]
    pub fn tool_names(&mut self) -> Vec<String> {
        self.tools.tool_names()
    }

    /// Whether the fleet has instantiated its pending `tool-*` guests (#59).
    /// Never goes back to `false`.
    #[must_use]
    pub const fn tools_instantiated(&self) -> bool {
        self.tools.tools_instantiated()
    }

    /// All tool + registry metadata as JSON (used in tests).
    ///
    /// # Errors
    /// Returns a [`CoreError`] if a pending tool or registry fails to
    /// instantiate or start on this, its first use.
    pub fn all_metas_json(&mut self) -> Result<serde_json::Value, CoreError> {
        self.tools.all_metas_json()
    }

    /// The durable transcript for `session`, oldest turn first. Reads from the
    /// host-side store, so it survives a `Runtime` restart against the same DB.
    #[must_use]
    pub fn transcript(&self, session: &str) -> Vec<turn::Message> {
        // Messages, not `store::Entry`: a projected turn has no namespace or key,
        // so entries would mean inventing fields. Unbounded, unlike `replay`.
        let events = self
            .store
            .lock()
            .map(|store| store.session_events(session).unwrap_or_default())
            .unwrap_or_default();
        projection::transcript(&events)
    }

    /// [`Self::transcript`], with each message paired to the log seq it was
    /// projected from, which [`Self::fork_session`] takes as `at_seq` (#106).
    #[must_use]
    pub fn placed_transcript(&self, session: &str) -> Vec<(u64, turn::Message)> {
        let events = self
            .store
            .lock()
            .map(|store| store.session_events(session).unwrap_or_default())
            .unwrap_or_default();
        projection::placed_transcript(&events)
    }

    /// Copies `from`'s log up to and including `at_seq` into `into`. The copy is
    /// a prefix, not a branch pointer, so nothing links the two sessions.
    ///
    /// # Errors
    /// Returns the store's error if `into` already has a log, or on a SQL failure.
    /// Copying nothing (`at_seq` 0, or no log) is `Ok(0)`; the REST surface decides if that is a mistake.
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

    /// What the loaded extensions contribute to a client's interface. Read fresh
    /// each call, since an invocation may change the set.
    pub fn contributions(&mut self) -> Vec<jk_protocol::contributions::Contributions> {
        self.dispatcher.contributions()
    }

    /// Run a contribution, routed to the extension that declared it.
    ///
    /// # Errors
    /// [`jk_protocol::contributions::InvokeError`] when no such extension or name is
    /// contributed, the arguments are unusable, or the extension fails.
    pub fn invoke_contribution(
        &mut self,
        extension: &str,
        name: &str,
        arguments: &[jk_protocol::contributions::ArgumentValue],
    ) -> Result<jk_protocol::contributions::InvokeOutcome, jk_protocol::contributions::InvokeError>
    {
        self.dispatcher
            .invoke_contribution(extension, name, arguments)
    }

    /// All known session ids, newest first.
    #[must_use]
    pub fn list_sessions(&self) -> Vec<String> {
        // Sessions come from the log, not `entries`: an empty list is a legitimate
        // answer, so a read of `entries` would go silently wrong once that write goes away.
        // A pre-log database has entries but no events until the migration runs.
        self.store
            .lock()
            .map(|store| {
                // Not `unwrap_or_default()`: that would report a broken query as "no sessions".
                store.event_sessions().unwrap_or_else(|err| {
                    eprintln!("WARN [core] listing sessions failed: {err}");
                    Vec::new()
                })
            })
            .unwrap_or_default()
            .into_iter()
            // Interceptor namespaces (`ext/<component>/…`) are never event sessions;
            // the filter guards against a slashed id reaching the picker.
            .filter(|session| !session.contains('/'))
            .collect()
    }
}

/// Drives one turn through the conductor. A free function so callers can pass
/// disjoint `&mut` borrows of session fields in one call.
#[allow(clippy::too_many_arguments)]
fn run_and_persist(
    dispatcher: &mut jk_protocol::intercept::Dispatcher,
    providers: &mut [Box<dyn jk_protocol::intercept::Completer>],
    store: &Mutex<store::Store>,
    limits: conductor::Limits,
    tools: &mut dyn jk_protocol::intercept::ToolInvoker,
    driver: &mut dyn turn::Driver,
    sink: &mut dyn turn::EventSink,
    session: &str,
    message: &str,
    principal: Option<String>,
) -> conductor::RunResult {
    // Locked only around each use, never across the turn: a guest writing
    // `host-storage` mid-dispatch takes the same lock and would deadlock.
    let history = store
        .lock()
        .map(|store| replay(&store, session))
        .unwrap_or_default();
    // Every entry point funnels through here, so every turn is logged without opting in.
    let mut logged_sink = event_log::PersistingSink::new(sink, store, session);
    let mut logged_driver = event_log::PersistingDriver::new(driver, store, session);
    // Logs the message the model receives, not `message`: a `before-loop`
    // interceptor may `replace` it (#84). Called once, before any event, so this row opens the log.
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
        principal,
        &mut log_effective_message,
    );
    // No transcript append: the turn records itself as it runs. A second
    // `{user, answer}` row would duplicate the session in two formats.
    result
}

/// How many past turns are replayed into a new one. Token-aware trimming is the
/// context interceptor's job. The bound is applied in SQL by `Store::recent_turns`,
/// since a turn is variable rows and a plain `LIMIT` cannot express it (#85).
const REPLAYED_TURNS: u32 = 20;

/// The conversation so far, oldest first, as loop messages. An unreadable row
/// is skipped rather than failing the turn.
fn replay(store: &store::Store, session: &str) -> Vec<turn::Message> {
    // Reads the event log, not `entries`. `recent_turns` is already oldest first.
    let events = store
        .recent_turns(session, REPLAYED_TURNS)
        .unwrap_or_default();
    projection::transcript(&events)
}

/// Headless driver: an `ask` takes the prompt's `default-answer`.
///
/// Public because `mcp` needs it. An MCP server owns stdin for protocol frames,
/// so a terminal prompt would read a frame as an answer.
pub struct HeadlessDriver;
impl turn::Driver for HeadlessDriver {
    fn ask(&mut self, prompt: &turn::UserPrompt) -> String {
        prompt.default_answer.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `Runtime` must be `Send + Sync`: each session builds its own agent (#228).
    #[test]
    fn a_runtime_can_be_shared_across_threads() {
        const fn needs<T: Send + Sync>() {}
        needs::<Runtime>();
    }

    /// A `before-loop` interceptor that always `replace`s the user message. Duplicates
    /// the stub in `pkgs/host/protocol/tests/intercept.rs`, which cannot reach `run_and_persist`.
    struct RewriteBeforeLoop {
        replacement: String,
    }
    impl jk_protocol::intercept::Interceptor for RewriteBeforeLoop {
        fn id(&self) -> &'static str {
            "rewrite"
        }
        fn subscribed_phases(&self) -> Vec<jk_protocol::intercept::Phase> {
            vec![jk_protocol::intercept::Phase::BeforeLoop]
        }
        fn intercept(
            &mut self,
            _input: &jk_protocol::intercept::InterceptInput,
        ) -> Result<jk_protocol::intercept::Decision, jk_protocol::intercept::InterceptorError>
        {
            Ok(jk_protocol::intercept::Decision::Replace(
                jk_protocol::intercept::HookState::BeforeLoop(jk_protocol::intercept::UserTurn {
                    session: "s".to_string(),
                    user_message: self.replacement.clone(),
                    principal: None,
                }),
            ))
        }
    }

    /// Its answer is irrelevant here; present only so the turn completes.
    struct FixedAnswer;
    impl jk_protocol::intercept::Completer for FixedAnswer {
        fn id(&self) -> &'static str {
            "p"
        }
        fn complete(
            &mut self,
            _request: &jk_protocol::intercept::PendingRequest,
        ) -> Result<jk_protocol::intercept::Completion, String> {
            Ok(jk_protocol::intercept::Completion {
                text: "hi".to_string(),
                tool_calls: vec![],
                finish_reason: "stop".to_string(),
            })
        }
    }

    /// #84: the log must hold the message a `before-loop` interceptor produced,
    /// not the caller's `message`. Drives the private `run_and_persist` and reads
    /// back the first log row.
    #[test]
    fn the_log_holds_the_message_a_before_loop_rewrite_produced() {
        let store = Mutex::new(store::Store::open_in_memory().unwrap());
        let mut dispatcher =
            jk_protocol::intercept::Dispatcher::new(vec![Box::new(RewriteBeforeLoop {
                replacement: "rewritten by the interceptor".to_string(),
            })]);
        let mut providers: Vec<Box<dyn jk_protocol::intercept::Completer>> =
            vec![Box::new(FixedAnswer)];
        let mut tools = jk_protocol::intercept::NoTools;
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
            None,
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
