//! Prompts waiting for an answer, and the route that delivers one (#225).
//!
//! A turn parked on an `ask` occupies the session's thread. So the answer
//! **must not be a session job**: queued behind the parked turn, it would
//! wait for the turn that is waiting for it. This is where the two meet
//! instead — the turn registers a one-shot and blocks on it, and the
//! accepting thread completes it without ever touching the session.
//!
//! # What it replaces
//!
//! `PromptDriver::wait_for_answer` used to *serve the socket itself* while
//! parked: `Server::recv_timeout`, the answer POST handled inline, every
//! other request refused `409` because "agent is mid-turn and
//! single-threaded". That is why only one client could be served at a time,
//! and it is the behaviour Phase 20 exists to remove.
//!
//! # One parked prompt per session, refused rather than replaced
//!
//! A session runs one turn at a time, so a second park is already a
//! surprise. Replacing the first would orphan a turn that is still waiting
//! and hand its answer to someone else; refusing the second keeps the
//! damage where it started. The caller takes its prompt's own default,
//! which is what it does for every other unanswerable case.
//!
//! # Nothing is left behind
//!
//! [`Parked`] removes its entry on drop, so an abandoned confirmation — a
//! client that vanished, a turn that timed out — leaves no row. A registry
//! that leaked one entry per abandoned confirmation would be a slow leak
//! with no symptom until a session could never be asked again.

use std::collections::HashMap;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Mutex;
use std::time::Duration;

/// Sessions with a turn parked on a question.
#[derive(Default)]
pub struct Pending {
    /// `session -> where its answer goes`.
    ///
    /// A `Mutex` and not a channel per session held elsewhere: the two ends
    /// are on different threads and neither owns the other, so the map is
    /// the only thing both can name.
    waiting: Mutex<HashMap<String, Sender<String>>>,
}

impl Pending {
    /// An empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Register `session` as waiting, and hand back what to wait on.
    ///
    /// `None` when that session already has a prompt parked — see the
    /// module docs for why the second is refused rather than the first
    /// replaced.
    pub fn park(&self, session: &str) -> Option<Parked<'_>> {
        let (answer, answers) = channel();
        let mut waiting = self.lock();
        if waiting.contains_key(session) {
            return None;
        }
        waiting.insert(session.to_string(), answer);
        drop(waiting);
        Some(Parked {
            pending: self,
            session: session.to_string(),
            answers,
        })
    }

    /// Deliver `answer` to the turn parked on `session`.
    ///
    /// `false` when nothing is parked — which the route reports rather than
    /// swallowing, because an answer accepted into silence looks to the
    /// client exactly like one that arrived.
    pub fn answer(&self, session: &str, answer: String) -> bool {
        // Removed, not read: an answer is delivered once, and leaving the
        // entry would let a second POST arrive at a waiter that has gone.
        let Some(waiting) = self.lock().remove(session) else {
            return false;
        };
        waiting.send(answer).is_ok()
    }

    /// Whether `session` has a turn parked on a question.
    #[must_use]
    pub fn is_parked(&self, session: &str) -> bool {
        self.lock().contains_key(session)
    }

    /// The map, recovering from a poisoned lock.
    ///
    /// A panic while holding it leaves the map structurally sound — it is a
    /// `HashMap` of channel senders, with no invariant a half-finished
    /// insert could break. Refusing to serve confirmations for the rest of
    /// the process because one job panicked would be the larger failure.
    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, Sender<String>>> {
        self.waiting
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

/// A registered wait. Removes its entry when dropped.
pub struct Parked<'a> {
    pending: &'a Pending,
    session: String,
    answers: Receiver<String>,
}

impl Parked<'_> {
    /// Wait up to `slice` for the answer.
    ///
    /// `None` means the slice expired, **not** that no answer is coming —
    /// the caller owns the real deadline and calls again, which is what
    /// lets it write a heartbeat between slices. That division is
    /// deliberate: the heartbeat is a *write* to the client's stream, and
    /// nothing here should know that a stream exists.
    #[must_use]
    pub fn wait(&self, slice: Duration) -> Option<String> {
        self.answers.recv_timeout(slice).ok()
    }
}

impl Drop for Parked<'_> {
    fn drop(&mut self) {
        self.pending.lock().remove(&self.session);
    }
}

#[cfg(test)]
mod tests {
    use super::Pending;
    use std::time::Duration;

    const SLICE: Duration = Duration::from_millis(200);

    #[test]
    fn an_answer_reaches_the_turn_parked_on_that_session() {
        let pending = Pending::new();
        let parked = pending.park("s1").expect("nothing was parked");
        assert!(pending.answer("s1", "yes".to_string()), "nobody took it");
        assert_eq!(parked.wait(SLICE).as_deref(), Some("yes"));
    }

    /// An answer nobody is waiting for is refused, not swallowed.
    ///
    /// Accepted into silence, it looks to the client exactly like one that
    /// arrived — and the turn it was meant for proceeds on its default
    /// while the client believes it answered.
    #[test]
    fn an_answer_for_a_session_nobody_parked_is_refused() {
        let pending = Pending::new();
        assert!(!pending.answer("s1", "yes".to_string()));
        assert!(!pending.is_parked("s1"));
    }

    /// A second park is refused and the first still gets its answer.
    ///
    /// Replacing would orphan a turn that is still waiting and hand its
    /// answer to whoever parked last.
    #[test]
    fn a_second_park_is_refused_and_the_first_still_answers() {
        let pending = Pending::new();
        let first = pending.park("s1").expect("the first parks");
        assert!(pending.park("s1").is_none(), "the second must be refused");
        assert!(pending.answer("s1", "for the first".to_string()));
        assert_eq!(first.wait(SLICE).as_deref(), Some("for the first"));
    }

    /// An abandoned confirmation leaves no row.
    ///
    /// The leak this prevents has no symptom until it has one: entries
    /// accumulate silently, and the first thing anyone notices is a session
    /// that can never be asked again, long after the turn that stranded it.
    #[test]
    fn a_dropped_wait_leaves_nothing_behind() {
        let pending = Pending::new();
        drop(pending.park("s1").expect("parks"));
        assert!(!pending.is_parked("s1"), "the entry outlived its waiter");
        assert!(
            !pending.answer("s1", "too late".to_string()),
            "an answer found a waiter that is gone"
        );
        // And the session can be parked again, which is the half a leak
        // would have taken away.
        assert!(pending.park("s1").is_some());
    }

    /// A slice that expires is not a refusal: the caller writes its
    /// heartbeat and asks again, which is how the deadline stays the
    /// caller's.
    #[test]
    fn an_expired_slice_can_be_waited_on_again() {
        let pending = Pending::new();
        let parked = pending.park("s1").expect("parks");
        assert_eq!(parked.wait(Duration::from_millis(10)), None);
        assert!(pending.answer("s1", "late but arrived".to_string()));
        assert_eq!(parked.wait(SLICE).as_deref(), Some("late but arrived"));
    }

    /// The shape the surface will use: the answer arrives from another
    /// thread, and the parked side never holds the lock while waiting.
    #[test]
    fn the_answer_may_come_from_another_thread() {
        let pending = Pending::new();
        let parked = pending.park("s1").expect("parks");
        std::thread::scope(|scope| {
            scope.spawn(|| {
                // Late enough that the waiter is already blocked.
                std::thread::sleep(Duration::from_millis(20));
                assert!(pending.answer("s1", "from elsewhere".to_string()));
            });
            assert_eq!(parked.wait(SLICE).as_deref(), Some("from elsewhere"));
        });
    }
}
