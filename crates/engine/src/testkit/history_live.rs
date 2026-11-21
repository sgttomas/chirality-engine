//! Live races between a read and Codex keeping a new thread's history,
//! each synchronised on Codex's refusal seen on the wire.

use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Value, json};
use tokio::sync::{broadcast, mpsc};

use super::harness::{Ctx, codex_present, settings, text};
use super::history::{codex_thread, reply};
use crate::api::*;
use crate::rpc::{Direction, Tap};

/// A watcher that tells, by the thread it named, each history read Codex refused.
fn refusals() -> (Tap, mpsc::UnboundedReceiver<String>) {
    let (tx, rx) = mpsc::unbounded_channel();
    let asked: Arc<Mutex<Vec<(i64, String)>>> = Arc::default();
    let tap: Tap = Arc::new(move |direction, line| {
        let Ok(frame) = serde_json::from_str::<Value>(line) else { return };
        let id = frame["id"].as_i64();
        match direction {
            Direction::Out if frame["method"] == "thread/turns/list" || frame["params"]["includeTurns"] == true => {
                asked.lock().unwrap().push((id.unwrap_or(-1), frame["params"]["threadId"].as_str().unwrap_or("").to_string()));
            }
            Direction::In if frame.get("error").is_some() && frame.get("method").is_none() => {
                if let Some((_, thread)) = asked.lock().unwrap().iter().find(|(asked, _)| Some(*asked) == id) {
                    let _ = tx.send(thread.clone());
                }
            }
            _ => {}
        }
    });
    (tap, rx)
}

async fn refused(rx: &mut mpsc::UnboundedReceiver<String>, thread: &ThreadId) {
    let wait = async {
        while let Some(t) = rx.recv().await {
            if t == thread.0 {
                return;
            }
        }
    };
    tokio::time::timeout(Duration::from_secs(10), wait).await.expect("Codex refuses the read before the first message");
}

/// The texts a view shows once the events after its number are applied: the
/// items it holds, and the items and streamed text of those events.
fn shown(view: &ThreadView, events: &[Numbered]) -> HashSet<String> {
    let mut texts = HashSet::new();
    let mut bodies: Vec<&ItemBody> = view.turns.iter().flat_map(|t| &t.items).map(|i| &i.body).collect();
    for n in events.iter().filter(|n| n.seq > view.seq) {
        match &n.event {
            Event::Item { thread, item, .. } if thread == &view.summary.id => bodies.push(&item.body),
            Event::ItemDelta { thread, text, kind: TextKind::AgentMessage, .. } if thread == &view.summary.id => {
                texts.insert(text.clone());
            }
            _ => {}
        }
    }
    for body in bodies {
        if let ItemBody::UserMessage { text, .. } | ItemBody::AgentMessage { text } = body {
            texts.insert(text.clone());
        }
    }
    texts
}

fn drain(rx: &mut broadcast::Receiver<Numbered>, into: &mut Vec<Numbered>) {
    while let Ok(n) = rx.try_recv() {
        into.push(n);
    }
}

/// A read that reaches Codex before it has kept a new thread's first message,
/// as the interface's read of a thread it has just started can: once Codex
/// has kept it, the read and the events after its number show the message
/// and the reply streamed during the wait.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_read_racing_a_first_message_loses_nothing() {
    if !codex_present() {
        eprintln!("skipped: CHIRALITY_TEST_CODEX names the pinned Codex");
        return;
    }
    let (tap, mut refusals) = refusals();
    let mut ctx = Ctx::live_watched("first_message_race", tap).await;
    let project = ctx.engine.add_project(ctx.run.project()).await.unwrap();
    let mut events = ctx.engine.subscribe();
    let mut seen = Vec::new();

    let thread = codex_thread(&ctx, &project).await;
    let reading = tokio::spawn({
        let (engine, thread) = (ctx.engine.clone(), thread.clone());
        async move { engine.read_thread(&thread).await }
    });
    refused(&mut refusals, &thread).await;
    reply(&mut ctx, &thread, "one").await;
    let view = reading.await.unwrap().expect("the read waits for Codex to keep the message");
    drain(&mut events, &mut seen);
    let texts = shown(&view, &seen);
    assert!(texts.contains("STUB[one] go") && texts.contains("one"), "{texts:?}, view {view:?}");

    // The interface's own way: start the thread, then read it at once.
    for key in ["a", "b", "c", "d", "e"] {
        ctx.script(key, super::stub::says(key, &format!("reply {key}")));
        let thread = ctx.engine.start_thread(&project.id, text(&format!("STUB[{key}] go")), settings(Access::Ask)).await.unwrap();
        let view = ctx.engine.read_thread(&thread).await.expect("the new thread reads");
        ctx.turn_ended(&thread).await;
        drain(&mut events, &mut seen);
        let texts = shown(&view, &seen);
        assert!(texts.contains(&format!("STUB[{key}] go")) && texts.contains(&format!("reply {key}")), "{texts:?}, view {view:?}");
    }
    ctx.finish("first_message_race").await;
}

/// A thread deleted while a read waits for its first message is not found.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_thread_deleted_while_a_read_waits_is_not_found() {
    if !codex_present() {
        eprintln!("skipped: CHIRALITY_TEST_CODEX names the pinned Codex");
        return;
    }
    let (tap, mut refusals) = refusals();
    let ctx = Ctx::live_watched("deleted_while_waiting", tap).await;
    let project = ctx.engine.add_project(ctx.run.project()).await.unwrap();
    let thread = codex_thread(&ctx, &project).await;
    let reading = tokio::spawn({
        let (engine, thread) = (ctx.engine.clone(), thread.clone());
        async move { engine.read_thread(&thread).await }
    });
    refused(&mut refusals, &thread).await;
    let session = ctx.engine.inner().session().unwrap();
    session.client.request_value("thread/delete", json!({ "threadId": thread.0 })).await.expect("Codex deletes the thread");
    let e = reading.await.unwrap().unwrap_err();
    assert!(matches!(e, EngineError::NotFound { .. }), "{e:?}");
    assert!(ctx.engine.snapshot().threads.iter().all(|t| t.id != thread));
    ctx.finish("deleted_while_waiting").await;
}
