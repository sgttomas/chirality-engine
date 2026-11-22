//! The bounds: the writer, the ring, unread events' strings and frames.

use serde_json::json;
use tokio::sync::mpsc::error::TryRecvError;

use super::tests::*;
use super::*;

/// The `thread/start` frame `open_thread` writes with these tools and options,
/// as the host reads it.
async fn thread_start_frame(tools: String, options: &ThreadOptions) -> String {
    let (engine, _events, mut host) = host_with(8, |c| c.tools = tools);
    let ((), ()) = tokio::join!(async { engine.connect().await.unwrap() }, async {
        let init = host.next().await;
        host.send(host.reply(&init["id"], json!({}))).await;
        host.next().await;
    });
    let mut open = std::pin::pin!(engine.open_thread(options));
    assert!(poll_once(open.as_mut()).await.is_pending());
    host.from_engine.recv().await.unwrap()
}

#[tokio::test]
async fn new_takes_tools_only_while_thread_start_fits() {
    let tools = |pad: usize| format!(r#"[{{"name":"a","description":"{}"}}]"#, "d".repeat(pad)).into_boxed_str().into_string();
    let longest = ThreadOptions { policy: ThreadPolicy { sandbox: SandboxMode::DangerFullAccess, approval: ApprovalPolicy::OnRequest }, ..OPTIONS };
    let room = 131_071 - thread_start_frame(tools(0), &longest).await.len();
    assert_eq!(thread_start_frame(tools(room), &longest).await.len(), 131_071, "the largest tools new takes are written");
    let refused = |tools: String| {
        let (_, from_codex) = tokio::sync::mpsc::channel(1);
        let (to_codex, _) = tokio::sync::mpsc::channel(1);
        let mut config = config(HostTransport { from_codex, to_codex });
        config.tools = tools;
        EmbeddedEngine::new(config).err()
    };
    assert_eq!(refused(tools(room + 1)), Some(EmbeddedError::Invalid { reason: "thread/start would be over 131,071 bytes" }));
    let single_line = tools(131_072 - tools(0).len());
    assert_eq!(single_line.len(), 131_072);
    assert!(refused(single_line).is_some());
}

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

    // Tools new takes, then instructions that take thread/start past the writer.
    let tools = format!("[{{\"name\":\"a\",\"description\":\"{}\"}}]", "d".repeat(130_000));
    let (engine, _events, mut host) = host_with(8, |c| c.tools = tools.into_boxed_str().into_string());
    let ((), ()) = tokio::join!(async { engine.connect().await.unwrap() }, async {
        let init = host.next().await;
        host.send(host.reply(&init["id"], json!({}))).await;
        host.next().await;
    });
    let options = ThreadOptions { base_instructions: Some("i".repeat(2_000)), ..OPTIONS };
    for _ in 0..2 {
        assert_eq!(engine.open_thread(&options).await, Err(EmbeddedError::Invalid { reason: "the frame is over 131,071 bytes" }));
    }
    assert!(host.from_engine.try_recv().is_err());
    let (opened, ()) = tokio::join!(engine.open_thread(&OPTIONS), async {
        let start = host.next().await;
        assert_eq!(start["id"], 2, "no id was used");
        host.send(host.reply(&start["id"], json!({"thread": {"id": "t"}}))).await;
    });
    assert_eq!(opened.unwrap().thread.0, "t", "the configuration was kept");
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
