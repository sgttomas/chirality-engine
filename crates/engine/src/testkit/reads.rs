//! A thread read while its turn streams, and after a Stop cut it short: the
//! view holds the text so far, numbered so the events after it apply on top.

use std::sync::Arc;

use serde_json::json;

use super::fake::start_engine;
use super::harness::{self, Ctx};
use super::stub::Reply;
use crate::api::*;

fn texts(view: &ThreadView) -> Vec<String> {
    let turn = view.turns.last().unwrap();
    turn.items
        .iter()
        .map(|i| match &i.body {
            ItemBody::UserMessage { text, .. } => format!("user: {text}"),
            ItemBody::AgentMessage { text } => format!("agent: {text}"),
            other => format!("{other:?}"),
        })
        .collect()
}

/// Against the pinned Codex: a partial reply, a read, a Stop, a read.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_read_holds_the_text_streamed_so_far_and_after_a_stop() {
    if !harness::codex_present() {
        eprintln!("skipped: set CHIRALITY_TEST_CODEX to the pinned Codex");
        return;
    }
    let mut ctx = Ctx::live("partial-read", false).await;
    ctx.script("partial", Arc::new(|_| Reply::Partial("This is a partial reply.".into())));
    let project = ctx.engine.add_project(ctx.run.project()).await.unwrap();
    let thread = ctx.engine.start_thread(&project.id, harness::text("STUB[partial]"), harness::settings(Access::Ask)).await.unwrap();
    ctx.wait("the streamed text", |e| matches!(e, Event::ItemDelta { .. }).then_some(())).await;
    let streaming = ctx.engine.read_thread(&thread).await.unwrap();
    assert_eq!(texts(&streaming), ["user: STUB[partial]", "agent: This is a partial reply."]);
    assert_eq!(streaming.turns.last().unwrap().state, TurnState::Running);
    ctx.engine.interrupt(&thread).await.unwrap();
    ctx.turn_ended(&thread).await;
    let stopped = ctx.engine.read_thread(&thread).await.unwrap();
    assert_eq!(texts(&stopped), ["user: STUB[partial]", "agent: This is a partial reply."]);
    assert_eq!(stopped.turns.last().unwrap().state, TurnState::Interrupted);
    assert!(stopped.seq > streaming.seq);
    ctx.finish("partial-read").await;
}

/// Against the scripted Codex: history that leaves out an item still
/// streaming and one a Stop cut short; the read holds both, at the number of
/// the last event it covers.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_read_merges_the_turn_its_events_told() {
    let (engine, _wires, mut wire, _codex, project) = start_engine("read-merge", None).await;
    let thread = |turns: serde_json::Value| {
        json!({"id": "t", "forkedFromId": null, "parentThreadId": null, "preview": "", "model": "m", "reasoningEffort": null,
               "updatedAt": 1, "status": {"type": "idle"}, "cwd": project.display().to_string(), "name": "T", "turns": turns})
    };
    let adding = tokio::spawn({
        let (engine, project) = (engine.clone(), project.clone());
        async move { engine.add_project(project).await }
    });
    wire.answer("thread/list", json!({"data": [thread(json!([]))], "nextCursor": null})).await;
    adding.await.unwrap().unwrap();
    let mut events = engine.subscribe();
    wire.send(json!({"method": "turn/started", "params": {"threadId": "t", "turn": {"id": "turn-1", "items": [], "status": "inProgress", "error": null}}}));
    let user = json!({"type": "userMessage", "id": "u", "content": [{"type": "text", "text": "Go", "text_elements": []}]});
    wire.send(json!({"method": "item/completed", "params": {"threadId": "t", "turnId": "turn-1", "item": user}}));
    wire.send(json!({"method": "item/started", "params": {"threadId": "t", "turnId": "turn-1", "item": {"type": "agentMessage", "id": "a", "text": ""}}}));
    wire.send(json!({"method": "item/agentMessage/delta", "params": {"threadId": "t", "turnId": "turn-1", "itemId": "a", "delta": "Half"}}));
    wire.send(json!({"method": "item/agentMessage/delta", "params": {"threadId": "t", "turnId": "turn-1", "itemId": "a", "delta": " done"}}));
    let mut last = 0;
    while !matches!(events.recv().await.map(|n| { last = n.seq; n.event }), Ok(Event::ItemDelta { ref text, .. }) if text == " done") {}

    let reading = tokio::spawn({
        let engine = engine.clone();
        async move { engine.read_thread(&ThreadId("t".into())).await }
    });
    wire.answer("thread/read", json!({"thread": thread(json!([]))})).await;
    // Codex's history has the turn with the person's message only.
    let history = json!({"id": "turn-1", "items": [user], "status": "inProgress", "error": null});
    wire.answer("thread/turns/list", json!({"data": [history], "nextCursor": null})).await;
    let view = reading.await.unwrap().unwrap();
    assert_eq!(texts(&view), ["user: Go", "agent: Half done"]);
    assert!(view.seq >= last, "the view covers the events it holds");
    engine.shutdown().await;
}
