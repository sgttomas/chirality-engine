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

use tokio::sync::oneshot;

use crate::api::{ThreadId, TurnId};

/// The namespace the engine's tools are given in; an application's tools cannot use it.
pub(crate) const NAMESPACE: &str = "chirality";

/// How long a look at an ended turn holds what waits for its thread, at most: Codex may never
/// announce the end of a turn it said was over.
const LOOK_LIMIT: Duration = Duration::from_secs(30);

#[derive(Default)]
pub(crate) struct Assignments {
    pub deliveries: delivery::Deliveries,
    /// Assignments closed while the engine runs, (assigner, assigned); after a
    /// restart a close is found in the assigner's history.
    closed: Mutex<HashSet<(ThreadId, ThreadId)>>,
    /// The turn running on a thread that another thread's message (not a report) opened, as the
    /// engine saw it start: its end reports back.
    opened: Mutex<HashMap<ThreadId, TurnId>>,
    /// Threads whose ended turns are being looked at for a report, with how many and since when: a
    /// delivery waiting for one starts after, so a turn's reply is read before the next turn starts there.
    looking: Mutex<HashMap<ThreadId, (usize, Instant)>>,
    /// The turn whose look began when Codex said its thread was idle, before it announced the turn's end.
    begun: Mutex<HashMap<ThreadId, TurnId>>,
    /// Each thread's last report looked at, so the next waits for it.
    reports_in_order: Mutex<HashMap<ThreadId, oneshot::Receiver<()>>>,
    /// Turn ends looked at for a report, for tests to wait on.
    #[cfg(test)]
    pub reports: std::sync::atomic::AtomicUsize,
}

impl Assignments {
    pub fn close(&self, assigner: &ThreadId, assigned: &ThreadId) {
        self.closed.lock().unwrap().insert((assigner.clone(), assigned.clone()));
        self.deliveries.drop_reports(assigner, assigned);
    }

    /// A place for a report after the thread's earlier ones: what to wait for, and what to tell when done.
    pub fn reserve_report(&self, thread: &ThreadId) -> (Option<oneshot::Receiver<()>>, oneshot::Sender<()>) {
        let (done, next) = oneshot::channel();
        (self.reports_in_order.lock().unwrap().insert(thread.clone(), next), done)
    }

    /// The turn began with a message from another thread.
    pub fn opened_by_message(&self, thread: &ThreadId, turn: &TurnId) {
        self.opened.lock().unwrap().insert(thread.clone(), turn.clone());
    }

    /// Whether the turn that ended began with a message from another thread; forgets it.
    pub fn take_opened(&self, thread: &ThreadId, turn: &TurnId) -> bool {
        let mut opened = self.opened.lock().unwrap();
        let was = opened.get(thread) == Some(turn);
        if was {
            opened.remove(thread);
        }
        was
    }

    /// The turn is over (Codex said its thread is idle, or announced its end): whether it is to be
    /// looked at for a report, the look begun the first time this is asked. A turn another thread's
    /// message began is; so is an assigned thread's (`assigned`), however it began.
    pub fn look_due(&self, thread: &ThreadId, turn: &TurnId, assigned: bool) -> bool {
        let mut begun = self.begun.lock().unwrap();
        if begun.get(thread) == Some(turn) {
            return true;
        }
        let due = self.take_opened(thread, turn) || assigned;
        if due {
            begun.insert(thread.clone(), turn.clone());
            let mut looking = self.looking.lock().unwrap();
            let at = looking.entry(thread.clone()).or_insert((0, Instant::now()));
            at.0 += 1;
            at.1 = Instant::now();
        }
        due
    }

    /// The turn's end was announced: a look begun when its thread went idle is now that turn's report's.
    pub fn announced(&self, thread: &ThreadId, turn: &TurnId) {
        let mut begun = self.begun.lock().unwrap();
        if begun.get(thread) == Some(turn) {
            begun.remove(thread);
        }
    }

    pub fn end_look(&self, thread: &ThreadId) {
        let mut looking = self.looking.lock().unwrap();
        if let Some(at) = looking.get_mut(thread) {
            at.0 -= 1;
            if at.0 == 0 {
                looking.remove(thread);
            }
        }
    }

    pub fn is_looking(&self, thread: &ThreadId) -> bool {
        self.looking.lock().unwrap().get(thread).is_some_and(|(_, since)| since.elapsed() < LOOK_LIMIT)
    }

    pub fn is_closed(&self, assigner: &ThreadId, assigned: &ThreadId) -> bool {
        self.closed.lock().unwrap().contains(&(assigner.clone(), assigned.clone()))
    }
}
