//! A session owns a thread, a thread owns an agent (#229); `AgentSession` is `!Send`, so each session builds its own (1.35 MB, ~3.5 ms).
//!
//! See [the decision record](../../../../docs/decisions/2026-09-18-two-turns-at-once/Decision.md).
//! `/sessions`, `/contributions` and `/contributions/invoke` use a dedicated housekeeping agent so answers do not depend on who is connected.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use jk_agent::{AgentSession, Runtime};
use jk_wasm::route::HttpFn;

use crate::session_thread::{self, Jobs};

/// Builds the HTTP client an agent's providers use. `Send + Sync` because each session builds its agent on its own thread.
pub type Factory = Arc<dyn Fn() -> HttpFn + Send + Sync>;

/// One session's thread, and the queue that reaches it.
struct Live {
    jobs: Jobs<AgentSession>,
    thread: std::thread::JoinHandle<()>,
    /// Which runtime generation this agent was built against; older means it cannot see later installs.
    built_at: u64,
}

/// Every live session's agent, built on first use.
pub struct Agents {
    /// Behind a lock because adoption needs `&mut Runtime` while session threads build with `&Runtime`; both are rare and short.
    runtime: Arc<std::sync::RwLock<Runtime>>,
    /// A handle back to itself for the session threads; `Weak`, since an `Arc` would be a cycle.
    me: std::sync::Weak<Self>,
    factory: Factory,
    /// `session id -> its agent's queue`, plus the housekeeping agent under a key no session can have.
    live: Mutex<HashMap<String, Live>>,
    /// Bumped whenever something is adopted. An agent built before the bump is replaced at its next request; nothing live is
    /// stopped, so a mid-adoption request is served rather than refused with `503`.
    generation: std::sync::atomic::AtomicU64,
}

/// The housekeeping agent's key. A slash cannot appear in a session id (routes are `/session/{id}/…`), so it cannot collide.
const HOUSEKEEPING: &str = "/housekeeping";

impl Agents {
    /// A registry over `runtime`, with nothing built yet.
    #[must_use]
    /// Takes the `Runtime` by value because adoption mutates it; `build_agent` only borrows, so callers can build their own agent first.
    pub fn new(runtime: Runtime, factory: Factory) -> Arc<Self> {
        Arc::new_cyclic(|me| Self {
            runtime: Arc::new(std::sync::RwLock::new(runtime)),
            me: me.clone(),
            factory,
            live: Mutex::new(HashMap::new()),
            generation: std::sync::atomic::AtomicU64::new(0),
        })
    }

    /// The queue for `session`, building its agent on first use (not at `session/create`, so merely mentioned sessions cost nothing).
    pub fn of(&self, session: &str) -> Jobs<AgentSession> {
        self.queue(session)
    }

    /// The queue for work that belongs to no session.
    pub fn housekeeping(&self) -> Jobs<AgentSession> {
        self.queue(HOUSEKEEPING)
    }

    /// How many agents are live.
    #[must_use]
    pub fn live(&self) -> usize {
        self.lock().len()
    }

    fn queue(&self, key: &str) -> Jobs<AgentSession> {
        let now = self.generation.load(std::sync::atomic::Ordering::Acquire);
        let mut live = self.lock();
        match live.get(key) {
            Some(existing) if existing.built_at == now => return existing.jobs.clone(),
            Some(_) => {
                if let Some(stale) = live.remove(key) {
                    stale.jobs.stop();
                }
            }
            None => {}
        }
        let (jobs, queued) = session_thread::queue::<AgentSession>();
        let runtime = Arc::clone(&self.runtime);
        let factory = Arc::clone(&self.factory);
        let registry = self.me.clone();
        let named = key.to_owned();
        let thread = std::thread::spawn(move || {
            let built = runtime
                .read()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .build_agent(&*factory);
            match built {
                Ok(mut agent) => session_thread::serve_each(&mut agent, &queued, |agent| {
                    if let Some(registry) = registry.upgrade() {
                        registry.after_turn(&named, agent);
                    }
                }),
                Err(err) => eprintln!("ERROR [host] session `{named}` has no agent: {err}"),
            }
        });
        let handle = jobs.clone();
        live.insert(
            key.to_owned(),
            Live {
                jobs,
                thread,
                built_at: now,
            },
        );
        handle
    }

    /// Adopt whatever `agent`'s last turn installed, and retire every agent so the next request builds one that can see it.
    ///
    /// Every session, the installer included, sees it at its next turn. Nothing is stopped or rebuilt here: adoption bumps a
    /// generation (stopping outright `503`ed requests that arrived mid-adoption).
    fn after_turn(&self, session: &str, agent: &mut AgentSession) {
        let installed = agent.take_installed();
        if installed.is_empty() {
            return;
        }
        for stem in &installed {
            let outcome = self
                .runtime
                .write()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .adopt_installed(stem);
            match &outcome {
                Ok(id) => agent.record_load(session, stem, Ok(id.as_str())),
                Err(err) => agent.record_load(session, stem, Err(&err.to_string())),
            }
        }
        self.generation
            .fetch_add(1, std::sync::atomic::Ordering::AcqRel);
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, Live>> {
        self.live
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

impl Drop for Agents {
    /// End every session's thread, and wait for it.
    /// Each loop is told to stop (an outstanding handle would hang the join); queued work still runs. Waiting makes shutdown observable to tests.
    fn drop(&mut self) {
        let live = std::mem::take(&mut *self.lock());
        let mut threads = Vec::with_capacity(live.len());
        for (_, Live { jobs, thread, .. }) in live {
            jobs.stop();
            drop(jobs);
            threads.push(thread);
        }
        for thread in threads {
            let _ = thread.join();
        }
    }
}
