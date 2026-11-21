//! A thread the engine has just started, and threads Codex refuses, against
//! the scripted Codex with Codex 0.171.0's own answers (`unkept_thread`
//! recorded the first): a read of a new thread that fails before Codex has
//! kept it is an empty view numbered from before the thread started; for
//! another thread a refusal is Codex's; a read does not put back a thread
//! archived while it was asked.

use std::path::Path;

use serde_json::{Value, json};

use super::fake::{Wire, start_engine};
use crate::Engine;
use crate::api::*;

const NOT_KEPT: &str = "thread t is not materialized yet; thread/turns/list is unavailable before first user message";
const T: fn() -> ThreadId = || ThreadId("t".into());

fn thread(cwd: &Path) -> Value {
    json!({"thread": {"id": "t", "forkedFromId": null, "parentThreadId": null, "preview": "", "model": "m", "reasoningEffort": null,
           "updatedAt": 1, "status": {"type": "idle"}, "cwd": cwd.display().to_string(), "name": null, "turns": []}})
}

fn message(workflow: Option<&str>) -> Message {
    Message { text: "go".into(), attachments: vec![], workflow: workflow.map(|w| WorkflowId(w.into())) }
}

fn settings() -> TurnSettings {
    TurnSettings { model: "m".into(), effort: None, access: Access::Ask }
}

/// An engine with thread `t` in its project; with `started`, as the engine
/// starts one, after the number it returns.
async fn with_thread(name: &str, started: bool) -> (Engine, Wire, std::path::PathBuf, u64) {
    let (engine, _wires, mut wire, _codex, project) = start_engine(name, None).await;
    let adding = tokio::spawn({
        let (engine, project) = (engine.clone(), project.clone());
        async move { engine.add_project(project).await }
    });
    wire.answer("thread/list", json!({"data": [thread(&project)["thread"]], "nextCursor": null})).await;
    adding.await.unwrap().unwrap();
    let before = engine.snapshot().seq;
    if started {
        let session = engine.inner().session().unwrap();
        session.mark_loaded(&T());
        session.mark_unkept(&T(), before);
    }
    (engine, wire, project, before)
}

fn read(engine: &Engine) -> tokio::task::JoinHandle<Result<ThreadView>> {
    let engine = engine.clone();
    tokio::spawn(async move { engine.read_thread(&T()).await })
}

/// Answers the engine's next request, which must be `method`, with Codex's error.
async fn refuse(wire: &mut Wire, method: &str, code: i64, message: &str) {
    let frame = wire.next().await;
    assert_eq!(frame["method"], method, "{frame}");
    wire.send(json!({"id": frame["id"], "error": {"code": code, "message": message}}));
}

fn listed(engine: &Engine) -> bool {
    engine.snapshot().threads.iter().any(|t| t.id == T())
}

/// Finding 5: text streamed before the read is after the view's number.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_new_threads_first_read_holds_nothing_and_is_numbered_from_its_start() {
    for (code, answer) in [(-32600, NOT_KEPT), (-32601, "list_turns is not supported yet")] {
        let (engine, mut wire, project, before) = with_thread("unkept-empty", true).await;
        let mut events = engine.subscribe();
        wire.send(json!({"method": "turn/started", "params": {"threadId": "t", "turn": {"id": "turn-1", "items": [], "status": "inProgress", "error": null}}}));
        wire.send(json!({"method": "item/agentMessage/delta", "params": {"threadId": "t", "turnId": "turn-1", "itemId": "msg-1", "delta": "First half."}}));
        let streamed = loop {
            let n = events.recv().await.unwrap();
            if matches!(n.event, Event::ItemDelta { .. }) {
                break n.seq;
            }
        };
        let reading = read(&engine);
        wire.answer("thread/read", thread(&project)).await;
        refuse(&mut wire, "thread/turns/list", code, answer).await;
        let view = reading.await.unwrap().unwrap();
        assert!(view.turns.is_empty() && view.seq == before && view.seq < streamed, "{view:?}, streamed at {streamed}");
        engine.shutdown().await;
    }
}

/// Whatever Codex answers for a new thread before it is kept (its metadata request failing on an
/// empty rollout, or another refusal of its first page), the read is the empty view, at once.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_new_threads_failed_read_is_the_empty_view() {
    let (engine, mut wire, _, before) = with_thread("unkept-metadata", true).await;
    let reading = read(&engine);
    refuse(&mut wire, "thread/read", -32603, "failed to read thread: empty rollout file").await;
    let view = reading.await.unwrap().unwrap();
    assert!(view.turns.is_empty() && view.seq == before, "{view:?}");
    engine.shutdown().await;

    let (engine, mut wire, project, before) = with_thread("unkept-lineage", true).await;
    let reading = read(&engine);
    wire.answer("thread/read", thread(&project)).await;
    refuse(&mut wire, "thread/turns/list", -32600, "invalid paginated history lineage for thread t: missing source rollout").await;
    let view = reading.await.unwrap().unwrap();
    assert!(view.turns.is_empty() && view.seq == before, "{view:?}");
    wire.quiet().await;
    engine.shutdown().await;
}

/// The refusal before the first message is an empty view only for a thread the engine started.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_thread_not_just_started_gets_codexs_refusal() {
    let (engine, mut wire, project, _) = with_thread("unkept-not-started", false).await;
    let reading = read(&engine);
    wire.answer("thread/read", thread(&project)).await;
    refuse(&mut wire, "thread/turns/list", -32600, NOT_KEPT).await;
    assert_eq!(reading.await.unwrap().unwrap_err(), EngineError::Codex { message: NOT_KEPT.into() });
    engine.shutdown().await;
}

/// Finding 4: an archive that completes while a read is asked is not undone by the read.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_read_does_not_put_back_a_thread_archived_meanwhile() {
    let (engine, mut wire, project, _) = with_thread("unkept-archived", false).await;
    let reading = read(&engine);
    let asked = wire.next().await;
    assert_eq!(asked["method"], "thread/read");
    let archiving = tokio::spawn({
        let engine = engine.clone();
        async move { engine.archive(&T()).await }
    });
    wire.answer("thread/archive", json!({})).await;
    archiving.await.unwrap().unwrap();
    wire.send(json!({"id": asked["id"], "result": thread(&project)}));
    wire.answer("thread/turns/list", json!({"data": [], "nextCursor": null})).await;
    assert!(matches!(reading.await.unwrap(), Err(EngineError::NotFound { .. })));
    assert!(!listed(&engine));
    engine.shutdown().await;
}

/// Finding 3: Codex's refusal of a fork leaves the parent where it is.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_refused_fork_leaves_the_parent() {
    let (engine, mut wire, _, _) = with_thread("unkept-fork", true).await;
    let forking = tokio::spawn({
        let engine = engine.clone();
        async move { engine.fork(&T(), &TurnId("stale".into())).await }
    });
    refuse(&mut wire, "thread/fork", -32600, "no rollout found for thread id t").await;
    assert!(matches!(forking.await.unwrap(), Err(EngineError::Codex { .. })));
    assert!(listed(&engine));
    engine.shutdown().await;
}

/// Finding 8 and the check before `thread/start`: a first message that cannot be made makes no
/// thread, and the thread of a refused first send is not listed.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_first_message_that_cannot_go_leaves_no_thread() {
    let (engine, mut wire, project, _) = with_thread("unkept-first-send", false).await;
    let project_id = engine.snapshot().projects[0].id.clone();
    assert!(matches!(engine.start_thread(&project_id, message(Some("nope")), settings()).await, Err(EngineError::NotFound { .. })));
    wire.quiet().await;

    let starting = tokio::spawn({
        let (engine, project_id) = (engine.clone(), project_id.clone());
        async move { engine.start_thread(&project_id, message(None), settings()).await }
    });
    let start = wire.next().await;
    assert_eq!(start["method"], "thread/start");
    let mut made = thread(&project);
    made["thread"]["id"] = json!("u");
    wire.send(json!({"id": start["id"], "result": made}));
    refuse(&mut wire, "turn/start", -32600, "turn refused").await;
    assert!(starting.await.unwrap().is_err());
    assert!(engine.snapshot().threads.iter().all(|t| t.id.0 != "u"));
    engine.shutdown().await;
}
