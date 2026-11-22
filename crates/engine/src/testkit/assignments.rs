//! Assignment between threads, recorded: an assignment and its report (and a
//! close, after which a turn the assigner started reports nothing); a message
//! to a running thread waiting for its turn to end, whose reply comes back
//! though the sender did not assign it; two threads messaging each other, each
//! hearing the other's reply; a message and an assignment across projects; and
//! a person's turn in an assigned thread reporting nothing.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Value, json};

use super::harness::{Ctx, settings, text};
use super::stub::{Reply, Script, message, says, shell_call, tool_call};
use crate::api::*;

/// A call to one of the engine's own tools.
pub(super) fn engine_call(id: &str, name: &str, args: Value) -> Value {
    tool_call(id, "chirality", name, args)
}

/// The calls `calls` gives, in order, each once the one before has its output, then `done`.
pub(super) fn steps(key: &'static str, calls: impl Fn() -> Vec<Value> + Send + Sync + 'static, done: &'static str) -> Script {
    Arc::new(move |request| match calls().into_iter().find(|c| !request.outputs.contains_key(c["call_id"].as_str().unwrap_or_default())) {
        Some(call) => Reply::Items(vec![call]),
        None => Reply::Items(vec![message(&format!("msg_{key}_done"), done)]),
    })
}

/// `script`, after a pause, so what the scenario waits on comes in one order.
pub(super) fn after(pause: Duration, script: Script) -> Script {
    Arc::new(move |request| {
        std::thread::sleep(pause);
        script(request)
    })
}

/// A thread id the scenario learns once the thread exists, for a script made before.
#[derive(Clone, Default)]
pub(super) struct Later(Arc<Mutex<String>>);

impl Later {
    pub(super) fn set(&self, thread: &ThreadId) {
        *self.0.lock().unwrap() = thread.0.clone();
    }

    pub(super) fn get(&self) -> String {
        self.0.lock().unwrap().clone()
    }
}

pub(super) fn summary(ctx: &Ctx, thread: &ThreadId) -> ThreadSummary {
    ctx.engine.snapshot().threads.into_iter().find(|t| &t.id == thread).expect("the thread is in the snapshot")
}

pub(super) fn bodies(turn: &Turn) -> Vec<ItemBody> {
    turn.items.iter().map(|i| i.body.clone()).collect()
}

/// The sender of the turn's first message.
pub(super) fn from(turn: &Turn) -> Option<MessageFrom> {
    match &turn.items[0].body {
        ItemBody::UserMessage { from, .. } => from.clone(),
        other => panic!("not a message: {other:?}"),
    }
}

pub(super) fn first_text(turn: &Turn) -> String {
    match &turn.items[0].body {
        ItemBody::UserMessage { text, .. } => text.clone(),
        other => panic!("not a message: {other:?}"),
    }
}

/// Waits until `done` holds, the engine's work for it settled.
pub(super) async fn until(ctx: &Ctx, what: &str, done: impl Fn(&Ctx) -> bool) {
    for _ in 0..3000 {
        if done(ctx) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("timed out waiting for {what}");
}

pub(super) fn reports(ctx: &Ctx) -> usize {
    ctx.engine.inner().assignments.reports.load(std::sync::atomic::Ordering::SeqCst)
}

pub(super) fn idle(ctx: &Ctx, threads: &[&ThreadId]) -> bool {
    threads.iter().all(|t| summary(ctx, t).status == ThreadStatus::Idle)
}

/// The turns that have ended on each of `threads`, counted from now until each has `n`.
pub(super) async fn turns_end(ctx: &mut Ctx, threads: &[(&ThreadId, usize)]) {
    let mut seen = vec![0; threads.len()];
    while seen.iter().zip(threads).any(|(s, (_, n))| s < n) {
        let ended = ctx.wait("turns to end", |e| match e {
            Event::TurnEnded { thread, .. } => Some(thread.clone()),
            _ => None,
        });
        let ended = ended.await;
        if let Some(i) = threads.iter().position(|(t, _)| **t == ended) {
            seen[i] += 1;
        }
    }
}

/// The lead assigns a worker; the worker's turn ends and its last reply comes back to the lead,
/// whose agent closes the assignment and sends one more message, whose turn reports nothing.
pub async fn assignment(ctx: &mut Ctx) {
    let folder = ctx.run.project().join(".chirality/workflows/survey");
    std::fs::create_dir_all(&folder).unwrap();
    std::fs::write(folder.join("WORKFLOW.md"), "---\nname: survey\ndescription: Survey the site.\n---\n# Survey\nMeasure, then record.\n").unwrap();
    let project = ctx.engine.add_project(ctx.run.project()).await.unwrap();
    let survey = ctx.engine.workflows(&project.id).await.unwrap().into_iter().find(|w| w.name == "survey").expect("the project's workflow");
    let worker = Later::default();
    let assign = json!({"title": "Piles", "brief": "STUB[worker] Check the pile lengths.", "effort": "medium", "access": "fullAccess", "workflow": "survey"});
    ctx.script("lead", steps("lead", move || vec![engine_call("call_threads", "threads", json!({})), engine_call("call_assign", "assign", assign.clone())], "Assigned."));
    ctx.script("worker", after(Duration::from_millis(1500), says("worker", "The piles are within tolerance. STUB[heard]")));
    let w = worker.clone();
    let heard = move || {
        vec![
            engine_call("call_close", "close", json!({"thread": w.get()})),
            engine_call("call_more", "message", json!({"thread": w.get(), "text": "STUB[more] One more look at pile 4."})),
        ]
    };
    ctx.script("heard", steps("heard", heard, "Closed."));
    ctx.script("more", says("more", "Pile 4 is fine."));
    let lead = ctx.engine.start_thread(&project.id, text("STUB[lead] Assign the piles."), settings(Access::Ask)).await.unwrap();
    let assigned = ctx.wait("the assigned thread", |e| match e {
        Event::Thread { thread } if thread.assigned_by.as_ref() == Some(&lead) => Some(thread.id.clone()),
        _ => None,
    });
    let assigned = assigned.await;
    worker.set(&assigned);
    // The lead's two turns (its own, and the report's), the worker's two (the brief, and the message).
    turns_end(ctx, &[(&lead, 2), (&assigned, 2)]).await;
    until(ctx, "both turns looked at", |ctx| reports(ctx) == 2 && idle(ctx, &[&lead, &assigned])).await;

    // An ordinary thread at the top, with the title, model and effort it was given, and the assigner's access.
    let s = summary(ctx, &assigned);
    assert_eq!((s.assigned_by.as_ref(), s.title.as_deref(), s.subagent.is_none(), &s.project), (Some(&lead), Some("Piles"), true, &project.id));
    assert_eq!(s.settings, Some(TurnSettings { effort: Some(Effort("medium".into())), ..settings(Access::Ask) }));
    assert_eq!(summary(ctx, &lead).assigned_by, None);

    let lead_view = ctx.read_thread(&lead).await;
    assert_eq!(lead_view.turns.len(), 2, "one report, and none after the close");
    let assign_item = ItemBody::ThreadTool { tool: ThreadTool::Assign, thread: Some(assigned.clone()), text: Some("STUB[worker] Check the pile lengths.".into()), status: ToolCallStatus::Completed };
    assert!(bodies(&lead_view.turns[0]).contains(&assign_item), "{:?}", bodies(&lead_view.turns[0]));
    let report = &lead_view.turns[1];
    assert_eq!(from(report), Some(MessageFrom { thread: assigned.clone(), title: Some("Piles".into()), report: Some(TurnEnd::Completed) }));
    assert_eq!(first_text(report), "The piles are within tolerance. STUB[heard]");
    let close = ItemBody::ThreadTool { tool: ThreadTool::Close, thread: Some(assigned.clone()), text: None, status: ToolCallStatus::Completed };
    let more = ItemBody::ThreadTool { tool: ThreadTool::Message, thread: Some(assigned.clone()), text: Some("STUB[more] One more look at pile 4.".into()), status: ToolCallStatus::Completed };
    assert!(bodies(report).contains(&close) && bodies(report).contains(&more), "{:?}", bodies(report));

    let worker_view = ctx.read_thread(&assigned).await;
    assert_eq!(worker_view.turns.len(), 2);
    assert!(matches!(&worker_view.turns[0].items[0].body, ItemBody::UserMessage { workflow: Some(w), .. } if *w == survey.id), "{:?}", worker_view.turns[0].items[0].body);
    let brief = from(&worker_view.turns[0]).expect("the brief is from the lead");
    assert_eq!((brief.thread, brief.report), (lead.clone(), None));
    assert_eq!(first_text(&worker_view.turns[0]), "STUB[worker] Check the pile lengths.");
    assert_eq!(from(&worker_view.turns[1]).map(|f| f.thread), Some(lead.clone()));

    // Listed again (as after a restart), the assignment is read from the brief.
    ctx.engine.remove_project(&project.id).await.unwrap();
    ctx.engine.add_project(ctx.run.project()).await.unwrap();
    assert_eq!(summary(ctx, &assigned).assigned_by, Some(lead.clone()));
    assert_eq!(summary(ctx, &lead).assigned_by, None);
}

/// A message to a thread whose turn waits on the person waits for that turn to end, then starts its own,
/// whose reply comes back to the sender, which did not assign it.
pub async fn assignment_waits(ctx: &mut Ctx) {
    let project = ctx.engine.add_project(ctx.run.project()).await.unwrap();
    ctx.script("busy", super::stub::calls("busy", shell_call("call_busy", "printf ok > busy.txt", true), false));
    let busy = ctx.engine.start_thread(&project.id, text("STUB[busy] Write the file."), settings(Access::Ask)).await.unwrap();
    let pending = ctx.pending(&busy).await;
    let target = Later::default();
    target.set(&busy);
    let t = target.clone();
    ctx.script("nudge", steps("nudge", move || vec![engine_call("call_nudge", "message", json!({"thread": t.get(), "text": "STUB[nudged] Also count the piles."}))], "Sent."));
    ctx.script("nudged", says("nudged", "Counted. STUB[counted]"));
    ctx.script("counted", says("counted", "Thanks."));
    let sender = ctx.engine.start_thread(&project.id, text("STUB[nudge] Tell the other thread."), settings(Access::Ask)).await.unwrap();
    assert_eq!(ctx.turn_ended(&sender).await.1, TurnState::Completed);
    assert_eq!(ctx.engine.inner().assignments.deliveries.waiting(&busy), 1, "it waits");
    assert_eq!(summary(ctx, &busy).status, ThreadStatus::NeedsApproval);

    ctx.engine.answer(&pending.id, Answer::Allow).await.unwrap();
    turns_end(ctx, &[(&busy, 2), (&sender, 1)]).await;
    until(ctx, "the reply looked at", |ctx| reports(ctx) == 1 && idle(ctx, &[&busy, &sender])).await;
    let view = ctx.read_thread(&busy).await;
    assert_eq!(view.turns.len(), 2, "a turn of its own, not a steer");
    assert_eq!(from(&view.turns[0]), None);
    assert_eq!(from(&view.turns[1]).map(|f| (f.thread, f.report)), Some((sender.clone(), None)));
    assert_eq!(first_text(&view.turns[1]), "STUB[nudged] Also count the piles.");
    let sent = ctx.read_thread(&sender).await;
    assert_eq!(sent.turns.len(), 2, "the reply of a thread it did not assign");
    assert_eq!(from(&sent.turns[1]).map(|f| (f.thread, f.report)), Some((busy.clone(), Some(TurnEnd::Completed))));
    assert_eq!(first_text(&sent.turns[1]), "Counted. STUB[counted]");
    let item = ItemBody::ThreadTool { tool: ThreadTool::Message, thread: Some(busy.clone()), text: Some("STUB[nudged] Also count the piles.".into()), status: ToolCallStatus::Completed };
    assert!(bodies(&sent.turns[0]).contains(&item), "{:?}", bodies(&sent.turns[0]));
}

/// Two threads message each other; each message starts a turn in the other, whose reply comes back, and a
/// reply's turn reports nothing.
pub async fn assignment_exchange(ctx: &mut Ctx) {
    let project = ctx.engine.add_project(ctx.run.project()).await.unwrap();
    ctx.script("b", says("b", "Ready."));
    let b = ctx.engine.start_thread(&project.id, text("STUB[b] Wait for a question."), settings(Access::Ask)).await.unwrap();
    assert_eq!(ctx.turn_ended(&b).await.1, TurnState::Completed);
    let (to_a, to_b) = (Later::default(), Later::default());
    to_b.set(&b);
    let t = to_b.clone();
    ctx.script("ping", steps("ping", move || vec![engine_call("call_ping", "message", json!({"thread": t.get(), "text": "STUB[pong] Please confirm the deck."}))], "Asked."));
    let t = to_a.clone();
    let pong = steps("pong", move || vec![engine_call("call_pong", "message", json!({"thread": t.get(), "text": "STUB[back] The deck is confirmed."}))], "Replied. STUB[replied]");
    ctx.script("pong", after(Duration::from_millis(1000), pong));
    // The reply to the second message comes once b's turn has ended, so b is idle for it.
    ctx.script("back", after(Duration::from_millis(1000), says("back", "Thanks. STUB[thanked]")));
    ctx.script("thanked", says("thanked", "Welcome."));
    ctx.script("replied", says("replied", "Good."));
    let a = ctx.engine.start_thread(&project.id, text("STUB[ping] Ask the other thread."), settings(Access::Ask)).await.unwrap();
    to_a.set(&a);
    turns_end(ctx, &[(&a, 3), (&b, 2)]).await;
    until(ctx, "both replies looked at", |ctx| reports(ctx) == 2 && idle(ctx, &[&a, &b])).await;

    let a_view = ctx.read_thread(&a).await;
    let b_view = ctx.read_thread(&b).await;
    assert_eq!((a_view.turns.len(), b_view.turns.len()), (3, 3));
    assert_eq!(from(&b_view.turns[1]).map(|f| (f.thread, f.report)), Some((a.clone(), None)));
    assert_eq!(from(&a_view.turns[1]).map(|f| (f.thread, f.report)), Some((b.clone(), None)));
    assert_eq!(first_text(&a_view.turns[1]), "STUB[back] The deck is confirmed.");
    // Each reply comes back to the thread whose message asked for it.
    assert_eq!(from(&a_view.turns[2]).map(|f| (f.thread, f.report)), Some((b.clone(), Some(TurnEnd::Completed))));
    assert_eq!(first_text(&a_view.turns[2]), "Replied. STUB[replied]");
    assert_eq!(from(&b_view.turns[2]).map(|f| (f.thread, f.report)), Some((a.clone(), Some(TurnEnd::Completed))));
    assert_eq!(first_text(&b_view.turns[2]), "Thanks. STUB[thanked]");
    assert!(summary(ctx, &a).assigned_by.is_none() && summary(ctx, &b).assigned_by.is_none());
}

/// Across projects: the agent lists the threads, messages one in the other project, and assigns a
/// thread there; the reply and the report come back.
pub async fn assignment_projects(ctx: &mut Ctx) {
    let here = ctx.engine.add_project(ctx.run.project()).await.unwrap();
    let other_path = ctx.run.root.join("other");
    std::fs::create_dir_all(&other_path).unwrap();
    let other = ctx.engine.add_project(other_path).await.unwrap();
    ctx.script("far", says("far", "Ready."));
    let far = ctx.engine.start_thread(&other.id, text("STUB[far] Wait for word."), settings(Access::Ask)).await.unwrap();
    assert_eq!(ctx.turn_ended(&far).await.1, TurnState::Completed);
    let target = Later::default();
    target.set(&far);
    let t = target.clone();
    let calls = move || {
        vec![
            engine_call("call_threads", "threads", json!({})),
            engine_call("call_reach", "message", json!({"thread": t.get(), "text": "STUB[reached] Word from the bridge."})),
            engine_call("call_deck", "assign", json!({"title": "Deck", "brief": "STUB[deck] Take off the deck.", "project": "other"})),
        ]
    };
    ctx.script("across", steps("across", calls, "Sent."));
    ctx.script("reached", says("reached", "Heard. STUB[heard]"));
    ctx.script("heard", says("heard", "Good."));
    ctx.script("deck", after(Duration::from_millis(1500), says("deck", "Deck taken off. STUB[decked]")));
    ctx.script("decked", says("decked", "Noted."));
    let lead = ctx.engine.start_thread(&here.id, text("STUB[across] Reach the other project."), settings(Access::Ask)).await.unwrap();
    let deck = ctx.wait("the assigned thread", |e| match e {
        Event::Thread { thread } if thread.assigned_by.as_ref() == Some(&lead) => Some(thread.id.clone()),
        _ => None,
    });
    let deck = deck.await;
    turns_end(ctx, &[(&lead, 3), (&far, 1), (&deck, 1)]).await;
    until(ctx, "the reply and report looked at", |ctx| reports(ctx) == 2 && idle(ctx, &[&lead, &far, &deck])).await;

    assert_eq!(summary(ctx, &deck).project, other.id);
    let far_view = ctx.read_thread(&far).await;
    assert_eq!(from(&far_view.turns[1]).map(|f| f.thread), Some(lead.clone()));
    let lead_view = ctx.read_thread(&lead).await;
    assert_eq!(lead_view.turns.len(), 3);
    assert_eq!(from(&lead_view.turns[1]).map(|f| (f.thread, f.report)), Some((far.clone(), Some(TurnEnd::Completed))));
    assert_eq!(from(&lead_view.turns[2]).map(|f| (f.thread, f.report)), Some((deck.clone(), Some(TurnEnd::Completed))));
    let listed = ItemBody::ThreadTool { tool: ThreadTool::Threads, thread: None, text: None, status: ToolCallStatus::Completed };
    assert!(bodies(&lead_view.turns[0]).contains(&listed), "{:?}", bodies(&lead_view.turns[0]));
}

/// A turn the person starts in an assigned thread reports nothing.
pub async fn assignment_person(ctx: &mut Ctx) {
    let project = ctx.engine.add_project(ctx.run.project()).await.unwrap();
    let assign = json!({"title": "Piles", "brief": "STUB[worker] Check the pile lengths."});
    ctx.script("lead", steps("lead", move || vec![engine_call("call_assign", "assign", assign.clone())], "Assigned."));
    ctx.script("worker", after(Duration::from_millis(1500), says("worker", "The piles are within tolerance. STUB[heard]")));
    ctx.script("heard", says("heard", "Good."));
    ctx.script("mine", says("mine", "The deck too."));
    let lead = ctx.engine.start_thread(&project.id, text("STUB[lead] Assign the piles."), settings(Access::Ask)).await.unwrap();
    let assigned = ctx.wait("the assigned thread", |e| match e {
        Event::Thread { thread } if thread.assigned_by.as_ref() == Some(&lead) => Some(thread.id.clone()),
        _ => None,
    });
    let assigned = assigned.await;
    turns_end(ctx, &[(&lead, 2), (&assigned, 1)]).await;
    until(ctx, "the report looked at", |ctx| reports(ctx) == 1 && idle(ctx, &[&lead, &assigned])).await;

    ctx.engine.send(&assigned, text("STUB[mine] Check the deck as well."), settings(Access::Ask)).await.unwrap();
    assert_eq!(ctx.turn_ended(&assigned).await.1, TurnState::Completed);
    until(ctx, "the person's turn looked at", |ctx| reports(ctx) == 2).await;
    let lead_view = ctx.read_thread(&lead).await;
    assert_eq!(lead_view.turns.len(), 2, "the person's turn reported nothing");
    assert_eq!(ctx.engine.inner().assignments.deliveries.waiting(&lead), 0);
    assert_eq!(summary(ctx, &lead).status, ThreadStatus::Idle);
}

/// After Codex restarts and the project is listed again, a fork of an assigned thread is not an
/// assignment (the listing does not say it is a fork, a read does), and a message to the assigned
/// thread, whose access the engine no longer knows, runs read only as both threads did.
pub async fn assignment_access(ctx: &mut Ctx) {
    let project = ctx.engine.add_project(ctx.run.project()).await.unwrap();
    let assign = json!({"title": "Piles", "brief": "STUB[worker] Check the pile lengths."});
    ctx.script("lead", steps("lead", move || vec![engine_call("call_assign", "assign", assign.clone())], "Assigned."));
    ctx.script("worker", after(Duration::from_millis(1500), says("worker", "Checked. STUB[heard]")));
    ctx.script("heard", says("heard", "Noted."));
    let lead = ctx.engine.start_thread(&project.id, text("STUB[lead] Assign the piles."), settings(Access::ReadOnly)).await.unwrap();
    let assigned = ctx.wait("the assigned thread", |e| match e {
        Event::Thread { thread } if thread.assigned_by.as_ref() == Some(&lead) => Some(thread.id.clone()),
        _ => None,
    });
    let assigned = assigned.await;
    turns_end(ctx, &[(&lead, 2), (&assigned, 1)]).await;
    until(ctx, "the report looked at", |ctx| reports(ctx) == 1 && idle(ctx, &[&lead, &assigned])).await;
    assert_eq!(summary(ctx, &assigned).settings.map(|s| s.access), Some(Access::ReadOnly), "the assigner's access");
    let first = ctx.read_thread(&assigned).await.turns[0].id.clone();
    let fork = ctx.engine.fork(&assigned, &first).await.unwrap();
    assert_eq!(summary(ctx, &fork).assigned_by, None);

    ctx.kill_codex();
    ctx.wait("Codex to be back", |e| matches!(e, Event::Codex { status: CodexStatus::Ready { .. } }).then_some(())).await;
    // The engine lists the project again once Codex is back; that ends before the project is removed.
    tokio::time::sleep(Duration::from_millis(1000)).await;
    ctx.engine.remove_project(&project.id).await.unwrap();
    ctx.engine.add_project(ctx.run.project()).await.unwrap();
    assert_eq!(summary(ctx, &fork).assigned_by, None, "a fork, though the listing does not say so");
    assert_eq!(summary(ctx, &assigned).assigned_by, Some(lead.clone()));

    let target = Later::default();
    target.set(&assigned);
    let t = target.clone();
    ctx.script("again", steps("again", move || vec![engine_call("call_again", "message", json!({"thread": t.get(), "text": "STUB[more] Once more."}))], "Sent."));
    ctx.script("more", after(Duration::from_millis(1000), says("more", "Again. STUB[heard2]")));
    ctx.script("heard2", says("heard2", "Noted again."));
    ctx.engine.send(&lead, text("STUB[again] Ask once more."), settings(Access::ReadOnly)).await.unwrap();
    turns_end(ctx, &[(&lead, 2), (&assigned, 1)]).await;
    until(ctx, "the report looked at", |ctx| reports(ctx) == 2 && idle(ctx, &[&lead, &assigned])).await;
    assert_eq!(summary(ctx, &assigned).settings.map(|s| s.access), Some(Access::ReadOnly), "not invented by the listing");
    let worker_view = ctx.read_thread(&assigned).await;
    assert_eq!(worker_view.turns.iter().map(first_text).collect::<Vec<_>>(), ["STUB[worker] Check the pile lengths.", "STUB[more] Once more."]);
    let lead_view = ctx.read_thread(&lead).await;
    assert_eq!(lead_view.turns.iter().map(first_text).collect::<Vec<_>>(), ["STUB[lead] Assign the piles.", "Checked. STUB[heard]", "STUB[again] Ask once more.", "Again. STUB[heard2]"]);
    assert_eq!(ctx.engine.snapshot().threads.len(), 3, "the lead, the assigned thread and its fork");
}

/// Live: messages to a running thread are delivered in order, each a turn of its own once the one
/// before has ended, and each of those turns reports back to the assigner, in order.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn messages_and_reports_keep_their_order() {
    if !super::harness::codex_present() {
        eprintln!("skipped: CHIRALITY_TEST_CODEX names the pinned Codex");
        return;
    }
    let mut ctx = Ctx::live("assignment_order", false).await;
    let project = ctx.engine.add_project(ctx.run.project()).await.unwrap();
    // The messages name the thread the assignment made, from the answer to it.
    let lead_script: Script = Arc::new(|request| {
        let assigned = request.outputs.get("call_assign").and_then(|o| serde_json::from_str::<Value>(o).ok()).and_then(|o| o["thread"].as_str().map(str::to_string));
        let mut calls = vec![engine_call("call_assign", "assign", json!({"title": "Piles", "brief": "STUB[w1] First."}))];
        if let Some(worker) = assigned {
            calls.extend([2, 3].map(|n| engine_call(&format!("call_m{n}"), "message", json!({"thread": worker, "text": format!("STUB[w{n}] Message {n}.")}))));
        }
        match calls.into_iter().find(|c| !request.outputs.contains_key(c["call_id"].as_str().unwrap_or_default())) {
            Some(call) => Reply::Items(vec![call]),
            None => Reply::Items(vec![message("msg_lead_done", "Sent.")]),
        }
    });
    ctx.script("lead", lead_script);
    // The worker's first reply waits until both messages wait behind its turn.
    let engine = ctx.engine.clone();
    let queued: Script = Arc::new(move |request| {
        for _ in 0..1500 {
            if engine.inner().assignments.deliveries.waiting_anywhere() >= 2 {
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        says("w1", "Reply 1. STUB[r1]")(request)
    });
    ctx.script("w1", queued);
    for n in 1..=3 {
        let (key, reply) = (format!("w{n}"), format!("r{n}"));
        if n > 1 {
            ctx.script(&key, says(&key, &format!("Reply {n}. STUB[r{n}]")));
        }
        ctx.script(&reply, says(&reply, "Ok."));
    }
    let lead = ctx.engine.start_thread(&project.id, text("STUB[lead] Assign and follow up."), settings(Access::Ask)).await.unwrap();
    let assigned = ctx.wait("the assigned thread", |e| match e {
        Event::Thread { thread } if thread.assigned_by.as_ref() == Some(&lead) => Some(thread.id.clone()),
        _ => None,
    });
    let assigned = assigned.await;
    turns_end(&mut ctx, &[(&lead, 4), (&assigned, 3)]).await;
    until(&ctx, "every turn looked at", |ctx| reports(ctx) == 3 && idle(ctx, &[&lead, &assigned])).await;
    let worker_view = ctx.engine.read_thread(&assigned).await.unwrap();
    let opened: Vec<String> = worker_view.turns.iter().map(first_text).collect();
    assert_eq!(opened, ["STUB[w1] First.", "STUB[w2] Message 2.", "STUB[w3] Message 3."]);
    let lead_view = ctx.engine.read_thread(&lead).await.unwrap();
    let reported: Vec<String> = lead_view.turns[1..].iter().map(first_text).collect();
    assert_eq!(reported, ["Reply 1. STUB[r1]", "Reply 2. STUB[r2]", "Reply 3. STUB[r3]"]);
    ctx.finish("assignment_order").await;
}
