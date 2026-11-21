//! What Codex tells the engine, turned into changes of the snapshot and
//! events: notifications, and the requests that wait on the person.

use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::api::*;
use crate::mapping;
use crate::protocol as p;
use crate::session::Inner;
use crate::store::{Pending, now};

fn decode<T: DeserializeOwned>(params: Value) -> Option<T> {
    serde_json::from_value(params).ok()
}

impl Inner {
    pub(crate) async fn on_notification(&self, method: &str, params: Value) {
        match method {
            "thread/started" => {
                if let Some(note) = decode::<p::ThreadResponse>(params) {
                    self.adopt_thread(&note.thread);
                }
            }
            "thread/status/changed" => {
                let Some(note) = decode::<p::StatusNote>(params) else { return };
                let thread = ThreadId(note.thread_id);
                match note.status {
                    p::ThreadStatus::SystemError => {
                        self.store.update_thread(&thread, |e| e.failed = Some("Codex reported an error".into()));
                    }
                    // Idle means no turn runs, even if a note was missed; the
                    // turn's own end (which follows) still says how it ended.
                    p::ThreadStatus::Idle => {
                        self.store.update_thread(&thread, |e| {
                            if let Some(turn) = e.active_turn.take() {
                                e.last_ended = Some(turn);
                            }
                        });
                    }
                    _ => {}
                }
            }
            "thread/name/updated" => {
                let Some(note) = decode::<p::NameNote>(params) else { return };
                self.store.update_thread(&ThreadId(note.thread_id), |e| e.title = note.thread_name);
            }
            "thread/archived" | "thread/deleted" | "thread/closed" => {
                let Some(note) = decode::<p::ThreadNote>(params) else { return };
                // Codex drops a closed thread's requests without saying so.
                self.tool_calls.close_thread(&note.thread_id);
                let thread = ThreadId(note.thread_id);
                if method == "thread/closed" {
                    if let Ok(session) = self.session() {
                        session.forget_loaded(&thread);
                    }
                } else {
                    self.store.remove_thread(&thread);
                }
            }
            "turn/started" => {
                let Some(note) = decode::<p::TurnNote>(params) else { return };
                let (thread, turn) = (ThreadId(note.thread_id), TurnId(note.turn.id));
                let known = self.store.update_thread(&thread, |e| {
                    e.active_turn = Some(turn.clone());
                    e.failed = None;
                    e.updated_at = now();
                });
                if known.is_some() {
                    self.store.emit(Event::TurnStarted { thread, turn });
                }
            }
            "turn/completed" => {
                let Some(note) = decode::<p::TurnNote>(params) else { return };
                self.tool_calls.cancel(|place| place.thread == note.thread_id && place.turn == note.turn.id);
                let state = mapping::turn_state(&note.turn);
                let (thread, turn) = (ThreadId(note.thread_id), TurnId(note.turn.id));
                let known = self.store.update_thread(&thread, |e| {
                    if e.active_turn.as_ref() == Some(&turn) {
                        e.active_turn = None;
                    }
                    e.last_ended = Some(turn.clone());
                    e.failed = match &state {
                        TurnState::Failed { message } => Some(message.clone()),
                        _ => None,
                    };
                    e.unread = true;
                    e.updated_at = now();
                });
                if known.is_some() {
                    self.store.emit(Event::TurnEnded { thread, turn, state });
                }
            }
            "item/started" | "item/completed" => {
                let Some(note) = decode::<p::ItemNote>(params) else { return };
                match &note.item {
                    p::ThreadItem::FileChange { id, changes } => self.store.remember_file_change(id, mapping::file_diffs(changes)),
                    // A call that ends unanswered was cancelled by Codex.
                    p::ThreadItem::DynamicToolCall { id, .. } if method == "item/completed" => {
                        self.tool_calls.cancel(|place| place.thread == note.thread_id && place.turn == note.turn_id && &place.item == id);
                    }
                    _ => {}
                }
                let Some(item) = mapping::item(&note.item, method == "item/completed") else { return };
                self.emit_for(note.thread_id, note.turn_id, |thread, turn| Event::Item { thread, turn, item });
            }
            "item/agentMessage/delta" | "item/reasoning/summaryTextDelta" | "item/commandExecution/outputDelta" | "item/plan/delta" => {
                let Some(note) = decode::<p::DeltaNote>(params) else { return };
                let item = ItemId(note.item_id);
                self.emit_for(note.thread_id, note.turn_id, |thread, turn| Event::ItemDelta { thread, turn, item, text: note.delta });
            }
            "turn/plan/updated" => {
                let Some(note) = decode::<p::PlanNote>(params) else { return };
                let item = mapping::plan(&note.turn_id, &note.plan);
                self.emit_for(note.thread_id, note.turn_id, |thread, turn| Event::Item { thread, turn, item });
            }
            "error" => {
                let Some(note) = decode::<p::ErrorNote>(params) else { return };
                if note.will_retry {
                    return;
                }
                let item = Item { id: ItemId(format!("error-{}", note.turn_id)), body: ItemBody::Error { message: note.error.message }, done: true };
                self.emit_for(note.thread_id, note.turn_id, |thread, turn| Event::Item { thread, turn, item });
            }
            "serverRequest/resolved" => {
                let Some(note) = decode::<p::ResolvedNote>(params) else { return };
                let Ok(session) = self.session() else { return };
                self.store.resolve(|p| p.generation == session.generation && p.rpc_id == note.request_id);
            }
            "account/updated" => self.read_account().await,
            "account/login/completed" => {
                if let Some(note) = decode::<p::LoginCompleted>(params) {
                    let _ = note.login_id;
                    if !note.success {
                        self.store.set_account(Account::SignedOut);
                    } else {
                        self.read_account().await;
                    }
                }
            }
            _ => {}
        }
    }

    fn emit_for(&self, thread: String, turn: String, event: impl FnOnce(ThreadId, TurnId) -> Event) {
        let thread = ThreadId(thread);
        if self.store.thread(&thread).is_some() {
            self.store.emit(event(thread, TurnId(turn)));
        }
    }

    /// Codex waits on the answer to every request it sends; each is shown to
    /// the person, given to the application's tool handler, or answered at once
    /// as one this engine does not take.
    pub(crate) fn on_request(&self, generation: u64, id: Value, method: &str, params: Value) {
        let Ok(session) = self.session() else { return };
        if session.generation != generation {
            return;
        }
        if let (crate::tools::TOOL_CALL, Some(tools)) = (method, &self.config.tools) {
            return self.on_tool_call(session, id, params, tools);
        }
        match mapping::request(method, &params, |item| self.store.file_change(item)) {
            Some((thread, request)) => {
                let thread = self.store.owner(thread);
                let request = PendingRequest { id: RequestId(format!("{generation}:{}", rpc_id_text(&id))), thread, request };
                self.store.add_pending(Pending { request, generation, rpc_id: id, method: method.to_string(), params });
            }
            None => {
                let _ = session.client.respond_error(id, -32601, "not supported by this client");
            }
        }
    }

    /// Records a thread Codex reports if it belongs to one of the projects.
    pub(crate) fn adopt_thread(&self, thread: &p::Thread) -> Option<ThreadId> {
        if let Some(parent) = &thread.parent_thread_id {
            self.store.set_parent(ThreadId(thread.id.clone()), ThreadId(parent.clone()));
            return None;
        }
        let project = self.store.projects().into_iter().find(|p| mapping::same_folder(&thread.cwd, &p.path))?;
        let id = ThreadId(thread.id.clone());
        let existing = self.store.thread(&id);
        let title = thread
            .name
            .clone()
            .or_else(|| existing.as_ref().and_then(|e| e.title.clone()))
            .or_else(|| mapping::title_from_preview(&thread.preview));
        let settings = existing.as_ref().and_then(|e| e.settings.clone()).or_else(|| {
            thread.model.clone().map(|model| TurnSettings { model, effort: thread.reasoning_effort.clone().map(Effort), access: Access::default() })
        });
        let active_turn = match &thread.status {
            p::ThreadStatus::Active => thread.turns.iter().rev().find(|t| t.status == p::TurnStatus::InProgress).map(|t| TurnId(t.id.clone())),
            _ => None,
        };
        let entry = crate::store::ThreadEntry {
            project: project.id,
            cwd: thread.cwd.clone(),
            title,
            forked_from: thread.forked_from_id.clone().map(ThreadId),
            updated_at: existing.as_ref().map(|e| e.updated_at.max(thread.updated_at)).unwrap_or(thread.updated_at),
            settings,
            active_turn: existing.as_ref().and_then(|e| e.active_turn.clone()).or(active_turn),
            last_ended: existing.as_ref().and_then(|e| e.last_ended.clone()),
            failed: existing.as_ref().and_then(|e| e.failed.clone()),
            unread: existing.as_ref().is_some_and(|e| e.unread),
        };
        self.store.put_thread(id.clone(), entry);
        Some(id)
    }
}

fn rpc_id_text(id: &Value) -> String {
    match id {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}
