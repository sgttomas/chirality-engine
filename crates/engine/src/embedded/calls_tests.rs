//! Calls over a scripted host: admission, refusal, the record, answers,
//! retirement and the host's check at dequeue.

use serde_json::{Value, json};
use tokio::sync::mpsc::error::TryRecvError;

use super::tests::*;
use super::*;

const TURN: &str = "turn-1";

/// An `item/tool/call` for the namespace's tool in `turn`, edited by `edit`.
fn call_frame(id: Value, call_id: &str, edit: impl FnOnce(&mut Value)) -> String {
    let mut frame = json!({"id": id, "method": "item/tool/call", "params": {
        "threadId": THREAD, "turnId": TURN, "callId": call_id, "namespace": "swbpipe", "tool": "swbpipe_solve", "arguments": {"x": 1},
    }});
    edit(&mut frame["params"]);
    frame.to_string()
}

/// A turn running, and a call admitted and read: its reference.
async fn called(engine: &EmbeddedEngine, events: &mut EmbeddedEvents, host: &mut Host, id: Value, call_id: &str) -> CallRef {
    running(engine, host, TURN).await;
    host.send(call_frame(id, call_id, |_| {})).await;
    match events.recv().await {
        Some(EmbeddedEvent::Call { call, tool, arguments }) => {
            assert_eq!((tool.as_str(), &*arguments), ("swbpipe_solve", r#"{"x":1}"#));
            call
        }
        other => panic!("{other:?}"),
    }
}

fn nothing_written(host: &mut Host) {
    assert_eq!(host.from_engine.try_recv(), Err(TryRecvError::Empty));
}

#[test]
fn a_call_serializes_with_its_rpc_id_untagged() {
    let call = |rpc_id| EmbeddedEvent::Call {
        call: CallRef { rpc_id, call_id: "c".into(), turn: TurnId("t".into()), generation: 3 },
        tool: "x".into(),
        arguments: "{}".into(),
    };
    let expected = json!({"kind": "call", "call": {"rpcId": 7, "callId": "c", "turn": "t", "generation": 3}, "tool": "x", "arguments": "{}"});
    assert_eq!(serde_json::to_value(call(RpcId::Integer(7))).unwrap(), expected);
    assert_eq!(serde_json::to_value(call(RpcId::Text("r".into()))).unwrap()["call"]["rpcId"], "r");
}

#[tokio::test]
async fn an_admitted_call_is_answered_once() {
    let (engine, mut events, mut host) = opened().await;
    let call = called(&engine, &mut events, &mut host, json!(0), "call_1").await;
    assert_eq!(call, CallRef { rpc_id: RpcId::Integer(0), call_id: "call_1".into(), turn: TurnId(TURN.into()), generation: 0 });
    assert!(engine.call_live(&call));
    engine.answer(&call, false, "no \"such\" node\n").unwrap();
    let written = host.next().await;
    assert_eq!(written, json!({"id": 0, "result": {"contentItems": [{"type": "inputText", "text": "no \"such\" node\n"}], "success": false}}));
    assert!(!engine.call_live(&call));
    assert_eq!(engine.answer(&call, true, "again"), Err(EmbeddedError::Invalid { reason: "the call is not live" }));
    nothing_written(&mut host);
}

#[tokio::test]
async fn an_escaped_call_id_and_rpc_id_are_unescaped_and_answered_escaped() {
    let (engine, mut events, mut host) = opened().await;
    running(&engine, &mut host, TURN).await;
    // As Codex sends a provider's callId: `call_"文\x`.
    host.send(r#"{"id":"r\"1","method":"item/tool/call","params":{"threadId":"thread-1","turnId":"turn-1","callId":"call_\"文\\x","namespace":"swbpipe","tool":"swbpipe_solve","arguments":{}}}"#).await;
    let Some(EmbeddedEvent::Call { call, tool, .. }) = events.recv().await else { panic!() };
    assert_eq!((&call.rpc_id, call.call_id.as_str(), tool.as_str()), (&RpcId::Text("r\"1".into()), "call_\"文\\x", "swbpipe_solve"));
    engine.answer(&call, true, "ok").unwrap();
    assert_eq!(host.next().await["id"], "r\"1");
}

#[tokio::test]
async fn anything_else_is_refused() {
    let (engine, mut events, mut host) = opened_with(|c| c.limits.argument_bytes = 16).await;
    running(&engine, &mut host, TURN).await;
    type Case = (&'static str, fn(&mut Value));
    let cases: [Case; 10] = [
        ("another namespace", |p| p["namespace"] = json!("other")),
        ("another tool", |p| p["tool"] = json!("swbpipe_other")),
        ("another thread", |p| p["threadId"] = json!("thread-2")),
        ("another turn", |p| p["turnId"] = json!("turn-0")),
        ("no arguments", |p| _ = p.as_object_mut().unwrap().remove("arguments")),
        ("arguments over argument_bytes", |p| p["arguments"] = json!({"x": "0123456789"})),
        ("no callId", |p| _ = p.as_object_mut().unwrap().remove("callId")),
        ("a callId with a control character", |p| p["callId"] = json!("c\u{1}")),
        ("a callId over 256 bytes", |p| p["callId"] = json!("c".repeat(257))),
        ("no namespace", |p| _ = p.as_object_mut().unwrap().remove("namespace")),
    ];
    for (n, (label, edit)) in cases.into_iter().enumerate() {
        host.send(call_frame(json!(n), &format!("call_{n}"), edit)).await;
        assert!(matches!(events.recv().await, Some(EmbeddedEvent::Refused { method, .. }) if method == "item/tool/call"), "{label}");
        assert_eq!(host.next().await, json!({"id": n, "error": {"code": -32601, "message": "not supported by this client"}}), "{label}");
    }
    let at_the_bound = call_frame(json!(99), "call_99", |p| p["arguments"] = json!({"x": "01234567"}));
    host.send(at_the_bound).await;
    assert!(matches!(events.recv().await, Some(EmbeddedEvent::Call { .. })), "16 bytes of arguments");
    // Present, so admitted: `null` is its raw text.
    host.send(call_frame(json!(98), "call_98", |p| p["arguments"] = Value::Null)).await;
    assert!(matches!(events.recv().await, Some(EmbeddedEvent::Call { arguments, .. }) if &*arguments == "null"), "null arguments");
    host.send(json!({"id": 100, "method": "item/commandExecution/requestApproval", "params": {"threadId": THREAD, "turnId": TURN, "callId": "call_100"}}))
        .await;
    assert!(matches!(events.recv().await, Some(EmbeddedEvent::Refused { .. })), "another method");
}

#[tokio::test]
async fn fewer_than_pending_calls_are_pending() {
    let (engine, mut events, mut host) = opened_with(|c| c.limits.pending_calls = 1).await;
    let first = called(&engine, &mut events, &mut host, json!(0), "call_0").await;
    host.send(call_frame(json!(1), "call_1", |_| {})).await;
    assert!(matches!(events.recv().await, Some(EmbeddedEvent::Refused { .. })));
    host.next().await;
    engine.answer(&first, true, "ok").unwrap();
    host.next().await;
    host.send(call_frame(json!(2), "call_2", |_| {})).await;
    assert!(matches!(events.recv().await, Some(EmbeddedEvent::Call { .. })), "a place freed by the answer");
}

#[tokio::test]
async fn a_repeated_rpc_id_faults_even_outside_a_turn() {
    let (_engine, mut events, host) = opened().await;
    for _ in 0..2 {
        host.send(json!({"id": "same", "method": "item/other"})).await;
    }
    assert_eq!(closed_with(&mut events).await, Some(Fault::RepeatedId));
    let (engine, mut events, mut host) = opened().await;
    called(&engine, &mut events, &mut host, json!(0), "call_0").await;
    host.send(call_frame(json!(1), "call_0", |_| {})).await;
    assert_eq!(closed_with(&mut events).await, Some(Fault::RepeatedId), "a repeated callId");
}

#[tokio::test]
async fn a_request_past_the_count_faults_and_a_new_turn_clears_the_record() {
    let (engine, mut events, mut host) = opened_with(|c| c.limits.calls_per_turn = 2).await;
    let call = called(&engine, &mut events, &mut host, json!(0), "call_0").await;
    engine.answer(&call, true, "ok").unwrap();
    host.next().await;
    host.send(note("turn/completed", json!({"threadId": THREAD, "turn": {"id": TURN, "status": "completed"}}))).await;
    settle().await;
    // The next turn's record is new: the same ids are taken again.
    let (started, ()) = tokio::join!(engine.send_text("again"), async {
        let start = host.next().await;
        host.send(host.reply(&start["id"], json!({"turn": {"id": "turn-2", "status": "inProgress"}}))).await;
    });
    started.unwrap();
    for n in 0..2 {
        host.send(json!({"id": n, "method": "item/other", "params": {"callId": format!("call_{n}")}})).await;
        host.next().await;
    }
    host.send(json!({"id": 2, "method": "item/other"})).await;
    assert_eq!(closed_with(&mut events).await, Some(Fault::TooManyRequests));
}

#[tokio::test]
async fn an_answer_over_its_bounds_is_invalid_and_the_call_stays_pending() {
    let (engine, mut events, mut host) = opened_with(|c| c.limits.result_bytes = 65_536).await;
    let call = called(&engine, &mut events, &mut host, json!(0), "call_0").await;
    assert_eq!(engine.answer(&call, true, &"x".repeat(65_537)), Err(EmbeddedError::Invalid { reason: "the text is over result_bytes" }));
    // Within result_bytes, but each control character is six bytes on the wire.
    assert_eq!(engine.answer(&call, true, &"\u{1}".repeat(65_536)), Err(EmbeddedError::Invalid { reason: "the frame is over 131,071 bytes" }));
    nothing_written(&mut host);
    assert!(engine.call_live(&call));
    engine.answer(&call, true, &"x".repeat(65_536)).unwrap();
    let empty = r#"{"id":0,"result":{"contentItems":[{"type":"inputText","text":""}],"success":true}}"#;
    assert_eq!(host.from_engine.recv().await.unwrap().len(), 65_536 + empty.len());
}

#[tokio::test]
async fn calls_are_retired_by_their_item_their_turn_the_thread_and_close() {
    let item = |call_id: &str, turn: &str| {
        note("item/completed", json!({"threadId": THREAD, "turnId": turn, "item": {"id": call_id, "type": "dynamicToolCall", "status": "failed"}}))
    };
    let (engine, mut events, mut host) = opened().await;
    let call = called(&engine, &mut events, &mut host, json!(0), "call_0").await;
    host.send(item("call_other", TURN)).await;
    events.recv().await;
    assert!(engine.call_live(&call), "another call's item");
    host.send(item("call_0", TURN)).await;
    events.recv().await;
    assert!(!engine.call_live(&call), "its item");
    assert_eq!(engine.answer(&call, true, "late"), Err(EmbeddedError::Invalid { reason: "the call is not live" }));

    for end in
        [note("turn/completed", json!({"threadId": THREAD, "turn": {"id": TURN, "status": "interrupted"}})), note("thread/closed", json!({"threadId": THREAD}))]
    {
        let (engine, mut events, mut host) = opened().await;
        let call = called(&engine, &mut events, &mut host, json!(0), "call_0").await;
        host.send(end).await;
        events.recv().await;
        assert!(!engine.call_live(&call));
        assert_eq!(engine.answer(&call, true, "late"), Err(EmbeddedError::Invalid { reason: "the call is not live" }));
        nothing_written(&mut host);
    }

    // Close retires every call: one the engine issued is `Invalid`, as any
    // retired call is; only a call it never issued is `Closed`.
    let (engine, mut events, mut host) = opened().await;
    let call = called(&engine, &mut events, &mut host, json!(0), "call_0").await;
    let never = CallRef { generation: 1, ..call.clone() };
    assert_eq!(engine.answer(&never, true, "x"), Err(EmbeddedError::Invalid { reason: "the call is not live" }), "before close");
    engine.close();
    assert!(!engine.call_live(&call));
    assert_eq!(engine.answer(&call, true, "late"), Err(EmbeddedError::Invalid { reason: "the call is not live" }));
    assert_eq!(engine.answer(&never, true, "late"), Err(EmbeddedError::Closed { fault: None }));
    assert!(host.from_engine.try_recv().is_err(), "nothing written");
}

/// Codex reuses an ended turn's id, the RPC id and the `callId`: the old
/// call, retired with its turn, is not the new one.
#[tokio::test]
async fn a_retired_call_stays_retired_when_its_ids_come_again() {
    let (engine, mut events, mut host) = opened().await;
    let old = called(&engine, &mut events, &mut host, json!(0), "call_0").await;
    host.send(note("turn/completed", json!({"threadId": THREAD, "turn": {"id": TURN, "status": "completed"}}))).await;
    assert!(matches!(events.recv().await, Some(EmbeddedEvent::Notification { method: Method::TurnCompleted, .. })));
    settle().await;
    let new = called(&engine, &mut events, &mut host, json!(0), "call_0").await;
    assert_eq!((&new.rpc_id, &new.call_id, &new.turn), (&old.rpc_id, &old.call_id, &old.turn));
    assert_ne!(new.generation, old.generation);
    assert!(!engine.call_live(&old));
    assert_eq!(engine.answer(&old, true, "late"), Err(EmbeddedError::Invalid { reason: "the call is not live" }));
    nothing_written(&mut host);
    assert!(engine.call_live(&new));
    engine.answer(&new, true, "ok").unwrap();
    assert_eq!(host.next().await["result"]["contentItems"][0]["text"], "ok");
}

#[tokio::test]
async fn interrupt_with_a_call_pending() {
    let (engine, mut events, mut host) = opened().await;
    let call = called(&engine, &mut events, &mut host, json!(0), "call_0").await;
    let (stopped, ()) = tokio::join!(engine.interrupt(), async {
        let interrupt = host.next().await;
        assert_eq!(interrupt["method"], "turn/interrupt");
        // Codex completes the call's item as failed before it replies.
        host.send(note("item/completed", json!({"threadId": THREAD, "turnId": TURN, "item": {"id": "call_0", "type": "dynamicToolCall", "status": "failed"}})))
            .await;
        host.send(host.reply(&interrupt["id"], json!({}))).await;
    });
    assert_eq!(stopped, Ok(()));
    assert!(!engine.call_live(&call));
    assert_eq!(engine.answer(&call, true, "late"), Err(EmbeddedError::Invalid { reason: "the call is not live" }));
}

/// The host has not read the events yet: they wait in wire order.
#[tokio::test]
async fn paused_an_ending_event_comes_before_the_call_after_it() {
    let (engine, mut events, mut host) = opened().await;
    running(&engine, &mut host, TURN).await;
    host.send(note("item/started", json!({"threadId": THREAD, "turnId": TURN, "item": {"id": "cmd", "type": "commandExecution"}}))).await;
    host.send(call_frame(json!(0), "call_0", |_| {})).await;
    settle().await;
    let first = events.try_recv().unwrap();
    assert!(matches!(&first, EmbeddedEvent::Notification { item: Some(item), .. } if item.item_type.as_deref() == Some("commandExecution")));
    // The host's rule ends the session on the first: the call after it is never answered.
    assert!(matches!(events.try_recv(), Ok(EmbeddedEvent::Call { .. })));
}

#[tokio::test]
async fn paused_a_call_retired_before_it_is_read_is_not_live() {
    let (engine, mut events, mut host) = opened().await;
    running(&engine, &mut host, TURN).await;
    host.send(call_frame(json!(0), "call_0", |_| {})).await;
    host.send(note("turn/completed", json!({"threadId": THREAD, "turn": {"id": TURN, "status": "interrupted"}}))).await;
    settle().await;
    let Ok(EmbeddedEvent::Call { call, .. }) = events.try_recv() else { panic!() };
    assert!(!engine.call_live(&call), "the host's check at dequeue");
    assert_eq!(engine.answer(&call, true, "work"), Err(EmbeddedError::Invalid { reason: "the call is not live" }));
    nothing_written(&mut host);
}
