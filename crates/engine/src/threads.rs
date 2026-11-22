//! Threads, which Codex keeps: listed per project folder, read, started,
//! resumed, forked, reverted, renamed and archived.

use std::sync::Arc;

use serde_json::json;

use crate::api::*;
use crate::assignment::marker::Marker;
use crate::protocol as p;
use crate::rpc::RpcError;
use crate::session::{Inner, Session, engine_error};

const PAGE: u32 = 100;

pub(crate) async fn list_turns(session: &Session, thread: &ThreadId) -> std::result::Result<Vec<Turn>, RpcError> {
    let mut turns = Vec::new();
    let mut cursor: Option<String> = None;
    loop {
        let params = json!({ "threadId": thread.0, "cursor": cursor, "limit": PAGE, "sortDirection": "asc", "itemsView": "full" });
        let page: p::TurnsListResponse = session.client.request("thread/turns/list", params).await?;
        let empty = page.data.is_empty();
        turns.extend(page.data.iter().map(crate::mapping::turn));
        match page.next_cursor {
            Some(next) if !empty => cursor = Some(next),
            _ => return Ok(turns),
        }
    }
}

/// The turn Codex runs on a thread it lists as active: a listing leaves its turns out.
async fn running_turn(session: &Session, thread: &ThreadId) -> Option<TurnId> {
    let params = json!({ "threadId": thread.0, "limit": 1, "sortDirection": "desc", "itemsView": "notLoaded" });
    let page: p::TurnsListResponse = session.client.request("thread/turns/list", params).await.ok()?;
    page.data.into_iter().next().filter(|t| t.status == p::TurnStatus::InProgress).map(|t| TurnId(t.id))
}

impl Inner {
    /// Reads a project's threads from Codex into the snapshot, then each one's
    /// subagents, which Codex lists only by the thread they descend from.
    pub(crate) async fn list_threads(&self, project: &Project) -> Result<()> {
        let mut roots = Vec::new();
        let folder = Some(vec![project.path.display().to_string()]);
        self.list_pages(|cursor| p::ThreadListParams { cwd: folder.clone(), ancestor_thread_id: None, limit: PAGE, cursor }, &mut roots).await?;
        for root in roots {
            let ancestor = Some(root.0);
            // A thread whose subagents cannot be listed is still listed itself.
            let _ = self.list_pages(|cursor| p::ThreadListParams { cwd: None, ancestor_thread_id: ancestor.clone(), limit: PAGE, cursor }, &mut Vec::new()).await;
        }
        Ok(())
    }

    /// Takes in every page of a listing; `taken` gets the threads taken in.
    async fn list_pages(&self, params: impl Fn(Option<String>) -> p::ThreadListParams, taken: &mut Vec<ThreadId>) -> Result<()> {
        let session = self.session()?;
        let mut cursor = None;
        loop {
            let read_at = self.store.renames();
            let page: p::ThreadListResponse = session.call("thread/list", params(cursor)).await?;
            for thread in &page.data {
                let Some(id) = self.adopt_listed(thread, read_at) else { continue };
                if self.needs_confirming(thread) {
                    // Whether it is a fork, which the listing leaves out; a thread that cannot be read is not taken as assigned.
                    let read_at = self.store.renames();
                    if let Ok(read) = session.call::<p::ThreadResponse>("thread/read", json!({ "threadId": id.0 })).await {
                        self.refresh_thread(&read.thread, read_at);
                    }
                }
                let unknown_turn = matches!(thread.status, p::ThreadStatus::Active) && self.store.thread(&id).is_some_and(|e| e.active_turn.is_none());
                if unknown_turn && let Some(turn) = running_turn(&session, &id).await {
                    self.take_running(&id, turn);
                }
                taken.push(id);
            }
            match page.next_cursor {
                Some(next) if !page.data.is_empty() => cursor = Some(next),
                _ => return Ok(()),
            }
        }
    }

    /// A turn Codex says runs, unless the engine already has one running or has
    /// since seen this one end.
    fn take_running(&self, thread: &ThreadId, turn: TurnId) {
        self.store.update_thread(thread, |e| {
            if e.active_turn.is_none() && e.last_ended.as_ref() != Some(&turn) {
                e.active_turn = Some(turn);
            }
        });
    }

    pub(crate) fn project(&self, id: &ProjectId) -> Result<Project> {
        self.store.projects().into_iter().find(|p| &p.id == id).ok_or_else(|| EngineError::NotFound { what: format!("project {}", id.0) })
    }

    pub(crate) fn known_thread(&self, id: &ThreadId) -> Result<crate::store::ThreadEntry> {
        self.store.thread(id).ok_or_else(|| EngineError::NotFound { what: format!("thread {}", id.0) })
    }

    /// Loads a thread into this Codex process if it is not yet, so turns can run on it.
    pub(crate) async fn ensure_loaded(&self, session: &Session, thread: &ThreadId) -> Result<()> {
        self.load(session, thread).await.map(|_| ())
    }

    /// Loads a thread as [`Inner::ensure_loaded`] does, and gives the access
    /// Codex resumed or forked it with, if it did (see
    /// [`p::LoadResponse::access`]), which bounds an access the engine does not know.
    pub(crate) async fn load(&self, session: &Session, thread: &ThreadId) -> Result<Option<Access>> {
        if let Some(access) = session.loaded_access(thread) {
            return Ok(access);
        }
        let entry = self.known_thread(thread)?;
        let resumed: p::LoadResponse = session.call("thread/resume", json!({ "threadId": thread.0, "cwd": entry.cwd, "excludeTurns": true })).await?;
        self.tool_calls.reopen_thread(&thread.0);
        let access = resumed.access();
        session.mark_loaded(thread, Some(access));
        Ok(Some(access))
    }

    pub(crate) async fn read_thread(&self, thread: &ThreadId) -> Result<ThreadView> {
        let session = self.session()?;
        self.known_thread(thread)?;
        // Every event after this is applied to the view; one before it, the history holds.
        let seq = self.store.seq();
        let read_at = self.store.renames();
        let history = async {
            let read: p::ThreadResponse = session.client.request("thread/read", json!({ "threadId": thread.0 })).await?;
            self.refresh_thread(&read.thread, read_at);
            list_turns(&session, thread).await
        };
        let (turns, seq) = match history.await {
            Ok(turns) => {
                session.mark_kept(thread);
                if let Some(turn) = turns.last().filter(|t| t.state == TurnState::Running) {
                    self.take_running(thread, turn.id.clone());
                }
                (turns, seq)
            }
            // A thread the engine started, before Codex has kept it and before any turn of it has
            // ended, whatever Codex answers: its history is empty, and every event since it
            // started is after the view's number.
            Err(e) => match session.unkept_since(thread) {
                Some(since) if self.known_thread(thread)?.last_ended.is_none() => (Vec::new(), since),
                _ => return Err(engine_error(e)),
            },
        };
        let summary = self.store.summary(thread).ok_or_else(|| EngineError::NotFound { what: format!("thread {}", thread.0) })?;
        Ok(ThreadView { summary, turns, seq })
    }

    pub(crate) async fn start_thread(self: &Arc<Self>, project: &ProjectId, message: Message, settings: TurnSettings) -> Result<ThreadId> {
        self.start_thread_from(project, message, settings, None).await
    }

    /// Starts a thread another thread assigned, with its brief marked as from that thread, and
    /// gives it its title once its first turn has started.
    pub(crate) async fn start_assigned(self: &Arc<Self>, project: &ProjectId, brief: Message, settings: TurnSettings, from: Marker, title: String) -> Result<ThreadId> {
        let id = self.start_thread_from(project, brief, settings, Some((from, title.clone()))).await?;
        // Codex keeps the title once the thread has its first turn; one it does not take leaves the brief's own when listed.
        self.store.renamed();
        if let Ok(session) = self.session() {
            let _ = session.call_value("thread/name/set", json!({ "threadId": id.0, "name": title })).await;
        }
        Ok(id)
    }

    /// `assigned`: the assigning thread's mark and the title it gave, which the thread has from the start.
    async fn start_thread_from(self: &Arc<Self>, project: &ProjectId, message: Message, settings: TurnSettings, assigned: Option<(Marker, String)>) -> Result<ThreadId> {
        let project = self.project(project)?;
        let session = self.session()?;
        // A first message that cannot be made makes no thread.
        if let Some(id) = &message.workflow {
            crate::workflows::pick(id, &self.config.workflow_roots, &project.path)?;
        }
        let before = self.store.seq();
        let params = p::ThreadStartParams { cwd: project.path.display().to_string(), model: settings.model.clone(), dynamic_tools: Some(crate::tools::specs(self.config.tools.as_ref())) };
        let started: p::ThreadResponse = session.call("thread/start", params).await?;
        let id = self.adopt_thread(&started.thread, None).ok_or_else(|| EngineError::Codex { message: "Codex started the thread in another folder".into() })?;
        session.mark_loaded(&id, None);
        session.mark_unkept(&id, before);
        if let Some((from, title)) = &assigned {
            self.store.update_thread(&id, |e| {
                e.assigned_by = Some(from.from.clone());
                e.title = Some(title.clone());
            });
        }
        if let Err(e) = self.send_from(&id, message, settings, assigned.map(|(from, _)| from)).await {
            // No turn started: the thread the engine just made is not listed.
            self.store.remove_thread(&id);
            session.forget_loaded(&id);
            return Err(e);
        }
        Ok(id)
    }

    pub(crate) async fn fork(&self, thread: &ThreadId, through: &TurnId) -> Result<ThreadId> {
        let session = self.session()?;
        if self.known_thread(thread)?.active_turn.as_ref() == Some(through) {
            return Err(EngineError::Invalid { message: "A turn can be forked once it has ended.".into() });
        }
        self.ensure_loaded(&session, thread).await?;
        let params = json!({ "threadId": thread.0, "lastTurnId": through.0, "excludeTurns": true });
        let forked: p::LoadResponse = session.call("thread/fork", params).await?;
        let id = self.adopt_thread(&forked.thread, None).ok_or_else(|| EngineError::Codex { message: "Codex forked the thread into another folder".into() })?;
        let parent = self.store.thread(thread);
        self.store.update_thread(&id, |e| {
            e.forked_from = Some(thread.clone());
            // A fork is not an assignment, though it has the same first message.
            e.assigned_by = None;
            if let Some(parent) = &parent {
                e.settings = parent.settings.clone();
                e.access_known = parent.access_known;
                if let Some(title) = &parent.title {
                    e.title.get_or_insert_with(|| title.clone());
                }
            }
        });
        session.mark_loaded(&id, Some(forked.access()));
        Ok(id)
    }

    pub(crate) async fn revert(&self, thread: &ThreadId, from: &TurnId) -> Result<()> {
        let session = self.session()?;
        self.ensure_loaded(&session, thread).await?;
        let _: p::ThreadResponse = session.call("thread/revert", json!({ "threadId": thread.0, "beforeTurnId": from.0 })).await?;
        self.store.update_thread(thread, |e| e.updated_at = crate::store::now());
        Ok(())
    }

    pub(crate) async fn rename(&self, thread: &ThreadId, title: String) -> Result<()> {
        let session = self.session()?;
        self.known_thread(thread)?;
        self.store.renamed();
        session.call_value("thread/name/set", json!({ "threadId": thread.0, "name": title })).await?;
        self.store.update_thread(thread, |e| e.title = Some(title));
        Ok(())
    }

    pub(crate) async fn archive(&self, thread: &ThreadId) -> Result<()> {
        let session = self.session()?;
        self.known_thread(thread)?;
        session.call_value("thread/archive", json!({ "threadId": thread.0 })).await?;
        self.assignments.deliveries.forget(thread);
        self.store.remove_thread(thread);
        self.tool_calls.close_thread(&thread.0, false);
        session.forget_loaded(thread);
        Ok(())
    }

    pub(crate) fn mark_read(&self, thread: &ThreadId) {
        self.store.update_thread(thread, |e| e.unread = false);
    }
}
