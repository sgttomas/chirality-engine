//! Threads, which Codex keeps: listed per project folder, read, started,
//! resumed, forked, reverted, renamed and archived.

use std::sync::Arc;

use serde_json::json;

use crate::api::*;
use crate::protocol as p;
use crate::rpc::RpcError;
use crate::session::{Inner, Session, engine_error};

const PAGE: u32 = 100;
const METHOD_NOT_FOUND: i64 = -32601;

async fn list_turns(session: &Session, thread: &ThreadId) -> std::result::Result<Vec<Turn>, RpcError> {
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

impl Inner {
    /// Reads a project's threads from Codex into the snapshot.
    pub(crate) async fn list_threads(&self, project: &Project) -> Result<()> {
        let session = self.session()?;
        let mut cursor = None;
        loop {
            let params = p::ThreadListParams { cwd: vec![project.path.display().to_string()], limit: PAGE, cursor };
            let page: p::ThreadListResponse = session.call("thread/list", params).await?;
            for thread in &page.data {
                self.adopt_thread(thread);
            }
            match page.next_cursor {
                Some(next) if !page.data.is_empty() => cursor = Some(next),
                _ => return Ok(()),
            }
        }
    }

    pub(crate) fn project(&self, id: &ProjectId) -> Result<Project> {
        self.store.projects().into_iter().find(|p| &p.id == id).ok_or_else(|| EngineError::NotFound { what: format!("project {}", id.0) })
    }

    pub(crate) fn known_thread(&self, id: &ThreadId) -> Result<crate::store::ThreadEntry> {
        self.store.thread(id).ok_or_else(|| EngineError::NotFound { what: format!("thread {}", id.0) })
    }

    /// Loads a thread into this Codex process if it is not yet, so turns can run on it.
    pub(crate) async fn ensure_loaded(&self, session: &Session, thread: &ThreadId) -> Result<()> {
        if session.is_loaded(thread) {
            return Ok(());
        }
        let entry = self.known_thread(thread)?;
        let _: p::ThreadResponse = session.call("thread/resume", json!({ "threadId": thread.0, "cwd": entry.cwd, "excludeTurns": true })).await?;
        self.tool_calls.reopen_thread(&thread.0);
        session.mark_loaded(thread);
        Ok(())
    }

    pub(crate) async fn read_thread(&self, thread: &ThreadId) -> Result<ThreadView> {
        let session = self.session()?;
        self.known_thread(thread)?;
        let read: p::ThreadResponse = session.call("thread/read", json!({ "threadId": thread.0 })).await?;
        self.adopt_thread(&read.thread);
        let turns = match list_turns(&session, thread).await {
            Err(RpcError::Server { code: METHOD_NOT_FOUND, .. }) => {
                // Threads kept in Codex's older history format cannot be paged; read them whole.
                let whole: p::ThreadResponse = session.call("thread/read", json!({ "threadId": thread.0, "includeTurns": true })).await?;
                whole.thread.turns.iter().map(crate::mapping::turn).collect()
            }
            other => other.map_err(engine_error)?,
        };
        let summary = self.store.summary(thread).ok_or_else(|| EngineError::NotFound { what: format!("thread {}", thread.0) })?;
        Ok(ThreadView { summary, turns })
    }

    pub(crate) async fn start_thread(self: &Arc<Self>, project: &ProjectId, message: Message, settings: TurnSettings) -> Result<ThreadId> {
        let project = self.project(project)?;
        let session = self.session()?;
        let params = p::ThreadStartParams { cwd: project.path.display().to_string(), model: settings.model.clone(), dynamic_tools: self.config.tools.as_ref().map(crate::tools::specs) };
        let started: p::ThreadResponse = session.call("thread/start", params).await?;
        let id = self.adopt_thread(&started.thread).ok_or_else(|| EngineError::Codex { message: "Codex started the thread in another folder".into() })?;
        session.mark_loaded(&id);
        self.send(&id, message, settings).await?;
        Ok(id)
    }

    pub(crate) async fn fork(&self, thread: &ThreadId, through: &TurnId) -> Result<ThreadId> {
        let session = self.session()?;
        self.ensure_loaded(&session, thread).await?;
        let params = json!({ "threadId": thread.0, "lastTurnId": through.0, "excludeTurns": true });
        let forked: p::ThreadResponse = session.call("thread/fork", params).await?;
        let id = self.adopt_thread(&forked.thread).ok_or_else(|| EngineError::Codex { message: "Codex forked the thread into another folder".into() })?;
        let parent = self.store.thread(thread);
        self.store.update_thread(&id, |e| {
            e.forked_from = Some(thread.clone());
            if let Some(parent) = &parent {
                e.settings = parent.settings.clone();
                if let Some(title) = &parent.title {
                    e.title.get_or_insert_with(|| title.clone());
                }
            }
        });
        session.mark_loaded(&id);
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
        session.call_value("thread/name/set", json!({ "threadId": thread.0, "name": title })).await?;
        self.store.update_thread(thread, |e| e.title = Some(title));
        Ok(())
    }

    pub(crate) async fn archive(&self, thread: &ThreadId) -> Result<()> {
        let session = self.session()?;
        self.known_thread(thread)?;
        session.call_value("thread/archive", json!({ "threadId": thread.0 })).await?;
        self.store.remove_thread(thread);
        self.tool_calls.close_thread(&thread.0, false);
        session.forget_loaded(thread);
        Ok(())
    }

    pub(crate) fn mark_read(&self, thread: &ThreadId) {
        self.store.update_thread(thread, |e| e.unread = false);
    }
}
