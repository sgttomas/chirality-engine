//! A recorded scenario: a new thread read before Codex has kept its first
//! message (by the person, and by another thread's agent), and after.

use serde_json::json;

use super::harness::{Ctx, settings, text};
use super::stub::says;
use crate::api::*;
use crate::assignment::read::{ReadArgs, View};
use crate::protocol as p;

/// A thread started without its first message, as the engine starts one.
pub(super) async fn codex_thread(ctx: &Ctx, project: &Project) -> (ThreadId, u64) {
    let session = ctx.engine.inner().session().unwrap();
    let before = ctx.engine.snapshot().seq;
    let params = json!({ "cwd": project.path.display().to_string(), "model": "gpt-6.2-sol" });
    let started: p::ThreadResponse = session.call("thread/start", params).await.expect("Codex starts the thread");
    let thread = ctx.engine.inner().adopt_thread(&started.thread, None).expect("the thread is in the project");
    session.mark_loaded(&thread, None);
    session.mark_unkept(&thread, before);
    (thread, before)
}

pub(super) async fn reply(ctx: &mut Ctx, thread: &ThreadId, key: &str) {
    ctx.script(key, says(key, &format!("reply {key}")));
    ctx.engine.send(thread, text(&format!("STUB[{key}] go")), settings(Access::Ask)).await.expect("the message is sent");
    assert_eq!(ctx.turn_ended(thread).await.1, TurnState::Completed);
}

/// Codex refuses the history of a thread it has not kept yet: the read is an
/// empty view numbered from before the thread started; once kept, it reads.
pub async fn unkept_thread(ctx: &mut Ctx) {
    let project = ctx.engine.add_project(ctx.run.project()).await.unwrap();
    let (thread, before) = codex_thread(ctx, &project).await;
    let early = ctx.read_thread(&thread).await;
    assert!(early.turns.is_empty() && early.seq == before, "{early:?}");
    // Another thread's agent reads it: its header, and no history.
    let args = ReadArgs { thread: thread.0.clone(), view: View::Messages, before: None, limit: None };
    let read = ctx.engine.inner().read_for(&ThreadId("reader".into()), args).await.expect("a new thread is read");
    assert_eq!((&read["thread"], &read["status"], &read["error"], &read["items"], &read["more"]), (&json!(thread.0), &json!("idle"), &json!(null), &json!([]), &json!(false)));
    reply(ctx, &thread, "one").await;
    assert_eq!(ctx.read_thread(&thread).await.turns.len(), 1);
}
