//! Reports, against the scripted Codex: reports to one thread come in the
//! order their turns ended, whichever thread they come from; a look at history
//! that does not end holds only the reports to the same thread; a report owed
//! whose look fails (late, or its turn too far back) is said in its turn and
//! changes nothing, and one not owed says nothing; only the look has a
//! deadline; a turn whose next one started first still reports; and a report
//! looked at while its thread was gone from the engine is not made.

use std::time::Duration;

use serde_json::{Value, json};

use super::assignment_paths::{add_listing, delivered_after_loading, engine_with, id, mark, note, post_from, quiet_for, thread, turn, until};
use super::harness::settings;
use crate::Engine;
use crate::api::*;
use crate::assignment::delivery::{Delivery, Posted};
use crate::assignment::delivery::REPORT_LIMIT;
use crate::assignment::marker;

/// A turn as Codex keeps it: opened by `from`'s message, with `reply` its last.
fn kept(turn_id: &str, from: &str, reply: &str) -> Value {
    let items = json!([
        {"type": "userMessage", "id": "u", "content": [{"type": "text", "text": marker::render(&mark(from, None))}, {"type": "text", "text": "Go."}]},
        {"type": "agentMessage", "id": "r", "text": reply}
    ]);
    turn(turn_id, "completed", items)
}

/// A thread as Codex lists it, with a model so a report has settings to run with.
fn listed(id: &str, preview: &str) -> Value {
    let mut t = thread(id, "", preview);
    t["model"] = json!("gpt-6.2-sol");
    t
}

/// A brief from `from`.
fn brief(from: &str) -> String {
    format!("{}Check.", marker::render(&mark(from, None)))
}

/// The next `turn/start`, answered as `turn_id`, within `within`.
async fn start_within(wire: &mut super::fake::Wire, within: Duration, turn_id: &str) -> Value {
    let frame = tokio::time::timeout(within, wire.next()).await.expect("a start in time");
    assert_eq!(frame["method"], "turn/start", "{frame}");
    wire.send(json!({"id": frame["id"], "result": {"turn": turn(turn_id, "inProgress", json!([]))}}));
    frame
}

fn reports(engine: &Engine) -> usize {
    engine.inner().assignments.reports.load(std::sync::atomic::Ordering::SeqCst)
}

/// Two threads a assigned end turns close together: b's report, whose look reads a's history for a
/// close, comes to a before c's, which ended after it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reports_to_one_thread_come_in_the_order_their_turns_ended() {
    let (engine, _wires, mut wire, _) = engine_with("order", vec![listed("a", "lead"), listed("b", &brief("a")), listed("c", &brief("a"))]).await;
    wire.send(note("turn/completed", "b", "tb", "completed"));
    wire.send(note("turn/completed", "c", "tc", "completed"));
    wire.answer("thread/turns/list", json!({"data": [kept("tb", "a", "B done.")], "nextCursor": null})).await;
    // b's look reads a's history for a close, and is answered late: c's look waits for it.
    let close = wire.next().await;
    assert_eq!((close["method"].as_str(), close["params"]["threadId"].as_str(), close["params"]["sortDirection"].as_str()), (Some("thread/turns/list"), Some("a"), Some("asc")), "{close}");
    quiet_for(&mut wire, 2).await;
    wire.send(json!({"id": close["id"], "result": {"data": [], "nextCursor": null}}));
    wire.answer("thread/resume", json!({"thread": {"id": "a", "status": {"type": "idle"}, "cwd": "/"}, "sandbox": {"type": "workspaceWrite"}})).await;
    wire.answer("thread/turns/list", json!({"data": [], "nextCursor": null})).await;
    let first = start_within(&mut wire, Duration::from_secs(2), "r1").await;
    assert_eq!(first["params"]["input"][1]["text"], "B done.", "{first}");
    wire.answer("thread/turns/list", json!({"data": [kept("tc", "a", "C done.")], "nextCursor": null})).await;
    wire.answer("thread/turns/list", json!({"data": [], "nextCursor": null})).await;
    until("c's report to wait for a's turn", || engine.inner().assignments.deliveries.waiting(&id("a")) == 1).await;
    engine.shutdown().await;
    // Nothing is kept after shutdown.
    until("the reports to be over", || engine.inner().assignments.held() == (0, 0, 0)).await;
}

/// b's look at a's history gets no answer: c's report to d, which ended after it, is made all the same.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_look_with_no_answer_holds_no_other_threads_reports() {
    let threads = vec![listed("a", "lead"), listed("d", "other lead"), listed("b", &brief("a")), listed("c", &brief("d"))];
    let (engine, _wires, mut wire, _) = engine_with("order-apart", threads).await;
    wire.send(note("turn/completed", "b", "tb", "completed"));
    wire.send(note("turn/completed", "c", "tc", "completed"));
    let mut held = None;
    let started = loop {
        let frame = wire.next().await;
        let answer = |result: Value| json!({"id": frame["id"], "result": result});
        let on = frame["params"]["threadId"].as_str().unwrap_or_default().to_string();
        match (frame["method"].as_str(), on.as_str()) {
            (Some("thread/turns/list"), "b") => wire.send(answer(json!({"data": [kept("tb", "a", "B done.")], "nextCursor": null}))),
            (Some("thread/turns/list"), "c") => wire.send(answer(json!({"data": [kept("tc", "d", "C done.")], "nextCursor": null}))),
            (Some("thread/turns/list"), "a") => held = Some(frame),
            (Some("thread/turns/list"), "d") => wire.send(answer(json!({"data": [], "nextCursor": null}))),
            (Some("thread/resume"), _) => wire.send(answer(json!({"thread": {"id": on, "status": {"type": "idle"}, "cwd": "/"}, "sandbox": {"type": "workspaceWrite"}}))),
            (Some("turn/start"), _) => break frame,
            _ => panic!("unexpected {frame}"),
        }
    };
    assert_eq!((started["params"]["threadId"].as_str(), started["params"]["input"][1]["text"].as_str()), (Some("d"), Some("C done.")), "{started}");
    assert!(held.is_some(), "b's look is still waiting");
    engine.shutdown().await;
}

/// a's message to b, delivered as turn `turn_id`, which Codex says started.
async fn deliver_to_b(engine: &Engine, wire: &mut super::fake::Wire, turn_id: &str) {
    let posting = post_from(engine, "b", "Go.", Access::Ask);
    delivered_after_loading(wire, turn_id, "workspaceWrite").await;
    assert_eq!(posting.await.unwrap(), Posted::Now);
    wire.send(note("turn/started", "b", turn_id, "inProgress"));
}

/// The notices said in b's turns, as items.
fn notices(events: &mut tokio::sync::broadcast::Receiver<Numbered>) -> Vec<(String, String)> {
    let mut found = Vec::new();
    while let Ok(event) = events.try_recv() {
        if let Event::Item { thread, turn, item: Item { body: ItemBody::Error { message }, .. } } = event.event
            && thread.0 == "b"
        {
            found.push((turn.0, message));
        }
    }
    found
}

/// b's turn ends and is not found: its look fails one way or another.
async fn b_turn_not_found(engine: &Engine, wire: &mut super::fake::Wire, how: &str) {
    match how {
        // No answer, past the look's time.
        "late" => {
            let unanswered = wire.next().await;
            assert_eq!(unanswered["params"]["threadId"], "b", "{unanswered}");
            tokio::time::sleep(REPORT_LIMIT + Duration::from_millis(300)).await;
        }
        // More than a report looks back: ten pages of other turns.
        _ => {
            for page in 0..10 {
                let frame = wire.next().await;
                assert_eq!(frame["params"]["threadId"], "b", "{frame}");
                let others: Vec<Value> = (0..10).map(|n| kept(&format!("o{page}-{n}"), "a", "Other.")).collect();
                wire.send(json!({"id": frame["id"], "result": {"data": others, "nextCursor": format!("p{page}")}}));
            }
        }
    }
    until("the report to be looked at", || reports(engine) == 1).await;
}

/// A report owed (the engine started b's turn with a's message) whose look fails, by time or by the turn
/// not being found: said in b's turn, which, and b, are left as they were.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_report_owed_and_not_made_is_said_in_its_turn_and_changes_nothing() {
    for how in ["late", "far"] {
        let (engine, _wires, mut wire, _) = engine_with(&format!("order-unmade-{how}"), vec![listed("a", "lead"), listed("b", "worker")]).await;
        deliver_to_b(&engine, &mut wire, "t1").await;
        let mut events = engine.subscribe();
        wire.send(note("turn/completed", "b", "t1", "completed"));
        b_turn_not_found(&engine, &mut wire, how).await;
        let said = notices(&mut events);
        assert!(said.len() == 1 && said[0].0 == "t1" && said[0].1.contains("not reported back"), "{how}: {said:?}");
        let b = engine.inner().store.thread(&id("b")).unwrap();
        assert!(b.failed.is_none(), "{how}: {:?}", b.failed);
        assert_eq!(engine.inner().store.summary(&id("b")).unwrap().status, ThreadStatus::Idle, "{how}");
        quiet_for(&mut wire, 1).await;
        engine.shutdown().await;
    }
}

/// An assigned thread's turn the person started owes no report: its look failing says nothing.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_turn_that_owed_no_report_says_nothing_when_its_look_fails() {
    let (engine, _wires, mut wire, _) = engine_with("order-unowed", vec![listed("a", "lead"), listed("b", &brief("a"))]).await;
    let mut events = engine.subscribe();
    wire.send(note("turn/completed", "b", "t1", "completed"));
    b_turn_not_found(&engine, &mut wire, "late").await;
    assert_eq!(notices(&mut events), vec![]);
    assert!(engine.inner().store.thread(&id("b")).unwrap().failed.is_none());
    engine.shutdown().await;
}

/// The recipient's resume takes longer than a look may: the report, found in time, is still made, and
/// a message after it to the same thread is delivered once its turn ends.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn only_the_look_has_a_deadline_not_the_delivery() {
    let (engine, _wires, mut wire, _) = engine_with("order-slow-start", vec![listed("a", "lead"), listed("b", &brief("a"))]).await;
    wire.send(note("turn/completed", "b", "tb", "completed"));
    wire.answer("thread/turns/list", json!({"data": [kept("tb", "a", "B done.")], "nextCursor": null})).await;
    wire.answer("thread/turns/list", json!({"data": [], "nextCursor": null})).await;
    let resume = wire.next().await;
    assert_eq!(resume["method"], "thread/resume", "{resume}");
    tokio::time::sleep(REPORT_LIMIT + Duration::from_millis(300)).await;
    wire.send(json!({"id": resume["id"], "result": {"thread": {"id": "a", "status": {"type": "idle"}, "cwd": "/"}, "sandbox": {"type": "workspaceWrite"}}}));
    wire.answer("thread/turns/list", json!({"data": [], "nextCursor": null})).await;
    let report = start_within(&mut wire, Duration::from_secs(2), "r1").await;
    assert_eq!(report["params"]["input"][1]["text"], "B done.", "{report}");
    wire.send(note("turn/started", "a", "r1", "inProgress"));
    // A message to a while its report's turn runs waits, and goes when that turn ends.
    let message = engine.inner().post(id("a"), Delivery::new(mark("b", None), "And this.".into(), Access::Ask, settings(Access::Ask)));
    assert_eq!(message.await, Posted::Later);
    wire.send(json!({"method": "thread/status/changed", "params": {"threadId": "a", "status": {"type": "idle"}}}));
    wire.send(note("turn/completed", "a", "r1", "completed"));
    let next = start_within(&mut wire, Duration::from_secs(3), "r2").await;
    assert_eq!((next["params"]["threadId"].as_str(), next["params"]["input"][1]["text"].as_str()), (Some("a"), Some("And this.")), "{next}");
    engine.shutdown().await;
}

/// Codex answers the start of b's next turn before it tells of the first one's end: the first still
/// reports to a.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_turn_started_before_the_last_ones_end_is_told_leaves_its_report() {
    let (engine, _wires, mut wire, _) = engine_with("order-overlap", vec![listed("a", "lead"), listed("b", "worker")]).await;
    deliver_to_b(&engine, &mut wire, "t1").await;
    let second = post_from(&engine, "b", "Next.", Access::Ask);
    assert_eq!(second.await.unwrap(), Posted::Later);
    wire.send(json!({"method": "thread/status/changed", "params": {"threadId": "b", "status": {"type": "idle"}}}));
    start_within(&mut wire, Duration::from_secs(3), "t2").await;
    // The answer is taken before the first turn's end is told.
    tokio::time::sleep(Duration::from_millis(200)).await;
    wire.send(note("turn/completed", "b", "t1", "completed"));
    wire.answer("thread/turns/list", json!({"data": [kept("t1", "a", "First done.")], "nextCursor": null})).await;
    wire.answer("thread/resume", json!({"thread": {"id": "a", "status": {"type": "idle"}, "cwd": "/"}, "sandbox": {"type": "workspaceWrite"}})).await;
    wire.answer("thread/turns/list", json!({"data": [], "nextCursor": null})).await;
    let report = start_within(&mut wire, Duration::from_secs(2), "r1").await;
    assert_eq!((report["params"]["threadId"].as_str(), report["params"]["input"][1]["text"].as_str()), (Some("a"), Some("First done.")), "{report}");
    engine.shutdown().await;
}

/// The project is removed while b's report is looked at, and added back before the look ends: the
/// report is not made.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_report_looked_at_across_a_removal_is_not_made() {
    let threads = vec![listed("a", "lead"), listed("b", &brief("a"))];
    let (engine, _wires, mut wire, folder) = engine_with("order-removed", threads.clone()).await;
    wire.send(note("turn/completed", "b", "tb", "completed"));
    let look = wire.next().await;
    assert_eq!(look["params"]["threadId"], "b", "{look}");
    let project = engine.inner().store.projects()[0].id.clone();
    engine.remove_project(&project).await.unwrap();
    add_listing(&engine, &mut wire, std::path::Path::new(&folder), threads).await;
    wire.send(json!({"id": look["id"], "result": {"data": [kept("tb", "a", "B done.")], "nextCursor": null}}));
    wire.answer("thread/turns/list", json!({"data": [], "nextCursor": null})).await;
    until("the report to be looked at", || reports(&engine) == 1).await;
    quiet_for(&mut wire, 2).await;
    assert_eq!(engine.inner().assignments.deliveries.waiting_anywhere(), 0);
    // Nothing kept of the report, or of the threads that went.
    until("the report to be over", || engine.inner().assignments.held() == (0, 0, 0)).await;
    engine.shutdown().await;
}
