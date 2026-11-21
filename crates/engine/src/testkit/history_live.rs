//! Live: a new thread read before Codex has kept its first message, and the
//! interface's own start-then-read, rebuilt from the read and the events after
//! its number, are item for item what Codex keeps once the turn is done.

use tokio::sync::broadcast;

use super::harness::{Ctx, codex_present, settings, text};
use super::history::{codex_thread, reply};
use crate::api::*;

/// A view with the events after its number applied, as an interface applies them:
/// an item replaces the one with its id (or is added), streamed text extends it.
fn rebuilt(view: &ThreadView, events: &[Numbered]) -> Vec<(String, ItemBody)> {
    let mut items: Vec<(String, ItemBody)> = view.turns.iter().flat_map(|t| &t.items).map(|i| (i.id.0.clone(), i.body.clone())).collect();
    for n in events.iter().filter(|n| n.seq > view.seq) {
        match &n.event {
            Event::Item { thread, item, .. } if thread == &view.summary.id => match items.iter_mut().find(|(id, _)| id == &item.id.0) {
                Some(found) => found.1 = item.body.clone(),
                None => items.push((item.id.0.clone(), item.body.clone())),
            },
            Event::ItemDelta { thread, item, kind: TextKind::AgentMessage, text, .. } if thread == &view.summary.id => {
                match items.iter_mut().find(|(id, _)| id == &item.0) {
                    Some((_, ItemBody::AgentMessage { text: shown })) => shown.push_str(text),
                    Some(_) => {}
                    None => items.push((item.0.clone(), ItemBody::AgentMessage { text: text.clone() })),
                }
            }
            _ => {}
        }
    }
    items
}

fn kept(view: &ThreadView) -> Vec<(String, ItemBody)> {
    view.turns.iter().flat_map(|t| &t.items).map(|i| (i.id.0.clone(), i.body.clone())).collect()
}

fn drain(rx: &mut broadcast::Receiver<Numbered>, into: &mut Vec<Numbered>) {
    while let Ok(n) = rx.try_recv() {
        into.push(n);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_new_threads_first_turn_is_whole_from_its_first_read_and_events() {
    if !codex_present() {
        eprintln!("skipped: CHIRALITY_TEST_CODEX names the pinned Codex");
        return;
    }
    let mut ctx = Ctx::live("first_read", false).await;
    let project = ctx.engine.add_project(ctx.run.project()).await.unwrap();
    let mut events = ctx.engine.subscribe();
    let mut seen = Vec::new();

    // Read before Codex has the first message: empty, numbered from before the thread started.
    let (thread, _) = codex_thread(&ctx, &project).await;
    let early = ctx.engine.read_thread(&thread).await.unwrap();
    assert!(early.turns.is_empty());
    reply(&mut ctx, &thread, "one").await;
    drain(&mut events, &mut seen);
    let whole = kept(&ctx.engine.read_thread(&thread).await.unwrap());
    assert_eq!(whole.len(), 2, "{whole:?}");
    assert_eq!(rebuilt(&early, &seen), whole);

    // The interface's own way: start the thread, then read it at once.
    for key in ["a", "b", "c", "d", "e"] {
        ctx.script(key, super::stub::says(key, &format!("reply {key}")));
        let thread = ctx.engine.start_thread(&project.id, text(&format!("STUB[{key}] go")), settings(Access::Ask)).await.unwrap();
        let first = ctx.engine.read_thread(&thread).await.expect("the new thread reads");
        ctx.turn_ended(&thread).await;
        drain(&mut events, &mut seen);
        let whole = kept(&ctx.engine.read_thread(&thread).await.unwrap());
        assert_eq!(rebuilt(&first, &seen), whole, "thread {key}");
    }
    ctx.finish("first_read").await;
}
