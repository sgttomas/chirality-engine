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

use tokio::sync::oneshot;

use crate::api::{ThreadId, TurnId};

/// The namespace the engine's tools are given in; an application's tools cannot use it.
pub(crate) const NAMESPACE: &str = "chirality";

#[derive(Default)]
pub(crate) struct Assignments {
    pub deliveries: delivery::Deliveries,
    /// Assignments closed while the engine runs, (assigner, assigned); after a
    /// restart a close is found in the assigner's history.
    closed: Mutex<HashSet<(ThreadId, ThreadId)>>,
    /// Each turn the engine started with another thread's message (not a report), by thread and
    /// turn, and that thread: the turn's end reports back to it. By turn, as Codex may answer the
    /// next turn's start before it tells of this one's end.
    opened: Mutex<HashMap<(ThreadId, TurnId), ThreadId>>,
    /// Threads whose `turn/start` for such a message is in flight, with the sender: Codex may tell
    /// of the turn before it answers.
    starting: Mutex<HashMap<ThreadId, ThreadId>>,
    /// Each recipient's last report looked at, so the next to it waits: a thread's reports come
    /// in the order their turns ended, and no recipient waits on another's.
    last_report: Mutex<HashMap<ThreadId, oneshot::Receiver<()>>>,
    /// Each thread a report in flight involves (reporting, or to hear it): how many such reports,
    /// and how many times it has gone from the engine (archived, its project removed) meanwhile; a
    /// report looked at across that is not made. Kept only while such a report is in flight.
    reporting: Mutex<HashMap<ThreadId, (usize, u64)>>,
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

    /// A place for a report to `recipient` after its earlier ones: what to wait for, and what to tell when done.
    pub fn reserve_report(&self, recipient: &ThreadId) -> (Option<oneshot::Receiver<()>>, oneshot::Sender<()>) {
        let (done, next) = oneshot::channel();
        (self.last_report.lock().unwrap().insert(recipient.clone(), next), done)
    }

    /// The engine is about to start a turn on the thread with `sender`'s message.
    pub fn starting(&self, thread: &ThreadId, sender: &ThreadId) {
        self.starting.lock().unwrap().insert(thread.clone(), sender.clone());
    }

    /// Codex answered that start: with the turn it began, unless that turn has ended already.
    pub fn started(&self, thread: &ThreadId, turn: Option<&TurnId>) {
        let Some(sender) = self.starting.lock().unwrap().remove(thread) else { return };
        if let Some(turn) = turn {
            self.opened.lock().unwrap().insert((thread.clone(), turn.clone()), sender);
        }
    }

    /// Codex tells of a turn whose first message is marked as from `sender` (not a report): it is
    /// the one the engine is starting, if a start from that sender is in flight there.
    pub fn marked_message(&self, thread: &ThreadId, turn: &TurnId, sender: &ThreadId) {
        if self.starting.lock().unwrap().get(thread) == Some(sender) {
            self.opened.lock().unwrap().insert((thread.clone(), turn.clone()), sender.clone());
        }
    }

    /// The thread whose message the engine started the ended turn with, if it did; forgotten.
    pub fn take_opened(&self, thread: &ThreadId, turn: &TurnId) -> Option<ThreadId> {
        self.opened.lock().unwrap().remove(&(thread.clone(), turn.clone()))
    }

    /// A report from `thread` to `recipient` begins: how many times each has gone so far.
    pub fn begin_report(&self, thread: &ThreadId, recipient: &ThreadId) -> (u64, u64) {
        let mut reporting = self.reporting.lock().unwrap();
        let mut begin = |t: &ThreadId| {
            let entry = reporting.entry(t.clone()).or_default();
            entry.0 += 1;
            entry.1
        };
        (begin(thread), begin(recipient))
    }

    pub fn generations(&self, thread: &ThreadId, recipient: &ThreadId) -> (u64, u64) {
        let reporting = self.reporting.lock().unwrap();
        let gone = |t: &ThreadId| reporting.get(t).map_or(0, |e| e.1);
        (gone(thread), gone(recipient))
    }

    pub fn end_report(&self, thread: &ThreadId, recipient: &ThreadId) {
        let mut reporting = self.reporting.lock().unwrap();
        for t in [thread, recipient] {
            if let Some(entry) = reporting.get_mut(t) {
                entry.0 -= 1;
                if entry.0 == 0 {
                    reporting.remove(t);
                }
            }
        }
    }

    /// The thread is gone from the engine (archived, or its project removed): nothing of it is kept,
    /// and no report in flight from or to it is made.
    pub fn forget_thread(&self, thread: &ThreadId) {
        self.deliveries.forget(thread);
        self.opened.lock().unwrap().retain(|(t, _), _| t != thread);
        self.starting.lock().unwrap().remove(thread);
        self.last_report.lock().unwrap().remove(thread);
        if let Some(entry) = self.reporting.lock().unwrap().get_mut(thread) {
            entry.1 += 1;
        }
    }

    /// How many entries `opened`, `last_report` and `reporting` hold, for tests.
    #[cfg(test)]
    pub fn held(&self) -> (usize, usize, usize) {
        (self.opened.lock().unwrap().len(), self.last_report.lock().unwrap().len(), self.reporting.lock().unwrap().len())
    }

    /// Codex stopped, or the engine shuts down: no turn it ran will end, no start in flight was
    /// answered, and no report waits on one before it.
    pub fn codex_stopped(&self) {
        self.opened.lock().unwrap().clear();
        self.starting.lock().unwrap().clear();
        self.last_report.lock().unwrap().clear();
    }
}
