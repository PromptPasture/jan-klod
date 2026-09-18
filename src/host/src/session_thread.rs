//! One thread owns the session; everything else reaches it by message (#223).
//!
//! `AgentSession` is `!Send`, so it cannot be handed to a worker: the thread
//! that built it has to keep it. The seam therefore runs the other way
//! round — the owning thread becomes a **loop that serves jobs**, and the
//! accepting, reading and polling move off it. `acp.rs:237` has done this
//! for the ACP pipe since Phase 13 (*"Reader thread: hands lines over.
//! Holds no session"*); this generalises it so any number of threads can
//! ask for work without holding the thing they are asking about.
//!
//! # Generic over what is owned, deliberately
//!
//! Nothing here mentions `AgentSession`. The module is about threading, and
//! a seam that named the session could only be tested by booting a fleet —
//! so its failure modes would be tested least where they matter most. `T`
//! carries **no `Send` bound**, which is the property that makes it usable
//! for a `!Send` session at all, and [`tests::the_owned_value_need_not_be_send`]
//! is compiled proof of it.
//!
//! # A job is a closure, not a command
//!
//! `serve.rs` alone has seven routes, and `rpc.rs`, `mcp.rs` and `acp.rs`
//! have their own verbs. An enum mirroring them would be a second dispatch
//! table to keep in step with the first, and every new route would mean
//! touching both. So a job is `FnOnce(&mut T)` and the existing handlers
//! keep the signatures they already have.
//!
//! The cost, stated because a later slice will meet it: the queue is
//! opaque. Nothing can inspect a pending job, prioritise one, or cancel
//! one. Phase 20's remaining slices do not need to; anything that does will
//! have to give jobs a shape.

use std::sync::mpsc::{channel, Receiver, Sender};

/// Work for the thread that owns a `T`.
type Job<T> = Box<dyn FnOnce(&mut T) + Send>;

/// Why a job produced no answer.
///
/// Distinguished rather than merged into one error, because they call for
/// different reactions: a stopped loop will never serve anything again and
/// the caller should stop asking, while a panicked job says nothing about
/// the next one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoAnswer {
    /// The owning loop is gone — it returned, or its thread died. Every
    /// later call will fail the same way.
    Stopped,
    /// The job itself panicked. The loop survived and the next job will
    /// run; **the owned value may have been left mid-change**, which is the
    /// unavoidable part and the reason this is not silently retried here.
    Panicked,
}

impl std::fmt::Display for NoAnswer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Stopped => write!(f, "the session's thread is no longer running"),
            Self::Panicked => write!(f, "the request panicked"),
        }
    }
}

impl std::error::Error for NoAnswer {}

/// A handle to the thread owning a `T`. Cheap to clone, safe to send, and
/// **shareable**: `&Jobs` works from several threads at once.
///
/// The `Mutex` is what makes it `Sync`. `mpsc::Sender` is `Send` but not
/// `Sync`, and an HTTP framework wants handler state that every request
/// can borrow — so the choice is a lock held for the length of a `send`,
/// or a handle that has to be cloned before it can be shared. The lock is
/// smaller, and a `send` onto an unbounded channel does not block.
pub struct Jobs<T> {
    hand: std::sync::Mutex<Sender<Job<T>>>,
}

impl<T> Clone for Jobs<T> {
    // Derived `Clone` would demand `T: Clone`, which is wrong: the handle
    // holds a channel, never a `T`.
    fn clone(&self) -> Self {
        Self {
            hand: std::sync::Mutex::new(self.sender()),
        }
    }
}

/// A handle and the queue its loop reads. The `T` is supplied later, by
/// whichever thread owns it, so that a `!Send` value never has to move.
#[must_use]
pub fn queue<T>() -> (Jobs<T>, Receiver<Job<T>>) {
    let (hand, jobs) = channel();
    (
        Jobs {
            hand: std::sync::Mutex::new(hand),
        },
        jobs,
    )
}

impl<T> Jobs<T> {
    /// A sender of our own, recovering from a poisoned lock.
    ///
    /// Poisoning here means a panic while cloning a `Sender`, which leaves
    /// nothing half-built; refusing every later job over it would turn one
    /// panicked request into a dead surface.
    fn sender(&self) -> Sender<Job<T>> {
        self.hand
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// Run `job` on the owning thread and wait for what it returns.
    ///
    /// Blocks the calling thread, which is the point: a request handler
    /// wants its answer, and the thread it blocks is no longer the one
    /// holding the session — so other requests are served meanwhile.
    ///
    /// # Errors
    /// [`NoAnswer::Stopped`] if the loop is gone, [`NoAnswer::Panicked`] if
    /// the job panicked. Neither blocks forever: the reply channel closes
    /// in both cases, which is what a caller waiting on a dead thread needs
    /// and what an unguarded `recv` would not give it.
    pub fn run<R, F>(&self, job: F) -> Result<R, NoAnswer>
    where
        F: FnOnce(&mut T) -> R + Send + 'static,
        R: Send + 'static,
    {
        let (answer, wait) = channel();
        let boxed: Job<T> = Box::new(move |owned| {
            // Caught here rather than around the loop below, so a panic
            // cannot escape *and* the caller learns which of the two
            // things happened. Unwind safety is asserted because the
            // alternative is requiring it of every caller: a panic mid-job
            // may leave `owned` half-changed, which `NoAnswer::Panicked`
            // exists to report rather than hide.
            let outcome =
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || job(owned)));
            // The receiver may already be gone (a caller that stopped
            // waiting). Its answer is dropped, not an error: the work was
            // done either way.
            let _ = answer.send(outcome.map_err(|_| NoAnswer::Panicked));
        });
        self.sender().send(boxed).map_err(|_| NoAnswer::Stopped)?;
        // A closed channel here means the job never sent: the loop died
        // between accepting the job and running it.
        wait.recv().unwrap_or(Err(NoAnswer::Stopped))
    }

    /// Queue `job` without waiting for it.
    ///
    /// For work whose result nobody reads — the session's own bookkeeping.
    /// A panic inside is still contained by [`Self::run`]'s guard, because
    /// this goes through the same box.
    ///
    /// # Errors
    /// [`NoAnswer::Stopped`] if the loop is gone.
    pub fn send<F>(&self, job: F) -> Result<(), NoAnswer>
    where
        F: FnOnce(&mut T) + Send + 'static,
    {
        let boxed: Job<T> = Box::new(move |owned| {
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || job(owned)));
        });
        self.sender().send(boxed).map_err(|_| NoAnswer::Stopped)
    }
}

/// Serve jobs against `owned` until every handle is dropped.
///
/// The caller owns the `T` and never gives it up — which is the whole
/// reason this works for a `!Send` session. Returns when the last [`Jobs`]
/// handle is gone, so a surface shuts down by dropping its handles rather
/// than by a flag nobody checks.
pub fn serve<T>(owned: &mut T, jobs: &Receiver<Job<T>>) {
    while let Ok(job) = jobs.recv() {
        job(owned);
    }
}

#[cfg(test)]
mod tests {
    use super::{queue, serve, NoAnswer};

    /// The ordinary case: a job runs where the value lives, and its result
    /// comes back to a thread that never touched it.
    #[test]
    fn a_job_runs_on_the_owning_thread_and_its_answer_comes_back() {
        let (jobs, queued) = queue::<Vec<String>>();
        let caller = std::thread::spawn(move || {
            let pushed = jobs
                .run(|owned: &mut Vec<String>| {
                    owned.push("from the job".to_string());
                    owned.len()
                })
                .expect("the loop is running");
            // Dropping the handle is what ends the loop below.
            drop(jobs);
            pushed
        });

        let mut owned = Vec::new();
        serve(&mut owned, &queued);

        assert_eq!(caller.join().expect("the caller finished"), 1);
        assert_eq!(owned, vec!["from the job".to_string()]);
    }

    /// The property the whole seam exists for: what is owned never has to
    /// be `Send`. `Rc` is not, so this is a compile-time assertion written
    /// as a test — if a `T: Send` bound appeared, this would stop building.
    #[test]
    fn the_owned_value_need_not_be_send() {
        let (jobs, queued) = queue::<std::rc::Rc<std::cell::RefCell<u32>>>();
        let caller = std::thread::spawn(move || {
            let seen = jobs.run(|owned: &mut std::rc::Rc<std::cell::RefCell<u32>>| {
                *owned.borrow_mut() += 1;
                *owned.borrow()
            });
            drop(jobs);
            seen
        });

        let mut owned = std::rc::Rc::new(std::cell::RefCell::new(41));
        serve(&mut owned, &queued);
        assert_eq!(caller.join().expect("finished"), Ok(42));
    }

    /// A caller must never wait on a loop that will not answer.
    ///
    /// This is the failure this seam would otherwise add to the process: a
    /// request thread blocked forever on a dead session is worse than the
    /// single-threadedness it replaces, because nothing reports it.
    #[test]
    fn a_handle_whose_loop_has_stopped_says_so_rather_than_blocking() {
        let (jobs, queued) = queue::<u32>();
        drop(queued);
        assert_eq!(
            jobs.run(|owned: &mut u32| *owned),
            Err(NoAnswer::Stopped),
            "a handle with no loop must fail, not hang"
        );
    }

    /// A panicking job does not take the loop with it, and its caller is
    /// told which of the two things happened.
    ///
    /// The second half is the part worth testing: a seam that reported
    /// every failure as "stopped" would have every surface give up on a
    /// session that is still perfectly alive.
    #[test]
    fn a_panicking_job_is_contained_and_the_next_one_still_runs() {
        let (jobs, queued) = queue::<u32>();
        let caller = std::thread::spawn(move || {
            let panicked = jobs.run(|_: &mut u32| panic!("the job gave up"));
            let after = jobs.run(|owned: &mut u32| {
                *owned += 1;
                *owned
            });
            drop(jobs);
            (panicked, after)
        });

        let mut owned = 7;
        serve(&mut owned, &queued);

        let (panicked, after) = caller.join().expect("the caller finished");
        assert_eq!(panicked, Err(NoAnswer::Panicked), "not reported as stopped");
        assert_eq!(after, Ok(8), "the loop died with the job");
        assert_eq!(owned, 8);
    }

    /// Two threads queue work against one owner, which is the property the
    /// rest of Phase 20 is built on.
    #[test]
    fn two_threads_queue_work_without_either_holding_the_value() {
        let (jobs, queued) = queue::<Vec<u32>>();
        let callers: Vec<_> = (0..2_u32)
            .map(|n| {
                let jobs = jobs.clone();
                std::thread::spawn(move || {
                    jobs.run(move |owned: &mut Vec<u32>| {
                        owned.push(n);
                        owned.len()
                    })
                })
            })
            .collect();
        drop(jobs);

        let mut owned = Vec::new();
        serve(&mut owned, &queued);

        for caller in callers {
            assert!(caller.join().expect("finished").is_ok());
        }
        owned.sort_unstable();
        assert_eq!(owned, vec![0, 1], "both threads' work reached the owner");
    }
}
