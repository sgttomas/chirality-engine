//! Turns: start one with the person's message and settings, steer the one
//! running, interrupt it; the person's answers to what Codex asks; and the
//! file search behind `@` references.

use serde_json::json;

use crate::api::*;
use crate::mapping;
use crate::protocol as p;
use crate::session::Inner;

impl Inner {
    pub(crate) async fn send(&self, thread: &ThreadId, message: Message, settings: TurnSettings) -> Result<()> {
        let session = self.session()?;
        let entry = self.known_thread(thread)?;
        let project = self.project(&entry.project)?;
        let workflow = match &message.workflow {
            Some(id) => Some(crate::workflows::pick(id, &self.config.workflow_roots, &project.path)?),
            None => None,
        };
        let input = mapping::message_input(&message, workflow.as_ref());

        if let Some(turn) = entry.active_turn {
            let params = p::TurnSteerParams { thread_id: thread.0.clone(), expected_turn_id: turn.0, input };
            session.call_value("turn/steer", params).await?;
            return Ok(());
        }

        self.ensure_loaded(&session, thread).await?;
        let (approval_policy, sandbox_policy) = mapping::access_policy(settings.access);
        let params = p::TurnStartParams {
            thread_id: thread.0.clone(),
            input,
            model: settings.model.clone(),
            effort: settings.effort.clone().map(|e| e.0),
            approval_policy,
            approvals_reviewer: "user",
            sandbox_policy,
        };
        let started: p::TurnResponse = session.call("turn/start", params).await?;
        let turn = TurnId(started.turn.id);
        self.store.update_thread(thread, |e| {
            e.settings = Some(settings);
            e.failed = None;
            if e.active_turn.is_none() && e.last_ended.as_ref() != Some(&turn) {
                e.active_turn = Some(turn);
            }
        });
        Ok(())
    }

    pub(crate) async fn interrupt(&self, thread: &ThreadId) -> Result<()> {
        let session = self.session()?;
        let Some(turn) = self.known_thread(thread)?.active_turn else { return Ok(()) };
        session.call_value("turn/interrupt", json!({ "threadId": thread.0, "turnId": turn.0 })).await?;
        Ok(())
    }

    pub(crate) async fn answer(&self, request: &RequestId, answer: Answer) -> Result<()> {
        let pending = self.store.pending(request).ok_or_else(|| EngineError::NotFound { what: format!("request {}", request.0) })?;
        let session = self.session()?;
        if session.generation != pending.generation {
            return Err(EngineError::NotFound { what: format!("request {}", request.0) });
        }
        let (reply, stop) = mapping::reply(&pending.method, &pending.params, &answer)?;
        let gone = || EngineError::NotFound { what: format!("request {}", request.0) };
        let pending = self.store.take_pending(request).ok_or_else(gone)?;
        session.client.respond(pending.rpc_id.clone(), reply).map_err(crate::session::engine_error)?;
        if stop {
            self.interrupt(&pending.request.thread).await?;
        }
        Ok(())
    }

    pub(crate) async fn find_files(&self, project: &ProjectId, query: String) -> Result<Vec<std::path::PathBuf>> {
        let project = self.project(project)?;
        let session = self.session()?;
        let params = p::FuzzyParams { query, roots: vec![project.path.display().to_string()], cancellation_token: None };
        let found: p::FuzzyResponse = session.call("fuzzyFileSearch", params).await?;
        Ok(found.files.into_iter().map(|f| std::path::Path::new(&f.root).join(f.path)).collect())
    }
}
