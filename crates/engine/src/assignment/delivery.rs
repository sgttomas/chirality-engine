//! Messages from one thread to another, delivered in order: to an idle thread
//! at once, starting a turn; to a running one once the engine has seen its
//! turn end (never steering it). And the report of a turn another thread's
//! message started (an assignment's brief, or any message): that turn's last
//! reply and how it ended, delivered back to that thread the same way. A report
//! starts a turn that reports nothing.
//!
//! A delivery's settings are the receiving thread's, read under its
//! admission just before its turn starts; where the engine does not know that
//! thread's access, no more than Ask, the sender's, or what Codex resumed or
//! forked the thread with (read only if it did not say). What waits is held in memory only: a delivery waiting when the
//! engine stops is not made.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::json;

use super::marker::{self, Marker};
use super::tools::lower;
use crate::api::*;
use crate::protocol as p;
use crate::session::{Inner, Session};
use crate::store::{ThreadEntry, is_idle};
use crate::turns::{NotStarted, Sent, Start};

/// How long a waiting delivery sleeps between looks when no event comes.
const RECHECK: Duration = Duration::from_millis(500);
/// How far back a report looks for its turn, in pages of ten.
const REPORT_PAGES: usize = 10;

/// A message for a thread, as another thread sent it.
#[derive(Clone, Debug)]
pub(crate) struct Delivery {
    /// Its place in the queue, given when queued.
    id: u64,
    pub from: Marker,
    pub text: String,
    /// The most access its turn may have when the receiving thread's is not known: the sender's.
    pub bound: Access,
    /// Model and effort for a receiving thread whose own are not known: the sender's.
    pub fallback: TurnSettings,
    /// Its `turn/start` went to Codex, which stopped before it answered: it may have taken the turn.
    uncertain: Option<Sent>,
}

impl Delivery {
    pub fn new(from: Marker, text: String, bound: Access, fallback: TurnSettings) -> Delivery {
        Delivery { id: 0, from, text, bound, fallback, uncertain: None }
    }
}

/// The settings a delivered turn runs with: the receiving thread's own, and
/// where its access is not known, no more than Ask, the sender's, or what Codex
/// resumed or forked it with; read only if Codex did not say.
pub(crate) fn resolve(entry: &ThreadEntry, resumed: Option<Access>, bound: Access, fallback: &TurnSettings) -> TurnSettings {
    let own = entry.starting.clone().or(entry.settings.clone());
    let (model, effort) = own.as_ref().map_or_else(|| (fallback.model.clone(), fallback.effort.clone()), |s| (s.model.clone(), s.effort.clone()));
    let access = match own {
        Some(own) if entry.access_known => own.access,
        _ => lower(lower(Access::Ask, bound), resumed.unwrap_or(Access::ReadOnly)),
    };
    TurnSettings { model, effort, access }
}

/// Each thread's deliveries waiting, in order; a thread has an entry while
/// its deliverer runs.
#[derive(Default)]
pub(crate) struct Deliveries {
    queues: Mutex<Queues>,
}

#[derive(Default)]
struct Queues {
    by_thread: HashMap<ThreadId, VecDeque<Delivery>>,
    next_id: u64,
}

impl Deliveries {
    /// Queues a delivery, giving its id; true when the thread had none
    /// waiting, so the caller delivers it.
    fn push(&self, thread: &ThreadId, mut delivery: Delivery) -> (bool, u64) {
        let mut queues = self.queues.lock().unwrap();
        queues.next_id += 1;
        let id = queues.next_id;
        delivery.id = id;
        match queues.by_thread.get_mut(thread) {
            Some(queue) => {
                queue.push_back(delivery);
                (false, id)
            }
            None => {
                queues.by_thread.insert(thread.clone(), VecDeque::from([delivery]));
                (true, id)
            }
        }
    }

    /// The next delivery for the thread; with none left, its deliverer ends.
    fn next(&self, thread: &ThreadId) -> Option<Delivery> {
        let mut queues = self.queues.lock().unwrap();
        let next = queues.by_thread.get(thread).and_then(|q| q.front().cloned());
        if next.is_none() {
            queues.by_thread.remove(thread);
        }
        next
    }

    /// Takes one delivery out, by its own id, wherever it now is in the queue.
    fn remove(&self, thread: &ThreadId, id: u64) {
        if let Some(queue) = self.queues.lock().unwrap().by_thread.get_mut(thread) {
            queue.retain(|d| d.id != id);
        }
    }

    fn mark_uncertain(&self, thread: &ThreadId, id: u64, sent: Sent) {
        if let Some(queue) = self.queues.lock().unwrap().by_thread.get_mut(thread) {
            queue.iter_mut().filter(|d| d.id == id).for_each(|d| d.uncertain = Some(sent.clone()));
        }
    }

    /// Nothing more for a thread that is gone.
    fn clear(&self, thread: &ThreadId) {
        self.queues.lock().unwrap().by_thread.remove(thread);
    }

    /// The thread is archived: nothing more is delivered to it or from it.
    pub fn forget(&self, thread: &ThreadId) {
        let mut queues = self.queues.lock().unwrap();
        queues.by_thread.remove(thread);
        for queue in queues.by_thread.values_mut() {
            queue.retain(|d| &d.from.from != thread);
        }
    }

    /// Reports from `assigned` still waiting for `assigner` are not made.
    pub fn drop_reports(&self, assigner: &ThreadId, assigned: &ThreadId) {
        if let Some(queue) = self.queues.lock().unwrap().by_thread.get_mut(assigner) {
            queue.retain(|d| !(&d.from.from == assigned && d.from.report.is_some()));
        }
    }

    #[cfg(test)]
    pub fn waiting(&self, thread: &ThreadId) -> usize {
        self.queues.lock().unwrap().by_thread.get(thread).map_or(0, VecDeque::len)
    }

    #[cfg(test)]
    pub fn waiting_anywhere(&self) -> usize {
        self.queues.lock().unwrap().by_thread.values().map(VecDeque::len).sum()
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

/// The mark and text a turn's first message carries.
fn opened_with(turn: &p::Turn) -> Option<(Marker, String)> {
    let content = turn.items.iter().find_map(|item| match item {
        p::ThreadItem::UserMessage { content, .. } => Some(content),
        _ => None,
    })?;
    let text = |part: Option<&p::InputInfo>| match part {
        Some(p::InputInfo::Text { text }) => Some(text.clone()),
        _ => None,
    };
    let mark = marker::part(&text(content.first())?)?;
    Some((mark, text(content.get(1)).unwrap_or_default()))
}

/// A turn's last reply.
fn last_reply(turn: &p::Turn) -> String {
    let reply = turn.items.iter().rev().find_map(|item| match item {
        p::ThreadItem::AgentMessage { text, .. } => Some(text.clone()),
        _ => None,
    });
    reply.unwrap_or_default()
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
    /// The thread is gone, or the message's sender or assignment is.
    Gone,
}

/// One step of delivering to a thread.
enum Step {
    /// Nothing waits; the deliverer ends.
    Empty,
    Started,
    Refused(String),
    /// Its sender is gone, or the assignment it reports on is closed: not made.
    Dropped,
    /// A turn runs there, or Codex is restarting.
    Wait,
    Gone,
}

impl Inner {
    /// Posts a message to a thread: delivered here and now when nothing waits
    /// before it and no turn runs there (so the turn's start comes before the
    /// caller goes on), else by the thread's deliverer once that turn ends.
    pub(crate) async fn post(self: &Arc<Self>, thread: ThreadId, delivery: Delivery) -> Posted {
        let (first, own) = self.assignments.deliveries.push(&thread, delivery);
        if !first {
            return Posted::Later;
        }
        let (step, took) = self.step(&thread).await;
        if !matches!(step, Step::Empty | Step::Gone) {
            // More may have come meanwhile; the deliverer takes them, or ends.
            tokio::spawn(self.clone().deliver(thread));
        }
        // What became of another delivery says nothing of this one.
        if took != Some(own) {
            return if matches!(step, Step::Gone) { Posted::Gone } else { Posted::Later };
        }
        match step {
            Step::Started => Posted::Now,
            Step::Refused(message) => Posted::Refused(message),
            Step::Wait => Posted::Later,
            Step::Empty | Step::Gone | Step::Dropped => Posted::Gone,
        }
    }

    /// Delivers the thread's waiting messages one at a time, each once no turn
    /// runs there. A turn Codex refuses is told on the thread.
    async fn deliver(self: Arc<Self>, thread: ThreadId) {
        let mut events = self.store.subscribe();
        loop {
            match self.step(&thread).await.0 {
                Step::Empty | Step::Gone => return,
                Step::Started | Step::Dropped => {}
                Step::Refused(message) => self.not_delivered(&thread, &message),
                Step::Wait => {
                    let _ = tokio::time::timeout(RECHECK, events.recv()).await;
                }
            }
        }
    }

    fn not_delivered(&self, thread: &ThreadId, message: &str) {
        self.store.update_thread(thread, |e| e.failed = Some(format!("A message from another thread was not delivered: {message}")));
    }

    /// Whether a delivery from `from` still has a place: its sender is still there, and a
    /// report from a thread this one assigned comes while the assignment is open.
    pub(crate) fn still_holds(&self, thread: &ThreadId, from: &Marker) -> bool {
        let Some(sender) = self.store.thread(&from.from) else { return false };
        from.report.is_none() || sender.assigned_by.as_ref() != Some(thread) || !self.assignments.is_closed(thread, &from.from)
    }

    /// One step, and the id of the delivery it took, if it took one.
    async fn step(self: &Arc<Self>, thread: &ThreadId) -> (Step, Option<u64>) {
        let deliveries = &self.assignments.deliveries;
        let Some(next) = deliveries.next(thread) else { return (Step::Empty, None) };
        (self.step_with(thread, &next).await, Some(next.id))
    }

    async fn step_with(self: &Arc<Self>, thread: &ThreadId, next: &Delivery) -> Step {
        let deliveries = &self.assignments.deliveries;
        // Archived, or the engine has stopped: the assignment has ended.
        let gone = || {
            deliveries.clear(thread);
            Step::Gone
        };
        if self.is_shut_down() {
            return gone();
        }
        let Some(entry) = self.store.thread(thread) else { return gone() };
        if !self.still_holds(thread, &next.from) {
            deliveries.remove(thread, next.id);
            return Step::Dropped;
        }
        // A turn running, or one that ended still being looked at for its report.
        if !is_idle(&entry) || self.assignments.is_looking(thread) {
            return Step::Wait;
        }
        if let Some(sent) = &next.uncertain {
            match self.delivered_already(thread, next, sent).await {
                Some(true) => {
                    deliveries.remove(thread, next.id);
                    return Step::Started;
                }
                Some(false) => {}
                None => return Step::Wait,
            }
        }
        match self.start_if_idle(thread, next).await {
            Ok(Start::Started) => {
                deliveries.remove(thread, next.id);
                Step::Started
            }
            Ok(Start::Busy) => Step::Wait,
            Ok(Start::Dropped) => {
                deliveries.remove(thread, next.id);
                Step::Dropped
            }
            Err(NotStarted { error: EngineError::CodexUnavailable { .. }, sent }) => {
                // Sent, Codex may have taken the turn before it stopped answering: look before trying again.
                if let Some(sent) = sent {
                    deliveries.mark_uncertain(thread, next.id, sent);
                }
                Step::Wait
            }
            Err(NotStarted { error: EngineError::NotFound { .. }, .. }) => gone(),
            Err(NotStarted { error, .. }) => {
                deliveries.remove(thread, next.id);
                Step::Refused(error.to_string())
            }
        }
    }

    /// Whether a turn of the thread's after those it had when the delivery was
    /// sent opens with it; `None` while Codex cannot say.
    async fn delivered_already(&self, thread: &ThreadId, delivery: &Delivery, sent: &Sent) -> Option<bool> {
        let session = self.session().ok()?;
        let params = json!({ "threadId": thread.0, "limit": 2, "sortDirection": "desc", "itemsView": "summary" });
        let page = session.call::<p::TurnsListResponse>("thread/turns/list", params).await.ok()?;
        let newer = page.data.iter().take_while(|t| sent.after.as_ref().is_none_or(|after| t.id != after.0));
        Some(newer.filter_map(opened_with).any(|(mark, text)| mark == delivery.from && text == delivery.text))
    }

    /// A turn that may have begun with another thread's message ended: its
    /// report is looked at after those of the thread's earlier turns, in the
    /// order the turns ended. The look began when the turn's end was heard.
    pub(crate) fn report_in_order(self: &Arc<Self>, thread: ThreadId, turn: TurnId, state: TurnState) {
        let (before, done) = self.assignments.reserve_report(&thread);
        let looked = Looked(self.clone(), thread.clone());
        tokio::spawn(async move {
            if let Some(before) = before {
                let _ = before.await;
            }
            looked.0.report(thread, turn, state).await;
            drop(looked);
            let _ = done.send(());
        });
    }

    /// If another thread's message started the turn, its last reply and how it
    /// ended go back to that thread, unless it assigned this one and has closed
    /// the assignment.
    async fn report(self: &Arc<Self>, thread: ThreadId, turn: TurnId, state: TurnState) {
        self.check_report(thread, turn, state).await;
        #[cfg(test)]
        self.assignments.reports.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    }

    async fn check_report(self: &Arc<Self>, thread: ThreadId, turn: TurnId, state: TurnState) {
        let (Some(entry), Some(end)) = (self.store.thread(&thread), end_of(&state)) else { return };
        if entry.subagent.is_some() {
            return;
        }
        let Ok(session) = self.session() else { return };
        // The turn as Codex keeps it: its first message and its last reply.
        let Some(kept) = find_turn(&session, &thread, &turn).await else { return };
        // A report's turn reports nothing, so exchanges of reports cannot go on.
        let Some(sender) = opened_with(&kept).map(|(mark, _)| mark).filter(|m| m.report.is_none() && m.from != thread).map(|m| m.from) else { return };
        if entry.assigned_by.as_ref() == Some(&sender) && self.closed(&session, &sender, &thread).await {
            return;
        }
        let bound = if entry.access_known { entry.settings.as_ref().map_or(Access::ReadOnly, |s| s.access) } else { Access::ReadOnly };
        let Some(fallback) = self.settings_of(&sender).or(entry.settings.clone()) else { return };
        let from = Marker { from: thread, title: entry.title, report: Some(end) };
        if let Posted::Refused(message) = self.post(sender.clone(), Delivery::new(from, last_reply(&kept), bound, fallback)).await {
            self.not_delivered(&sender, &message);
        }
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

/// A look at a thread's ended turn, over when this is dropped, whatever became of it.
struct Looked(Arc<Inner>, ThreadId);

impl Drop for Looked {
    fn drop(&mut self) {
        self.0.assignments.end_look(&self.1);
    }
}

/// The thread's latest turn, if it has one.
pub(crate) async fn latest_turn(session: &Session, thread: &ThreadId) -> Result<Option<TurnId>> {
    let params = json!({ "threadId": thread.0, "limit": 1, "sortDirection": "desc", "itemsView": "notLoaded" });
    let page: p::TurnsListResponse = session.call("thread/turns/list", params).await?;
    Ok(page.data.first().map(|t| TurnId(t.id.clone())))
}

/// The turn, by its id, from the newest back.
async fn find_turn(session: &Session, thread: &ThreadId, turn: &TurnId) -> Option<p::Turn> {
    let mut cursor: Option<String> = None;
    for _ in 0..REPORT_PAGES {
        let params = json!({ "threadId": thread.0, "cursor": cursor, "limit": 10, "sortDirection": "desc", "itemsView": "summary" });
        let page = session.call::<p::TurnsListResponse>("thread/turns/list", params).await.ok()?;
        if let Some(found) = page.data.iter().find(|t| t.id == turn.0) {
            return Some(found.clone());
        }
        cursor = page.next_cursor.filter(|_| !page.data.is_empty());
        cursor.as_ref()?;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_turn_names_who_opened_it_and_its_last_reply() {
        let lead = Marker { from: ThreadId("lead".into()), title: None, report: None };
        let turn: p::Turn = serde_json::from_value(json!({"id": "t", "status": "completed", "error": null, "items": [
            {"type": "userMessage", "id": "u", "content": [{"type": "text", "text": marker::render(&lead)}, {"type": "text", "text": "Check the piles."}]},
            {"type": "agentMessage", "id": "a", "text": "First."},
            {"type": "agentMessage", "id": "b", "text": "The piles are fine."}
        ]}))
        .unwrap();
        assert_eq!(opened_with(&turn), Some((lead, "Check the piles.".into())));
        assert_eq!(last_reply(&turn), "The piles are fine.");
        let person: p::Turn = serde_json::from_value(json!({"id": "t", "status": "failed", "error": null, "items": [
            {"type": "userMessage", "id": "u", "content": [{"type": "text", "text": "Mine."}]}
        ]}))
        .unwrap();
        assert_eq!((opened_with(&person), last_reply(&person)), (None, String::new()));
    }

    #[test]
    fn a_delivery_is_taken_out_by_its_own_id_whatever_was_taken_out_before() {
        let deliveries = Deliveries::default();
        let lead = ThreadId("lead".into());
        let one = |from: &str, report| Delivery::new(Marker { from: ThreadId(from.into()), title: None, report }, "x".into(), Access::Ask, TurnSettings { model: "m".into(), effort: None, access: Access::Ask });
        assert!(deliveries.push(&lead, one("worker", Some(TurnEnd::Completed))).0);
        assert!(!deliveries.push(&lead, one("other", None)).0);
        // The deliverer has the report in hand when the assignment is closed, then delivers it.
        let held = deliveries.next(&lead).unwrap();
        deliveries.drop_reports(&lead, &ThreadId("worker".into()));
        deliveries.remove(&lead, held.id);
        assert_eq!(deliveries.next(&lead).map(|d| d.from.from), Some(ThreadId("other".into())), "the message after it is still there");
    }

    fn entry(settings: Option<TurnSettings>, access_known: bool) -> ThreadEntry {
        ThreadEntry {
            project: ProjectId("p".into()),
            cwd: "/p".into(),
            title: None,
            forked_from: None,
            updated_at: 0,
            settings,
            access_known,
            starting: None,
            active_turn: None,
            last_ended: None,
            unsettled: None,
            failed: None,
            unread: false,
            subagent: None,
            assigned_by: None,
        }
    }

    #[test]
    fn a_delivered_turn_never_has_access_the_engine_invents() {
        let with = |access| TurnSettings { model: "m".into(), effort: Some(Effort("high".into())), access };
        let sender = TurnSettings { model: "s".into(), effort: None, access: Access::FullAccess };
        // Known: the receiving thread's own, whatever the sender's.
        assert_eq!(resolve(&entry(Some(with(Access::FullAccess)), true), None, Access::ReadOnly, &sender), with(Access::FullAccess));
        assert_eq!(resolve(&entry(Some(with(Access::ReadOnly)), true), Some(Access::FullAccess), Access::FullAccess, &sender), with(Access::ReadOnly));
        // Listed only: its model and effort, and no more than Ask, the sender's, or what Codex resumed it with.
        assert_eq!(resolve(&entry(Some(with(Access::Ask)), false), Some(Access::FullAccess), Access::FullAccess, &sender), with(Access::Ask));
        assert_eq!(resolve(&entry(Some(with(Access::Ask)), false), Some(Access::Ask), Access::ReadOnly, &sender), with(Access::ReadOnly));
        assert_eq!(resolve(&entry(Some(with(Access::Ask)), false), Some(Access::ReadOnly), Access::FullAccess, &sender), with(Access::ReadOnly));
        // Codex did not say what it loaded the thread with: read only.
        assert_eq!(resolve(&entry(Some(with(Access::FullAccess)), false), None, Access::FullAccess, &sender), with(Access::ReadOnly));
        // Nothing known of it: the sender's model and effort.
        assert_eq!(resolve(&entry(None, false), Some(Access::FullAccess), Access::FullAccess, &sender), TurnSettings { access: Access::Ask, ..sender });
    }
}
