//! Threads, which Codex keeps: listed per project folder, read, started,
//! resumed, forked, reverted, renamed and archived.

use std::sync::Arc;

use serde_json::json;

use crate::api::*;
use crate::protocol as p;
use crate::session::{Inner, Session};

const PAGE: u32 = 100;

impl Inner {
    /// Reads a project's threads from Codex into the snapshot.
    pub(crate) async fn list_threads(&self, project: &Project) -> Result<()> {
        let session = self.session()?;
        let mut cursor = None;
        loop {
            let params = p::ThreadListParams { cwd: vec![project.path.display().to_string()], limit: PAGE, cursor };
            let read_at = self.store.renames();
            let page: p::ThreadListResponse = session.call("thread/list", params).await?;
            for thread in &page.data {
                self.adopt_thread(thread, Some(read_at));
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
        let read_at = self.store.renames();
        let read: p::ThreadResponse = session.call("thread/read", json!({ "threadId": thread.0 })).await?;
        self.adopt_thread(&read.thread, Some(read_at));
        // Every event after `seq` is applied to the view; one before it, the history holds.
        let (turns, seq) = crate::history::read(&session, thread, read.thread.history_mode, || self.store.seq()).await?;
        let summary = self.store.summary(thread).ok_or_else(|| EngineError::NotFound { what: format!("thread {}", thread.0) })?;
        Ok(ThreadView { summary, turns, seq })
    }

    pub(crate) async fn start_thread(self: &Arc<Self>, project: &ProjectId, message: Message, settings: TurnSettings) -> Result<ThreadId> {
        let project = self.project(project)?;
        let session = self.session()?;
        let params = p::ThreadStartParams { cwd: project.path.display().to_string(), model: settings.model.clone(), dynamic_tools: self.config.tools.as_ref().map(crate::tools::specs) };
        let started: p::ThreadResponse = session.call("thread/start", params).await?;
        let id = self.adopt_thread(&started.thread, None).ok_or_else(|| EngineError::Codex { message: "Codex started the thread in another folder".into() })?;
        session.mark_loaded(&id);
        self.send(&id, message, settings).await?;
        Ok(id)
    }

    pub(crate) async fn fork(&self, thread: &ThreadId, through: &TurnId) -> Result<ThreadId> {
        let session = self.session()?;
        self.ensure_loaded(&session, thread).await?;
        let params = json!({ "threadId": thread.0, "lastTurnId": through.0, "excludeTurns": true });
        let forked: p::ThreadResponse = session.call("thread/fork", params).await?;
        let id = self.adopt_thread(&forked.thread, None).ok_or_else(|| EngineError::Codex { message: "Codex forked the thread into another folder".into() })?;
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
        self.known_thread(thread)?;
        let read: p::ThreadResponse = session.call("thread/read", json!({ "threadId": thread.0 })).await?;
        if read.thread.history_mode == p::HistoryMode::Legacy {
            return Err(EngineError::Unsupported { message: "Codex can't remove turns from a thread kept in its older format.".into() });
        }
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
        self.store.remove_thread(thread);
        self.tool_calls.close_thread(&thread.0, false);
        session.forget_loaded(thread);
        Ok(())
    }

    pub(crate) fn mark_read(&self, thread: &ThreadId) {
        self.store.update_thread(thread, |e| e.unread = false);
    }
}
