//! Threads in each of Codex's history formats, and a new thread's history
//! before Codex has kept it: recorded scenarios, and one live race.

use serde_json::json;

use super::harness::{Ctx, codex_present, settings, text};
use super::stub::{hangs, says};
use crate::api::*;
use crate::protocol as p;

/// A thread Codex starts in `mode`, adopted by the engine as one it started.
async fn codex_thread(ctx: &Ctx, project: &Project, mode: &str) -> ThreadId {
    let session = ctx.engine.inner().session().unwrap();
    let params = json!({ "cwd": project.path.display().to_string(), "model": "gpt-6.2-sol", "historyMode": mode });
    let started: p::ThreadResponse = session.call("thread/start", params).await.expect("Codex starts the thread");
    let thread = ctx.engine.inner().adopt_thread(&started.thread, None).expect("the thread is in the project");
    session.mark_loaded(&thread);
    thread
}

async fn reply(ctx: &mut Ctx, thread: &ThreadId, key: &str) -> TurnId {
    ctx.script(key, says(key, key));
    ctx.engine.send(thread, text(&format!("STUB[{key}] go")), settings(Access::Ask)).await.expect("the message is sent");
    let (turn, state) = ctx.turn_ended(thread).await;
    assert_eq!(state, TurnState::Completed);
    turn
}

async fn restart_codex(ctx: &mut Ctx) {
    ctx.kill_codex();
    ctx.wait("Codex to stop", |e| matches!(e, Event::Codex { status: CodexStatus::Starting }).then_some(())).await;
    ctx.wait("Codex to be back", |e| matches!(e, Event::Codex { status: CodexStatus::Ready { .. } }).then_some(())).await;
}

/// Every operation on a thread Codex keeps in its legacy format: each works,
/// but revert, which Codex refuses for such a thread before doing anything.
pub async fn legacy_thread(ctx: &mut Ctx) {
    let project = ctx.engine.add_project(ctx.run.project()).await.unwrap();
    let thread = codex_thread(ctx, &project, "legacy").await;
    let first = reply(ctx, &thread, "one").await;
    // As a person's older threads are when the application opens: not loaded.
    restart_codex(ctx).await;
    assert_eq!(ctx.read_thread(&thread).await.turns.len(), 1);
    let second = reply(ctx, &thread, "two").await;
    reply(ctx, &thread, "three").await;

    ctx.script("wait", hangs());
    ctx.engine.send(&thread, text("STUB[wait] take your time"), settings(Access::Ask)).await.unwrap();
    ctx.wait("the turn to start", |e| matches!(e, Event::TurnStarted { thread: t, .. } if t == &thread).then_some(())).await;
    ctx.engine.interrupt(&thread).await.unwrap();
    assert_eq!(ctx.turn_ended(&thread).await.1, TurnState::Interrupted);
    let states: Vec<TurnState> = ctx.read_thread(&thread).await.turns.into_iter().map(|t| t.state).collect();
    assert_eq!(states, vec![TurnState::Completed, TurnState::Completed, TurnState::Completed, TurnState::Interrupted]);

    let fork = ctx.engine.fork(&thread, &first).await.unwrap();
    assert_eq!(ctx.read_thread(&fork).await.turns.len(), 1);

    let refused = ctx.engine.revert(&thread, &second).await.unwrap_err();
    assert!(matches!(refused, EngineError::Unsupported { .. }), "{refused:?}");
    assert_eq!(ctx.read_thread(&thread).await.turns.len(), 4, "nothing was reverted");

    ctx.engine.rename(&thread, "Renamed".into()).await.unwrap();
    assert_eq!(ctx.read_thread(&thread).await.summary.title.as_deref(), Some("Renamed"));
    ctx.engine.archive(&thread).await.unwrap();
    assert!(ctx.engine.snapshot().threads.iter().all(|t| t.id != thread));
}

/// A new thread's history before Codex has kept its first message: the read
/// waits a while for it, then says so; after the message, it reads.
pub async fn unkept_thread(ctx: &mut Ctx) {
    let project = ctx.engine.add_project(ctx.run.project()).await.unwrap();
    let thread = codex_thread(ctx, &project, "paginated").await;
    let early = ctx.engine.read_thread(&thread).await.unwrap_err();
    assert!(matches!(early, EngineError::NotReady { .. }), "{early:?}");
    reply(ctx, &thread, "one").await;
    assert_eq!(ctx.read_thread(&thread).await.turns.len(), 1);
}

/// A read that reaches Codex before it has kept a new thread's first message,
/// as the interface's read of a thread it has just started can: it returns
/// that message's turn once Codex has kept it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_read_racing_a_first_message_waits_for_it() {
    if !codex_present() {
        eprintln!("skipped: CHIRALITY_TEST_CODEX names the pinned Codex");
        return;
    }
    let mut ctx = Ctx::live("first_message_race", false).await;
    let project = ctx.engine.add_project(ctx.run.project()).await.unwrap();
    let thread = codex_thread(&ctx, &project, "paginated").await;
    let reading = tokio::spawn({
        let (engine, thread) = (ctx.engine.clone(), thread.clone());
        async move { engine.read_thread(&thread).await }
    });
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    reply(&mut ctx, &thread, "one").await;
    let view = reading.await.unwrap().expect("the read waits for Codex to keep the message");
    assert_eq!(view.turns.len(), 1);
    ctx.finish("first_message_race").await;
}
