//! The bounds: the writer, the ring, unread events' strings and frames.

use serde_json::json;
use tokio::sync::mpsc::error::TryRecvError;

use super::tests::*;
use super::*;

#[tokio::test]
async fn a_frame_over_the_writer_is_invalid_and_changes_nothing() {
    let (engine, _events, mut host) = opened().await;
    let long = "x".repeat(131_072);
    for _ in 0..2 {
        assert_eq!(engine.send_text(&long).await, Err(EmbeddedError::Invalid { reason: "the frame is over 131,071 bytes" }));
    }
    assert!(host.from_engine.try_recv().is_err());
    let (sent, ()) = tokio::join!(engine.send_text("hi"), async {
        let start = host.next().await;
        assert_eq!(start["id"], 3, "no id was used");
        host.send(host.reply(&start["id"], json!({"turn": {"id": "t", "status": "inProgress"}}))).await;
    });
    assert_eq!(sent.unwrap().0, "t");

    let tools = format!("[{{\"name\":\"a\",\"description\":\"{}\"}}]", "d".repeat(130_950));
    let (engine, _events, mut host) = host_with(8, |c| c.tools = tools.into_boxed_str().into_string());
    let ((), ()) = tokio::join!(async { engine.connect().await.unwrap() }, async {
        let init = host.next().await;
        host.send(host.reply(&init["id"], json!({}))).await;
        host.next().await;
    });
    for _ in 0..2 {
        assert_eq!(engine.open_thread(&OPTIONS).await, Err(EmbeddedError::Invalid { reason: "the frame is over 131,071 bytes" }));
    }
    assert!(host.from_engine.try_recv().is_err());
}

#[tokio::test]
async fn a_full_ring_faults_and_a_drained_one_takes_more() {
    let (_engine, mut events, host) = opened().await;
    let slots = LIMITS.event_bytes as usize / 512;
    for round in 0..2 {
        for n in 0..slots {
            host.send(note("account/rateLimits/updated", json!({"n": n, "round": "r".repeat(round * 100)}))).await;
        }
        settle().await;
        for _ in 0..slots {
            assert!(matches!(events.try_recv(), Ok(EmbeddedEvent::Notification { method: Method::RateLimits, .. })));
        }
        assert_eq!(events.try_recv(), Err(TryRecvError::Empty));
    }
    for n in 0..=slots {
        host.send(note("account/rateLimits/updated", json!({"n": n}))).await;
    }
    settle().await;
    assert_eq!(closed_with(&mut events).await, Some(Fault::OverBudget));
}

#[tokio::test]
async fn unread_strings_are_bounded_by_event_bytes() {
    let (_engine, mut events, host) = opened().await;
    let big = note("account/rateLimits/updated", json!({"pad": "p".repeat(100_000)}));
    host.send(&big).await;
    host.send(&big).await;
    settle().await;
    assert!(events.try_recv().is_ok());
    host.send(&big).await;
    host.send(&big).await;
    settle().await;
    assert_eq!(closed_with(&mut events).await, Some(Fault::OverBudget));
}

#[tokio::test]
async fn a_frame_over_its_bounds_faults() {
    let (_engine, mut events, host) = opened().await;
    let mut frame = String::with_capacity(128 * 1024 + 1);
    frame.push_str(&note("account/rateLimits/updated", json!({})));
    host.send_as_is(frame).await;
    assert_eq!(closed_with(&mut events).await, Some(Fault::OverBudget));

    let (_engine, mut events, host) = opened().await;
    let pad = |n: usize| note("account/rateLimits/updated", json!({"pad": "p".repeat(n)}));
    let frame = pad(131_072 - pad(0).len());
    assert_eq!(frame.len(), 131_072, "a frame's text is at most 131,071 bytes");
    host.send(frame).await;
    assert_eq!(closed_with(&mut events).await, Some(Fault::InvalidFrame));
}
