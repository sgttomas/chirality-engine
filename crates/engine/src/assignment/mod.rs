//! Assignment between threads: the engine's own tools (namespace
//! `chirality`), given to the agent on every thread the engine starts, with
//! which one thread gives work to another, ordinary thread and the two
//! exchange messages.
//!
//! - `tools`: the tools as Codex takes them, their items in the
//!   conversation, and the answers to their calls.
//! - `delivery`: messages waiting for a thread's running turn to end, and the
//!   report of an assigned thread's turn to its assigner.
//! - `marker`: the mark on a message one thread sends another.
//!
//! The engine keeps nothing Codex does not: an assignment is read from the
//! assigned thread's first message, reports and messages are messages in
//! Codex's history, and a close is the assigner's own call in its history.
//! Only the deliveries waiting for a turn to end are held, in memory.

pub(crate) mod delivery;
pub(crate) mod marker;
pub(crate) mod tools;

use std::collections::{HashMap, HashSet};
use std::sync::Mutex;

use tokio::sync::oneshot;

use crate::api::ThreadId;

/// The namespace the engine's tools are given in; an application's tools cannot use it.
pub(crate) const NAMESPACE: &str = "chirality";

#[derive(Default)]
pub(crate) struct Assignments {
    pub deliveries: delivery::Deliveries,
    /// Assignments closed while the engine runs, (assigner, assigned); after a
    /// restart a close is found in the assigner's history.
    closed: Mutex<HashSet<(ThreadId, ThreadId)>>,
    /// Each assigned thread's last report looked at, so the next waits for it.
    reports_in_order: Mutex<HashMap<ThreadId, oneshot::Receiver<()>>>,
    /// Turn ends of assigned threads looked at for a report, for tests to wait on.
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

    pub fn is_closed(&self, assigner: &ThreadId, assigned: &ThreadId) -> bool {
        self.closed.lock().unwrap().contains(&(assigner.clone(), assigned.clone()))
    }
}
