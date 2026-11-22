//! Subagents, recorded: the agent starts one (which asks the person, and
//! starts one of its own) and waits for it; and the person stops a running
//! one, then its parent, then archives the parent. Codex 0.171.0's
//! multi-agent tools are on by default; the stub model calls them.

use std::sync::Arc;

use serde_json::{Value, json};

use super::harness::{Ctx, settings, text};
use super::apptools::{CHECK, NAMESPACE};
use super::stub::{Reply, Script, hangs, message, shell_call, tool_call};
use crate::api::*;

/// A call to one of Codex's own subagent tools.
fn agent_call(id: &str, name: &str, args: Value) -> Value {
    json!({"type": "function_call", "namespace": "collaboration", "name": name, "call_id": id, "id": format!("fc_{id}"), "arguments": args.to_string()})
}

pub(super) fn spawn(id: &str, task: &str, message: &str) -> Value {
    agent_call(id, "spawn_agent", json!({"task_name": task, "message": message, "fork_turns": "none"}))
}

pub(super) fn wait(id: &str) -> Value {
    agent_call(id, "wait_agent", json!({"timeout_ms": 60000}))
}

/// The calls in order, each once the one before has its output, then `done`.
fn steps(key: &'static str, calls: Vec<Value>, done: &'static str) -> Script {
    Arc::new(move |request| match calls.iter().find(|c| !request.outputs.contains_key(c["call_id"].as_str().unwrap_or_default())) {
        Some(call) => Reply::Items(vec![call.clone()]),
        None => Reply::Items(vec![message(&format!("msg_{key}_done"), done)]),
    })
}

fn summary(ctx: &Ctx, thread: &ThreadId) -> ThreadSummary {
    ctx.engine.snapshot().threads.into_iter().find(|t| &t.id == thread).expect("the thread is in the snapshot")
}

fn bodies(view: &ThreadView) -> Vec<ItemBody> {
    view.turns.iter().flat_map(|t| &t.items).map(|i| i.body.clone()).collect()
}

/// The subagent at `path`, once the engine has taken it in.
pub(super) async fn new_subagent(ctx: &mut Ctx, path: &str) -> ThreadSummary {
    ctx.wait("a subagent", |e| match e {
        Event::Thread { thread } if thread.subagent.as_ref().is_some_and(|s| s.path == path) => Some(thread.clone()),
        _ => None,
    })
    .await
}

pub async fn subagent(ctx: &mut Ctx) {
    let project = ctx.engine.add_project(ctx.run.project()).await.unwrap();
    ctx.script("lead", steps("lead", vec![spawn("call_spawn", "worker", "STUB[worker] check the piles"), wait("call_wait")], "Lead done."));
    ctx.script("worker", steps("worker", vec![shell_call("call_piles", "printf ok > piles.txt", true), spawn("call_inner", "inner", "STUB[inner] check one pile"), wait("call_inner_wait")], "Worker done."));
    ctx.script("inner", steps("inner", vec![], "Inner done."));
    let lead = ctx.engine.start_thread(&project.id, text("STUB[lead] delegate the piles"), settings(Access::Ask)).await.unwrap();

    // The subagent is a thread of the project, under its parent; its approval is its own.
    let worker = new_subagent(ctx, "/root/worker").await;
    assert_eq!(worker.subagent, Some(Subagent { parent: lead.clone(), path: "/root/worker".into(), nickname: None }));
    assert_eq!(worker.project, project.id);
    let pending = ctx.pending(&worker.id).await;
    assert!(matches!(&pending.request, Request::Command { command, .. } if command.contains("printf ok > piles.txt")), "{pending:?}");
    assert_eq!(summary(ctx, &worker.id).status, ThreadStatus::NeedsApproval);
    assert_eq!(summary(ctx, &lead).status, ThreadStatus::Working);
    // Codex takes no message from the person for a subagent: nothing is sent.
    let refused = ctx.engine.send(&worker.id, text("STUB[worker] and this"), settings(Access::Ask)).await;
    assert!(matches!(refused, Err(EngineError::Invalid { .. })), "{refused:?}");
    ctx.engine.answer(&pending.id, Answer::Allow).await.unwrap();
    let inner = new_subagent(ctx, "/root/worker/inner").await;
    assert_eq!(inner.subagent.as_ref().map(|s| (&s.parent, s.path.as_str())), Some((&worker.id, "/root/worker/inner")));
    assert_eq!(ctx.turn_ended(&lead).await.1, TurnState::Completed);
    // Their parent tells what they did: a subagent's own approval leaves it unread no more than its end does.
    assert!(!summary(ctx, &worker.id).unread && !summary(ctx, &inner.id).unread);

    // The parent's history shows where it delegated and what came back, each naming the subagent.
    let lead_view = ctx.read_thread(&lead).await;
    let named = |activity| ItemBody::Subagent { thread: worker.id.clone(), path: "/root/worker".into(), activity };
    let items = bodies(&lead_view);
    assert!(items.contains(&named(SubagentActivity::Started)) && items.contains(&named(SubagentActivity::Finished)), "{items:?}");
    assert!(items.contains(&ItemBody::SubagentTool { tool: SubagentTool::Wait, status: ToolCallStatus::Completed, threads: vec![], prompt: None }), "{items:?}");
    assert_eq!(items.last(), Some(&ItemBody::AgentMessage { text: "Lead done.".into() }));
    // The subagent reads like any thread: its own work, and its own subagent.
    let worker_view = ctx.read_thread(&worker.id).await;
    let items = bodies(&worker_view);
    assert!(items.iter().any(|b| matches!(b, ItemBody::Command { exit_code: Some(0), .. })), "{items:?}");
    assert!(items.contains(&ItemBody::Subagent { thread: inner.id.clone(), path: "/root/worker/inner".into(), activity: SubagentActivity::Finished }), "{items:?}");
    assert_eq!(items.last(), Some(&ItemBody::AgentMessage { text: "Worker done.".into() }));
    if ctx.is_live() {
        assert_eq!(std::fs::read_to_string(ctx.run.project().join("piles.txt")).unwrap(), "ok");
    }

    // Listed again (as after a restart), every subagent comes back under its parent, with its nickname.
    ctx.engine.remove_project(&project.id).await.unwrap();
    ctx.engine.add_project(ctx.run.project()).await.unwrap();
    let listed = summary(ctx, &inner.id).subagent.expect("a subagent");
    assert_eq!((listed.parent, listed.path.as_str()), (worker.id.clone(), "/root/worker/inner"));
    assert!(listed.nickname.is_some() && summary(ctx, &worker.id).subagent.unwrap().nickname.is_some());
    assert!(summary(ctx, &lead).subagent.is_none());
}

/// A subagent's message to the agent that started it names that agent, which
/// stays its parent; the parent's own tool calls are still its own.
pub async fn subagent_message(ctx: &mut Ctx) {
    let project = ctx.engine.add_project(ctx.run.project()).await.unwrap();
    let tools = ctx.app_tools();
    ctx.script("lead", steps("lead", vec![spawn("call_spawn", "worker", "STUB[worker] report back"), wait("call_wait")], "Lead done."));
    let to_root = agent_call("call_report", "send_message", json!({"target": "/root", "message": "The piles are fine."}));
    ctx.script("worker", steps("worker", vec![to_root], "Worker done."));
    let lead = ctx.engine.start_thread(&project.id, text("STUB[lead] delegate and hear back"), settings(Access::Ask)).await.unwrap();
    let worker = new_subagent(ctx, "/root/worker").await.id;
    assert_eq!(ctx.turn_ended(&lead).await.1, TurnState::Completed);
    let worker_view = ctx.read_thread(&worker).await;
    let messaged = ItemBody::Subagent { thread: lead.clone(), path: "/root".into(), activity: SubagentActivity::Messaged };
    assert!(bodies(&worker_view).contains(&messaged), "{:?}", bodies(&worker_view));
    assert!(summary(ctx, &lead).subagent.is_none());
    assert_eq!(summary(ctx, &worker).subagent.map(|s| s.parent), Some(lead.clone()));

    let (_, turn) = super::scenarios::check_turn(ctx, Some(&lead), &project, "check", 7).await;
    assert_eq!(tools.called(1).await[0].thread, lead);
    assert!(turn.items.iter().any(|i| matches!(&i.body, ItemBody::ToolCall { status: ToolCallStatus::Completed, .. })), "{:?}", turn.items);
}

/// A subagent has its parent's tools: its call to the application's comes to
/// the handler, naming the subagent; its call to the engine's is refused, and
/// it and its parent go on.
pub async fn subagent_tools(ctx: &mut Ctx) {
    let project = ctx.engine.add_project(ctx.run.project()).await.unwrap();
    let tools = ctx.app_tools();
    ctx.script("lead", steps("lead", vec![spawn("call_spawn", "worker", "STUB[worker] check and hand on"), wait("call_wait")], "Lead done."));
    let check = tool_call("call_check", NAMESPACE, CHECK, json!({"total": 9}));
    let assign = tool_call("call_assign", crate::assignment::NAMESPACE, "assign", json!({"title": "Piles", "brief": "Check the piles."}));
    ctx.script("worker", Arc::new(move |request| match (request.outputs.get("call_check"), request.outputs.get("call_assign")) {
        (None, _) => Reply::Items(vec![check.clone()]),
        (Some(_), None) => Reply::Items(vec![assign.clone()]),
        (Some(checked), Some(assigned)) => Reply::Items(vec![message("msg_worker_done", &format!("{checked} {assigned}"))]),
    }));
    let lead = ctx.engine.start_thread(&project.id, text("STUB[lead] delegate the check"), settings(Access::Ask)).await.unwrap();
    let worker = new_subagent(ctx, "/root/worker").await.id;
    assert_eq!(ctx.turn_ended(&lead).await.1, TurnState::Completed);

    let worker_view = ctx.read_thread(&worker).await;
    let call = ToolCall {
        thread: worker.clone(),
        project: Some(project.id.clone()),
        turn: worker_view.turns[0].id.clone(),
        item: ItemId("call_check".into()),
        namespace: Some(NAMESPACE.into()),
        tool: CHECK.into(),
        arguments: json!({"total": 9}),
    };
    assert_eq!(tools.called(1).await, vec![call]);
    let items = bodies(&worker_view);
    let checked = ItemBody::ToolCall { server: Some(NAMESPACE.into()), tool: CHECK.into(), arguments: r#"{"total":9}"#.into(),
        status: ToolCallStatus::Completed, result: Some("The estimate of 9 checks out.".into()), images: vec![] };
    assert!(items.contains(&checked), "{items:?}");
    let refused = ItemBody::ThreadTool { tool: ThreadTool::Assign, thread: None, text: Some("Check the piles.".into()), status: ToolCallStatus::Failed };
    assert!(items.contains(&refused), "{items:?}");
    let Some(ItemBody::AgentMessage { text: heard }) = items.last() else { panic!("{items:?}") };
    assert!(heard.contains("checks out") && heard.contains(crate::assignment::tools::SUBAGENT_REFUSED), "{heard}");
    // Nothing was assigned, and the parent went on.
    assert_eq!(ctx.engine.snapshot().threads.len(), 2);
    assert_eq!(bodies(&ctx.read_thread(&lead).await).last(), Some(&ItemBody::AgentMessage { text: "Lead done.".into() }));
}

pub async fn subagent_stop(ctx: &mut Ctx) {
    let project = ctx.engine.add_project(ctx.run.project()).await.unwrap();
    ctx.script("hold", steps("hold", vec![spawn("call_spawn", "worker", "STUB[stall] take your time"), wait("call_wait")], "Hold done."));
    ctx.script("stall", hangs());
    let lead = ctx.engine.start_thread(&project.id, text("STUB[hold] delegate and wait"), settings(Access::Ask)).await.unwrap();
    let worker = new_subagent(ctx, "/root/worker").await.id;
    ctx.wait("the subagent's turn", |e| matches!(e, Event::TurnStarted { thread, .. } if thread == &worker).then_some(())).await;
    assert_eq!(summary(ctx, &worker).status, ThreadStatus::Working);

    // Listed again while both run (Codex lists no turns), each still runs, and Stop still reaches it.
    ctx.engine.remove_project(&project.id).await.unwrap();
    ctx.engine.add_project(ctx.run.project()).await.unwrap();
    assert_eq!(summary(ctx, &worker).status, ThreadStatus::Working);
    assert_eq!(summary(ctx, &lead).status, ThreadStatus::Working);

    // Stopped by the person: the subagent's turn ends; its parent is not told, and goes on waiting.
    ctx.engine.interrupt(&worker).await.unwrap();
    assert_eq!(summary(ctx, &worker).status, ThreadStatus::Idle);
    assert_eq!(summary(ctx, &lead).status, ThreadStatus::Working);
    ctx.engine.interrupt(&lead).await.unwrap();
    assert_eq!(summary(ctx, &lead).status, ThreadStatus::Idle);
    let worker_view = ctx.read_thread(&worker).await;
    assert_eq!(worker_view.turns.iter().map(|t| t.state.clone()).collect::<Vec<_>>(), vec![TurnState::Interrupted]);

    // Archiving the parent archives its subagent too.
    ctx.engine.archive(&lead).await.unwrap();
    // (The project's removal above announced the subagent gone once already.)
    for _ in 0..250 {
        if ctx.engine.snapshot().threads.is_empty() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    assert!(ctx.engine.snapshot().threads.is_empty(), "the subagent goes with its parent");
}

/// After Codex restarts on its own, each project is listed again with its
/// threads' subagents: one the engine was never told of is there, under its parent.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn after_codex_restarts_a_subagent_the_engine_had_not_seen_is_listed_under_its_parent() {
    let (engine, mut wires, mut wire, codex, folder) = super::fake::start_engine("restart-subagents", None).await;
    let cwd = folder.display().to_string();
    let lead = json!({"id": "lead", "status": {"type": "notLoaded"}, "cwd": cwd});
    let worker = json!({"id": "worker", "status": {"type": "notLoaded"}, "cwd": cwd, "parentThreadId": "lead", "agentNickname": "Hume",
        "source": {"subAgent": {"thread_spawn": {"agent_path": "/root/worker", "agent_nickname": "Hume", "depth": 1, "parent_thread_id": "lead"}}}});
    let page = |threads: Vec<&Value>| json!({"data": threads, "nextCursor": null});
    codex.hold("thread/list");
    let adding = tokio::spawn({
        let (engine, folder) = (engine.clone(), folder.clone());
        async move { engine.add_project(folder).await }
    });
    wire.answer("thread/list", page(vec![&lead])).await;
    wire.answer("thread/list", page(vec![])).await;
    adding.await.unwrap().expect("the project is added");
    assert_eq!(engine.snapshot().threads.len(), 1);

    wire.kill();
    let mut wire = tokio::time::timeout(std::time::Duration::from_secs(5), wires.recv()).await.unwrap().expect("Codex is started again");
    wire.answer("thread/list", page(vec![&lead])).await;
    let frame = wire.next().await;
    assert_eq!((frame["method"].as_str(), frame["params"]["ancestorThreadId"].as_str()), (Some("thread/list"), Some("lead")), "{frame}");
    wire.send(json!({"id": frame["id"], "result": page(vec![&worker])}));
    for _ in 0..250 {
        if let Some(found) = engine.snapshot().threads.into_iter().find(|t| t.id.0 == "worker") {
            let expected = Subagent { parent: ThreadId("lead".into()), path: "/root/worker".into(), nickname: Some("Hume".into()) };
            assert_eq!(found.subagent, Some(expected));
            engine.shutdown().await;
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    panic!("the subagent is not in the snapshot");
}

/// Live: a subagent read the moment the engine takes it in (before Codex has
/// kept anything of it, or as it does), with the events after its number
/// applied, is item for item what Codex keeps once it has finished.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_subagent_read_at_once_is_whole_with_its_events() {
    if !super::harness::codex_present() {
        eprintln!("skipped: CHIRALITY_TEST_CODEX names the pinned Codex");
        return;
    }
    let mut ctx = Ctx::live("subagent_first_read", false).await;
    let project = ctx.engine.add_project(ctx.run.project()).await.unwrap();
    for key in ["a", "b", "c"] {
        let (lead, worker) = (format!("lead{key}"), format!("worker{key}"));
        ctx.script(&lead, steps("lead", vec![spawn("call_spawn", "worker", &format!("STUB[{worker}] go")), wait("call_wait")], "Lead done."));
        ctx.script(&worker, steps("worker", vec![shell_call("call_ls", "ls", false)], "Worker done."));
        let mut events = ctx.engine.subscribe();
        let thread = ctx.engine.start_thread(&project.id, text(&format!("STUB[{lead}] go")), settings(Access::FullAccess)).await.unwrap();
        let child = new_subagent(&mut ctx, "/root/worker").await.id;
        let first = ctx.engine.read_thread(&child).await.expect("the new subagent reads");
        assert_eq!(ctx.turn_ended(&thread).await.1, TurnState::Completed);
        let mut seen = Vec::new();
        while let Ok(n) = events.try_recv() {
            seen.push(n);
        }
        let kept = super::history_live::kept(&ctx.engine.read_thread(&child).await.unwrap());
        assert!(kept.len() >= 2, "{kept:?}");
        assert_eq!(super::history_live::rebuilt(&first, &seen), kept, "run {key}");
    }
    ctx.finish("subagent_first_read").await;
}
