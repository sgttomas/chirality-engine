//! Assignment between threads: the engine's own tools (namespace
//! `chirality`), given to the agent on every thread the engine starts, with
//! which one thread gives work to another, ordinary thread and the two
//! exchange messages.
//!
//! - `tools`: the tools as Codex takes them, their items in the
//!   conversation, and the answers to their calls.
//! - `delivery`: messages waiting for a thread's running turn to end, and the
//!   report of a turn another thread's message started, back to that thread.
//! - `marker`: the mark on a message one thread sends another, and a thread a message mentions.
//! - `read`: another thread, as an agent reads it.
//!
//! The engine keeps nothing Codex does not: an assignment is read from the
//! assigned thread's first message, reports and messages are messages in
//! Codex's history, and a close is the assigner's own call in its history.
//! Only the deliveries waiting for a turn to end are held, in memory.

pub(crate) mod delivery;
pub(crate) mod marker;
pub(crate) mod read;
pub(crate) mod tools;

use std::collections::{HashMap, HashSet};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use tokio::sync::{Notify, oneshot};

use crate::api::{ThreadId, TurnId};

/// The namespace the engine's tools are given in; an application's tools cannot use it.
pub(crate) const NAMESPACE: &str = "chirality";

/// How long a look at an ended turn holds what waits for its thread, at most: Codex may never
/// announce the end of a turn it said was over.
pub(crate) const LOOK_LIMIT: Duration = if cfg!(test) { Duration::from_secs(3) } else { Duration::from_secs(30) };

#[derive(Default)]
pub(crate) struct Assignments {
    pub deliveries: delivery::Deliveries,
    /// Assignments closed while the engine runs, (assigner, assigned); after a
    /// restart a close is found in the assigner's history.
    closed: Mutex<HashSet<(ThreadId, ThreadId)>>,
    /// Turns the engine started with another thread's message (not a report): their ends report back.
    opened: Mutex<HashSet<(ThreadId, TurnId)>>,
    /// Threads whose `turn/start` for such a message is in flight: the turn whose first message is
    /// marked, while it is, is that one (Codex may tell of it before it answers).
    starting: Mutex<HashSet<ThreadId>>,
    /// Ended turns being looked at for their report, each since when: what waits for that thread
    /// starts after, so a turn's reply is read before the next turn starts there.
    looks: Mutex<HashMap<(ThreadId, TurnId), Instant>>,
    /// Told when a look ends, so what waits on it goes on.
    pub looked: Notify,
    /// The last report looked at, so the next, from any thread, waits for it: reports are made in
    /// the order their turns ended.
    last_report: Mutex<Option<oneshot::Receiver<()>>>,
    /// Turn ends looked at for a report, for tests to wait on.
    #[cfg(test)]
    pub reports: std::sync::atomic::AtomicUsize,
}

impl Assignments {
    pub fn close(&self, assigner: &ThreadId, assigned: &ThreadId) {
        self.closed.lock().unwrap().insert((assigner.clone(), assigned.clone()));
        self.deliveries.drop_reports(assigner, assigned);
    }

    pub fn is_closed(&self, assigner: &ThreadId, assigned: &ThreadId) -> bool {
        self.closed.lock().unwrap().contains(&(assigner.clone(), assigned.clone()))
    }

    /// A place for a report after every earlier one: what to wait for, and what to tell when done.
    pub fn reserve_report(&self) -> (Option<oneshot::Receiver<()>>, oneshot::Sender<()>) {
        let (done, next) = oneshot::channel();
        (self.last_report.lock().unwrap().replace(next), done)
    }

    /// The engine is about to start a turn on the thread with another thread's message.
    pub fn starting(&self, thread: &ThreadId) {
        self.starting.lock().unwrap().insert(thread.clone());
    }

    /// Codex answered that start: with the turn it began, if it did.
    pub fn started(&self, thread: &ThreadId, turn: Option<&TurnId>) {
        self.starting.lock().unwrap().remove(thread);
        if let Some(turn) = turn {
            self.opened.lock().unwrap().insert((thread.clone(), turn.clone()));
        }
    }

    /// Codex tells of a turn whose first message is marked as from another thread (not a report):
    /// it is one the engine started if a start is in flight there.
    pub fn marked_message(&self, thread: &ThreadId, turn: &TurnId) {
        if self.starting.lock().unwrap().contains(thread) {
            self.opened.lock().unwrap().insert((thread.clone(), turn.clone()));
        }
    }

    /// Whether the engine started the ended turn with another thread's message; forgets it.
    pub fn take_opened(&self, thread: &ThreadId, turn: &TurnId) -> bool {
        self.opened.lock().unwrap().remove(&(thread.clone(), turn.clone()))
    }

    /// The turn is over (Codex said its thread is idle, or announced its end): whether it is to be
    /// looked at for a report, the look begun the first time. One the engine started with another
    /// thread's message is, and so is an assigned thread's (`assigned`), however it began.
    pub fn look_due(&self, thread: &ThreadId, turn: &TurnId, assigned: bool) -> bool {
        let key = (thread.clone(), turn.clone());
        let mut looks = self.looks.lock().unwrap();
        if looks.contains_key(&key) {
            return true;
        }
        let due = assigned || self.opened.lock().unwrap().contains(&key);
        if due {
            looks.insert(key, Instant::now());
        }
        due
    }

    pub fn end_look(&self, thread: &ThreadId, turn: &TurnId) {
        self.looks.lock().unwrap().remove(&(thread.clone(), turn.clone()));
        self.looked.notify_waiters();
    }

    /// Whether a turn of the thread is being looked at; a look past its time is over.
    pub fn is_looking(&self, thread: &ThreadId) -> bool {
        let mut looks = self.looks.lock().unwrap();
        looks.retain(|_, since| since.elapsed() < LOOK_LIMIT);
        looks.keys().any(|(t, _)| t == thread)
    }

    /// The thread is gone from the engine (archived, or its project removed): nothing of it is kept.
    pub fn forget_thread(&self, thread: &ThreadId) {
        self.deliveries.forget(thread);
        self.opened.lock().unwrap().retain(|(t, _)| t != thread);
        self.starting.lock().unwrap().remove(thread);
        self.looks.lock().unwrap().retain(|(t, _), _| t != thread);
        self.looked.notify_waiters();
    }

    /// Codex stopped: no turn it ran will end, and no start in flight was answered.
    pub fn codex_stopped(&self) {
        self.opened.lock().unwrap().clear();
        self.starting.lock().unwrap().clear();
        self.looks.lock().unwrap().clear();
        self.looked.notify_waiters();
    }

    #[cfg(test)]
    pub fn looks(&self) -> usize {
        self.looks.lock().unwrap().len()
    }
}
