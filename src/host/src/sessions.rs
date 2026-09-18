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
}

/// Every live session's agent, built on first use.
pub struct Agents {
    runtime: Arc<Runtime>,
    factory: Factory,
    /// `session id -> its agent's queue`. The housekeeping agent is in
    /// here too, under a key no session can have.
    live: Mutex<HashMap<String, Live>>,
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
    pub fn new(runtime: Arc<Runtime>, factory: Factory) -> Self {
        Self {
            runtime,
            factory,
            live: Mutex::new(HashMap::new()),
        }
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
        let mut live = self.lock();
        if let Some(existing) = live.get(key) {
            return existing.jobs.clone();
        }
        let (jobs, queued) = session_thread::queue::<AgentSession>();
        let runtime = Arc::clone(&self.runtime);
        let factory = Arc::clone(&self.factory);
        let named = key.to_owned();
        let thread = std::thread::spawn(move || {
            // Built here, on the thread that will own it: an
            // `AgentSession` cannot be sent to a thread, so it must be
            // born on one.
            match runtime.build_agent(&*factory) {
                Ok(mut agent) => session_thread::serve(&mut agent, &queued),
                // The queue outlives this thread by a moment: every
                // caller gets `NoAnswer::Stopped`, which the surface turns
                // into a 503. Said here too, because a 503 with no line in
                // the log is a mystery.
                Err(err) => eprintln!("ERROR [host] session `{named}` has no agent: {err}"),
            }
        });
        let handle = jobs.clone();
        live.insert(key.to_owned(), Live { jobs, thread });
        handle
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
        for (_, Live { jobs, thread }) in live {
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
