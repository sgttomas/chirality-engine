//! Assignment between threads, recorded: an assignment and its report (and a
//! close, after which a turn the assigner started reports nothing); a message
//! to a running thread waiting for its turn to end; two threads messaging each
//! other; a message and an assignment across projects; and a person's turn in
//! an assigned thread reporting nothing.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Value, json};

use super::harness::{Ctx, settings, text};
use super::stub::{Reply, Script, message, says, shell_call, tool_call};
use crate::api::*;

/// A call to one of the engine's own tools.
fn engine_call(id: &str, name: &str, args: Value) -> Value {
    tool_call(id, "chirality", name, args)
}

/// The calls `calls` gives, in order, each once the one before has its output, then `done`.
fn steps(key: &'static str, calls: impl Fn() -> Vec<Value> + Send + Sync + 'static, done: &'static str) -> Script {
    Arc::new(move |request| match calls().into_iter().find(|c| !request.outputs.contains_key(c["call_id"].as_str().unwrap_or_default())) {
        Some(call) => Reply::Items(vec![call]),
        None => Reply::Items(vec![message(&format!("msg_{key}_done"), done)]),
    })
}

/// `script`, after a pause, so what the scenario waits on comes in one order.
fn after(pause: Duration, script: Script) -> Script {
    Arc::new(move |request| {
        std::thread::sleep(pause);
        script(request)
    })
}

/// A thread id the scenario learns once the thread exists, for a script made before.
#[derive(Clone, Default)]
struct Later(Arc<Mutex<String>>);

impl Later {
    fn set(&self, thread: &ThreadId) {
        *self.0.lock().unwrap() = thread.0.clone();
    }

    fn get(&self) -> String {
        self.0.lock().unwrap().clone()
    }
}

fn summary(ctx: &Ctx, thread: &ThreadId) -> ThreadSummary {
    ctx.engine.snapshot().threads.into_iter().find(|t| &t.id == thread).expect("the thread is in the snapshot")
}

fn bodies(turn: &Turn) -> Vec<ItemBody> {
    turn.items.iter().map(|i| i.body.clone()).collect()
}

/// The sender of the turn's first message.
fn from(turn: &Turn) -> Option<MessageFrom> {
    match &turn.items[0].body {
        ItemBody::UserMessage { from, .. } => from.clone(),
        other => panic!("not a message: {other:?}"),
    }
}

fn first_text(turn: &Turn) -> String {
    match &turn.items[0].body {
        ItemBody::UserMessage { text, .. } => text.clone(),
        other => panic!("not a message: {other:?}"),
    }
}

/// Waits until `done` holds, the engine's work for it settled.
async fn until(ctx: &Ctx, what: &str, done: impl Fn(&Ctx) -> bool) {
    for _ in 0..3000 {
        if done(ctx) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("timed out waiting for {what}");
}

fn reports(ctx: &Ctx) -> usize {
    ctx.engine.inner().assignments.reports.load(std::sync::atomic::Ordering::SeqCst)
}

fn idle(ctx: &Ctx, threads: &[&ThreadId]) -> bool {
    threads.iter().all(|t| summary(ctx, t).status == ThreadStatus::Idle)
}

/// The turns that have ended on each of `threads`, counted from now until each has `n`.
async fn turns_end(ctx: &mut Ctx, threads: &[(&ThreadId, usize)]) {
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
    let project = ctx.engine.add_project(ctx.run.project()).await.unwrap();
    let worker = Later::default();
    let assign = json!({"title": "Piles", "brief": "STUB[worker] Check the pile lengths.", "effort": "medium", "access": "fullAccess"});
    ctx.script("lead", steps("lead", move || vec![engine_call("call_assign", "assign", assign.clone())], "Assigned."));
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

/// A message to a thread whose turn waits on the person waits for that turn to end, then starts its own.
pub async fn assignment_waits(ctx: &mut Ctx) {
    let project = ctx.engine.add_project(ctx.run.project()).await.unwrap();
    ctx.script("busy", super::stub::calls("busy", shell_call("call_busy", "printf ok > busy.txt", true), false));
    let busy = ctx.engine.start_thread(&project.id, text("STUB[busy] Write the file."), settings(Access::Ask)).await.unwrap();
    let pending = ctx.pending(&busy).await;
    let target = Later::default();
    target.set(&busy);
    let t = target.clone();
    ctx.script("nudge", steps("nudge", move || vec![engine_call("call_nudge", "message", json!({"thread": t.get(), "text": "STUB[nudged] Also count the piles."}))], "Sent."));
    ctx.script("nudged", says("nudged", "Counted."));
    let sender = ctx.engine.start_thread(&project.id, text("STUB[nudge] Tell the other thread."), settings(Access::Ask)).await.unwrap();
    assert_eq!(ctx.turn_ended(&sender).await.1, TurnState::Completed);
    assert_eq!(ctx.engine.inner().assignments.deliveries.waiting(&busy), 1, "it waits");
    assert_eq!(summary(ctx, &busy).status, ThreadStatus::NeedsApproval);

    ctx.engine.answer(&pending.id, Answer::Allow).await.unwrap();
    turns_end(ctx, &[(&busy, 2)]).await;
    let view = ctx.read_thread(&busy).await;
    assert_eq!(view.turns.len(), 2, "a turn of its own, not a steer");
    assert_eq!(from(&view.turns[0]), None);
    assert_eq!(from(&view.turns[1]).map(|f| (f.thread, f.report)), Some((sender.clone(), None)));
    assert_eq!(first_text(&view.turns[1]), "STUB[nudged] Also count the piles.");
    let sent = ctx.read_thread(&sender).await;
    assert_eq!(sent.turns.len(), 1, "no report from a thread it did not assign");
    let item = ItemBody::ThreadTool { tool: ThreadTool::Message, thread: Some(busy.clone()), text: Some("STUB[nudged] Also count the piles.".into()), status: ToolCallStatus::Completed };
    assert!(bodies(&sent.turns[0]).contains(&item), "{:?}", bodies(&sent.turns[0]));
}

/// Two threads message each other; each message starts a turn in the other.
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
    let pong = steps("pong", move || vec![engine_call("call_pong", "message", json!({"thread": t.get(), "text": "STUB[back] The deck is confirmed."}))], "Replied.");
    ctx.script("pong", after(Duration::from_millis(1000), pong));
    ctx.script("back", says("back", "Thanks."));
    let a = ctx.engine.start_thread(&project.id, text("STUB[ping] Ask the other thread."), settings(Access::Ask)).await.unwrap();
    to_a.set(&a);
    turns_end(ctx, &[(&a, 2), (&b, 1)]).await;
    until(ctx, "both idle", |ctx| idle(ctx, &[&a, &b])).await;

    let a_view = ctx.read_thread(&a).await;
    let b_view = ctx.read_thread(&b).await;
    assert_eq!((a_view.turns.len(), b_view.turns.len()), (2, 2));
    assert_eq!(from(&b_view.turns[1]).map(|f| f.thread), Some(a.clone()));
    assert_eq!(from(&a_view.turns[1]).map(|f| (f.thread, f.report)), Some((b.clone(), None)));
    assert_eq!(first_text(&a_view.turns[1]), "STUB[back] The deck is confirmed.");
    assert!(summary(ctx, &a).assigned_by.is_none() && summary(ctx, &b).assigned_by.is_none());
}

/// Across projects: the agent lists the threads, messages one in the other project, and assigns a
/// thread there, whose report comes back.
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
    ctx.script("reached", says("reached", "Heard."));
    ctx.script("deck", after(Duration::from_millis(1500), says("deck", "Deck taken off. STUB[decked]")));
    ctx.script("decked", says("decked", "Noted."));
    let lead = ctx.engine.start_thread(&here.id, text("STUB[across] Reach the other project."), settings(Access::Ask)).await.unwrap();
    let deck = ctx.wait("the assigned thread", |e| match e {
        Event::Thread { thread } if thread.assigned_by.as_ref() == Some(&lead) => Some(thread.id.clone()),
        _ => None,
    });
    let deck = deck.await;
    turns_end(ctx, &[(&lead, 2), (&far, 1), (&deck, 1)]).await;
    until(ctx, "the report looked at", |ctx| reports(ctx) == 1 && idle(ctx, &[&lead, &far, &deck])).await;

    assert_eq!(summary(ctx, &deck).project, other.id);
    let far_view = ctx.read_thread(&far).await;
    assert_eq!(from(&far_view.turns[1]).map(|f| f.thread), Some(lead.clone()));
    let lead_view = ctx.read_thread(&lead).await;
    assert_eq!(lead_view.turns.len(), 2);
    assert_eq!(from(&lead_view.turns[1]).map(|f| (f.thread, f.report)), Some((deck.clone(), Some(TurnEnd::Completed))));
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
    let worker = Later::default();
    let w = worker.clone();
    let calls = move || {
        let mut calls = vec![engine_call("call_assign", "assign", json!({"title": "Piles", "brief": "STUB[w1] First."}))];
        if !w.get().is_empty() {
            for n in [2, 3] {
                calls.push(engine_call(&format!("call_m{n}"), "message", json!({"thread": w.get(), "text": format!("STUB[w{n}] Message {n}.")})));
            }
        }
        calls
    };
    ctx.script("lead", steps("lead", calls, "Sent."));
    for n in 1..=3 {
        let (key, reply) = (format!("w{n}"), format!("r{n}"));
        ctx.script(&key, after(Duration::from_millis(800), says(&key, &format!("Reply {n}. STUB[r{n}]"))));
        ctx.script(&reply, says(&reply, "Ok."));
    }
    let lead = ctx.engine.start_thread(&project.id, text("STUB[lead] Assign and follow up."), settings(Access::Ask)).await.unwrap();
    let assigned = ctx.wait("the assigned thread", |e| match e {
        Event::Thread { thread } if thread.assigned_by.as_ref() == Some(&lead) => Some(thread.id.clone()),
        _ => None,
    });
    let assigned = assigned.await;
    worker.set(&assigned);
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
