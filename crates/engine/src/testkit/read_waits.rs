//! A read of a thread Codex has not kept yet, against the scripted Codex with
//! Codex's own answers (as `unkept_thread` recorded them): the read waits on
//! a new thread's first page only, keeps its number across the waits, passes
//! any other refusal on at once, and stops when the thread goes.

use std::path::Path;

use serde_json::{Value, json};

use super::fake::{Wire, start_engine};
use crate::Engine;
use crate::api::*;

const NOT_KEPT: &str = "thread t is not materialized yet; thread/turns/list is unavailable before first user message";

fn thread(cwd: &Path) -> Value {
    json!({"thread": {"id": "t", "forkedFromId": null, "parentThreadId": null, "preview": "", "model": "m", "reasoningEffort": null,
           "updatedAt": 1, "status": {"type": "idle"}, "cwd": cwd.display().to_string(), "name": null, "turns": []}})
}

fn page(turns: Value) -> Value {
    json!({"data": turns, "nextCursor": null})
}

/// An engine with thread `t` in its project; `fresh` when the engine has just started it.
async fn with_thread(name: &str, fresh: bool) -> (Engine, Wire, std::path::PathBuf) {
    let (engine, _wires, mut wire, _codex, project) = start_engine(name, None).await;
    let adding = tokio::spawn({
        let (engine, project) = (engine.clone(), project.clone());
        async move { engine.add_project(project).await }
    });
    wire.answer("thread/list", json!({"data": [thread(&project)["thread"]], "nextCursor": null})).await;
    adding.await.unwrap().unwrap();
    if fresh {
        let session = engine.inner().session().unwrap();
        session.mark_loaded(&ThreadId("t".into()));
        session.mark_fresh(&ThreadId("t".into()));
    }
    (engine, wire, project)
}

fn read(engine: &Engine) -> tokio::task::JoinHandle<Result<ThreadView>> {
    let engine = engine.clone();
    tokio::spawn(async move { engine.read_thread(&ThreadId("t".into())).await })
}

/// Answers the next history request with Codex's error.
async fn refuse(wire: &mut Wire, code: i64, message: &str) {
    let list = wire.next().await;
    assert_eq!(list["method"], "thread/turns/list", "{list}");
    wire.send(json!({"id": list["id"], "error": {"code": code, "message": message}}));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_new_threads_unsupported_answer_is_waited_out() {
    let (engine, mut wire, project) = with_thread("waits-unsupported", true).await;
    let reading = read(&engine);
    wire.answer("thread/read", thread(&project)).await;
    refuse(&mut wire, -32601, "list_turns is not supported yet").await;
    wire.answer("thread/read", thread(&project)).await;
    wire.answer("thread/turns/list", page(json!([{"id": "turn-1", "items": [], "status": "completed", "error": null}]))).await;
    assert_eq!(reading.await.unwrap().unwrap().turns.len(), 1);
    // Read once, the thread is Codex's: a later refusal is not waited out.
    let again = read(&engine);
    wire.answer("thread/read", thread(&project)).await;
    refuse(&mut wire, -32601, "list_turns is not supported yet").await;
    assert!(matches!(again.await.unwrap(), Err(EngineError::Unsupported { .. })));
    engine.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_read_keeps_its_number_across_the_waits() {
    let (engine, mut wire, project) = with_thread("waits-number", true).await;
    let before = engine.snapshot().seq;
    let mut events = engine.subscribe();
    let reading = read(&engine);
    wire.answer("thread/read", thread(&project)).await;
    refuse(&mut wire, -32600, NOT_KEPT).await;
    // Streamed during the wait, and not in the history the read returns.
    wire.send(json!({"method": "turn/started", "params": {"threadId": "t", "turn": {"id": "turn-1", "items": [], "status": "inProgress", "error": null}}}));
    wire.send(json!({"method": "item/agentMessage/delta", "params": {"threadId": "t", "turnId": "turn-1", "itemId": "msg-1", "delta": "Hel"}}));
    let delta = loop {
        let n = events.recv().await.unwrap();
        if matches!(n.event, Event::ItemDelta { .. }) {
            break n.seq;
        }
    };
    wire.answer("thread/read", thread(&project)).await;
    wire.answer("thread/turns/list", page(json!([{"id": "turn-1", "items": [], "status": "inProgress", "error": null}]))).await;
    let view = reading.await.unwrap().unwrap();
    assert!(view.seq >= before && view.seq < delta, "the view ({}) holds the text streamed at {delta}", view.seq);
    engine.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_thread_not_just_started_is_not_waited_for() {
    let (engine, mut wire, project) = with_thread("waits-not-fresh", false).await;
    let reading = read(&engine);
    wire.answer("thread/read", thread(&project)).await;
    let lineage = "invalid paginated history lineage for thread t: missing source rollout";
    refuse(&mut wire, -32600, lineage).await;
    assert_eq!(reading.await.unwrap().unwrap_err(), EngineError::Codex { message: lineage.into() });
    engine.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_thread_archived_during_the_wait_is_not_found() {
    let (engine, mut wire, project) = with_thread("waits-archived", true).await;
    let reading = read(&engine);
    wire.answer("thread/read", thread(&project)).await;
    refuse(&mut wire, -32600, NOT_KEPT).await;
    let archiving = tokio::spawn({
        let engine = engine.clone();
        async move { engine.archive(&ThreadId("t".into())).await }
    });
    wire.answer("thread/archive", json!({})).await;
    archiving.await.unwrap().unwrap();
    assert!(matches!(reading.await.unwrap(), Err(EngineError::NotFound { .. })));
    wire.quiet().await;
    engine.shutdown().await;
}
