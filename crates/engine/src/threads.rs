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
        let _: p::ThreadResponse = self.thread_call(session, thread, "thread/resume", json!({ "threadId": thread.0, "cwd": entry.cwd, "excludeTurns": true })).await?;
        self.tool_calls.reopen_thread(&thread.0);
        session.mark_loaded(thread);
        Ok(())
    }

    pub(crate) async fn start_thread(self: &Arc<Self>, project: &ProjectId, message: Message, settings: TurnSettings) -> Result<ThreadId> {
        let project = self.project(project)?;
        let session = self.session()?;
        // A first message that cannot be made makes no thread.
        if let Some(id) = &message.workflow {
            crate::workflows::pick(id, &self.config.workflow_roots, &project.path)?;
        }
        let params = p::ThreadStartParams { cwd: project.path.display().to_string(), model: settings.model.clone(), dynamic_tools: self.config.tools.as_ref().map(crate::tools::specs) };
        let started: p::ThreadResponse = session.call("thread/start", params).await?;
        let id = self.adopt_thread(&started.thread, None).ok_or_else(|| EngineError::Codex { message: "Codex started the thread in another folder".into() })?;
        session.mark_loaded(&id);
        session.mark_fresh(&id);
        if let Err(e) = self.send(&id, message, settings).await {
            // No turn started, so Codex has not kept the thread: it is not listed.
            self.forget_thread(&session, &id);
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
        let forked: p::ThreadResponse = self.thread_call(&session, thread, "thread/fork", params).await?;
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
        self.ensure_loaded(&session, thread).await?;
        let _: p::ThreadResponse = self.thread_call(&session, thread, "thread/revert", json!({ "threadId": thread.0, "beforeTurnId": from.0 })).await?;
        self.store.update_thread(thread, |e| e.updated_at = crate::store::now());
        Ok(())
    }

    pub(crate) async fn rename(&self, thread: &ThreadId, title: String) -> Result<()> {
        let session = self.session()?;
        self.known_thread(thread)?;
        self.store.renamed();
        let _: serde_json::Value = self.thread_call(&session, thread, "thread/name/set", json!({ "threadId": thread.0, "name": title })).await?;
        self.store.update_thread(thread, |e| e.title = Some(title));
        Ok(())
    }

    pub(crate) async fn archive(&self, thread: &ThreadId) -> Result<()> {
        let session = self.session()?;
        self.known_thread(thread)?;
        match self.thread_call::<serde_json::Value>(&session, thread, "thread/archive", json!({ "threadId": thread.0 })).await {
            // A thread Codex does not have is gone, as asked.
            Ok(_) | Err(EngineError::NotFound { .. }) => {}
            Err(e) => return Err(e),
        }
        self.forget_thread(&session, thread);
        Ok(())
    }

    pub(crate) fn mark_read(&self, thread: &ThreadId) {
        self.store.update_thread(thread, |e| e.unread = false);
    }
}
