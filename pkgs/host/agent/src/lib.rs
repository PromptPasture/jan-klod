//! Composes the component runtime and session store, and runs turns on them.

pub mod conductor;
pub mod delegate;
pub mod session;
mod session_entries;

use jk_protocol::turn;
use jk_session::{event_log, projection, store};
use jk_wasm::{egress, route, BootReport, CombinedFleet, CoreError, LoadedExtension};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// A booted core: the component runtime plus the session store every agent shares.
pub struct Runtime {
    components: jk_wasm::Runtime,
    /// One store per runtime, not per agent, so sessions agree on what was said.
    store: Arc<Mutex<store::Store>>,
}

impl Runtime {
    /// Boots the components and opens the session store; a bad `storage.path` fails here.
    ///
    /// # Errors
    /// Whatever [`jk_wasm::Runtime::boot`] returns, or [`CoreError::Store`].
    pub fn boot(
        config_path: impl AsRef<Path>,
        ext_dir: impl AsRef<Path>,
    ) -> Result<Self, CoreError> {
        let config_dir = config_path
            .as_ref()
            .parent()
            .map_or_else(|| PathBuf::from("."), Path::to_path_buf);
        Self::boot_with_store_dir(config_path, ext_dir, &config_dir)
    }

    /// As [`Self::boot`], but a relative `storage.path` resolves against `store_dir`.
    ///
    /// # Errors
    /// As [`Self::boot`].
    pub fn boot_with_store_dir(
        config_path: impl AsRef<Path>,
        ext_dir: impl AsRef<Path>,
        store_dir: &Path,
    ) -> Result<Self, CoreError> {
        let components = jk_wasm::Runtime::boot(config_path, ext_dir)?;
        let store = Self::open_store(components.agent_config(), store_dir)?;
        Ok(Self { components, store })
    }

    /// The underlying component runtime.
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

    /// Adopts an installed component; the next [`Self::build_agent`] serves it.
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

    /// Builds the thin-loop agent from config.
    ///
    /// # Errors
    /// Returns a [`CoreError`] if an interceptor or provider fails to instantiate or start.
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

    /// The iteration cap stops a model that calls tools forever; `limits:` can raise it.
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

    /// Storage is not an extension: a store guest would need `host-fs` and see the transcript.
    fn open_store(
        agent: &serde_json::Value,
        store_dir: &Path,
    ) -> Result<Arc<Mutex<store::Store>>, CoreError> {
        let sqlite_path = agent
            .get("storage")
            .and_then(|storage| storage.get("path"))
            .and_then(serde_json::Value::as_str)
            .map(|path| store_dir.join(path));
        let store = sqlite_path
            .map_or_else(store::Store::open_in_memory, store::Store::open)
            .map_err(|source| CoreError::Store {
                message: source.to_string(),
            })?;
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
    limits: conductor::Limits,
    tools: CombinedFleet,
}

impl AgentSession {
    /// Records whether an adopted component loaded; best-effort, since loads happen between turns.
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

    /// Drains installed stems; adopt them between turns so a turn never runs half-reconfigured.
    #[must_use]
    pub fn take_installed(&mut self) -> Vec<String> {
        self.tools.take_installed()
    }

    /// Runs one turn headless; an interceptor `ask` takes its `default-answer`.
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

    /// Runs one turn on the fleet, with `driver` answering interceptor `ask`s.
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

    /// Like [`Self::run_with`], but streams [`jk_protocol::turn::Event`]s to `sink`.
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

    /// Non-streaming turn on the fleet; `driver` answers interceptor `ask`s.
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

    /// Streaming counterpart of [`Self::run_with_driver`].
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

    /// Headless streaming turn; an `ask` takes the prompt's default answer.
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

    /// Advertised tool names; `&mut self` because a pending fleet instantiates here.
    #[must_use]
    pub fn tool_names(&mut self) -> Vec<String> {
        self.tools.tool_names()
    }

    /// Whether the fleet has instantiated its pending `tool-*` guests; never reverts.
    #[must_use]
    pub const fn tools_instantiated(&self) -> bool {
        self.tools.tools_instantiated()
    }

    /// All tool and registry metadata as JSON.
    ///
    /// # Errors
    /// Returns a [`CoreError`] if a pending tool or registry fails to instantiate or start.
    pub fn all_metas_json(&mut self) -> Result<serde_json::Value, CoreError> {
        self.tools.all_metas_json()
    }

    /// The durable transcript for `session`, oldest first, read from the store.
    #[must_use]
    pub fn transcript(&self, session: &str) -> Vec<turn::Message> {
        let events = self
            .store
            .lock()
            .map(|store| store.session_events(session).unwrap_or_default())
            .unwrap_or_default();
        projection::transcript(&events)
    }

    /// [`Self::transcript`] with each message's log seq, the `at_seq` for [`Self::fork_session`].
    #[must_use]
    pub fn placed_transcript(&self, session: &str) -> Vec<(u64, turn::Message)> {
        let events = self
            .store
            .lock()
            .map(|store| store.session_events(session).unwrap_or_default())
            .unwrap_or_default();
        projection::placed_transcript(&events)
    }

    /// Copies `from`'s log up to and including `at_seq` into `into`, as a prefix.
    ///
    /// # Errors
    /// If `into` already has a log, or on a SQL failure; copying nothing is `Ok(0)`.
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

    /// What loaded extensions contribute to a client's interface, read fresh each call.
    pub fn contributions(&mut self) -> Vec<jk_protocol::contributions::Contributions> {
        self.dispatcher.contributions()
    }

    /// Runs a contribution, routed to the extension that declared it.
    ///
    /// # Errors
    /// [`jk_protocol::contributions::InvokeError`] if the contribution is unknown, unusable or fails.
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
        self.store
            .lock()
            .map(|store| {
                store.event_sessions().unwrap_or_else(|err| {
                    eprintln!("WARN [core] listing sessions failed: {err}");
                    Vec::new()
                })
            })
            .unwrap_or_default()
            .into_iter()
            .filter(|session| !session.contains('/'))
            .collect()
    }
}

/// Free function so callers can pass disjoint `&mut` borrows of session fields.
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
    let history = store
        .lock()
        .map(|store| replay(&store, session))
        .unwrap_or_default();
    let mut logged_sink = event_log::PersistingSink::new(sink, store, session);
    let mut logged_driver = event_log::PersistingDriver::new(driver, store, session);
    let mut log_effective_message =
        |effective: &str| event_log::log_user_message(store, session, effective);
    conductor::run_turn(
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
    )
}

/// Token-aware trimming is the context interceptor's job; `Store::recent_turns` bounds this in SQL.
const REPLAYED_TURNS: u32 = 20;

/// The conversation so far, oldest first; an unreadable row is skipped, not fatal.
fn replay(store: &store::Store, session: &str) -> Vec<turn::Message> {
    let events = store
        .recent_turns(session, REPLAYED_TURNS)
        .unwrap_or_default();
    projection::transcript(&events)
}

/// Headless driver: an `ask` takes the prompt's `default-answer`.
pub struct HeadlessDriver;
impl turn::Driver for HeadlessDriver {
    fn ask(&mut self, prompt: &turn::UserPrompt) -> String {
        prompt.default_answer.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_runtime_can_be_shared_across_threads() {
        const fn needs<T: Send + Sync>() {}
        needs::<Runtime>();
    }

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
