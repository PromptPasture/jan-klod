//! A session owns a thread, a thread owns an agent (#229).
//!
//! `AgentSession` is `!Send` and a turn holds it for its whole life,
//! including while parked on a person — so one agent means one turn at a
//! time for the whole process. Phase 20's gate asks for two sessions live
//! at once, and [the decision
//! record](../../../docs/decisions/2026-09-18-two-turns-at-once/Decision.md)
//! took the route the measurements favoured: an agent each.
//!
//! What makes that affordable is that almost nothing is duplicated. The
//! store is shared already (`Arc<Mutex<Store>>`), so sessions write one
//! database; what a second agent costs is its wasm instances, measured at
//! **1.35 MB and ~3.5 ms**.
//!
//! What makes it *possible* is that `Runtime` is `Send + Sync` and
//! `build_agent` takes `&self`, so each thread builds its own agent and
//! nothing `!Send` ever crosses a thread boundary.
//!
//! # The housekeeping agent
//!
//! `/sessions`, `/contributions` and `/contributions/invoke` are not
//! session-scoped but are served *through* an agent, because that is how
//! the store is read. They get one of their own rather than borrowing a
//! live session's — which would make the answer depend on who happened to
//! be connected.
//!
//! It costs one more agent. The open question it leaves is
//! `contributions/invoke`, which would act on *its* extension instances
//! and no session's; that is the same question #231 asks about adoption,
//! and the two want one answer rather than two.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use jan_klod_core::route::HttpFn;
use jan_klod_core::{AgentSession, Runtime};

use crate::session_thread::{self, Jobs};

/// Builds the HTTP client an agent's providers use.
///
/// `Send + Sync` because each session builds its agent on its own thread.
/// `HttpFn` was already both; the factory had no bounds while only one
/// thread ever called it.
pub type Factory = Arc<dyn Fn() -> HttpFn + Send + Sync>;

/// One session's thread, and the queue that reaches it.
struct Live {
    jobs: Jobs<AgentSession>,
    thread: std::thread::JoinHandle<()>,
    /// Which generation of the runtime this agent was built against.
    /// Older than the current one means it cannot see something that has
    /// since been installed.
    built_at: u64,
}

/// Every live session's agent, built on first use.
pub struct Agents {
    /// Behind a lock because adoption needs `&mut Runtime` while every
    /// session thread needs `&Runtime` to build with. Both are rare and
    /// short: a build is 3.5 ms, an adoption is one component.
    runtime: Arc<std::sync::RwLock<Runtime>>,
    /// A handle back to itself, for the session threads. `Weak`, because
    /// an `Arc` here would be a cycle: the registry owns the threads and
    /// the threads would own the registry, and nothing would ever drop.
    me: std::sync::Weak<Self>,
    factory: Factory,
    /// `session id -> its agent's queue`. The housekeeping agent is in
    /// here too, under a key no session can have.
    live: Mutex<HashMap<String, Live>>,
    /// Bumped whenever something is adopted.
    ///
    /// An agent built before the bump cannot see what was installed, so
    /// the next request for that session builds a new one. Nothing live
    /// is stopped: a turn in flight finishes on the agent it started on,
    /// and a request arriving mid-adoption is served rather than refused
    /// — which is what retiring every agent outright got wrong, by
    /// answering `503` to sessions that had asked for nothing.
    generation: std::sync::atomic::AtomicU64,
}

/// The housekeeping agent's key.
///
/// A slash cannot appear in a session id — the routes are
/// `/session/{id}/…` and an id containing one would not route — so this
/// cannot collide with a real session however a client names one.
const HOUSEKEEPING: &str = "/housekeeping";

impl Agents {
    /// A registry over `runtime`, with nothing built yet.
    #[must_use]
    /// Takes the `Runtime` by value, because adoption mutates it and two
    /// owners of one runtime cannot both be right about what is
    /// installed. A caller that also wants an agent of its own can build
    /// one first — `build_agent` only borrows.
    pub fn new(runtime: Runtime, factory: Factory) -> Arc<Self> {
        // Cyclic so each session thread can reach the registry it belongs
        // to — through a `Weak`, so the cycle does not keep it alive.
        Arc::new_cyclic(|me| Self {
            runtime: Arc::new(std::sync::RwLock::new(runtime)),
            me: me.clone(),
            factory,
            live: Mutex::new(HashMap::new()),
            generation: std::sync::atomic::AtomicU64::new(0),
        })
    }

    /// The queue for `session`, building its agent if this is the first
    /// time anyone asked.
    ///
    /// On first use rather than at `session/create`: a client that lists
    /// sessions should not spend 3.5 ms and 1.35 MB on each one it merely
    /// mentions.
    pub fn of(&self, session: &str) -> Jobs<AgentSession> {
        self.queue(session)
    }

    /// The queue for work that belongs to no session.
    pub fn housekeeping(&self) -> Jobs<AgentSession> {
        self.queue(HOUSEKEEPING)
    }

    /// How many agents are live. For tests and for anything that ever
    /// wants to bound them.
    #[must_use]
    pub fn live(&self) -> usize {
        self.lock().len()
    }

    fn queue(&self, key: &str) -> Jobs<AgentSession> {
        let now = self.generation.load(std::sync::atomic::Ordering::Acquire);
        let mut live = self.lock();
        match live.get(key) {
            Some(existing) if existing.built_at == now => return existing.jobs.clone(),
            // Built before something was adopted: it cannot see the new
            // component, so it is replaced here rather than anywhere
            // else — which is what makes "everyone, at their next turn"
            // true without a broadcast.
            Some(_) => {
                if let Some(stale) = live.remove(key) {
                    // Finishes what it has, then ends. Whoever is still
                    // holding its queue keeps being served.
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
            // Built here, on the thread that will own it: an
            // `AgentSession` cannot be sent to a thread, so it must be
            // born on one.
            let built = runtime
                .read()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .build_agent(&*factory);
            match built {
                Ok(mut agent) => session_thread::serve_each(&mut agent, &queued, |agent| {
                    // Between jobs, not inside one: what a turn installed
                    // becomes callable here, for every session (#231).
                    if let Some(registry) = registry.upgrade() {
                        registry.after_turn(&named, agent);
                    }
                }),
                // The queue outlives this thread by a moment: every
                // caller gets `NoAnswer::Stopped`, which the surface turns
                // into a 503. Said here too, because a 503 with no line in
                // the log is a mystery.
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

    /// Adopt whatever `agent`'s last turn installed, and retire every
    /// agent so the next request builds one that can see it.
    ///
    /// **Every session, at its next turn** — the installing one included,
    /// because it is not special, it is only the one that asked. The
    /// alternative, telling just the installer, would leave two sessions
    /// disagreeing about what the fleet is with nothing saying so.
    ///
    /// Nothing is stopped and nothing is rebuilt here. Adoption bumps a
    /// generation, and the *next* request for a session finds its agent
    /// out of date and builds a fresh one — 3.5 ms, paid by whoever asks
    /// next. Stopping every agent outright was the first attempt and was
    /// wrong: it refused, with a `503`, requests from sessions that had
    /// asked for nothing and happened to arrive mid-adoption.
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
            // Recorded on the session that installed it, before that
            // agent goes: the install promised "callable from the next
            // turn", and whether the promise held belongs in the log
            // either way (#214).
            match &outcome {
                Ok(id) => agent.record_load(session, stem, Ok(id.as_str())),
                Err(err) => agent.record_load(session, stem, Err(&err.to_string())),
            }
        }
        // Every agent built before this moment is now out of date. None
        // is stopped: they finish what they are doing, and each session's
        // next request notices the bump and builds a fresh one.
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
    ///
    /// Each loop is *told* to stop rather than left to notice: an
    /// outstanding handle — a caller mid-request, a queue stashed
    /// somewhere — would otherwise keep its thread alive and make this
    /// join hang. Work already queued still runs, so nobody loses an
    /// answer they were waiting for.
    ///
    /// Waiting at all is what makes shutdown observable: a test that
    /// drops this and then asserts can trust that nothing is still
    /// running.
    fn drop(&mut self) {
        let live = std::mem::take(&mut *self.lock());
        let mut threads = Vec::with_capacity(live.len());
        for (_, Live { jobs, thread, .. }) in live {
            // Told, not merely released. Dropping the registry's handle
            // is not enough: a caller holding a clone would keep the loop
            // alive and the join below would wait for work nobody is
            // going to ask for. Found by a test that hung.
            jobs.stop();
            drop(jobs);
            threads.push(thread);
        }
        for thread in threads {
            let _ = thread.join();
        }
    }
}
