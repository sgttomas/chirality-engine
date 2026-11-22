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

pub(super) fn thread(id: &str, cwd: &str, preview: &str) -> Value {
    json!({"id": id, "status": {"type": "idle"}, "cwd": cwd, "preview": preview})
}

pub(super) fn turn(id: &str, status: &str, items: Value) -> Value {
    json!({"id": id, "status": status, "error": null, "items": items})
}

pub(super) fn note(method: &str, thread: &str, turn_id: &str, status: &str) -> Value {
    json!({"method": method, "params": {"threadId": thread, "turn": turn(turn_id, status, json!([]))}})
}

pub(super) fn mark(from: &str, report: Option<TurnEnd>) -> Marker {
    Marker { from: ThreadId(from.into()), title: None, report }
}

pub(super) fn id(s: &str) -> ThreadId {
    ThreadId(s.into())
}

/// The engine with one project listing `threads`; a marked thread is read to confirm it.
pub(super) async fn engine_with(name: &str, threads: Vec<Value>) -> (Engine, tokio::sync::mpsc::UnboundedReceiver<Wire>, Wire, String) {
    let (engine, wires, mut wire, codex, folder) = start_engine(name, None).await;
    codex.hold("thread/list");
    add_listing(&engine, &mut wire, &folder, threads).await;
    (engine, wires, wire, folder.display().to_string())
}

/// Adds the project at `folder`, which Codex lists with `threads`.
pub(super) async fn add_listing(engine: &Engine, wire: &mut Wire, folder: &std::path::Path, threads: Vec<Value>) -> Project {
    let cwd = folder.display().to_string();
    let threads: Vec<Value> = threads.into_iter().map(|mut t| {
        t["cwd"] = json!(cwd);
        t
    }).collect();
    let mut adding = tokio::spawn({
        let (engine, folder) = (engine.clone(), folder.to_path_buf());
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
    added.unwrap().expect("the project is added")
}

/// Starts a turn the person sends, as the engine sends it: resumed first when not loaded.
pub(super) async fn person_turn(engine: &Engine, wire: &mut Wire, thread: &str, turn_id: &str, access: Access, resumed: Option<Value>) {
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
pub(super) async fn delivered(wire: &mut Wire, turn_id: &str) -> Value {
    let frame = wire.next().await;
    assert_eq!(frame["method"], "turn/start", "{frame}");
    wire.send(json!({"id": frame["id"], "result": {"turn": turn(turn_id, "inProgress", json!([]))}}));
    frame
}

/// Answers what the engine asks before a delivered turn starts (a resume, at
/// `sandbox`; a look at the thread's latest turn, of which it has none), then
/// the `turn/start`, as `turn_id`.
pub(super) async fn delivered_after_loading(wire: &mut Wire, turn_id: &str, sandbox: &str) -> Value {
    loop {
        let frame = wire.next().await;
        let answer = |result: Value| json!({"id": frame["id"], "result": result});
        match frame["method"].as_str() {
            Some("thread/resume") => wire.send(answer(json!({"thread": {"id": frame["params"]["threadId"], "status": {"type": "idle"}, "cwd": "/"}, "sandbox": {"type": sandbox}}))),
            Some("thread/turns/list") => wire.send(answer(json!({"data": [], "nextCursor": null}))),
            Some("turn/start") => {
                wire.send(answer(json!({"turn": turn(turn_id, "inProgress", json!([]))})));
                return frame;
            }
            _ => panic!("unexpected {frame}"),
        }
    }
}

pub(super) fn post_from(engine: &Engine, to: &str, text: &str, access: Access) -> tokio::task::JoinHandle<Posted> {
    let (engine, to, text) = (engine.clone(), id(to), text.to_string());
    tokio::spawn(async move { engine.inner().post(to, Delivery::new(mark("a", None), text, access, settings(access))).await })
}

pub(super) async fn quiet_for(wire: &mut Wire, times: usize) {
    for _ in 0..times {
        wire.quiet().await;
    }
}

pub(super) async fn until(what: &str, done: impl Fn() -> bool) {
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
    let posting = post_from(&engine, "b", "Go.", Access::FullAccess);
    let frame = delivered_after_loading(&mut wire, "t1", "readOnly").await;
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

/// A thread forked after a restart, and the thread it was forked from, are
/// loaded at the access Codex gives each: a delivery from an Ask thread does
/// not raise a read-only one.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_fork_after_a_restart_keeps_the_access_codex_loaded_it_with() {
    let (engine, _wires, mut wire, cwd) = engine_with("deliver-fork", vec![thread("a", "", "lead"), thread("b", "", "worker")]).await;
    let forking = tokio::spawn({
        let engine = engine.clone();
        async move { engine.fork(&id("b"), &TurnId("t0".into())).await }
    });
    wire.answer("thread/resume", json!({"thread": {"id": "b", "status": {"type": "idle"}, "cwd": cwd}, "sandbox": {"type": "readOnly"}})).await;
    wire.answer("thread/fork", json!({"thread": {"id": "c", "status": {"type": "idle"}, "cwd": cwd, "preview": "worker"}, "sandbox": {"type": "readOnly"}})).await;
    assert_eq!(forking.await.unwrap().expect("the thread is forked"), id("c"));
    for (thread, turn_id) in [("b", "t1"), ("c", "t2")] {
        let posting = post_from(&engine, thread, "Go.", Access::Ask);
        let frame = delivered_after_loading(&mut wire, turn_id, "workspaceWrite").await;
        assert_eq!(posting.await.unwrap(), Posted::Now);
        assert_eq!((frame["params"]["threadId"].as_str(), frame["params"]["sandboxPolicy"]["type"].as_str()), (Some(thread), Some("readOnly")), "{frame}");
    }
    engine.shutdown().await;
}

/// A project removed and added back is taken in afresh: its threads are
/// resumed again, and a delivery from an Ask thread does not raise a read-only one.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_project_added_back_loads_its_threads_afresh() {
    let threads = || vec![thread("a", "", "lead"), thread("b", "", "worker")];
    let (engine, _wires, mut wire, cwd) = engine_with("deliver-readd", threads()).await;
    person_turn(&engine, &mut wire, "b", "t1", Access::ReadOnly, Some(json!({"type": "readOnly"}))).await;
    wire.send(note("turn/completed", "b", "t1", "completed"));
    until("the turn to end", || engine.snapshot().threads.iter().any(|t| t.id.0 == "b" && t.status == ThreadStatus::Idle)).await;
    let project = engine.snapshot().projects[0].id.clone();
    engine.remove_project(&project).await.unwrap();
    add_listing(&engine, &mut wire, std::path::Path::new(&cwd), threads()).await;
    let posting = post_from(&engine, "b", "Go.", Access::Ask);
    let frame = delivered_after_loading(&mut wire, "t2", "readOnly").await;
    assert_eq!(posting.await.unwrap(), Posted::Now);
    assert_eq!(frame["params"]["sandboxPolicy"]["type"], "readOnly", "{frame}");
    engine.shutdown().await;
}

/// Codex stops before a delivery's start is sent: it is sent once Codex is
/// back, though an earlier turn opened with the same message.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_start_never_sent_is_sent_when_codex_is_back_though_its_text_repeats() {
    let (engine, mut wires, mut wire, cwd) = engine_with("deliver-unsent", vec![thread("a", "", "lead"), thread("b", "", "worker")]).await;
    let posting = post_from(&engine, "b", "Go.", Access::Ask);
    delivered_after_loading(&mut wire, "t1", "workspaceWrite").await;
    assert_eq!(posting.await.unwrap(), Posted::Now);
    wire.send(note("turn/started", "b", "t1", "inProgress"));
    wire.send(note("turn/completed", "b", "t1", "completed"));
    // The engine started that turn with a's message: it looks at it for its report (none here).
    wire.answer("thread/turns/list", json!({"data": [], "nextCursor": null})).await;
    // Codex unloads the thread: the next delivery resumes it first, and Codex stops then.
    wire.send(json!({"method": "thread/closed", "params": {"threadId": "b"}}));
    quiet_for(&mut wire, 2).await;
    let posting = post_from(&engine, "b", "Go.", Access::Ask);
    let frame = wire.next().await;
    assert_eq!(frame["method"], "thread/resume", "{frame}");
    wire.kill();
    assert_eq!(posting.await.unwrap(), Posted::Later);
    let mut wire = tokio::time::timeout(Duration::from_secs(5), wires.recv()).await.unwrap().expect("Codex is started again");
    let first = turn("t1", "completed", json!([{"type": "userMessage", "id": "u", "content": [{"type": "text", "text": marker::render(&mark("a", None))}, {"type": "text", "text": "Go."}]}]));
    let end = tokio::time::Instant::now() + Duration::from_secs(4);
    let started = loop {
        assert!(tokio::time::Instant::now() < end, "the delivery was not sent");
        let Ok(frame) = tokio::time::timeout(Duration::from_millis(600), wire.next()).await else { continue };
        let answer = |result: Value| json!({"id": frame["id"], "result": result});
        match frame["method"].as_str() {
            Some("thread/list") => {
                let listed = [thread("a", &cwd, "lead"), thread("b", &cwd, "worker")];
                let threads = if frame["params"]["ancestorThreadId"].is_string() { vec![] } else { listed.to_vec() };
                wire.send(answer(json!({"data": threads, "nextCursor": null})));
            }
            Some("thread/resume") => wire.send(answer(json!({"thread": {"id": "b", "status": {"type": "idle"}, "cwd": cwd}, "sandbox": {"type": "workspaceWrite"}}))),
            Some("thread/turns/list") => wire.send(answer(json!({"data": [first.clone()], "nextCursor": null}))),
            Some("turn/start") => {
                wire.send(answer(json!({"turn": turn("t2", "inProgress", json!([]))})));
                break frame;
            }
            _ => panic!("unexpected {frame}"),
        }
    };
    assert_eq!(started["params"]["input"][1]["text"], "Go.", "{started}");
    until("the delivery to be taken out", || engine.inner().assignments.deliveries.waiting(&id("b")) == 0).await;
    engine.shutdown().await;
}
