//! One thread owns the session (`AgentSession` is `!Send`); everything else reaches it by message (#223).
//!
//! Generic over the owned `T` with no `Send` bound. A job is `FnOnce(&mut T)`, not a command enum, so handlers keep their signatures.
//! The queue is opaque: pending jobs cannot be inspected, prioritised or cancelled.

use std::sync::mpsc::{channel, Receiver, Sender};

/// Work for the thread that owns a `T`.
/// Returns how to deliver its answer so the loop can run its between-jobs hook first; otherwise a caller races the hook (#231).
type Job<T> = Box<dyn FnOnce(&mut T) -> Deliver + Send>;

/// Hands a finished job's answer to whoever is waiting.
type Deliver = Box<dyn FnOnce() + Send>;

/// What arrives on the queue: work, or the end of it.
/// Stopping is a message, not a refcount: an outstanding clone would otherwise keep the thread alive and hang shutdown (#229).
pub enum Task<T> {
    /// Run this against the owned value.
    Work(Job<T>),
    /// Return from the loop.
    Stop,
}

/// Why a job produced no answer. Distinguished because a stopped loop is final while a panicked job says nothing about the next one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoAnswer {
    /// The owning loop is gone; every later call fails the same way.
    Stopped,
    /// The job panicked. The loop survived, but **the owned value may have been left mid-change**, so it is not retried.
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

/// A handle to the thread owning a `T`. Cheap to clone, safe to send, and `&Jobs` is shareable across threads.
///
/// The `Mutex` makes it `Sync` (`mpsc::Sender` is not), which HTTP handler state needs; it is held only for a non-blocking `send`.
pub struct Jobs<T> {
    hand: std::sync::Mutex<Sender<Task<T>>>,
}

impl<T> Clone for Jobs<T> {
    fn clone(&self) -> Self {
        Self {
            hand: std::sync::Mutex::new(self.sender()),
        }
    }
}

/// A handle and the queue its loop reads; `T` is supplied later by the owning thread so a `!Send` value never moves.
#[must_use]
pub fn queue<T>() -> (Jobs<T>, Receiver<Task<T>>) {
    let (hand, jobs) = channel();
    (
        Jobs {
            hand: std::sync::Mutex::new(hand),
        },
        jobs,
    )
}

impl<T> Jobs<T> {
    /// A sender of our own, recovering from a poisoned lock: a panic while cloning a `Sender` leaves nothing half-built.
    fn sender(&self) -> Sender<Task<T>> {
        self.hand
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// Run `job` on the owning thread and wait for what it returns.
    /// Blocks the caller, so other requests are served meanwhile.
    ///
    /// # Errors
    /// [`NoAnswer::Stopped`] if the loop is gone, [`NoAnswer::Panicked`] if the job panicked; the reply channel closes in both cases, so this never blocks forever.
    pub fn run<R, F>(&self, job: F) -> Result<R, NoAnswer>
    where
        F: FnOnce(&mut T) -> R + Send + 'static,
        R: Send + 'static,
    {
        let (answer, wait) = channel();
        let boxed: Job<T> = Box::new(move |owned| {
            let outcome =
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || job(owned)));
            Box::new(move || {
                let _ = answer.send(outcome.map_err(|_| NoAnswer::Panicked));
            })
        });
        self.sender()
            .send(Task::Work(boxed))
            .map_err(|_| NoAnswer::Stopped)?;
        wait.recv().unwrap_or(Err(NoAnswer::Stopped))
    }

    /// Queue `job` without waiting, for work nobody reads. A panic is still contained by [`Self::run`]'s guard.
    ///
    /// # Errors
    /// [`NoAnswer::Stopped`] if the loop is gone.
    pub fn send<F>(&self, job: F) -> Result<(), NoAnswer>
    where
        F: FnOnce(&mut T) + Send + 'static,
    {
        let boxed: Job<T> = Box::new(move |owned| {
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || job(owned)));
            Box::new(|| {})
        });
        self.sender()
            .send(Task::Work(boxed))
            .map_err(|_| NoAnswer::Stopped)
    }

    /// Tell the loop to return once it reaches this point. Queued work still runs; later callers get [`NoAnswer::Stopped`].
    pub fn stop(&self) {
        let _ = self.sender().send(Task::Stop);
    }
}

/// Serve jobs against `owned` until told to stop or every handle drops.
/// The caller keeps the `T`, which is why this works for a `!Send` session.
pub fn serve<T>(owned: &mut T, jobs: &Receiver<Task<T>>) {
    serve_each(owned, jobs, |_| {});
}

/// [`serve`], with `after` run on the owned value once per job.
/// For work between jobs (#231); putting it here means no transport can forget it.
pub fn serve_each<T, F>(owned: &mut T, jobs: &Receiver<Task<T>>, mut after: F)
where
    F: FnMut(&mut T),
{
    while let Ok(task) = jobs.recv() {
        match task {
            Task::Work(job) => {
                let deliver = job(owned);
                after(owned);
                deliver();
            }
            Task::Stop => return,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{queue, serve, NoAnswer};

    /// A job runs where the value lives; its result comes back to a thread that never touched it.
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
            drop(jobs);
            pushed
        });

        let mut owned = Vec::new();
        serve(&mut owned, &queued);

        assert_eq!(caller.join().expect("the caller finished"), 1);
        assert_eq!(owned, vec!["from the job".to_string()]);
    }

    /// What is owned never has to be `Send`: `Rc` is not, so a `T: Send` bound would stop this building.
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

    /// A caller must never wait on a loop that will not answer; a request thread blocked forever on a dead session would go unreported.
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

    /// A panicking job does not take the loop down, and its caller is told which happened; reporting every failure as "stopped"
    /// would make surfaces abandon live sessions.
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

    /// Two threads queue work against one owner.
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
