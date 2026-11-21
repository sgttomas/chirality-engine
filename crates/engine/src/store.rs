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
    pub settings: Option<TurnSettings>,
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
}

pub(crate) struct Store {
    state: Mutex<State>,
    events: broadcast::Sender<Event>,
}

pub(crate) fn now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

impl Store {
    pub fn new() -> Store {
        Store { state: Mutex::default(), events: broadcast::channel(4096).0 }
    }

    pub fn subscribe(&self) -> broadcast::Receiver<Event> {
        self.events.subscribe()
    }

    pub fn emit(&self, event: Event) {
        let _state = self.state.lock().unwrap();
        let _ = self.events.send(event);
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
        }
    }

    pub fn codex(&self) -> CodexStatus {
        self.state.lock().unwrap().codex.clone()
    }

    pub fn set_codex(&self, status: CodexStatus) {
        let mut state = self.state.lock().unwrap();
        if state.codex != status {
            state.codex = status.clone();
            let _ = self.events.send(Event::Codex { status });
        }
    }

    pub fn set_account(&self, account: Account) {
        let mut state = self.state.lock().unwrap();
        if state.account != account {
            state.account = account.clone();
            let _ = self.events.send(Event::Account { account });
        }
    }

    pub fn set_models(&self, models: Vec<Model>) {
        let mut state = self.state.lock().unwrap();
        state.models = models.clone();
        let _ = self.events.send(Event::Models { models });
    }

    pub fn projects(&self) -> Vec<Project> {
        self.state.lock().unwrap().projects.clone()
    }

    pub fn set_projects(&self, projects: Vec<Project>) {
        let mut state = self.state.lock().unwrap();
        state.projects = projects.clone();
        let _ = self.events.send(Event::Projects { projects });
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
    pub fn upsert_thread(&self, id: ThreadId, build: impl FnOnce(Option<&ThreadEntry>) -> ThreadEntry) {
        let mut state = self.state.lock().unwrap();
        let before = state.threads.contains_key(&id).then(|| summary(&state, &id));
        let entry = build(state.threads.get(&id));
        state.threads.insert(id.clone(), entry);
        let after = summary(&state, &id);
        if before.as_ref() != Some(&after) {
            let _ = self.events.send(Event::Thread { thread: after });
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
            let _ = self.events.send(Event::Thread { thread: after });
        }
        Some(result)
    }

    pub fn remove_thread(&self, id: &ThreadId) {
        let mut state = self.state.lock().unwrap();
        if state.threads.remove(id).is_some() {
            let _ = self.events.send(Event::ThreadRemoved { thread: id.clone() });
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
        let _ = self.events.send(Event::Pending { request: pending.request.clone() });
        state.pending.push(pending);
        if let Some(entry) = state.threads.get_mut(&thread) {
            entry.unread = true;
        }
        if state.threads.contains_key(&thread) {
            let _ = self.events.send(Event::Thread { thread: summary(&state, &thread) });
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
            let _ = self.events.send(Event::Resolved { request: p.request.id.clone() });
            if state.threads.contains_key(&p.request.thread) {
                let _ = self.events.send(Event::Thread { thread: summary(&state, &p.request.thread) });
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
        settings: entry.settings.clone(),
    }
}
