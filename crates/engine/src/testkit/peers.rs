//! Threads as peers, recorded: a thread started without the engine's tools
//! (as one started before them) answers a message, and its reply comes back; a
//! thread reads a peer and a subagent, and a subagent's read is refused; and a
//! message mentions threads, which its history names again.

use std::sync::Arc;

use serde_json::json;

use super::assignments::{Later, after, bodies, engine_call, first_text, from, idle, reports, steps, summary, turns_end, until};
use super::harness::{Ctx, settings, text};
use super::history::{codex_thread, reply};
use super::stub::{Reply, message, says, shell_call};
use super::subagents::{new_subagent, spawn, wait};
use crate::api::*;
use crate::assignment::read::{ANSWER_LIMIT, ReadArgs, View};
use crate::assignment::tools::SUBAGENT_REFUSED;

/// A thread started without the engine's tools cannot message, but is messaged like any: its reply comes back.
pub async fn assignment_untooled(ctx: &mut Ctx) {
    let project = ctx.engine.add_project(ctx.run.project()).await.unwrap();
    let (old, _) = codex_thread(ctx, &project).await;
    reply(ctx, &old, "old").await;
    ctx.script("asked", after(std::time::Duration::from_millis(500), says("asked", "Still here. STUB[answered]")));
    ctx.script("answered", says("answered", "Good."));
    let target = Later::default();
    target.set(&old);
    let t = target.clone();
    ctx.script("ask", steps("ask", move || vec![engine_call("call_ask", "message", json!({"thread": t.get(), "text": "STUB[asked] Are you there?"}))], "Asked."));
    let lead = ctx.engine.start_thread(&project.id, text("STUB[ask] Ask the old thread."), settings(Access::Ask)).await.unwrap();
    turns_end(ctx, &[(&lead, 2), (&old, 1)]).await;
    until(ctx, "the reply looked at", |ctx| reports(ctx) == 1 && idle(ctx, &[&lead, &old])).await;

    let old_view = ctx.read_thread(&old).await;
    assert_eq!(old_view.turns.len(), 2);
    assert_eq!(from(&old_view.turns[1]).map(|f| (f.thread, f.report)), Some((lead.clone(), None)));
    let lead_view = ctx.read_thread(&lead).await;
    assert_eq!(lead_view.turns.len(), 2);
    assert_eq!(from(&lead_view.turns[1]).map(|f| (f.thread, f.report)), Some((old.clone(), Some(TurnEnd::Completed))));
    assert_eq!(first_text(&lead_view.turns[1]), "Still here. STUB[answered]");
    assert!(summary(ctx, &old).assigned_by.is_none());
}

/// A peer's long reply, cut in a read.
fn long_reply() -> String {
    format!("Surveyed. {}", "y".repeat(20_000))
}

/// A lead reads a peer (messages, then the work) and its own subagent, whose own read is refused; a read
/// wakes neither thread.
pub async fn assignment_read(ctx: &mut Ctx) {
    let project = ctx.engine.add_project(ctx.run.project()).await.unwrap();
    ctx.script("peer", steps("peer", || vec![shell_call("call_survey", "printf ok", false)], "Survey done."));
    let peer = ctx.engine.start_thread(&project.id, text("STUB[peer] Survey."), settings(Access::Ask)).await.unwrap();
    assert_eq!(ctx.turn_ended(&peer).await.1, TurnState::Completed);
    let long = long_reply();
    ctx.script("peer2", Arc::new(move |_| Reply::Items(vec![message("msg_peer2", &long)])));
    ctx.engine.send(&peer, text("STUB[peer2] Again."), settings(Access::Ask)).await.unwrap();
    assert_eq!(ctx.turn_ended(&peer).await.1, TurnState::Completed);
    ctx.engine.mark_read(&peer);

    let (to_peer, to_worker) = (Later::default(), Later::default());
    to_peer.set(&peer);
    let p = to_peer.clone();
    ctx.script("worker", Arc::new(move |request| match request.outputs.get("call_peek") {
        None => Reply::Items(vec![engine_call("call_peek", "read", json!({"thread": p.get()}))]),
        Some(answer) => Reply::Items(vec![message("msg_worker_done", &format!("Not read: {answer}"))]),
    }));
    let (p, w) = (to_peer.clone(), to_worker.clone());
    let reads = move || {
        vec![
            spawn("call_spawn", "worker", "STUB[worker] look"),
            wait("call_wait"),
            engine_call("call_read_peer", "read", json!({"thread": p.get(), "limit": 3})),
            engine_call("call_read_worker", "read", json!({"thread": w.get(), "view": "activity"})),
        ]
    };
    ctx.script("reader", steps("reader", reads, "Read."));
    let lead = ctx.engine.start_thread(&project.id, text("STUB[reader] Read the peer and the worker."), settings(Access::Ask)).await.unwrap();
    let worker = new_subagent(ctx, "/root/worker").await.id;
    to_worker.set(&worker);
    assert_eq!(ctx.turn_ended(&lead).await.1, TurnState::Completed);

    // The lead's reads show as its work with each thread; the subagent's was refused, and it went on.
    let lead_view = ctx.read_thread(&lead).await;
    let read = |thread: &ThreadId, status| ItemBody::ThreadTool { tool: ThreadTool::Read, thread: Some(thread.clone()), text: None, status };
    let items = bodies(&lead_view.turns[0]);
    assert!(items.contains(&read(&peer, ToolCallStatus::Completed)) && items.contains(&read(&worker, ToolCallStatus::Completed)), "{items:?}");
    let worker_view = ctx.read_thread(&worker).await;
    let items = bodies(&worker_view.turns[0]);
    assert!(items.contains(&read(&peer, ToolCallStatus::Failed)), "{items:?}");
    assert!(matches!(items.last(), Some(ItemBody::AgentMessage { text }) if text.contains(SUBAGENT_REFUSED)), "{items:?}");
    // Nothing was started or queued on the threads read, and the peer is not marked as having news.
    assert_eq!(ctx.read_thread(&peer).await.turns.len(), 2);
    assert!(!summary(ctx, &peer).unread && summary(ctx, &peer).status == ThreadStatus::Idle);
    assert_eq!(ctx.engine.inner().assignments.deliveries.waiting_anywhere(), 0);

    // What a read gives: the messages newest first, the long reply cut, numbered to page back.
    let args = |view, before, limit| ReadArgs { thread: peer.0.clone(), view, before, limit };
    let messages = ctx.engine.inner().read_for(&lead, args(View::Messages, None, Some(3))).await.unwrap();
    assert_eq!((messages["title"].as_str(), messages["status"].as_str(), &messages["error"]), (Some("STUB[peer] Survey."), Some("idle"), &json!(null)));
    let kinds: Vec<&str> = messages["items"].as_array().unwrap().iter().map(|i| i["kind"].as_str().unwrap()).collect();
    assert_eq!(kinds, ["reply", "person", "reply"]);
    assert!(messages["items"][0]["text"].as_str().unwrap().ends_with("[cut: 18010 more characters]"), "{}", messages["items"][0]["text"]);
    assert_eq!(messages["items"][2]["text"], "Survey done.");
    assert_eq!((&messages["more"], &messages["before"]), (&json!(true), &json!(2)));
    let earlier = ctx.engine.inner().read_for(&lead, args(View::Messages, Some(2), None)).await.unwrap();
    assert_eq!((earlier["items"][0]["text"].as_str(), &earlier["more"]), (Some("STUB[peer] Survey."), &json!(false)));
    let activity = ctx.engine.inner().read_for(&lead, args(View::Activity, None, None)).await.unwrap();
    assert!(activity.to_string().len() <= ANSWER_LIMIT);
    let command = activity["items"].as_array().unwrap().iter().find(|i| i["kind"] == "command").expect("the peer's command");
    assert!(command["command"].as_str().unwrap().contains("printf ok") && command["output"] == "ok", "{command}");
    let own = ctx.engine.inner().read_for(&lead, ReadArgs { thread: lead.0.clone(), view: View::Messages, before: None, limit: None }).await;
    assert!(own.is_err(), "{own:?}");
}

/// A thread with one turn, renamed.
async fn named(ctx: &mut Ctx, project: &ProjectId, key: &str, title: &str) -> ThreadId {
    ctx.script(key, says(key, "Ready."));
    let thread = ctx.engine.start_thread(project, text(&format!("STUB[{key}] Start.")), settings(Access::Ask)).await.unwrap();
    assert_eq!(ctx.turn_ended(&thread).await.1, TurnState::Completed);
    ctx.engine.rename(&thread, title.into()).await.unwrap();
    thread
}

/// A message mentioning threads, in this project and another: the agent gets a reference to each, its
/// history names them again, and a new thread's title leaves them out.
pub async fn thread_refs(ctx: &mut Ctx) {
    let here = ctx.engine.add_project(ctx.run.project()).await.unwrap();
    let other_path = ctx.run.root.join("other");
    std::fs::create_dir_all(&other_path).unwrap();
    let other = ctx.engine.add_project(other_path).await.unwrap();
    let piles = named(ctx, &here.id, "piles", "Pile plan").await;
    let steel = named(ctx, &other.id, "steel", "Steel estimate").await;
    let refs = vec![ThreadRef { thread: piles.clone(), title: Some("Pile plan".into()) }, ThreadRef { thread: steel.clone(), title: Some("Steel estimate".into()) }];

    // A new thread whose first message mentions one: its title, read from Codex's listing, is the text alone.
    ctx.script("compare", says("compare", "Compared."));
    let first = Message { text: "STUB[compare] Check @Pile plan".into(), attachments: vec![], workflow: None, threads: refs[..1].to_vec() };
    let compare = ctx.engine.start_thread(&here.id, first, settings(Access::Ask)).await.unwrap();
    assert_eq!(ctx.turn_ended(&compare).await.1, TurnState::Completed);
    ctx.engine.remove_project(&here.id).await.unwrap();
    let here = ctx.engine.add_project(ctx.run.project()).await.unwrap();
    assert_eq!(summary(ctx, &compare).title.as_deref(), Some("STUB[compare] Check @Pile plan"));
    assert_eq!((summary(ctx, &compare).project, summary(ctx, &piles).title), (here.id.clone(), Some("Pile plan".into())));

    // A message mentioning a thread here and one in the other project: the agent's input holds a
    // reference to each, with its project, and the history names them again.
    let deck = named(ctx, &here.id, "deck", "Deck survey").await;
    ctx.script("refs", Arc::new(|request| {
        let input = request.body["input"].to_string();
        let seen = ["Pile plan", "Steel estimate"].iter().all(|t| input.contains(&format!("title=\\\"{t}\\\""))) && input.contains("project=\\\"other\\\"");
        Reply::Items(vec![message("msg_refs", if seen { "Both threads are referenced." } else { "No references." })])
    }));
    let mention = Message { text: "STUB[refs] Compare @Pile plan with @Steel estimate".into(), attachments: vec![], workflow: None, threads: refs.clone() };
    ctx.engine.send(&deck, mention, settings(Access::Ask)).await.unwrap();
    assert_eq!(ctx.turn_ended(&deck).await.1, TurnState::Completed);
    let view = ctx.read_thread(&deck).await;
    let ItemBody::UserMessage { text, threads, .. } = &view.turns[1].items[0].body else { panic!("{:?}", view.turns[1].items[0]) };
    assert_eq!((text.as_str(), threads), ("STUB[refs] Compare @Pile plan with @Steel estimate", &refs));
    assert_eq!(bodies(&view.turns[1]).last(), Some(&ItemBody::AgentMessage { text: "Both threads are referenced.".into() }));
    ctx.engine.mark_read(&piles);
}
