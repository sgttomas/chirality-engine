//! The look at an ended turn for its report, against the scripted Codex: a
//! delivery waiting for the thread starts after it, and never waits on a look
//! that will not end (a turn end Codex never announced, or Codex stopping);
//! and reports to one thread come in the order their turns ended, whichever
//! thread they come from.

use std::time::Duration;

use serde_json::{Value, json};

use super::assignment_paths::{delivered_after_loading, engine_with, id, mark, note, post_from, quiet_for, thread, turn, until};
use crate::Engine;
use crate::api::*;
use crate::assignment::LOOK_LIMIT;
use crate::assignment::delivery::Posted;
use crate::assignment::marker;

fn idle(thread: &str) -> Value {
    json!({"method": "thread/status/changed", "params": {"threadId": thread, "status": {"type": "idle"}}})
}

/// A turn as Codex keeps it: opened by `from`'s message, with `reply` its last.
fn kept(turn_id: &str, from: &str, reply: &str) -> Value {
    let items = json!([
        {"type": "userMessage", "id": "u", "content": [{"type": "text", "text": marker::render(&mark(from, None))}, {"type": "text", "text": "Go."}]},
        {"type": "agentMessage", "id": "r", "text": reply}
    ]);
    turn(turn_id, "completed", items)
}

fn looks(engine: &Engine) -> usize {
    engine.inner().assignments.looks()
}

/// Delivers a's message to b as turn `turn_id`, and has Codex tell of it.
async fn deliver_to_b(engine: &Engine, wire: &mut super::fake::Wire, turn_id: &str) {
    let posting = post_from(engine, "b", "Go.", Access::Ask);
    delivered_after_loading(wire, turn_id, "workspaceWrite").await;
    assert_eq!(posting.await.unwrap(), Posted::Now);
    wire.send(note("turn/started", "b", turn_id, "inProgress"));
}

/// The next `turn/start`, answered as `turn_id`, within `within`.
async fn start_within(wire: &mut super::fake::Wire, within: Duration, turn_id: &str) -> Value {
    let frame = tokio::time::timeout(within, wire.next()).await.expect("a start in time");
    assert_eq!(frame["method"], "turn/start", "{frame}");
    wire.send(json!({"id": frame["id"], "result": {"turn": turn(turn_id, "inProgress", json!([]))}}));
    frame
}

/// Codex says b is idle and never announces its turn's end: what waits for b goes once the look is
/// past its time, and is gone; the next turn's look ends with its report and holds nothing after.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_turn_end_never_announced_holds_deliveries_only_for_a_while_and_not_after() {
    let (engine, _wires, mut wire, _) = engine_with("look-missed", vec![thread("a", "", "lead"), thread("b", "", "worker")]).await;
    deliver_to_b(&engine, &mut wire, "t1").await;
    wire.send(idle("b"));
    until("the look to begin", || looks(&engine) == 1).await;
    let waiting = post_from(&engine, "b", "And this.", Access::Ask);
    assert_eq!(waiting.await.unwrap(), Posted::Later, "it waits for the look");
    quiet_for(&mut wire, 2).await;
    start_within(&mut wire, LOOK_LIMIT + Duration::from_secs(2), "t2").await;
    assert_eq!(looks(&engine), 0, "the look past its time is gone");

    // A turn whose end is announced: its look ends with its report, and holds nothing after.
    wire.send(note("turn/started", "b", "t2", "inProgress"));
    wire.send(idle("b"));
    wire.send(note("turn/completed", "b", "t2", "completed"));
    wire.answer("thread/turns/list", json!({"data": [kept("t2", "a", "Done.")], "nextCursor": null})).await;
    // The report goes to a: loaded, then started.
    wire.answer("thread/resume", json!({"thread": {"id": "a", "status": {"type": "idle"}, "cwd": "/"}, "sandbox": {"type": "workspaceWrite"}})).await;
    wire.answer("thread/turns/list", json!({"data": [], "nextCursor": null})).await;
    let report = start_within(&mut wire, Duration::from_secs(2), "r1").await;
    assert_eq!(report["params"]["threadId"], "a");
    until("the look to end", || looks(&engine) == 0).await;
    let next = post_from(&engine, "b", "Once more.", Access::Ask);
    let frame = start_within(&mut wire, Duration::from_secs(1), "t3").await;
    assert_eq!((frame["params"]["threadId"].as_str(), next.await.unwrap()), (Some("b"), Posted::Now));
    engine.shutdown().await;
}

/// Codex stops while a turn is looked at: the look is over, and a delivery to the thread goes as
/// soon as Codex is back.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn codex_stopping_mid_look_holds_nothing() {
    let (engine, mut wires, mut wire, cwd) = engine_with("look-death", vec![thread("a", "", "lead"), thread("b", "", "worker")]).await;
    deliver_to_b(&engine, &mut wire, "t1").await;
    wire.send(idle("b"));
    until("the look to begin", || looks(&engine) == 1).await;
    wire.kill();
    until("the look to be over", || looks(&engine) == 0).await;
    let mut wire = tokio::time::timeout(Duration::from_secs(5), wires.recv()).await.unwrap().expect("Codex is started again");
    // Listed again once back: the project's threads, then each one's subagents.
    for _ in 0..3 {
        let frame = wire.next().await;
        assert_eq!(frame["method"], "thread/list", "{frame}");
        let listed = [thread("a", &cwd, "lead"), thread("b", &cwd, "worker")];
        let threads = if frame["params"]["ancestorThreadId"].is_string() { vec![] } else { listed.to_vec() };
        wire.send(json!({"id": frame["id"], "result": {"data": threads, "nextCursor": null}}));
    }
    let posting = post_from(&engine, "b", "Go on.", Access::Ask);
    let started = tokio::time::timeout(Duration::from_secs(1), delivered_after_loading(&mut wire, "t2", "workspaceWrite")).await.expect("not held by the look");
    assert_eq!((started["params"]["threadId"].as_str(), posting.await.unwrap()), (Some("b"), Posted::Now));
    engine.shutdown().await;
}

/// Two threads a assigned end turns close together: b's report, whose look reads a's history for a
/// close, comes to a before c's, which ended after it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reports_to_one_thread_come_in_the_order_their_turns_ended() {
    let brief = format!("{}Check.", marker::render(&mark("a", None)));
    // Listed with a model, so a report has settings to run with.
    let listed = |id: &str, preview: &str| {
        let mut t = thread(id, "", preview);
        t["model"] = json!("gpt-6.2-sol");
        t
    };
    let (engine, _wires, mut wire, _) = engine_with("look-order", vec![listed("a", "lead"), listed("b", &brief), listed("c", &brief)]).await;
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
