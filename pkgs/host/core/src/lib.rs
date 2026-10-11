//! Jan-Klod core: the harness that composes the component runtime
//! ([`jk_wasm`]) with the session store ([`jk_session`]) and runs turns on
//! them.
//!
//! [`Runtime`] boots both halves — the components and the one store every
//! agent shares — and [`Runtime::build_agent`] turns them into an
//! [`AgentSession`], whose turns run through the [`conductor`] and land in the
//! event log. Nothing here names `wasmtime`: what a component is, and what it
//! may reach, is `jk-wasm`'s business (#181).

pub mod conductor;
pub mod delegate;
pub mod session;
mod session_entries;

use jan_klod_protocol::turn;
use jk_session::{event_log, projection, store};
use jk_wasm::{egress, route, BootReport, CombinedFleet, CoreError, LoadedExtension};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// A booted core: the component runtime, and the one session store every
/// agent it builds shares.
pub struct Runtime {
    /// Engine, linker and every enabled instance resolved against `ext/`.
    components: jk_wasm::Runtime,
    /// The one store, shared by every agent this runtime builds.
    ///
    /// One per runtime rather than one per agent: sessions have to agree
    /// about what has been said, and with per-session agents (#229) a
    /// store each meant they did not (#233).
    store: Arc<Mutex<store::Store>>,
}

impl Runtime {
    /// Boot the components ([`jk_wasm::Runtime::boot`]) and open the session
    /// store.
    ///
    /// The store is opened here rather than per agent, and so a
    /// `storage.path` that cannot be opened fails the boot — where an
    /// operator is already reading output — instead of the first request.
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

    /// Boot the thin-loop agent from config: the guests
    /// [`jk_wasm::Runtime::instantiate`] hands back, behind an interceptor
    /// dispatcher, with this runtime's store and `limits:`, ready for turns
    /// via [`conductor`].
    ///
    /// A guest that opts into `persist: true` writes to the same store the
    /// log does, through [`session_entries::SessionEntries`].
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
            dispatcher: jan_klod_protocol::intercept::Dispatcher::new(interceptors),
            providers,
            store,
            limits: self.limits(),
            tools,
        })
    }

    /// Bounds a turn runs under, from the top-level `limits:` block.
    ///
    /// The cycle cap prevents a model that keeps emitting tool calls from spinning
    /// (or spending, on metered endpoints) forever. Eight is a real constraint
    /// for coding work, so it has to be raisable by whoever is paying.
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

    /// Open the host-side persistent store: top-level `storage.path` gives a
    /// durable `SQLite` file, its absence gives ephemeral in-memory.
    ///
    /// Storage is **not** an extension: a store guest would need `host-fs`
    /// granted back to it, and the transcript is the most sensitive thing the
    /// runtime holds. Guests reach it only via `host-storage`, namespaced to
    /// themselves.
    ///
    /// A relative `path` resolves against the directory holding `config.yaml`,
    /// not the working directory — otherwise each startup directory gets its
    /// own `jan-klod.db` and conversation history.
    ///
    /// Called **once**, at boot, and shared by every agent (#233). Opened
    /// per agent it was a different database per session whenever no path
    /// was configured, so a transcript existed for the session that wrote
    /// it and for nobody else — and with a path it was several handles to
    /// one file agreeing by luck.
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
}

/// A booted thin-loop agent: the interceptor dispatcher and provider fallback
/// chain, ready to run turns via [`conductor`].
pub struct AgentSession {
    dispatcher: jan_klod_protocol::intercept::Dispatcher,
    providers: Vec<Box<dyn jan_klod_protocol::intercept::Completer>>,
    store: Arc<Mutex<store::Store>>,
    /// Bounds this session's turns run under, from top-level `limits:`.
    limits: conductor::Limits,
    /// The enabled tool + registry extensions, dispatched by the loop as a `ToolInvoker`.
    tools: CombinedFleet,
}

impl AgentSession {
    /// Record that a component was adopted into the runtime, or refused.
    ///
    /// The other half of [`Self::take_installed`]: the install says
    /// "callable from the next turn" and that promise can fail, so the log
    /// has to say whether it held. Written as a record rather than a turn
    /// event because a load happens *between* turns — see
    /// [`event_log::KIND_EXTENSION_LOADED`].
    ///
    /// Best-effort, like every other append: a store that cannot be written
    /// must not stop a session from running.
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

    /// Components installed by turns on this session, drained.
    ///
    /// **The seam between installing and loading** (#214). `ext-install`
    /// runs inside a turn, from inside the fleet, and cannot reach the
    /// [`Runtime`] that would compile what it landed — so it leaves the
    /// stem here and whoever drives turns picks it up *between* them:
    ///
    /// ```ignore
    /// for stem in agent.take_installed() {
    ///     runtime.adopt_installed(&stem)?;
    ///     agent = runtime.build_agent(&factory)?;
    /// }
    /// ```
    ///
    /// Between turns and not during one, deliberately. A turn runs on the
    /// fleet it started with, which is the only answer that cannot leave a
    /// turn half-reconfigured — and rebuilding is ~9 ms, so the wait costs
    /// the next turn nothing worth measuring.
    ///
    /// Draining, so a caller that ignores it does not adopt the same
    /// component on every turn for the rest of the session.
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

    /// Run one turn with an explicit `driver` and `tools` (overriding the fleet).
    /// On completion, user message + answer are appended to the session's
    /// durable transcript. The seam a test injects stub tools through.
    pub fn run_with(
        &mut self,
        driver: &mut dyn turn::Driver,
        tools: &mut dyn jan_klod_protocol::intercept::ToolInvoker,
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

    /// Run one turn with the session's tool fleet but an explicit `driver` (for
    /// a client to answer an interceptor `ask` — e.g. permission confirmation).
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

    /// Like [`Self::run_with`], but streams incremental [`jan_klod_protocol::turn::Event`]s to
    /// `sink` as the turn runs (live TUI transcript or SSE).
    pub fn run_streaming(
        &mut self,
        driver: &mut dyn turn::Driver,
        tools: &mut dyn jan_klod_protocol::intercept::ToolInvoker,
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

    /// A non-streaming turn using the session's own tool fleet, with `driver`
    /// answering any interceptor `ask` — the entry a chat surface uses,
    /// no event stream but someone to ask.
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

    /// Non-streaming turn with principal: like [`Self::run_with_driver`] but
    /// with an authenticated principal from the request (e.g. REST bearer token).
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

    /// Streaming turn using the session's own tool fleet, with `driver` answering
    /// any interceptor `ask` — the entry the REST surface's SSE handler uses.
    ///
    /// [`Self::run_streaming`] exists for callers with their own fleet;
    /// this one borrows `self.tools`, which a caller cannot do while holding
    /// `&mut self`.
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

    /// Streaming turn with principal: like [`Self::run_streaming_with_driver`] but
    /// with an authenticated principal from the request (e.g. REST bearer token).
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

    /// Headless streaming turn using the session's tool fleet: an `ask` takes
    /// the prompt's default answer (denial for the permission gate). Used by
    /// non-interactive surfaces (Telegram, the blocking JSON turn).
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

    /// The names of the tools the loop can call (advertised names from the
    /// fleet). `&mut self` since #59: a fleet nothing has asked for yet
    /// instantiates here on this call, not before.
    #[must_use]
    pub fn tool_names(&mut self) -> Vec<String> {
        self.tools.tool_names()
    }

    /// Whether the tool fleet has instantiated its pending `tool-*` guests yet
    /// (#59). `false` right after `build_agent` returns for a config with no
    /// interceptor to advertise them to; `true` from the first turn's `invoke`
    /// or metadata request — never goes back to `false`.
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

    /// [`Self::transcript`], with each message paired to the log seq it was
    /// projected from — the number [`Self::fork`] takes as `at_seq`.
    ///
    /// Serving these together is the point: a client that reads a session gets
    /// the fork points with it, instead of needing a number the protocol never
    /// gave it ([#106](https://github.com/PromptPasture/jan-klod/issues/106)).
    #[must_use]
    pub fn placed_transcript(&self, session: &str) -> Vec<(u64, turn::Message)> {
        let events = self
            .store
            .lock()
            .map(|store| store.session_events(session).unwrap_or_default())
            .unwrap_or_default();
        projection::placed_transcript(&events)
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

    /// What the loaded extensions contribute to a client's interface.
    ///
    /// Read from the components each time rather than cached: an invocation
    /// may report that the set changed, and a cache would leave every attached
    /// client showing the old one.
    pub fn contributions(&mut self) -> Vec<jan_klod_protocol::contributions::Contributions> {
        self.dispatcher.contributions()
    }

    /// Run a contribution, routed to the extension that declared it.
    ///
    /// # Errors
    /// [`jan_klod_protocol::contributions::InvokeError`] when no such extension or name is
    /// contributed, the arguments are unusable, or the extension fails.
    pub fn invoke_contribution(
        &mut self,
        extension: &str,
        name: &str,
        arguments: &[jan_klod_protocol::contributions::ArgumentValue],
    ) -> Result<
        jan_klod_protocol::contributions::InvokeOutcome,
        jan_klod_protocol::contributions::InvokeError,
    > {
        self.dispatcher
            .invoke_contribution(extension, name, arguments)
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
/// A free function (not a method) so `run`/streaming can pass disjoint `&mut`
/// borrows of session fields (dispatcher, providers, tools) in one call.
#[allow(clippy::too_many_arguments)]
fn run_and_persist(
    dispatcher: &mut jan_klod_protocol::intercept::Dispatcher,
    providers: &mut [Box<dyn jan_klod_protocol::intercept::Completer>],
    store: &Mutex<store::Store>,
    limits: conductor::Limits,
    tools: &mut dyn jan_klod_protocol::intercept::ToolInvoker,
    driver: &mut dyn turn::Driver,
    sink: &mut dyn turn::EventSink,
    session: &str,
    message: &str,
    principal: Option<String>,
) -> conductor::RunResult {
    // Locked around each use, never across the turn: an interceptor writing its
    // own `host-storage` mid-dispatch takes the same lock, holding it here
    // would deadlock the first guest that remembered anything.
    let history = store
        .lock()
        .map(|store| replay(&store, session))
        .unwrap_or_default();
    // The event log, wrapped around whatever sink and driver the caller passed.
    // Every entry point into a turn funnels through here, one wrap covers
    // them all — `run`, `run_with`, `run_streaming` etc. cannot acquire
    // an unlogged turn by forgetting to opt in.
    let mut logged_sink = event_log::PersistingSink::new(sink, store, session);
    let mut logged_driver = event_log::PersistingDriver::new(driver, store, session);
    // Logs the message the model is actually about to receive, not `message`
    // itself: a `before-loop` interceptor may `replace` it before the
    // conductor resolves `effective_message` (see `conductor::run_turn`'s
    // `on_effective_message` hook), the log records what happened, not what
    // was asked — #84. The conductor calls this exactly once, when
    // `effective_message` is resolved and before it emits a single event,
    // so this row opens the session's log ahead of everything the turn records.
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
    // No transcript append. The turn recorded itself as it ran — user message
    // before `run_turn`, every event through the sink — writing a `{user, answer}`
    // row here too would duplicate the session in two formats, which this phase
    // removes. A migration converts rows written before that was true.
    result
}

/// How many past turns are replayed into a new one, bounded here and in
/// `select-context`; token-aware trimming is the context interceptor's job.
///
/// This used to bound the store read (`recent(session, 20)` over transcript),
/// then stopped when #45's box 4 moved the log into memory and trimmed it via
/// `projection::last_turns`, because bounding an event log by *turns* is not
/// a plain `LIMIT` operation — a turn is variable rows. That made every turn
/// read and decode a session's entire log to keep the last 20, a cost growing
/// unbounded in session length (#85). `Store::recent_turns` puts the rule back
/// in SQL — the bound's seq is itself a query, not a `LIMIT` — so the read is
/// bounded in the store, not after.
const REPLAYED_TURNS: u32 = 20;

/// The conversation so far, oldest-first, as loop messages.
///
/// Each stored turn is `{"user":…,"answer":…}`; a malformed or unreadable entry
/// is skipped rather than failing the turn — a corrupt transcript row should cost
/// context, not the ability to talk.
fn replay(store: &store::Store, session: &str) -> Vec<turn::Message> {
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

/// Headless driver: no interactive surface, so an `ask` takes the prompt's
/// `default-answer`.
///
/// Public because `mcp` needs it and must not have any other kind. An MCP
/// server owns **stdin for protocol frames**, so a driver that prompted on the
/// terminal would read a frame as an answer and the client's next request would
/// vanish into a confirmation. The default answer is a refusal, which is also
/// the right policy there — an editor cannot answer a confirmation prompt.
pub struct HeadlessDriver;
impl turn::Driver for HeadlessDriver {
    fn ask(&mut self, prompt: &turn::UserPrompt) -> String {
        prompt.default_answer.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The core's `Runtime` crosses threads for the same reason the
    /// component runtime does (#228): each session builds its own agent.
    #[test]
    fn a_runtime_can_be_shared_across_threads() {
        const fn needs<T: Send + Sync>() {}
        needs::<Runtime>();
    }

    /// A `before-loop` interceptor that unconditionally `replace`s the user
    /// message — the in-process stub `pkgs/host/protocol/tests/intercept.rs` already uses for
    /// exactly this phase (`Behavior::ReplaceUserMessage`), reproduced here
    /// because that file only sees `jan_klod_core`'s public API and cannot
    /// reach `run_and_persist`, which is private to this crate.
    struct RewriteBeforeLoop {
        replacement: String,
    }
    impl jan_klod_protocol::intercept::Interceptor for RewriteBeforeLoop {
        fn id(&self) -> &'static str {
            "rewrite"
        }
        fn subscribed_phases(&self) -> Vec<jan_klod_protocol::intercept::Phase> {
            vec![jan_klod_protocol::intercept::Phase::BeforeLoop]
        }
        fn intercept(
            &mut self,
            _input: &jan_klod_protocol::intercept::InterceptInput,
        ) -> Result<
            jan_klod_protocol::intercept::Decision,
            jan_klod_protocol::intercept::InterceptorError,
        > {
            Ok(jan_klod_protocol::intercept::Decision::Replace(
                jan_klod_protocol::intercept::HookState::BeforeLoop(
                    jan_klod_protocol::intercept::UserTurn {
                        session: "s".to_string(),
                        user_message: self.replacement.clone(),
                        principal: None,
                    },
                ),
            ))
        }
    }

    /// A provider whose answer plays no part in what this test checks —
    /// present only so the turn has something to complete with.
    struct FixedAnswer;
    impl jan_klod_protocol::intercept::Completer for FixedAnswer {
        fn id(&self) -> &'static str {
            "p"
        }
        fn complete(
            &mut self,
            _request: &jan_klod_protocol::intercept::PendingRequest,
        ) -> Result<jan_klod_protocol::intercept::Completion, String> {
            Ok(jan_klod_protocol::intercept::Completion {
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
    /// crate, hence a test here rather than in `pkgs/host/core/tests/`) with a
    /// `before-loop` interceptor that rewrites the message, and reading back
    /// the very first row of the session's log.
    #[test]
    fn the_log_holds_the_message_a_before_loop_rewrite_produced() {
        let store = Mutex::new(store::Store::open_in_memory().unwrap());
        let mut dispatcher =
            jan_klod_protocol::intercept::Dispatcher::new(vec![Box::new(RewriteBeforeLoop {
                replacement: "rewritten by the interceptor".to_string(),
            })]);
        let mut providers: Vec<Box<dyn jan_klod_protocol::intercept::Completer>> =
            vec![Box::new(FixedAnswer)];
        let mut tools = jan_klod_protocol::intercept::NoTools;
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
