//! A recorded scenario: a new thread's history before Codex has kept it, and
//! threads Codex does not have.

use serde_json::json;

use super::harness::{Ctx, settings, text};
use super::stub::says;
use crate::api::*;
use crate::protocol as p;

/// A thread started without its first message, as the engine starts one.
pub(super) async fn codex_thread(ctx: &Ctx, project: &Project) -> ThreadId {
    let session = ctx.engine.inner().session().unwrap();
    let params = json!({ "cwd": project.path.display().to_string(), "model": "gpt-6.2-sol" });
    let started: p::ThreadResponse = session.call("thread/start", params).await.expect("Codex starts the thread");
    let thread = ctx.engine.inner().adopt_thread(&started.thread, None).expect("the thread is in the project");
    session.mark_loaded(&thread);
    session.mark_fresh(&thread);
    thread
}

pub(super) async fn reply(ctx: &mut Ctx, thread: &ThreadId, key: &str) -> TurnId {
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

fn listed(ctx: &Ctx, thread: &ThreadId) -> bool {
    ctx.engine.snapshot().threads.iter().any(|t| &t.id == thread)
}

/// A new thread's history before Codex has kept its first message: the read
/// waits a while for it, then says so; after the message, it reads. A thread
/// whose first message is refused is not listed, and one Codex does not have
/// after it restarts is not found.
pub async fn unkept_thread(ctx: &mut Ctx) {
    let project = ctx.engine.add_project(ctx.run.project()).await.unwrap();
    let thread = codex_thread(ctx, &project).await;
    let early = ctx.engine.read_thread(&thread).await.unwrap_err();
    assert!(matches!(early, EngineError::NotReady { .. }), "{early:?}");
    reply(ctx, &thread, "one").await;
    assert_eq!(ctx.read_thread(&thread).await.turns.len(), 1);

    let message = Message { workflow: Some(WorkflowId("no-such-workflow".into())), ..text("go") };
    let before: Vec<ThreadId> = ctx.engine.snapshot().threads.into_iter().map(|t| t.id).collect();
    ctx.engine.start_thread(&project.id, message, settings(Access::Ask)).await.unwrap_err();
    let after: Vec<ThreadId> = ctx.engine.snapshot().threads.into_iter().map(|t| t.id).collect();
    assert_eq!(after, before, "the refused thread is not listed");

    let (read, sent) = (codex_thread(ctx, &project).await, codex_thread(ctx, &project).await);
    restart_codex(ctx).await;
    let e = ctx.engine.read_thread(&read).await.unwrap_err();
    assert!(matches!(e, EngineError::NotFound { .. }) && !listed(ctx, &read), "{e:?}");
    let e = ctx.engine.send(&sent, text("STUB[none] go"), settings(Access::Ask)).await.unwrap_err();
    assert!(matches!(e, EngineError::NotFound { .. }) && !listed(ctx, &sent), "{e:?}");
    assert!(listed(ctx, &thread));
}
