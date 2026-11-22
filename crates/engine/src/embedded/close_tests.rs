//! Close, `finished` and the reply slots, on every path.

use std::pin::pin;

use serde_json::json;
use tokio::sync::mpsc::error::TryRecvError;

use super::budget::FIXED;
use super::tests::*;
use super::*;

#[tokio::test]
async fn close_ends_everything_once_and_the_first_fault_wins() {
    let (engine, mut events, mut host) = opened().await;
    running(&engine, &mut host, "t").await;
    host.send(note("thread/status/changed", json!({"threadId": THREAD, "status": {"type": "active"}}))).await;
    let mut stop = pin!(engine.interrupt());
    assert!(poll_once(stop.as_mut()).await.is_pending());
    host.next().await;
    host.send("not json").await;
    settle().await;
    engine.close();
    engine.close();
    assert_eq!(stop.await, Err(EmbeddedError::Closed { fault: Some(Fault::InvalidFrame) }));
    assert_eq!(events.recv().await, Some(EmbeddedEvent::Closed { fault: Some(Fault::InvalidFrame) }), "unread events are discarded");
    assert_eq!(events.recv().await, None);
    assert_eq!(events.try_recv(), Err(TryRecvError::Disconnected));
    assert_eq!(host.from_engine.recv().await, None, "to_codex ended");
    let finished = engine.finished();
    finished.clone().await;
    finished.await;
    assert_eq!(engine.shared.ledger.used(), FIXED);
    assert_eq!(engine.send_text("hi").await, Err(EmbeddedError::Closed { fault: Some(Fault::InvalidFrame) }));
}

#[tokio::test]
async fn close_before_connect_releases_the_transport() {
    let (engine, mut events, mut host) = host_with(8, |_| {});
    let finished = engine.finished();
    drop(engine);
    finished.await;
    assert_eq!(host.from_engine.recv().await, None);
    assert!(host.to_engine.take().unwrap().send("x".into()).await.is_err(), "from_codex freed with the transport");
    assert_eq!(events.recv().await, Some(EmbeddedEvent::Closed { fault: None }));
}

#[tokio::test]
async fn input_ended_output_closed_and_output_full_fault() {
    let (_engine, mut events, mut host) = opened().await;
    host.to_engine = None;
    assert_eq!(closed_with(&mut events).await, Some(Fault::InputEnded));

    let (engine, mut events, host) = opened().await;
    drop(host.from_engine);
    assert_eq!(engine.send_text("hi").await, Err(EmbeddedError::Closed { fault: Some(Fault::OutputClosed) }));
    assert_eq!(closed_with(&mut events).await, Some(Fault::OutputClosed));
    engine.finished().await;

    let (engine, mut events, host) = host_with(1, |_| {});
    let (connected, ()) = tokio::join!(engine.connect(), async {
        host.send(json!({"id": 1, "result": {}})).await;
    });
    // `initialize` still waits in the host's channel of one, so `initialized` did not fit.
    assert_eq!(connected, Err(EmbeddedError::Closed { fault: Some(Fault::OutputFull) }));
    assert_eq!(closed_with(&mut events).await, Some(Fault::OutputFull));
}

#[tokio::test]
async fn a_caller_dropped_after_its_reply_frees_the_slot() {
    let (engine, _events, mut host) = opened().await;
    running(&engine, &mut host, "t").await;
    {
        let mut stop = pin!(engine.interrupt());
        assert!(poll_once(stop.as_mut()).await.is_pending());
        let interrupt = host.next().await;
        host.send(host.reply(&interrupt["id"], json!({}))).await;
        settle().await;
    }
    let (stopped, ()) = tokio::join!(engine.interrupt(), async {
        let interrupt = host.next().await;
        host.send(host.reply(&interrupt["id"], json!({}))).await;
    });
    assert_eq!(stopped, Ok(()));
}

#[tokio::test]
async fn close_empties_a_reply_left_uncollected() {
    let (engine, _events, mut host) = opened().await;
    running(&engine, &mut host, "t").await;
    let mut stop = pin!(engine.interrupt());
    assert!(poll_once(stop.as_mut()).await.is_pending());
    let interrupt = host.next().await;
    host.send(host.reply(&interrupt["id"], json!({}))).await;
    settle().await;
    engine.close();
    engine.finished().await;
    assert_eq!(engine.shared.ledger.used(), FIXED, "the reply was released");
    assert_eq!(stop.await, Err(EmbeddedError::Closed { fault: None }));
}

#[tokio::test]
async fn a_failed_initialize_frees_codexs_input_before_finishing() {
    let (engine, mut events, host) = host_with(8, |_| {});
    let Host { to_engine, from_engine } = host;
    drop(from_engine);
    assert_eq!(engine.connect().await, Err(EmbeddedError::Closed { fault: Some(Fault::OutputClosed) }));
    assert!(to_engine.unwrap().is_closed(), "from_codex was freed");
    engine.finished().await;
    assert_eq!(closed_with(&mut events).await, Some(Fault::OutputClosed));
}

/// `finished` across threads: awaited on one worker while the engine closes
/// on another, by `close`, by a fault, and by Codex's input ending.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn finished_wakes_across_threads() {
    for round in 0..300 {
        let (engine, events, host) = opened().await;
        let waiter = tokio::spawn(engine.finished());
        let spin = round % 5;
        let closer = tokio::spawn(async move {
            for _ in 0..spin {
                tokio::task::yield_now().await;
            }
            match round % 3 {
                0 => engine.close(),
                1 => host.send("not json").await,
                _ => drop(host),
            }
            engine
        });
        tokio::time::timeout(std::time::Duration::from_secs(5), waiter).await.expect("finished woke").unwrap();
        let engine = closer.await.unwrap();
        assert_eq!(engine.shared.ledger.used(), FIXED);
        drop((engine, events));
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn finished_clones_and_late_waiters_resolve() {
    for _ in 0..100 {
        let (engine, events, _host) = opened().await;
        let early = engine.finished();
        let waiters: Vec<_> = (0..4).map(|_| tokio::spawn(early.clone())).collect();
        let closer = tokio::spawn(async move { engine.close() });
        for waiter in waiters {
            tokio::time::timeout(std::time::Duration::from_secs(5), waiter).await.expect("finished woke").unwrap();
        }
        closer.await.unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(5), early).await.expect("a late poll resolves");
        drop(events);
    }
}
