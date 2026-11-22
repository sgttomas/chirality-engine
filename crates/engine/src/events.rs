//! What Codex tells the engine, turned into changes of the snapshot and
//! events: notifications, and the requests that wait on the person.

use serde::de::DeserializeOwned;
use serde_json::Value;

use std::sync::Arc;

use crate::api::*;
use crate::assignment::marker;
use crate::mapping;
use crate::protocol as p;
use crate::session::Inner;
use crate::store::{Pending, now};

fn decode<T: DeserializeOwned>(params: Value) -> Option<T> {
    serde_json::from_value(params).ok()
}

impl Inner {
    pub(crate) async fn on_notification(self: &Arc<Self>, method: &str, params: Value) {
        match method {
            "thread/started" => {
                if let Some(note) = decode::<p::ThreadResponse>(params) {
                    self.tool_calls.thread_live(&note.thread.id);
                    self.adopt_thread(&note.thread, None);
                }
            }
            "thread/unarchived" => {
                if let Some(note) = decode::<p::ThreadNote>(params) {
                    self.tool_calls.thread_live(&note.thread_id);
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
                        // Codex says so before it announces the turn's end: a look at the turn for its
                        // report begins now, before anything waiting can start a turn there.
                        if let Some(entry) = self.store.thread(&thread)
                            && let Some(turn) = &entry.active_turn
                        {
                            self.assignments.look_due(&thread, turn, entry.assigned_by.is_some());
                        }
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
                self.store.renamed();
                self.store.update_thread(&ThreadId(note.thread_id), |e| e.title = note.thread_name);
            }
            "thread/archived" | "thread/deleted" | "thread/closed" => {
                let Some(note) = decode::<p::ThreadNote>(params) else { return };
                // Codex drops a closed thread's requests without saying so.
                self.tool_calls.close_thread(&note.thread_id, true);
                let thread = ThreadId(note.thread_id);
                // Codex no longer has it loaded: a later turn resumes it first.
                if let Ok(session) = self.session() {
                    session.forget_loaded(&thread);
                }
                if method == "thread/closed" {
                    self.store.drop_requests(&thread);
                } else {
                    self.assignments.deliveries.forget(&thread);
                    self.store.remove_thread(&thread);
                }
            }
            "turn/started" => {
                let Some(note) = decode::<p::TurnNote>(params) else { return };
                self.tool_calls.thread_live(&note.thread_id);
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
                // A turn another thread's message started reports back to it; an assigned thread's
                // turn is looked at whether or not the engine saw how it began. Decided before the
                // thread is idle (if Codex has not said so already), so what waits for it starts after the look.
                let report = self.assignments.look_due(&thread, &turn, self.store.thread(&thread).is_some_and(|e| e.assigned_by.is_some()));
                self.assignments.announced(&thread, &turn);
                let known = self.store.update_thread(&thread, |e| {
                    if e.active_turn.as_ref() == Some(&turn) {
                        e.active_turn = None;
                    }
                    e.last_ended = Some(turn.clone());
                    e.failed = match &state {
                        TurnState::Failed { message } => Some(message.clone()),
                        _ => None,
                    };
                    // A subagent's ending is its parent's to tell.
                    e.unread |= e.subagent.is_none();
                    e.updated_at = now();
                });
                if known.is_some() {
                    self.store.emit(Event::TurnEnded { thread: thread.clone(), turn: turn.clone(), state: state.clone() });
                    if report {
                        self.report_in_order(thread, turn, state);
                    }
                } else if report {
                    self.assignments.end_look(&thread);
                }
            }
            "item/started" | "item/completed" => {
                let Some(note) = decode::<p::ItemNote>(params) else { return };
                match &note.item {
                    p::ThreadItem::FileChange { id, changes } => self.store.remember_file_change(id, mapping::file_diffs(changes)),
                    // A message from another thread (not a report) began the turn: its end reports back.
                    p::ThreadItem::UserMessage { content, .. } if method == "item/started" => {
                        let mark = match content.first() {
                            Some(p::InputInfo::Text { text }) => marker::part(text),
                            _ => None,
                        };
                        if mark.is_some_and(|m| m.report.is_none() && m.from.0 != note.thread_id) {
                            self.assignments.opened_by_message(&ThreadId(note.thread_id.clone()), &TurnId(note.turn_id.clone()));
                        }
                    }
                    // A call that ends unanswered was cancelled by Codex.
                    p::ThreadItem::DynamicToolCall { id, .. } if method == "item/completed" => {
                        self.tool_calls.cancel(|place| place.thread == note.thread_id && place.turn == note.turn_id && &place.item == id);
                    }
                    // Codex announces no subagent's thread: its start is the first the engine hears of one.
                    // Only a start makes a parent; a message can go to any agent, the thread's own parent too.
                    p::ThreadItem::SubAgentActivity { kind, agent_thread_id, agent_path, .. } if kind == "started" => {
                        self.adopt_subagent(&note.thread_id, agent_thread_id, agent_path)
                    }
                    _ => {}
                }
                let Some(item) = mapping::item(&note.item, method == "item/completed") else { return };
                self.emit_for(note.thread_id, note.turn_id, |thread, turn| Event::Item { thread, turn, item });
            }
            "item/agentMessage/delta" | "item/reasoning/summaryTextDelta" | "item/commandExecution/outputDelta" | "item/plan/delta" => {
                let Some(note) = decode::<p::DeltaNote>(params) else { return };
                let item = ItemId(note.item_id);
                let kind = match method {
                    "item/agentMessage/delta" => TextKind::AgentMessage,
                    "item/reasoning/summaryTextDelta" => TextKind::Reasoning,
                    "item/commandExecution/outputDelta" => TextKind::Command,
                    _ => TextKind::Plan,
                };
                self.emit_for(note.thread_id, note.turn_id, |thread, turn| Event::ItemDelta { thread, turn, item, kind, text: note.delta });
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
    pub(crate) fn on_request(self: &Arc<Self>, generation: u64, id: Value, method: &str, params: Value) {
        let Ok(session) = self.session() else { return };
        if session.generation != generation {
            return;
        }
        if method == crate::tools::TOOL_CALL {
            return self.on_tool_call(session, id, params, self.config.tools.as_ref());
        }
        match mapping::request(method, &params, |item| self.store.file_change(item)) {
            Some((asked_by, request)) => {
                let thread = self.store.shown_on(asked_by.clone());
                let request = PendingRequest { id: RequestId(format!("{generation}:{}", rpc_id_text(&id))), thread, request };
                self.store.add_pending(Pending { request, generation, rpc_id: id, method: method.to_string(), params, asked_by });
            }
            None => {
                let _ = session.client.respond_error(id, -32601, "not supported by this client");
            }
        }
    }

    /// Records a thread Codex reports if it belongs to one of the projects.
    /// `read_at`: [`crate::store::Store::renames`] when the reply holding `thread` was asked for.
    pub(crate) fn adopt_thread(&self, thread: &p::Thread, read_at: Option<u64>) -> Option<ThreadId> {
        self.take_thread(thread, read_at, false, true)
    }

    /// A thread as Codex's listing gives it. The listing does not say whether a
    /// thread is a fork, so a marked first message is not taken as an
    /// assignment from it (see [`Inner::needs_confirming`]).
    pub(crate) fn adopt_listed(&self, thread: &p::Thread, read_at: u64) -> Option<ThreadId> {
        self.take_thread(thread, Some(read_at), false, false)
    }

    /// What a read says of a thread, for a thread still in the snapshot.
    pub(crate) fn refresh_thread(&self, thread: &p::Thread, read_at: u64) {
        self.take_thread(thread, Some(read_at), true, true);
    }

    /// A listed thread whose first message names an assigner, not yet taken as
    /// assigned: a read of it says whether it is a fork, which a listing does not.
    pub(crate) fn needs_confirming(&self, thread: &p::Thread) -> bool {
        let marked = marker::from_preview(&thread.preview).0.is_some_and(|m| m.report.is_none() && m.from.0 != thread.id);
        let known = self.store.thread(&ThreadId(thread.id.clone()));
        marked && thread.parent_thread_id.is_none() && known.is_some_and(|e| e.assigned_by.is_none() && e.forked_from.is_none() && e.subagent.is_none())
    }

    /// A subagent the engine has not seen, started by a thread it has, in that
    /// thread's project. Until Codex has kept it, a read of it is empty, from
    /// before it was taken in (as for a thread the engine starts).
    fn adopt_subagent(&self, parent: &str, child: &str, path: &str) {
        let (parent, child) = (ThreadId(parent.to_string()), ThreadId(child.to_string()));
        self.store.set_parent(child.clone(), parent.clone());
        let (Some(of), None) = (self.store.thread(&parent), self.store.thread(&child)) else { return };
        let before = self.store.seq();
        // Unkept before anyone hears of it: a read made on the news of it is the empty one.
        if let Ok(session) = self.session() {
            session.mark_unkept(&child, before);
        }
        let subagent = Subagent { parent, path: path.to_string(), nickname: None };
        self.store.upsert_thread(child.clone(), None, false, |existing| match existing {
            Some(existing) => existing.clone(),
            None => crate::store::ThreadEntry {
                project: of.project,
                cwd: of.cwd,
                title: None,
                forked_from: None,
                updated_at: now(),
                // The settings of the turn that started it, whether or not Codex's answer to that turn's start is in yet.
                settings: of.starting.or(of.settings),
                access_known: false,
                starting: None,
                active_turn: None,
                last_ended: None,
                unsettled: None,
                failed: None,
                unread: false,
                subagent: Some(subagent),
                assigned_by: None,
            },
        });
    }

    /// `trusted`: the reply says whether the thread is a fork, so its first message's mark can be taken.
    fn take_thread(&self, thread: &p::Thread, read_at: Option<u64>, known_only: bool, trusted: bool) -> Option<ThreadId> {
        let parent = thread.parent_thread_id.clone().map(ThreadId);
        if let Some(parent) = &parent {
            self.store.set_parent(ThreadId(thread.id.clone()), parent.clone());
        }
        let project = self.store.projects().into_iter().find(|p| mapping::same_folder(&thread.cwd, &p.path))?;
        let id = ThreadId(thread.id.clone());
        // An assignment is read from the first message's mark; a fork has that message too, and is none.
        let forked = thread.forked_from_id.is_some() || existing_forked(&self.store, &id);
        let assigned_by = match marker::from_preview(&thread.preview).0 {
            Some(mark) if trusted && parent.is_none() && !forked && mark.report.is_none() && mark.from != id => Some(mark.from),
            _ => None,
        };
        let active_turn = match &thread.status {
            p::ThreadStatus::Active => thread.turns.iter().rev().find(|t| t.status == p::TurnStatus::InProgress).map(|t| TurnId(t.id.clone())),
            _ => None,
        };
        self.store.upsert_thread(id.clone(), read_at, known_only, |existing| crate::store::ThreadEntry {
            project: project.id,
            cwd: thread.cwd.clone(),
            title: thread.name.clone().or_else(|| existing.and_then(|e| e.title.clone())).or_else(|| mapping::title_from_preview(&thread.preview)),
            // A fork made from this thread keeps the parent it was given.
            forked_from: thread.forked_from_id.clone().map(ThreadId).or_else(|| existing.and_then(|e| e.forked_from.clone())),
            updated_at: existing.map(|e| e.updated_at.max(thread.updated_at)).unwrap_or(thread.updated_at),
            access_known: existing.is_some_and(|e| e.access_known),
            settings: existing.and_then(|e| e.settings.clone()).or_else(|| {
                thread.model.clone().map(|model| TurnSettings { model, effort: thread.reasoning_effort.clone().map(Effort), access: Access::default() })
            }),
            starting: existing.and_then(|e| e.starting.clone()),
            active_turn: existing.and_then(|e| e.active_turn.clone()).or(active_turn),
            last_ended: existing.and_then(|e| e.last_ended.clone()),
            unsettled: existing.and_then(|e| e.unsettled.clone()),
            failed: existing.and_then(|e| e.failed.clone()),
            unread: existing.is_some_and(|e| e.unread),
            subagent: parent.clone().map(|parent| Subagent {
                parent,
                path: thread.agent_path().or_else(|| existing.and_then(|e| e.subagent.as_ref().map(|s| s.path.clone()))).unwrap_or_default(),
                nickname: thread.agent_nickname.clone().or_else(|| existing.and_then(|e| e.subagent.as_ref().and_then(|s| s.nickname.clone()))),
            }),
            assigned_by: assigned_by.clone().or_else(|| existing.and_then(|e| e.assigned_by.clone())).filter(|_| !forked && parent.is_none()),
        });
        Some(id)
    }
}

fn existing_forked(store: &crate::store::Store, id: &ThreadId) -> bool {
    store.thread(id).is_some_and(|e| e.forked_from.is_some())
}

fn rpc_id_text(id: &Value) -> String {
    match id {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}
