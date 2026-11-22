//! Reports, against the scripted Codex: reports to one thread come in the
//! order their turns ended, whichever thread they come from; a look at history
//! that does not end holds only the reports to the same thread, and is given
//! up in time with the reason shown on the reporting thread; and a report
//! looked at while its thread was gone from the engine is not made.

use std::time::Duration;

use serde_json::{Value, json};

use super::assignment_paths::{add_listing, engine_with, id, mark, note, quiet_for, thread, turn, until};
use crate::Engine;
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

/// b's look gets no answer: past its time the report is given up, b says why, and c's report to the
/// same thread, which waited for it, is made.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_look_past_its_time_is_given_up_and_said() {
    let (engine, _wires, mut wire, _) = engine_with("order-late", vec![listed("a", "lead"), listed("b", &brief("a")), listed("c", &brief("a"))]).await;
    wire.send(note("turn/completed", "b", "tb", "completed"));
    wire.send(note("turn/completed", "c", "tc", "completed"));
    let unanswered = wire.next().await;
    assert_eq!(unanswered["params"]["threadId"], "b", "{unanswered}");
    let given_up = tokio::time::timeout(REPORT_LIMIT + Duration::from_secs(2), wire.next()).await.expect("c's look after b's time");
    assert_eq!((given_up["method"].as_str(), given_up["params"]["threadId"].as_str()), (Some("thread/turns/list"), Some("c")), "{given_up}");
    let failed = engine.inner().store.thread(&id("b")).and_then(|e| e.failed);
    assert!(failed.as_deref().is_some_and(|f| f.contains("not reported back")), "{failed:?}");
    wire.send(json!({"id": given_up["id"], "result": {"data": [kept("tc", "a", "C done.")], "nextCursor": null}}));
    wire.answer("thread/turns/list", json!({"data": [], "nextCursor": null})).await;
    wire.answer("thread/resume", json!({"thread": {"id": "a", "status": {"type": "idle"}, "cwd": "/"}, "sandbox": {"type": "workspaceWrite"}})).await;
    wire.answer("thread/turns/list", json!({"data": [], "nextCursor": null})).await;
    let report = start_within(&mut wire, Duration::from_secs(2), "r1").await;
    assert_eq!(report["params"]["input"][1]["text"], "C done.", "{report}");
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
    engine.shutdown().await;
}
