//! The snapshot an application reads, and the event stream that keeps it
//! current. Every change goes through here, so events follow state in order.

use std::collections::HashMap;
use std::sync::Mutex;

use serde_json::Value;
use tokio::sync::broadcast;

use crate::api::*;

/// What the engine keeps about a thread besides what Codex keeps.
#[derive(Clone, Debug)]
pub(crate) struct ThreadEntry {
    pub project: ProjectId,
    pub cwd: String,
    pub title: Option<String>,
    pub forked_from: Option<ThreadId>,
    pub updated_at: i64,
    /// The settings of the last turn Codex took.
    pub settings: Option<TurnSettings>,
    /// The settings of a turn being started, until Codex takes or refuses it.
    pub starting: Option<TurnSettings>,
    pub active_turn: Option<TurnId>,
    /// The last turn Codex ended, so a late `turn/start` answer does not revive it.
    pub last_ended: Option<TurnId>,
    /// A running turn a Stop could not settle: it still shows as running and
    /// can be stopped again, and the next send goes to Codex as a start.
    pub unsettled: Option<TurnId>,
    pub failed: Option<String>,
    pub unread: bool,
    pub subagent: Option<Subagent>,
}

/// A request Codex waits on, with what is needed to answer it.
#[derive(Clone, Debug)]
pub(crate) struct Pending {
    pub request: PendingRequest,
    pub generation: u64,
    pub rpc_id: Value,
    pub method: String,
    pub params: Value,
    /// The thread Codex named, which a request shown on an ancestor is still about.
    pub asked_by: ThreadId,
}

impl Pending {
    fn about(&self, thread: &ThreadId) -> bool {
        &self.request.thread == thread || &self.asked_by == thread
    }
}

#[derive(Default)]
struct State {
    codex: CodexStatus,
    account: Account,
    models: Vec<Model>,
    projects: Vec<Project>,
    threads: HashMap<ThreadId, ThreadEntry>,
    pending: Vec<Pending>,
    /// File changes by Codex item id, for the approval that names only the item.
    file_changes: HashMap<String, Vec<FileDiff>>,
    /// Each subagent's parent, the engine's or not.
    parents: HashMap<ThreadId, ThreadId>,
    /// Counts renames, so a read sent before one does not undo it.
    renames: u64,
    /// The number of the last event sent.
    seq: u64,
}

pub(crate) struct Store {
    state: Mutex<State>,
    events: broadcast::Sender<Numbered>,
}

pub(crate) fn now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

impl Store {
    pub fn new() -> Store {
        Store { state: Mutex::default(), events: broadcast::channel(4096).0 }
    }

    pub fn subscribe(&self) -> broadcast::Receiver<Numbered> {
        self.events.subscribe()
    }

    pub fn emit(&self, event: Event) {
        let mut state = self.state.lock().unwrap();
        self.send(&mut state, event);
    }

    /// Numbers an event and sends it, under the state's lock, so the numbers
    /// follow the order of the changes and a snapshot knows the last it holds.
    fn send(&self, state: &mut State, event: Event) {
        state.seq += 1;
        let _ = self.events.send(Numbered { seq: state.seq, event });
    }

    /// The number of the last event sent.
    pub fn seq(&self) -> u64 {
        self.state.lock().unwrap().seq
    }

    pub fn snapshot(&self) -> Snapshot {
        let state = self.state.lock().unwrap();
        let mut threads: Vec<ThreadSummary> = state.threads.keys().map(|id| summary(&state, id)).collect();
        threads.sort_by(|a, b| b.updated_at.cmp(&a.updated_at).then_with(|| a.id.cmp(&b.id)));
        Snapshot {
            codex: state.codex.clone(),
            account: state.account.clone(),
            models: state.models.clone(),
            projects: state.projects.clone(),
            threads,
            pending: state.pending.iter().map(|p| p.request.clone()).collect(),
            seq: state.seq,
        }
    }

    pub fn codex(&self) -> CodexStatus {
        self.state.lock().unwrap().codex.clone()
    }

    pub fn set_codex(&self, status: CodexStatus) {
        let mut state = self.state.lock().unwrap();
        if state.codex != status {
            state.codex = status.clone();
            { let event = Event::Codex { status }; self.send(&mut state, event); }
        }
    }

    pub fn set_account(&self, account: Account) {
        let mut state = self.state.lock().unwrap();
        if state.account != account {
            state.account = account.clone();
            { let event = Event::Account { account }; self.send(&mut state, event); }
        }
    }

    pub fn set_models(&self, models: Vec<Model>) {
        let mut state = self.state.lock().unwrap();
        state.models = models.clone();
        { let event = Event::Models { models }; self.send(&mut state, event); }
    }

    pub fn projects(&self) -> Vec<Project> {
        self.state.lock().unwrap().projects.clone()
    }

    pub fn set_projects(&self, projects: Vec<Project>) {
        let mut state = self.state.lock().unwrap();
        state.projects = projects.clone();
        { let event = Event::Projects { projects }; self.send(&mut state, event); }
    }

    /// How many renames there have been; see [`Store::upsert_thread`].
    pub fn renames(&self) -> u64 {
        self.state.lock().unwrap().renames
    }

    /// A rename is starting, or Codex reported one.
    pub fn renamed(&self) {
        self.state.lock().unwrap().renames += 1;
    }

    pub fn thread(&self, id: &ThreadId) -> Option<ThreadEntry> {
        self.state.lock().unwrap().threads.get(id).cloned()
    }

    pub fn summary(&self, id: &ThreadId) -> Option<ThreadSummary> {
        let state = self.state.lock().unwrap();
        state.threads.contains_key(id).then(|| summary(&state, id))
    }

    pub fn thread_ids(&self) -> Vec<ThreadId> {
        self.state.lock().unwrap().threads.keys().cloned().collect()
    }

    /// Adds or replaces a thread, built from what is known of it in the same
    /// step (so no change made meanwhile is lost), and announces it if its
    /// summary changed.
    /// `read_at` is [`Store::renames`] when the reply being adopted was asked
    /// for: after a rename since, the known title stays.
    /// With `known_only`, a thread no longer in the snapshot (archived meanwhile) is left out.
    pub fn upsert_thread(&self, id: ThreadId, read_at: Option<u64>, known_only: bool, build: impl FnOnce(Option<&ThreadEntry>) -> ThreadEntry) {
        let mut state = self.state.lock().unwrap();
        if known_only && !state.threads.contains_key(&id) {
            return;
        }
        let before = state.threads.contains_key(&id).then(|| summary(&state, &id));
        let mut entry = build(state.threads.get(&id));
        let renamed_since = read_at.is_some_and(|at| at != state.renames);
        if let Some(title) = state.threads.get(&id).and_then(|e| e.title.clone()).filter(|_| renamed_since) {
            entry.title = Some(title);
        }
        state.threads.insert(id.clone(), entry);
        let after = summary(&state, &id);
        if before.as_ref() != Some(&after) {
            { let event = Event::Thread { thread: after }; self.send(&mut state, event); }
        }
    }

    /// Changes a known thread and announces it if its summary changed.
    /// Returns what `change` returned, or `None` for an unknown thread.
    pub fn update_thread<R>(&self, id: &ThreadId, change: impl FnOnce(&mut ThreadEntry) -> R) -> Option<R> {
        let mut state = self.state.lock().unwrap();
        let before = state.threads.contains_key(id).then(|| summary(&state, id))?;
        let result = change(state.threads.get_mut(id)?);
        let after = summary(&state, id);
        if after != before {
            { let event = Event::Thread { thread: after }; self.send(&mut state, event); }
        }
        Some(result)
    }

    /// Removes a thread, and the requests about it, which no one can answer once it is gone.
    pub fn remove_thread(&self, id: &ThreadId) {
        self.drop_requests(id);
        let mut state = self.state.lock().unwrap();
        if state.threads.remove(id).is_some() {
            { let event = Event::ThreadRemoved { thread: id.clone() }; self.send(&mut state, event); }
        }
    }

    /// Drops the requests about a thread Codex has archived, deleted or closed.
    pub fn drop_requests(&self, thread: &ThreadId) {
        self.resolve(|p| p.about(thread));
    }

    pub fn remember_file_change(&self, item: &str, files: Vec<FileDiff>) {
        self.state.lock().unwrap().file_changes.insert(item.to_string(), files);
    }

    pub fn file_change(&self, item: &str) -> Vec<FileDiff> {
        self.state.lock().unwrap().file_changes.get(item).cloned().unwrap_or_default()
    }

    pub fn add_pending(&self, pending: Pending) {
        let mut state = self.state.lock().unwrap();
        let thread = pending.request.thread.clone();
        { let event = Event::Pending { request: pending.request.clone() }; self.send(&mut state, event); }
        state.pending.push(pending);
        // A subagent's finishing is its parent's to tell, so its request leaves no mark once answered.
        if let Some(entry) = state.threads.get_mut(&thread).filter(|e| e.subagent.is_none()) {
            entry.unread = true;
        }
        if state.threads.contains_key(&thread) {
            { let event = Event::Thread { thread: summary(&state, &thread) }; self.send(&mut state, event); }
        }
    }

    /// Removes one request and announces it; `None` if it is already gone,
    /// so a request is answered once however often the person presses.
    pub fn take_pending(&self, id: &RequestId) -> Option<Pending> {
        let taken = self.pending(id)?;
        let mut found = false;
        self.resolve(|p| {
            let hit = &p.request.id == id;
            found |= hit;
            hit
        });
        found.then_some(taken)
    }

    /// Records a thread's parent, unless that would make a thread its own ancestor.
    pub fn set_parent(&self, child: ThreadId, parent: ThreadId) {
        let mut state = self.state.lock().unwrap();
        if !ancestors(&state.parents, parent.clone()).any(|a| a == child) {
            state.parents.insert(child, parent);
        }
    }

    /// The thread a request is shown on: its own, or for a subagent the engine
    /// does not have, the nearest ancestor it has.
    pub fn shown_on(&self, thread: ThreadId) -> ThreadId {
        let state = self.state.lock().unwrap();
        ancestors(&state.parents, thread.clone()).find(|t| state.threads.contains_key(t)).unwrap_or(thread)
    }

    /// The thread an application's tool call shows on: its root ancestor.
    pub fn owner(&self, thread: ThreadId) -> ThreadId {
        let state = self.state.lock().unwrap();
        ancestors(&state.parents, thread.clone()).last().unwrap_or(thread)
    }

    pub fn pending(&self, id: &RequestId) -> Option<Pending> {
        self.state.lock().unwrap().pending.iter().find(|p| &p.request.id == id).cloned()
    }

    /// Removes the requests `matches` selects, announcing each.
    pub fn resolve(&self, mut matches: impl FnMut(&Pending) -> bool) {
        let mut state = self.state.lock().unwrap();
        let (gone, kept): (Vec<_>, Vec<_>) = std::mem::take(&mut state.pending).into_iter().partition(|p| matches(p));
        state.pending = kept;
        for p in gone {
            { let event = Event::Resolved { request: p.request.id.clone() }; self.send(&mut state, event); }
            if state.threads.contains_key(&p.request.thread) {
                { let event = Event::Thread { thread: summary(&state, &p.request.thread) }; self.send(&mut state, event); }
            }
        }
    }
}

/// A thread, then each ancestor in turn; it ends at a thread seen before, so a cycle cannot hold it.
fn ancestors(parents: &HashMap<ThreadId, ThreadId>, thread: ThreadId) -> impl Iterator<Item = ThreadId> + '_ {
    let mut seen = std::collections::HashSet::new();
    std::iter::successors(Some(thread), |t| parents.get(t).cloned()).take_while(move |t| seen.insert(t.clone()))
}

fn summary(state: &State, id: &ThreadId) -> ThreadSummary {
    let entry = &state.threads[id];
    let waiting = |question: bool| state.pending.iter().any(|p| &p.request.thread == id && matches!(p.request.request, Request::Question { .. }) == question);
    let status = if waiting(false) {
        ThreadStatus::NeedsApproval
    } else if waiting(true) {
        ThreadStatus::NeedsAnswer
    } else if entry.active_turn.is_some() {
        ThreadStatus::Working
    } else if let Some(message) = &entry.failed {
        ThreadStatus::Failed { message: message.clone() }
    } else {
        ThreadStatus::Idle
    };
    ThreadSummary {
        id: id.clone(),
        project: entry.project.clone(),
        title: entry.title.clone(),
        status,
        unread: entry.unread,
        forked_from: entry.forked_from.clone(),
        subagent: entry.subagent.clone(),
        updated_at: entry.updated_at,
        settings: entry.starting.clone().or_else(|| entry.settings.clone()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn events_are_numbered_in_order_and_a_snapshot_knows_the_last_it_holds() {
        let store = Store::new();
        let mut events = store.subscribe();
        assert_eq!(store.snapshot().seq, 0);
        store.set_codex(CodexStatus::Ready { version: "0.171.0".into() });
        store.set_account(Account::SignedOut);
        store.emit(Event::ThreadRemoved { thread: ThreadId("t".into()) });
        let seqs: Vec<u64> = std::iter::from_fn(|| events.try_recv().ok()).map(|n| n.seq).collect();
        assert_eq!(seqs, [1, 2, 3]);
        assert_eq!(store.snapshot().seq, 3);
        // No change, no event, no number.
        store.set_account(Account::SignedOut);
        assert_eq!(store.snapshot().seq, 3);
    }

    #[test]
    fn no_thread_becomes_its_own_ancestor_and_every_walk_ends() {
        let id = |s: &str| ThreadId(s.into());
        let store = Store::new();
        store.set_parent(id("worker"), id("root"));
        store.set_parent(id("inner"), id("worker"));
        // A worker's message to its root names the root; were it a parent, the two would be each other's.
        store.set_parent(id("root"), id("worker"));
        store.set_parent(id("root"), id("inner"));
        store.set_parent(id("root"), id("root"));
        assert_eq!(store.owner(id("inner")), id("root"));
        assert_eq!(store.owner(id("root")), id("root"));
        // A map that holds a cycle all the same ends where the walk comes back.
        store.state.lock().unwrap().parents.insert(id("root"), id("inner"));
        assert_eq!(store.owner(id("inner")), id("root"));
        assert_eq!(store.shown_on(id("inner")), id("inner"));
    }

    #[test]
    fn a_thread_removed_takes_the_requests_about_it() {
        let id = |s: &str| ThreadId(s.into());
        let store = Store::new();
        let ask = |n: &str, shown: &str, asked: &str| Pending {
            request: PendingRequest { id: RequestId(n.into()), thread: id(shown), request: Request::Permissions { reason: None } },
            generation: 1,
            rpc_id: Value::from(n),
            method: "item/permissions/requestApproval".into(),
            params: Value::Null,
            asked_by: id(asked),
        };
        store.add_pending(ask("1", "worker", "worker"));
        store.add_pending(ask("2", "lead", "inner"));
        store.add_pending(ask("3", "lead", "lead"));
        store.remove_thread(&id("worker"));
        store.drop_requests(&id("inner"));
        let left: Vec<_> = store.snapshot().pending.into_iter().map(|p| p.id.0).collect();
        assert_eq!(left, ["3"]);
    }
}
