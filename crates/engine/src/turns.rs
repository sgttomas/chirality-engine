//! Turns: start one with the person's message and settings, steer the one
//! running, interrupt it; the person's answers to what Codex asks; and the
//! file search behind `@` references.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::json;

use crate::api::*;
use crate::mapping;
use crate::protocol as p;
use crate::session::Inner;

/// How long a Stop holds the thread for Codex to answer it and end the turn
/// it stopped (Codex answers `turn/interrupt` before it ends the turn, or,
/// in some states, only once it has).
#[cfg(not(test))]
const STOP_SETTLE: Duration = Duration::from_secs(10);
#[cfg(test)]
const STOP_SETTLE: Duration = Duration::from_millis(1200);

/// One admission at a time per thread: starting a turn, steering it and
/// interrupting it each wait for the one before, so two sends cannot both see
/// an idle thread, and a Stop sent while a turn is starting stops that turn.
/// Each runs as the engine's own task, so a caller that gives up does not end
/// it halfway: the thread stays admitted until Codex has answered.
#[derive(Default)]
pub(crate) struct Admission(Mutex<HashMap<ThreadId, Arc<tokio::sync::Mutex<()>>>>);

impl Admission {
    async fn admit(&self, thread: &ThreadId) -> tokio::sync::OwnedMutexGuard<()> {
        let lock = self.0.lock().unwrap().entry(thread.clone()).or_default().clone();
        lock.lock_owned().await
    }
}

/// Runs `op` as the engine's own task; the caller only waits for it.
async fn owned<F>(op: F) -> Result<()>
where
    F: Future<Output = Result<()>> + Send + 'static,
{
    tokio::spawn(op).await.unwrap_or_else(|e| Err(EngineError::Invalid { message: format!("the engine's task ended: {e}") }))
}

impl Inner {
    /// Sends a message: starts a turn, or steers the one running.
    pub(crate) async fn send(self: &Arc<Self>, thread: &ThreadId, message: Message, settings: TurnSettings) -> Result<()> {
        let (inner, thread) = (self.clone(), thread.clone());
        owned(async move {
            let _admitted = inner.admission.admit(&thread).await;
            inner.send_admitted(&thread, message, settings).await
        })
        .await
    }

    /// Stops the running turn, and holds the thread until Codex has ended it;
    /// a turn still starting is stopped once Codex has started it.
    pub(crate) async fn interrupt(self: &Arc<Self>, thread: &ThreadId) -> Result<()> {
        let (inner, thread) = (self.clone(), thread.clone());
        owned(async move {
            let _admitted = inner.admission.admit(&thread).await;
            inner.interrupt_admitted(&thread).await
        })
        .await
    }

    async fn send_admitted(&self, thread: &ThreadId, message: Message, settings: TurnSettings) -> Result<()> {
        let session = self.session()?;
        let entry = self.known_thread(thread)?;
        if entry.subagent.is_some() {
            return Err(EngineError::Invalid { message: "A subagent takes its messages from the agent that started it.".into() });
        }
        let project = self.project(&entry.project)?;
        let workflow = match &message.workflow {
            Some(id) => Some(crate::workflows::pick(id, &self.config.workflow_roots, &project.path)?),
            None => None,
        };
        let input = mapping::message_input(&message, workflow.as_ref());

        // A turn a Stop could not settle may have ended unannounced: the send is
        // a start, which Codex takes as a steer if that turn still runs.
        if let Some(turn) = entry.active_turn.filter(|t| entry.unsettled.as_ref() != Some(t)) {
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
        // The summary shows the turn's settings from before Codex hears of it, so
        // every summary the turn causes has them; they are kept only once Codex
        // has taken the turn.
        self.store.update_thread(thread, |e| e.starting = Some(settings));
        let started: p::TurnResponse = match session.call("turn/start", params).await {
            Ok(started) => started,
            Err(e) => {
                self.store.update_thread(thread, |entry| entry.starting = None);
                return Err(e);
            }
        };
        let turn = TurnId(started.turn.id);
        self.store.update_thread(thread, |e| {
            if let Some(settings) = e.starting.take() {
                e.settings = Some(settings);
            }
            e.failed = None;
            if e.active_turn.is_none() && e.last_ended.as_ref() != Some(&turn) {
                e.active_turn = Some(turn);
            }
        });
        Ok(())
    }

    async fn interrupt_admitted(&self, thread: &ThreadId) -> Result<()> {
        let session = self.session()?;
        let Some(turn) = self.known_thread(thread)?.active_turn else { return Ok(()) };
        let mut events = self.store.subscribe();
        let running = || self.store.thread(thread).is_some_and(|e| e.active_turn.as_ref() == Some(&turn));
        let stopped = tokio::time::timeout(STOP_SETTLE, async {
            session.call_value("turn/interrupt", json!({ "threadId": thread.0, "turnId": turn.0 })).await?;
            while running() {
                if let Err(tokio::sync::broadcast::error::RecvError::Closed) = events.recv().await {
                    break;
                }
            }
            Ok(())
        })
        .await;
        stopped.unwrap_or_else(|_| {
            // Codex has not ended the turn, which may still run: it shows as running and can be
            // stopped again, and the thread is let go for the next send.
            self.store.update_thread(thread, |e| {
                if e.active_turn.as_ref() == Some(&turn) {
                    e.unsettled = Some(turn.clone());
                }
            });
            Err(EngineError::Codex { message: "Codex did not stop the turn".into() })
        })
    }

    pub(crate) async fn answer(self: &Arc<Self>, request: &RequestId, answer: Answer) -> Result<()> {
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
