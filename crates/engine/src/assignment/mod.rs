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
    /// Each thread's turn the engine started with another thread's message (not a report), and
    /// that thread: the turn's end reports back to it. One a thread at most, as it runs one turn.
    opened: Mutex<HashMap<ThreadId, (TurnId, ThreadId)>>,
    /// Threads whose `turn/start` for such a message is in flight, with the sender: Codex may tell
    /// of the turn before it answers.
    starting: Mutex<HashMap<ThreadId, ThreadId>>,
    /// Each recipient's last report looked at, so the next to it waits: a thread's reports come
    /// in the order their turns ended, and no recipient waits on another's.
    last_report: Mutex<HashMap<ThreadId, oneshot::Receiver<()>>>,
    /// How many times each thread has gone from the engine (archived, its project removed): a
    /// report looked at across that is not made.
    gone: Mutex<HashMap<ThreadId, u64>>,
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
            self.opened.lock().unwrap().insert(thread.clone(), (turn.clone(), sender));
        }
    }

    /// Codex tells of a turn whose first message is marked as from `sender` (not a report): it is
    /// the one the engine is starting, if a start from that sender is in flight there.
    pub fn marked_message(&self, thread: &ThreadId, turn: &TurnId, sender: &ThreadId) {
        if self.starting.lock().unwrap().get(thread) == Some(sender) {
            self.opened.lock().unwrap().insert(thread.clone(), (turn.clone(), sender.clone()));
        }
    }

    /// The thread whose message the engine started the ended turn with, if it did; forgotten.
    pub fn take_opened(&self, thread: &ThreadId, turn: &TurnId) -> Option<ThreadId> {
        let mut opened = self.opened.lock().unwrap();
        let sender = opened.get(thread).filter(|(t, _)| t == turn).map(|(_, s)| s.clone())?;
        opened.remove(thread);
        Some(sender)
    }

    pub fn generation(&self, thread: &ThreadId) -> u64 {
        self.gone.lock().unwrap().get(thread).copied().unwrap_or(0)
    }

    /// The thread is gone from the engine (archived, or its project removed): nothing of it is kept,
    /// and no report looked at before is made.
    pub fn forget_thread(&self, thread: &ThreadId) {
        self.deliveries.forget(thread);
        self.opened.lock().unwrap().remove(thread);
        self.starting.lock().unwrap().remove(thread);
        *self.gone.lock().unwrap().entry(thread.clone()).or_default() += 1;
    }

    /// Codex stopped: no turn it ran will end, and no start in flight was answered.
    pub fn codex_stopped(&self) {
        self.opened.lock().unwrap().clear();
        self.starting.lock().unwrap().clear();
    }
}
