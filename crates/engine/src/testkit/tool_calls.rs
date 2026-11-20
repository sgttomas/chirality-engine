//! The application's tool calls on the paths a real Codex cannot be made to
//! take on demand, against the scripted Codex: the same model call id on two
//! threads, a request sent again, handlers that panic, and calls cancelled by
//! archiving, Codex dying, shutdown, or the call's item ending first.

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
                _ => ToolOutput::text("quick"),
            }
        }
    };
    let tool = |name: &str| AppTool { name: name.into(), description: "A test tool.".into(), input_schema: json!({"type": "object"}) };
    AppTools {
        namespace: ToolNamespace { name: NS.into(), description: "Test tools.".into() },
        tools: ["hold", "quick", "panic_build", "panic_run"].into_iter().map(tool).collect(),
        handler: Arc::new(handler),
    }
}

async fn start(probe: &Arc<Probe>) -> (Engine, mpsc::UnboundedReceiver<Wire>, Wire) {
    let root = std::env::temp_dir().join(format!("chirality-tool-calls-{}-{}", std::process::id(), RUN.fetch_add(1, Ordering::SeqCst)));
    std::fs::create_dir_all(&root).unwrap();
    let config = EngineConfig {
        codex_bin: "codex".into(),
        codex_home: Some(root.join("home")),
        state_dir: root.join("state"),
        workflow_roots: Vec::new(),
        codex_settings: Vec::new(),
        tools: Some(tools(probe)),
    };
    let (connector, mut wires) = FakeCodex::new();
    let engine = Engine::start_with(config, connector, None).await.expect("the engine starts");
    let wire = wires.recv().await.expect("a connection");
    (engine, wires, wire)
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
