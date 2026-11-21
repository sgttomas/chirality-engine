//! The application's tool calls on the paths a real Codex cannot be made to
//! take on demand, against the scripted Codex: the same model call id on two
//! threads, a request sent again, handlers that panic, calls cancelled by
//! archiving (also with the call still queued behind the archive), Codex
//! dying, shutdown, or the call's item ending first, and shutdown overlapping
//! a restart.

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Value, json};
use tokio::sync::{Notify, mpsc};

use super::fake::{FakeCodex, Wire};
use crate::api::*;
use crate::Engine;

const NS: &str = "estimate";

#[derive(Default)]
struct Probe {
    calls: Mutex<Vec<ToolCall>>,
    /// `hold` answers once its thread's gate opens.
    gates: Mutex<HashMap<String, Arc<Notify>>>,
    dropped: AtomicUsize,
}

impl Probe {
    fn gate(&self, thread: &str) -> Arc<Notify> {
        self.gates.lock().unwrap().entry(thread.to_string()).or_default().clone()
    }

    fn called(&self) -> usize {
        self.calls.lock().unwrap().len()
    }

    async fn settle(&self, what: impl Fn(&Probe) -> bool) {
        for _ in 0..250 {
            if what(self) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        panic!("the handler did not get there");
    }
}

/// Counts a dropped future only after a while, as a handler cleaning up would.
struct SlowDrop(Arc<Probe>);

impl Drop for SlowDrop {
    fn drop(&mut self) {
        std::thread::sleep(Duration::from_millis(700));
        self.0.dropped.fetch_add(1, Ordering::SeqCst);
    }
}

struct Unfinished(Arc<Probe>);

impl Drop for Unfinished {
    fn drop(&mut self) {
        self.0.dropped.fetch_add(1, Ordering::SeqCst);
    }
}

fn tools(probe: &Arc<Probe>) -> AppTools {
    let probe = probe.clone();
    let handler = move |call: ToolCall| {
        if call.tool == "panic_build" {
            panic!("a handler that panics before its future");
        }
        let probe = probe.clone();
        async move {
            probe.calls.lock().unwrap().push(call.clone());
            match call.tool.as_str() {
                "hold" => {
                    let unfinished = Unfinished(probe.clone());
                    probe.gate(&call.thread.0).notified().await;
                    std::mem::forget(unfinished);
                    ToolOutput::text(format!("held {}", call.thread.0))
                }
                "panic_run" => panic!("a handler whose future panics"),
                "slow_drop" => {
                    let _slow = SlowDrop(probe.clone());
                    std::future::pending().await
                }
                _ => ToolOutput::text("quick"),
            }
        }
    };
    let tool = |name: &str| AppTool { name: name.into(), description: "A test tool.".into(), input_schema: json!({"type": "object"}) };
    AppTools {
        namespace: ToolNamespace { name: NS.into(), description: "Test tools.".into() },
        tools: ["hold", "quick", "panic_build", "panic_run", "slow_drop"].into_iter().map(tool).collect(),
        handler: Arc::new(handler),
    }
}

async fn start(probe: &Arc<Probe>) -> (Engine, mpsc::UnboundedReceiver<Wire>, Wire) {
    let (engine, wires, wire, _, _) = start_with(probe).await;
    (engine, wires, wire)
}

/// Also the scripted Codex itself, and a project folder the engine knows nothing of yet.
async fn start_with(probe: &Arc<Probe>) -> (Engine, mpsc::UnboundedReceiver<Wire>, Wire, Arc<FakeCodex>, std::path::PathBuf) {
    start_configured(Some(tools(probe))).await
}

async fn start_configured(tools: Option<AppTools>) -> (Engine, mpsc::UnboundedReceiver<Wire>, Wire, Arc<FakeCodex>, std::path::PathBuf) {
    let root = std::env::temp_dir().join(format!("chirality-tool-calls-{}-{}", std::process::id(), RUN.fetch_add(1, Ordering::SeqCst)));
    std::fs::create_dir_all(&root).unwrap();
    let config = EngineConfig {
        codex_bin: "codex".into(),
        codex_home: Some(root.join("home")),
        state_dir: root.join("state"),
        workflow_roots: Vec::new(),
        codex_settings: Vec::new(),
        tools,
    };
    let (connector, mut wires) = FakeCodex::new();
    let engine = Engine::start_with(config, connector.clone(), None).await.expect("the engine starts");
    let wire = wires.recv().await.expect("a connection");
    std::fs::create_dir_all(root.join("project")).unwrap();
    (engine, wires, wire, connector, root.join("project").canonicalize().unwrap())
}

static RUN: AtomicUsize = AtomicUsize::new(0);

fn call(id: i64, thread: &str, item: &str, tool: &str) -> Value {
    json!({"id": id, "method": "item/tool/call", "params": {"threadId": thread, "turnId": format!("turn-{thread}"), "callId": item, "namespace": NS, "tool": tool, "arguments": {}}})
}

fn item_completed(thread: &str, item: &str) -> Value {
    let item = json!({"type": "dynamicToolCall", "id": item, "namespace": NS, "tool": "hold", "arguments": {}, "status": "failed", "contentItems": [], "success": false});
    json!({"method": "item/completed", "params": {"threadId": thread, "turnId": format!("turn-{thread}"), "item": item}})
}

fn reply(id: i64, text: &str, success: bool) -> Value {
    json!({"id": id, "result": {"contentItems": [{"type": "inputText", "text": text}], "success": success}})
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_same_model_call_id_on_two_threads_is_two_calls() {
    let probe = Arc::new(Probe::default());
    let (engine, _wires, mut wire) = start(&probe).await;
    wire.send(call(1, "a", "call_x", "hold"));
    wire.send(call(2, "b", "call_x", "hold"));
    probe.settle(|p| p.called() == 2).await;
    // Codex ends a's call; b's, with the same call id, goes on and is answered.
    wire.send(item_completed("a", "call_x"));
    probe.settle(|p| p.dropped.load(Ordering::SeqCst) == 1).await;
    probe.gate("b").notify_one();
    assert_eq!(wire.next().await, reply(2, "held b", true));
    wire.quiet().await;
    assert_eq!(engine.inner().tool_calls.running(), 0);
    engine.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_request_sent_again_is_run_and_answered_once() {
    let probe = Arc::new(Probe::default());
    let (engine, _wires, mut wire) = start(&probe).await;
    wire.send(call(4, "a", "call_h", "hold"));
    wire.send(call(4, "a", "call_h", "hold"));
    probe.settle(|p| p.called() == 1).await;
    probe.gate("a").notify_one();
    assert_eq!(wire.next().await, reply(4, "held a", true));
    // Sent again after its answer (Codex replays requests still pending when a thread is resumed).
    wire.send(call(4, "a", "call_h", "hold"));
    wire.quiet().await;
    assert_eq!(probe.called(), 1);
    engine.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_handler_that_panics_answers_its_call_as_failed() {
    let probe = Arc::new(Probe::default());
    let (engine, _wires, mut wire) = start(&probe).await;
    wire.send(call(1, "a", "call_1", "panic_build"));
    assert_eq!(wire.next().await, reply(1, "The tool failed.", false));
    wire.send(call(2, "a", "call_2", "panic_run"));
    assert_eq!(wire.next().await, reply(2, "The tool failed.", false));
    assert_eq!(engine.inner().tool_calls.running(), 0);
    engine.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_tool_the_application_does_not_have_is_answered_as_failed() {
    let probe = Arc::new(Probe::default());
    let (engine, _wires, mut wire) = start(&probe).await;
    let mut other = call(1, "a", "call_1", "quick");
    other["params"]["namespace"] = json!("another_app");
    wire.send(other);
    assert_eq!(wire.next().await, reply(1, "This application has no tool another_app.quick.", false));
    assert_eq!(probe.called(), 0);
    engine.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn archiving_a_thread_cancels_its_calls() {
    let probe = Arc::new(Probe::default());
    let (engine, _wires, mut wire) = start(&probe).await;
    wire.send(call(1, "a", "call_a", "hold"));
    wire.send(call(2, "b", "call_b", "hold"));
    probe.settle(|p| p.called() == 2).await;
    wire.send(json!({"method": "thread/archived", "params": {"threadId": "a"}}));
    probe.settle(|p| p.dropped.load(Ordering::SeqCst) == 1).await;
    probe.gate("a").notify_one();
    probe.gate("b").notify_one();
    assert_eq!(wire.next().await, reply(2, "held b", true));
    wire.quiet().await;
    engine.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn codex_dying_cancels_its_calls_and_the_next_codex_takes_new_ones() {
    let probe = Arc::new(Probe::default());
    let (engine, mut wires, wire) = start(&probe).await;
    wire.send(call(1, "a", "call_a", "hold"));
    probe.settle(|p| p.called() == 1).await;
    wire.kill();
    probe.settle(|p| p.dropped.load(Ordering::SeqCst) == 1).await;
    let mut next = tokio::time::timeout(Duration::from_secs(5), wires.recv()).await.unwrap().expect("Codex is started again");
    probe.settle(|_| matches!(engine.snapshot().codex, CodexStatus::Ready { .. })).await;
    // The new process numbers its requests from the start again.
    next.send(call(1, "a", "call_a", "quick"));
    assert_eq!(next.next().await, reply(1, "quick", true));
    engine.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shutdown_returns_once_every_call_is_dropped() {
    let probe = Arc::new(Probe::default());
    let (engine, _wires, wire) = start(&probe).await;
    wire.send(call(1, "a", "call_a", "hold"));
    wire.send(call(2, "b", "call_b", "hold"));
    probe.settle(|p| p.called() == 2).await;
    engine.shutdown().await;
    assert_eq!(probe.dropped.load(Ordering::SeqCst), 2, "no handler outlives shutdown");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_call_cancelled_before_its_handler_finishes_is_not_answered() {
    let probe = Arc::new(Probe::default());
    let (engine, _wires, mut wire) = start(&probe).await;
    wire.send(call(1, "a", "call_a", "hold"));
    probe.settle(|p| p.called() == 1).await;
    // Codex ends the call first; the handler, free to answer after, is gone.
    wire.send(item_completed("a", "call_a"));
    probe.settle(|p| p.dropped.load(Ordering::SeqCst) == 1).await;
    probe.gate("a").notify_one();
    wire.quiet().await;
    // Completion and cancellation racing: each call is answered at most once.
    for id in 10..60 {
        wire.send(call(id, "r", &format!("call_{id}"), "quick"));
        wire.send(item_completed("r", &format!("call_{id}")));
    }
    let mut answered = Vec::new();
    while let Ok(Some(frame)) = tokio::time::timeout(Duration::from_millis(300), async { Some(wire.next().await) }).await {
        answered.push(frame["id"].as_i64().unwrap());
    }
    answered.sort();
    let mut unique = answered.clone();
    unique.dedup();
    assert_eq!(answered, unique, "a call answered twice");
    assert_eq!(engine.inner().tool_calls.running(), 0);
    engine.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_refused_request_sent_again_is_answered_once() {
    let probe = Arc::new(Probe::default());
    let (engine, _wires, mut wire) = start(&probe).await;
    let mut other = call(7, "a", "call_7", "quick");
    other["params"]["tool"] = json!("not_ours");
    wire.send(other.clone());
    wire.send(other);
    assert_eq!(wire.next().await, reply(7, "This application has no tool estimate.not_ours.", false));
    wire.quiet().await;
    engine.shutdown().await;
}

/// The reader gives Codex's answers to their callers at once but queues its
/// requests for the session loop, so a call can still be queued when archive returns.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_call_queued_behind_an_archive_is_refused() {
    let probe = Arc::new(Probe::default());
    let (engine, _wires, mut wire, codex, folder) = start_with(&probe).await;
    let adding = tokio::spawn({
        let engine = engine.clone();
        let folder = folder.clone();
        async move { engine.add_project(folder).await }
    });
    wire.answer("thread/list", json!({"data": [], "nextCursor": null})).await;
    adding.await.unwrap().expect("the project is added");
    wire.send(json!({"method": "thread/started", "params": {"thread": {"id": "t1", "status": {"type": "idle"}, "cwd": folder.display().to_string()}}}));
    probe.settle(|_| engine.snapshot().threads.iter().any(|t| t.id.0 == "t1")).await;
    // Hold the session loop on an account read, and queue the call behind it.
    codex.hold("account/read");
    wire.send(json!({"method": "account/updated", "params": {}}));
    let read = wire.next().await;
    assert_eq!(read["method"], "account/read");
    wire.send(call(1, "t1", "call_q", "quick"));
    let archiving = tokio::spawn({
        let engine = engine.clone();
        async move { engine.archive(&ThreadId("t1".into())).await }
    });
    wire.answer("thread/archive", json!({})).await;
    archiving.await.unwrap().expect("the thread is archived");
    wire.send(json!({"id": read["id"], "result": {"account": null}}));
    assert_eq!(wire.next().await, reply(1, "The thread is closed.", false));
    assert_eq!(probe.called(), 0, "no application work for an archived thread");
    engine.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 3)]
async fn shutdown_waits_for_the_calls_of_a_codex_that_died() {
    let probe = Arc::new(Probe::default());
    let (engine, mut wires, wire) = start(&probe).await;
    wire.send(call(1, "a", "call_a", "slow_drop"));
    probe.settle(|p| p.called() == 1).await;
    wire.kill();
    let _next = tokio::time::timeout(Duration::from_secs(5), wires.recv()).await.unwrap().expect("Codex is started again");
    probe.settle(|_| matches!(engine.snapshot().codex, CodexStatus::Ready { .. })).await;
    engine.shutdown().await;
    assert_eq!(probe.dropped.load(Ordering::SeqCst), 1, "the dead session's handler was still being dropped");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shutdowns_overlapping_a_restart_wait_for_it_and_end_its_codex() {
    let probe = Arc::new(Probe::default());
    let (engine, mut wires, wire, codex, _) = start_with(&probe).await;
    codex.hold("initialize");
    wire.kill();
    let mut next = tokio::time::timeout(Duration::from_secs(5), wires.recv()).await.unwrap().expect("Codex is started again");
    let init = next.next().await;
    assert_eq!(init["method"], "initialize");
    // Two callers shut down while the new Codex is still starting.
    let first = tokio::spawn({
        let engine = engine.clone();
        async move { engine.shutdown().await }
    });
    let second = tokio::spawn({
        let engine = engine.clone();
        async move { engine.shutdown().await }
    });
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(!first.is_finished() && !second.is_finished(), "shutdown returned while Codex was starting");
    next.send(json!({"id": init["id"], "result": {"userAgent": "chirality/0.171.0 (fake)"}}));
    first.await.unwrap();
    second.await.unwrap();
    assert!(next.is_ended(), "the Codex started during shutdown was ended");
    assert!(engine.inner().session().is_err(), "no session after shutdown");
}

/// Adds the project and has Codex report thread `id` in it.
async fn known_thread(engine: &Engine, wire: &mut Wire, folder: &std::path::Path, id: &str) {
    let adding = tokio::spawn({
        let (engine, folder) = (engine.clone(), folder.to_path_buf());
        async move { engine.add_project(folder).await }
    });
    wire.answer("thread/list", json!({"data": [], "nextCursor": null})).await;
    adding.await.unwrap().expect("the project is added");
    started(wire, folder, id);
    for _ in 0..250 {
        if engine.snapshot().threads.iter().any(|t| t.id.0 == id) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("the thread is not in the snapshot");
}

fn started(wire: &Wire, folder: &std::path::Path, id: &str) {
    wire.send(json!({"method": "thread/started", "params": {"thread": {"id": id, "status": {"type": "idle"}, "cwd": folder.display().to_string()}}}));
}

fn thread(folder: &std::path::Path, id: &str) -> Value {
    json!({"thread": {"id": id, "status": {"type": "idle"}, "cwd": folder.display().to_string()}})
}

/// Codex ending holds requests it delivered just before: none of them reaches the handler.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn calls_delivered_just_before_codex_dies_are_not_run() {
    let probe = Arc::new(Probe::default());
    let (engine, mut wires, mut wire, codex, _) = start_with(&probe).await;
    codex.hold("account/read");
    wire.send(json!({"method": "account/updated", "params": {}}));
    assert_eq!(wire.next().await["method"], "account/read");
    for id in 1..=32 {
        wire.send(call(id, "a", &format!("call_{id}"), "quick"));
    }
    wire.kill();
    let _next = tokio::time::timeout(Duration::from_secs(5), wires.recv()).await.unwrap().expect("Codex is started again");
    probe.settle(|_| matches!(engine.snapshot().codex, CodexStatus::Ready { .. })).await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(probe.called(), 0, "a call of a dead connection ran");
    engine.shutdown().await;
}

/// After archive, Codex no longer has the thread loaded: if it comes back
/// (unarchived elsewhere and listed again), the engine resumes it before a turn.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_archived_thread_that_comes_back_is_resumed_before_its_next_turn() {
    let probe = Arc::new(Probe::default());
    let (engine, _wires, mut wire, _, folder) = start_with(&probe).await;
    known_thread(&engine, &mut wire, &folder, "t1").await;
    let settings = TurnSettings { model: "m".into(), effort: None, access: Access::Ask };
    let message = || Message { text: "go".into(), attachments: vec![], workflow: None };
    let sending = tokio::spawn({
        let (engine, settings) = (engine.clone(), settings.clone());
        async move { engine.send(&ThreadId("t1".into()), message(), settings).await }
    });
    wire.answer("thread/resume", thread(&folder, "t1")).await;
    wire.answer("turn/start", json!({"turn": {"id": "u1", "status": "completed", "items": []}})).await;
    sending.await.unwrap().expect("the turn starts");
    let archiving = tokio::spawn({
        let engine = engine.clone();
        async move { engine.archive(&ThreadId("t1".into())).await }
    });
    wire.answer("thread/archive", json!({})).await;
    archiving.await.unwrap().expect("the thread is archived");
    wire.send(json!({"method": "thread/archived", "params": {"threadId": "t1"}}));
    wire.send(json!({"method": "thread/unarchived", "params": {"threadId": "t1"}}));
    started(&wire, &folder, "t1");
    probe.settle(|_| engine.snapshot().threads.iter().any(|t| t.id.0 == "t1")).await;
    let sending = tokio::spawn({
        let engine = engine.clone();
        async move { engine.send(&ThreadId("t1".into()), message(), settings).await }
    });
    wire.answer("thread/resume", thread(&folder, "t1")).await;
    wire.answer("turn/start", json!({"turn": {"id": "u2", "status": "inProgress", "items": []}})).await;
    sending.await.unwrap().expect("the turn starts");
    // The thread is live again: its calls reach the handler.
    wire.send(call(9, "t1", "call_9", "quick"));
    assert_eq!(wire.next().await, reply(9, "quick", true));
    engine.shutdown().await;
}

/// Codex can close a subagent's thread and wake it again under the same id.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_new_turn_on_a_closed_thread_takes_its_calls_again() {
    let probe = Arc::new(Probe::default());
    let (engine, _wires, mut wire) = start(&probe).await;
    wire.send(json!({"method": "thread/closed", "params": {"threadId": "child"}}));
    wire.send(call(1, "child", "call_1", "quick"));
    assert_eq!(wire.next().await, reply(1, "The thread is closed.", false));
    wire.send(json!({"method": "turn/started", "params": {"threadId": "child", "turn": {"id": "u2", "status": "inProgress", "items": []}}}));
    wire.send(call(2, "child", "call_2", "quick"));
    assert_eq!(wire.next().await, reply(2, "quick", true));
    engine.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn with_no_tools_a_call_sent_again_gets_one_error() {
    let (engine, _wires, mut wire, _, _) = start_configured(None).await;
    wire.send(call(3, "a", "call_3", "quick"));
    wire.send(call(3, "a", "call_3", "quick"));
    assert_eq!(wire.next().await, json!({"id": 3, "error": {"code": -32601, "message": "not supported by this client"}}));
    wire.quiet().await;
    engine.shutdown().await;
}

/// Codex ended with many calls still buffered between the transport and the
/// session loop, which goes on handling them as fast as it can: none runs.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn calls_still_buffered_when_the_transport_ends_never_run() {
    let probe = Arc::new(Probe::default());
    let (engine, mut wires, wire) = start(&probe).await;
    wire.end_with((1..=512).map(|id| call(id, "a", &format!("call_{id}"), "quick")).collect());
    let _next = tokio::time::timeout(Duration::from_secs(5), wires.recv()).await.unwrap().expect("Codex is started again");
    probe.settle(|_| matches!(engine.snapshot().codex, CodexStatus::Ready { .. })).await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(probe.called(), 0, "a call delivered after the transport ended ran");
    engine.shutdown().await;
}
