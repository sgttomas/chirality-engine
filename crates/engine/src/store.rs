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
    pub failed: Option<String>,
    pub unread: bool,
}

/// A request Codex waits on, with what is needed to answer it.
#[derive(Clone, Debug)]
pub(crate) struct Pending {
    pub request: PendingRequest,
    pub generation: u64,
    pub rpc_id: Value,
    pub method: String,
    pub params: Value,
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
    /// Subagent threads, by the thread that started them.
    parents: HashMap<ThreadId, ThreadId>,
    /// Counts renames, so a read sent before one does not undo it.
    renames: u64,
    /// The number of the last event sent.
    seq: u64,
    /// Each thread's last turn as its events told it: Codex's history leaves
    /// out text still streaming, and an item a Stop cut short.
    live: HashMap<ThreadId, Turn>,
}

pub(crate) struct Store {
    state: Mutex<State>,
    events: broadcast::Sender<Numbered>,
}

/// Keeps each thread's last turn current from its events.
fn follow(live: &mut HashMap<ThreadId, Turn>, event: &Event) {
    let fresh = |turn: &TurnId| Turn { id: turn.clone(), items: Vec::new(), state: TurnState::Running };
    match event {
        Event::TurnStarted { thread, turn } => {
            live.insert(thread.clone(), fresh(turn));
        }
        Event::TurnEnded { thread, turn, state } => {
            if let Some(last) = live.get_mut(thread).filter(|t| &t.id == turn) {
                last.state = state.clone();
            }
        }
        Event::Item { thread, turn, item } => {
            let last = live.entry(thread.clone()).or_insert_with(|| fresh(turn));
            if &last.id != turn {
                *last = fresh(turn);
            }
            match last.items.iter_mut().find(|i| i.id == item.id) {
                Some(known) => *known = item.clone(),
                None => last.items.push(item.clone()),
            }
        }
        Event::ItemDelta { thread, turn, item, text } => {
            let found = live.get_mut(thread).filter(|t| &t.id == turn).and_then(|t| t.items.iter_mut().find(|i| &i.id == item));
            if let Some(item) = found {
                match &mut item.body {
                    ItemBody::AgentMessage { text: so_far } | ItemBody::Reasoning { summary: so_far } | ItemBody::Command { output: so_far, .. } => so_far.push_str(text),
                    _ => {}
                }
            }
        }
        Event::ThreadRemoved { thread } => {
            live.remove(thread);
        }
        _ => {}
    }
}

/// Codex's history with the turn the events told: an item Codex has finished
/// is Codex's; one it has not, or has left out, is the events'.
fn merge(turns: &mut Vec<Turn>, live: &Turn) {
    let Some(turn) = turns.iter_mut().find(|t| t.id == live.id) else {
        turns.push(live.clone());
        return;
    };
    if turn.state == TurnState::Running {
        turn.state = live.state.clone();
    }
    let mut after: Option<usize> = None;
    for item in &live.items {
        let at = match turn.items.iter().position(|i| i.id == item.id) {
            Some(at) => {
                if !turn.items[at].done {
                    turn.items[at] = item.clone();
                }
                at
            }
            None => {
                let at = after.map_or(turn.items.len(), |a| a + 1);
                turn.items.insert(at, item.clone());
                at
            }
        };
        after = Some(at);
    }
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
        follow(&mut state.live, &event);
        state.seq += 1;
        let _ = self.events.send(Numbered { seq: state.seq, event });
    }

    /// Codex's history alone holds the thread's turns from now (after a revert).
    pub fn forget_live(&self, thread: &ThreadId) {
        self.state.lock().unwrap().live.remove(thread);
    }

    /// A thread's history from Codex with its last turn as the events told it,
    /// and the number of the last event that covers.
    pub fn merged(&self, thread: &ThreadId, mut turns: Vec<Turn>) -> (Vec<Turn>, u64) {
        let state = self.state.lock().unwrap();
        if let Some(live) = state.live.get(thread) {
            merge(&mut turns, live);
        }
        (turns, state.seq)
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
    pub fn upsert_thread(&self, id: ThreadId, read_at: Option<u64>, build: impl FnOnce(Option<&ThreadEntry>) -> ThreadEntry) {
        let mut state = self.state.lock().unwrap();
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

    pub fn remove_thread(&self, id: &ThreadId) {
        let mut state = self.state.lock().unwrap();
        if state.threads.remove(id).is_some() {
            { let event = Event::ThreadRemoved { thread: id.clone() }; self.send(&mut state, event); }
        }
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
        if let Some(entry) = state.threads.get_mut(&thread) {
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

    pub fn set_parent(&self, child: ThreadId, parent: ThreadId) {
        self.state.lock().unwrap().parents.insert(child, parent);
    }

    /// The thread a subagent's requests are shown on: its root ancestor.
    pub fn owner(&self, thread: ThreadId) -> ThreadId {
        let state = self.state.lock().unwrap();
        let mut current = thread;
        while let Some(parent) = state.parents.get(&current) {
            current = parent.clone();
        }
        current
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

    fn agent(id: &str, text: &str, done: bool) -> Item {
        Item { id: ItemId(id.into()), body: ItemBody::AgentMessage { text: text.into() }, done }
    }

    #[test]
    fn a_read_takes_codex_s_finished_items_and_the_events_for_the_rest() {
        let store = Store::new();
        let (thread, turn) = (ThreadId("t".into()), TurnId("turn-1".into()));
        store.emit(Event::TurnStarted { thread: thread.clone(), turn: turn.clone() });
        for (id, text) in [("a", ""), ("b", "")] {
            store.emit(Event::Item { thread: thread.clone(), turn: turn.clone(), item: agent(id, text, false) });
        }
        store.emit(Event::ItemDelta { thread: thread.clone(), turn: turn.clone(), item: ItemId("a".into()), text: "Fin".into() });
        store.emit(Event::ItemDelta { thread: thread.clone(), turn: turn.clone(), item: ItemId("b".into()), text: "Stre".into() });
        store.emit(Event::TurnEnded { thread: thread.clone(), turn: turn.clone(), state: TurnState::Interrupted });
        // Codex finished `a` (its text is whole) and has `b` only as begun; an older turn is Codex's alone.
        let older = Turn { id: TurnId("turn-0".into()), items: vec![agent("z", "Old", true)], state: TurnState::Completed };
        let history = Turn { id: turn.clone(), items: vec![agent("a", "Finished", true), agent("b", "", false)], state: TurnState::Running };
        let (turns, seq) = store.merged(&thread, vec![older.clone(), history]);
        assert_eq!(seq, 6);
        assert_eq!(turns[0], older);
        assert_eq!(turns[1].items, [agent("a", "Finished", true), agent("b", "Stre", false)]);
        assert_eq!(turns[1].state, TurnState::Interrupted);
        // History without the turn at all: the turn as its events told it.
        let (turns, _) = store.merged(&thread, vec![older]);
        assert_eq!(turns[1].items, [agent("a", "Fin", false), agent("b", "Stre", false)]);
    }
}
