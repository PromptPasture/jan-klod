//! Parked prompts and their answers; answers bypass the session queue, which the parked turn is blocking.

use std::collections::HashMap;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Mutex;
use std::time::Duration;

/// Sessions with a turn parked on a question.
#[derive(Default)]
pub struct Pending {
    /// `session -> where its answer goes`; a `Mutex` because the two ends are on different threads.
    waiting: Mutex<HashMap<String, Sender<String>>>,
}

impl Pending {
    /// An empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers `session` as waiting and returns what to wait on; `None` if one is already parked.
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

    /// Delivers `answer` to the turn parked on `session`; `false` when none is parked.
    pub fn answer(&self, session: &str, answer: String) -> bool {
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

    /// The map, recovering from a poisoned lock: it holds only channel senders, so no invariant can break.
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
    /// Waits up to `slice`; `None` means the slice expired, not that no answer is coming.
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

    #[test]
    fn an_answer_for_a_session_nobody_parked_is_refused() {
        let pending = Pending::new();
        assert!(!pending.answer("s1", "yes".to_string()));
        assert!(!pending.is_parked("s1"));
    }

    #[test]
    fn a_second_park_is_refused_and_the_first_still_answers() {
        let pending = Pending::new();
        let first = pending.park("s1").expect("the first parks");
        assert!(pending.park("s1").is_none(), "the second must be refused");
        assert!(pending.answer("s1", "for the first".to_string()));
        assert_eq!(first.wait(SLICE).as_deref(), Some("for the first"));
    }

    #[test]
    fn a_dropped_wait_leaves_nothing_behind() {
        let pending = Pending::new();
        drop(pending.park("s1").expect("parks"));
        assert!(!pending.is_parked("s1"), "the entry outlived its waiter");
        assert!(
            !pending.answer("s1", "too late".to_string()),
            "an answer found a waiter that is gone"
        );
        assert!(pending.park("s1").is_some());
    }

    #[test]
    fn an_expired_slice_can_be_waited_on_again() {
        let pending = Pending::new();
        let parked = pending.park("s1").expect("parks");
        assert_eq!(parked.wait(Duration::from_millis(10)), None);
        assert!(pending.answer("s1", "late but arrived".to_string()));
        assert_eq!(parked.wait(SLICE).as_deref(), Some("late but arrived"));
    }

    /// The parked side never holds the lock while waiting.
    #[test]
    fn the_answer_may_come_from_another_thread() {
        let pending = Pending::new();
        let parked = pending.park("s1").expect("parks");
        std::thread::scope(|scope| {
            scope.spawn(|| {
                std::thread::sleep(Duration::from_millis(20));
                assert!(pending.answer("s1", "from elsewhere".to_string()));
            });
            assert_eq!(parked.wait(SLICE).as_deref(), Some("from elsewhere"));
        });
    }
}
