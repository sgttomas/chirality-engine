//! Messages from one thread to another, delivered in order: to an idle thread
//! at once, starting a turn; to a running one once its turn has ended (never
//! steering it). And the report of an assigned thread's turn that its
//! assigner started: that turn's last reply and how it ended, delivered to
//! the assigner the same way.
//!
//! What waits is held in memory only: a delivery waiting when the engine
//! stops is not made.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::json;

use super::marker::{self, Marker};
use crate::api::*;
use crate::protocol as p;
use crate::session::{Inner, Session};
use crate::store::is_idle;

/// How long a waiting delivery sleeps between looks when no event comes.
const RECHECK: Duration = Duration::from_millis(500);

/// A message for a thread, as another thread sent it.
#[derive(Clone, Debug)]
pub(crate) struct Delivery {
    pub from: Marker,
    pub text: String,
    /// The settings of the turn it starts.
    pub settings: TurnSettings,
}

/// Each thread's deliveries waiting, in order; a thread has an entry while
/// its deliverer runs.
#[derive(Default)]
pub(crate) struct Deliveries {
    queues: Mutex<HashMap<ThreadId, VecDeque<Delivery>>>,
}

impl Deliveries {
    /// Queues a delivery; true when the thread had none waiting, so the
    /// caller delivers it (nothing else is delivering to the thread).
    fn push(&self, thread: &ThreadId, delivery: Delivery) -> bool {
        let mut queues = self.queues.lock().unwrap();
        match queues.get_mut(thread) {
            Some(queue) => {
                queue.push_back(delivery);
                false
            }
            None => {
                queues.insert(thread.clone(), VecDeque::from([delivery]));
                true
            }
        }
    }

    /// The next delivery for the thread; with none left, its deliverer ends.
    fn next(&self, thread: &ThreadId) -> Option<Delivery> {
        let mut queues = self.queues.lock().unwrap();
        let next = queues.get(thread).and_then(|q| q.front().cloned());
        if next.is_none() {
            queues.remove(thread);
        }
        next
    }

    fn pop(&self, thread: &ThreadId) {
        if let Some(queue) = self.queues.lock().unwrap().get_mut(thread) {
            queue.pop_front();
        }
    }

    /// Nothing more for a thread that is gone.
    fn clear(&self, thread: &ThreadId) {
        self.queues.lock().unwrap().remove(thread);
    }

    /// Reports from `assigned` still waiting for `assigner` are not made.
    pub fn drop_reports(&self, assigner: &ThreadId, assigned: &ThreadId) {
        if let Some(queue) = self.queues.lock().unwrap().get_mut(assigner) {
            queue.retain(|d| !(&d.from.from == assigned && d.from.report.is_some()));
        }
    }

    #[cfg(test)]
    pub fn waiting(&self, thread: &ThreadId) -> usize {
        self.queues.lock().unwrap().get(thread).map_or(0, VecDeque::len)
    }
}

fn end_of(state: &TurnState) -> Option<TurnEnd> {
    match state {
        TurnState::Completed => Some(TurnEnd::Completed),
        TurnState::Interrupted => Some(TurnEnd::Stopped),
        TurnState::Failed { .. } => Some(TurnEnd::Failed),
        TurnState::Running => None,
    }
}

/// The mark a turn's first message carries, and the turn's last reply.
fn opener_and_reply(turn: &p::Turn) -> (Option<Marker>, String) {
    let opener = turn.items.iter().find_map(|item| match item {
        p::ThreadItem::UserMessage { content, .. } => Some(match content.first() {
            Some(p::InputInfo::Text { text }) => marker::part(text),
            _ => None,
        }),
        _ => None,
    });
    let reply = turn.items.iter().rev().find_map(|item| match item {
        p::ThreadItem::AgentMessage { text, .. } => Some(text.clone()),
        _ => None,
    });
    (opener.flatten(), reply.unwrap_or_default())
}

/// What became of a message posted to a thread.
#[derive(Debug, PartialEq)]
pub(crate) enum Posted {
    /// It started a turn.
    Now,
    /// It waits for the turn running there to end, or for those before it.
    Later,
    /// Codex refused the turn.
    Refused(String),
    /// The thread is gone.
    Gone,
}

/// One step of delivering to a thread.
enum Step {
    /// Nothing waits; the deliverer ends.
    Empty,
    Started,
    Refused(String),
    /// A turn runs there, or Codex is restarting.
    Wait,
    Gone,
}

impl Inner {
    /// Posts a message to a thread: delivered here and now when nothing waits
    /// before it and no turn runs there (so the turn's start comes before the
    /// caller goes on), else by the thread's deliverer once that turn ends.
    pub(crate) async fn post(self: &Arc<Self>, thread: ThreadId, delivery: Delivery) -> Posted {
        if !self.assignments.deliveries.push(&thread, delivery) {
            return Posted::Later;
        }
        let step = self.step(&thread).await;
        if !matches!(step, Step::Empty | Step::Gone) {
            // More may have come meanwhile; the deliverer takes them, or ends.
            tokio::spawn(self.clone().deliver(thread));
        }
        match step {
            Step::Started => Posted::Now,
            Step::Refused(message) => Posted::Refused(message),
            Step::Wait => Posted::Later,
            Step::Empty | Step::Gone => Posted::Gone,
        }
    }

    /// Delivers the thread's waiting messages one at a time, each once no turn runs there.
    async fn deliver(self: Arc<Self>, thread: ThreadId) {
        let mut events = self.store.subscribe();
        loop {
            match self.step(&thread).await {
                Step::Empty | Step::Gone => return,
                Step::Started | Step::Refused(_) => {}
                Step::Wait => {
                    let _ = tokio::time::timeout(RECHECK, events.recv()).await;
                }
            }
        }
    }

    async fn step(self: &Arc<Self>, thread: &ThreadId) -> Step {
        let deliveries = &self.assignments.deliveries;
        let Some(next) = deliveries.next(thread) else { return Step::Empty };
        // Archived, or the engine has stopped: the assignment has ended.
        let gone = || {
            deliveries.clear(thread);
            Step::Gone
        };
        if self.is_shut_down() {
            return gone();
        }
        match self.store.thread(thread) {
            None => gone(),
            Some(entry) if !is_idle(&entry) => Step::Wait,
            Some(_) => {
                let message = Message { text: next.text.clone(), attachments: Vec::new(), workflow: None };
                match self.start_if_idle(thread, message, next.settings.clone(), next.from.clone()).await {
                    Ok(true) => {
                        deliveries.pop(thread);
                        Step::Started
                    }
                    // A turn began first, or Codex is restarting.
                    Ok(false) | Err(EngineError::CodexUnavailable { .. }) => Step::Wait,
                    Err(EngineError::NotFound { .. }) => gone(),
                    Err(e) => {
                        deliveries.pop(thread);
                        Step::Refused(e.to_string())
                    }
                }
            }
        }
    }

    /// A turn of an assigned thread ended: if its assigner started it, its
    /// last reply and how it ended go to the assigner, unless the assigner
    /// has closed the assignment.
    pub(crate) async fn report(self: Arc<Self>, thread: ThreadId, turn: TurnId, state: TurnState) {
        self.check_report(thread, turn, state).await;
        #[cfg(test)]
        self.assignments.reports.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    }

    async fn check_report(self: &Arc<Self>, thread: ThreadId, turn: TurnId, state: TurnState) {
        let (Some(entry), Some(end)) = (self.store.thread(&thread), end_of(&state)) else { return };
        let Some(assigner) = entry.assigned_by.clone() else { return };
        let Ok(session) = self.session() else { return };
        // The turn as Codex keeps it: its first message and its last reply.
        let params = json!({ "threadId": thread.0, "limit": 4, "sortDirection": "desc", "itemsView": "summary" });
        let Ok(page) = session.call::<p::TurnsListResponse>("thread/turns/list", params).await else { return };
        let Some(kept) = page.data.iter().find(|t| t.id == turn.0) else { return };
        let (opener, reply) = opener_and_reply(kept);
        if !opener.is_some_and(|m| m.from == assigner && m.report.is_none()) || self.closed(&session, &assigner, &thread).await {
            return;
        }
        let Some(settings) = self.store.thread(&assigner).and_then(|e| e.starting.or(e.settings)) else { return };
        let from = Marker { from: thread, title: entry.title, report: Some(end) };
        self.post(assigner, Delivery { from, text: reply, settings }).await;
    }

    /// Whether the assigner has closed the assignment: this run, or by its own call in its history.
    async fn closed(&self, session: &Session, assigner: &ThreadId, assigned: &ThreadId) -> bool {
        if self.assignments.is_closed(assigner, assigned) {
            return true;
        }
        let Ok(turns) = crate::threads::list_turns(session, assigner).await else { return false };
        let closed = turns.iter().flat_map(|t| &t.items).any(|i| {
            matches!(&i.body, ItemBody::ThreadTool { tool: ThreadTool::Close, thread: Some(t), status: ToolCallStatus::Completed, .. } if t == assigned)
        });
        if closed {
            self.assignments.close(assigner, assigned);
        }
        closed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_turn_names_who_opened_it_and_its_last_reply() {
        let mark = marker::render(&Marker { from: ThreadId("lead".into()), title: None, report: None });
        let turn: p::Turn = serde_json::from_value(json!({"id": "t", "status": "completed", "error": null, "items": [
            {"type": "userMessage", "id": "u", "content": [{"type": "text", "text": mark}, {"type": "text", "text": "Check the piles."}]},
            {"type": "agentMessage", "id": "a", "text": "First."},
            {"type": "agentMessage", "id": "b", "text": "The piles are fine."}
        ]}))
        .unwrap();
        let (opener, reply) = opener_and_reply(&turn);
        assert_eq!((opener.map(|m| m.from), reply.as_str()), (Some(ThreadId("lead".into())), "The piles are fine."));
        let person: p::Turn = serde_json::from_value(json!({"id": "t", "status": "failed", "error": null, "items": [
            {"type": "userMessage", "id": "u", "content": [{"type": "text", "text": "Mine."}]}
        ]}))
        .unwrap();
        assert_eq!(opener_and_reply(&person), (None, String::new()));
    }
}
