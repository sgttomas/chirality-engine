//! Deliveries on the paths a real Codex cannot be made to take on demand,
//! against the scripted Codex: a Stop Codex never settles, access lowered
//! while a delivery waits, a thread archived with a report waiting for its
//! assigner, reports of turns that end close together, a turn Codex refuses,
//! and a start whose answer is lost when Codex stops.

use std::time::Duration;

use serde_json::{Value, json};

use super::fake::{Wire, start_engine};
use super::harness::{settings, text};
use crate::Engine;
use crate::api::*;
use crate::assignment::delivery::{Delivery, Posted};
use crate::assignment::marker::{self, Marker};

fn thread(id: &str, cwd: &str, preview: &str) -> Value {
    json!({"id": id, "status": {"type": "idle"}, "cwd": cwd, "preview": preview})
}

fn turn(id: &str, status: &str, items: Value) -> Value {
    json!({"id": id, "status": status, "error": null, "items": items})
}

fn note(method: &str, thread: &str, turn_id: &str, status: &str) -> Value {
    json!({"method": method, "params": {"threadId": thread, "turn": turn(turn_id, status, json!([]))}})
}

fn mark(from: &str, report: Option<TurnEnd>) -> Marker {
    Marker { from: ThreadId(from.into()), title: None, report }
}

fn id(s: &str) -> ThreadId {
    ThreadId(s.into())
}

/// The engine with one project listing `threads`; a marked thread is read to confirm it.
async fn engine_with(name: &str, threads: Vec<Value>) -> (Engine, tokio::sync::mpsc::UnboundedReceiver<Wire>, Wire, String) {
    let (engine, wires, mut wire, codex, folder) = start_engine(name, None).await;
    codex.hold("thread/list");
    let cwd = folder.display().to_string();
    let threads: Vec<Value> = threads.into_iter().map(|mut t| {
        t["cwd"] = json!(cwd);
        t
    }).collect();
    let mut adding = tokio::spawn({
        let (engine, folder) = (engine.clone(), folder.clone());
        async move { engine.add_project(folder).await }
    });
    wire.answer("thread/list", json!({"data": threads, "nextCursor": null})).await;
    let added = loop {
        tokio::select! {
            added = &mut adding => break added,
            frame = wire.next() => match frame["method"].as_str() {
                Some("thread/read") => {
                    let t = threads.iter().find(|t| t["id"] == frame["params"]["threadId"]).unwrap().clone();
                    wire.send(json!({"id": frame["id"], "result": {"thread": t}}));
                }
                // A thread's subagents, which these have none of.
                Some("thread/list") => wire.send(json!({"id": frame["id"], "result": {"data": [], "nextCursor": null}})),
                _ => panic!("unexpected {frame}"),
            },
        }
    };
    added.unwrap().expect("the project is added");
    (engine, wires, wire, cwd)
}

/// Starts a turn the person sends, as the engine sends it: resumed first when not loaded.
async fn person_turn(engine: &Engine, wire: &mut Wire, thread: &str, turn_id: &str, access: Access, resumed: Option<Value>) {
    let sending = tokio::spawn({
        let engine = engine.clone();
        let thread = id(thread);
        async move { engine.send(&thread, text("go"), settings(access)).await }
    });
    if let Some(sandbox) = resumed {
        wire.answer("thread/resume", json!({"thread": {"id": thread, "status": {"type": "idle"}, "cwd": "/"}, "sandbox": sandbox})).await;
    }
    wire.answer("turn/start", json!({"turn": turn(turn_id, "inProgress", json!([]))})).await;
    sending.await.unwrap().expect("the turn starts");
    wire.send(note("turn/started", thread, turn_id, "inProgress"));
}

/// The next `turn/start` the engine sends, answered as `turn_id`.
async fn delivered(wire: &mut Wire, turn_id: &str) -> Value {
    let frame = wire.next().await;
    assert_eq!(frame["method"], "turn/start", "{frame}");
    wire.send(json!({"id": frame["id"], "result": {"turn": turn(turn_id, "inProgress", json!([]))}}));
    frame
}

async fn quiet_for(wire: &mut Wire, times: usize) {
    for _ in 0..times {
        wire.quiet().await;
    }
}

async fn until(what: &str, done: impl Fn() -> bool) {
    for _ in 0..250 {
        if done() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("timed out waiting for {what}");
}

/// A Stop Codex never settles leaves the turn running: a waiting message is
/// not delivered (it would steer the turn), until Codex ends it. And access the
/// person lowered while it waited is the access its turn runs with.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_delivery_waits_out_an_unsettled_stop_and_takes_the_access_lowered_meanwhile() {
    let (engine, _wires, mut wire, _) = engine_with("deliver-stop", vec![thread("a", "", "lead"), thread("b", "", "worker")]).await;
    person_turn(&engine, &mut wire, "b", "t1", Access::FullAccess, Some(json!({"type": "dangerFullAccess"}))).await;
    let inner = engine.inner();
    let posted = inner.post(id("b"), Delivery::new(mark("a", None), "And this.".into(), Access::FullAccess, settings(Access::FullAccess))).await;
    assert_eq!(posted, Posted::Later);

    // Stop is sent, answered, and the turn never ends.
    let stopping = tokio::spawn({
        let engine = engine.clone();
        async move { engine.interrupt(&id("b")).await }
    });
    wire.answer("turn/interrupt", json!({})).await;
    assert!(stopping.await.unwrap().is_err(), "the Stop is reported as not taken");
    quiet_for(&mut wire, 4).await;

    // The person sends with less access: a start (the turn may have ended unannounced), which Codex takes as a new turn.
    person_turn(&engine, &mut wire, "b", "t2", Access::ReadOnly, None).await;
    wire.send(note("turn/completed", "b", "t1", "interrupted"));
    quiet_for(&mut wire, 2).await;
    wire.send(note("turn/completed", "b", "t2", "completed"));
    let frame = delivered(&mut wire, "t3").await;
    assert_eq!(frame["params"]["sandboxPolicy"]["type"], "readOnly", "{frame}");
    assert_eq!(frame["params"]["input"][0]["text"], marker::render(&mark("a", None)));
    engine.shutdown().await;
}

/// After a restart nothing is known of a thread's access: a delivered turn has
/// no more than the sender's, and no more than Codex resumed the thread with.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_delivery_to_a_thread_whose_access_is_not_known_is_bounded() {
    let (engine, _wires, mut wire, _) = engine_with("deliver-unknown", vec![thread("a", "", "lead"), thread("b", "", "worker")]).await;
    let summary = engine.snapshot().threads.into_iter().find(|t| t.id.0 == "b").unwrap();
    assert_eq!(summary.settings, None, "a listing without a model says nothing of settings");
    let posting = tokio::spawn({
        let inner = engine.clone();
        async move { inner.inner().post(id("b"), Delivery::new(mark("a", None), "Go.".into(), Access::FullAccess, settings(Access::FullAccess))).await }
    });
    wire.answer("thread/resume", json!({"thread": {"id": "b", "status": {"type": "idle"}, "cwd": "/"}, "sandbox": {"type": "readOnly"}})).await;
    let frame = delivered(&mut wire, "t1").await;
    assert_eq!(posting.await.unwrap(), Posted::Now);
    assert_eq!(frame["params"]["sandboxPolicy"]["type"], "readOnly", "{frame}");
    engine.shutdown().await;
}

/// A report waiting for its assigner is not made once the assigned thread is archived.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn archiving_the_assigned_thread_drops_its_waiting_report() {
    let brief = format!("{}Check the piles.", marker::render(&mark("a", None)));
    let (engine, _wires, mut wire, _) = engine_with("deliver-archive", vec![thread("a", "", "lead"), thread("b", "", &brief)]).await;
    assert_eq!(engine.snapshot().threads.iter().find(|t| t.id.0 == "b").unwrap().assigned_by, Some(id("a")));
    person_turn(&engine, &mut wire, "a", "t1", Access::Ask, Some(json!({"type": "workspaceWrite"}))).await;
    let report = Delivery::new(mark("b", Some(TurnEnd::Completed)), "Done.".into(), Access::Ask, settings(Access::Ask));
    assert_eq!(engine.inner().post(id("a"), report).await, Posted::Later);
    let archiving = tokio::spawn({
        let engine = engine.clone();
        async move { engine.archive(&id("b")).await }
    });
    wire.answer("thread/archive", json!({})).await;
    archiving.await.unwrap().unwrap();
    assert_eq!(engine.inner().assignments.deliveries.waiting(&id("a")), 0);
    wire.send(note("turn/completed", "a", "t1", "completed"));
    quiet_for(&mut wire, 3).await;
    engine.shutdown().await;
}

/// Two turns of an assigned thread that end close together are looked at in
/// the order they ended: the second waits for the first.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reports_are_looked_at_in_the_order_their_turns_ended() {
    let brief = format!("{}Check the piles.", marker::render(&mark("a", None)));
    let (engine, _wires, mut wire, _) = engine_with("deliver-order", vec![thread("a", "", "lead"), thread("b", "", &brief)]).await;
    person_turn(&engine, &mut wire, "b", "t1", Access::Ask, Some(json!({"type": "workspaceWrite"}))).await;
    wire.send(note("turn/completed", "b", "t1", "completed"));
    let first = wire.next().await;
    assert_eq!((first["method"].as_str(), first["params"]["threadId"].as_str()), (Some("thread/turns/list"), Some("b")), "{first}");
    // A second turn ends before the first's history is read: its look waits.
    person_turn(&engine, &mut wire, "b", "t2", Access::Ask, None).await;
    wire.send(note("turn/completed", "b", "t2", "completed"));
    quiet_for(&mut wire, 2).await;
    wire.send(json!({"id": first["id"], "result": {"data": [turn("t1", "completed", json!([{"type": "userMessage", "id": "u", "content": [{"type": "text", "text": "go"}]}]))], "nextCursor": null}}));
    let second = wire.next().await;
    assert_eq!(second["method"], "thread/turns/list", "{second}");
    engine.shutdown().await;
}

/// A delivery Codex refuses after it waited is told on the receiving thread.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_refused_delivery_is_told_on_the_thread() {
    let (engine, _wires, mut wire, _) = engine_with("deliver-refused", vec![thread("a", "", "lead"), thread("b", "", "worker")]).await;
    person_turn(&engine, &mut wire, "b", "t1", Access::Ask, Some(json!({"type": "workspaceWrite"}))).await;
    let posted = engine.inner().post(id("b"), Delivery::new(mark("a", None), "Go.".into(), Access::Ask, settings(Access::Ask))).await;
    assert_eq!(posted, Posted::Later);
    wire.send(note("turn/completed", "b", "t1", "completed"));
    let frame = wire.next().await;
    assert_eq!(frame["method"], "turn/start", "{frame}");
    wire.send(json!({"id": frame["id"], "error": {"code": -32600, "message": "the model is not available"}}));
    until("the thread to say so", || matches!(engine.snapshot().threads.iter().find(|t| t.id.0 == "b").unwrap().status, ThreadStatus::Failed { .. })).await;
    let ThreadStatus::Failed { message } = engine.snapshot().threads.into_iter().find(|t| t.id.0 == "b").unwrap().status else { unreachable!() };
    assert!(message.contains("the model is not available"), "{message}");
    engine.shutdown().await;
}

/// Codex stops after a delivery's start is sent and before it answers: once
/// back, the receiving thread's history shows the turn began, so it is not sent again.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_start_whose_answer_was_lost_is_not_sent_again_when_history_has_it() {
    let (engine, mut wires, mut wire, cwd) = engine_with("deliver-lost", vec![thread("a", "", "lead"), thread("b", "", "worker")]).await;
    person_turn(&engine, &mut wire, "b", "t1", Access::Ask, Some(json!({"type": "workspaceWrite"}))).await;
    let delivery = Delivery::new(mark("a", None), "Go.".into(), Access::Ask, settings(Access::Ask));
    assert_eq!(engine.inner().post(id("b"), delivery).await, Posted::Later);
    wire.send(note("turn/completed", "b", "t1", "completed"));
    let frame = wire.next().await;
    assert_eq!(frame["method"], "turn/start", "{frame}");
    wire.kill();
    let mut wire = tokio::time::timeout(Duration::from_secs(5), wires.recv()).await.unwrap().expect("Codex is started again");
    let kept = turn("t2", "completed", json!([{"type": "userMessage", "id": "u", "content": [{"type": "text", "text": marker::render(&mark("a", None))}, {"type": "text", "text": "Go."}]}]));
    let mut looked = false;
    let end = tokio::time::Instant::now() + Duration::from_secs(3);
    while tokio::time::Instant::now() < end {
        let Ok(frame) = tokio::time::timeout(Duration::from_millis(600), wire.next()).await else { continue };
        match frame["method"].as_str() {
            Some("thread/list") => {
                let listed = [thread("a", &cwd, "lead"), thread("b", &cwd, "worker")];
                let threads = if frame["params"]["ancestorThreadId"].is_string() { vec![] } else { listed.to_vec() };
                wire.send(json!({"id": frame["id"], "result": {"data": threads, "nextCursor": null}}));
            }
            Some("thread/turns/list") => {
                looked = true;
                wire.send(json!({"id": frame["id"], "result": {"data": [kept.clone()], "nextCursor": null}}));
            }
            _ => panic!("sent again, or more: {frame}"),
        }
    }
    assert!(looked, "the history was read before trying again");
    assert_eq!(engine.inner().assignments.deliveries.waiting(&id("b")), 0);
    engine.shutdown().await;
}
