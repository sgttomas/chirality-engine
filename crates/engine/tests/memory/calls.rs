//! Calls, measured: recorded calls, many pending and answered, and a full
//! record.

use super::*;

/// An `item/tool/call` on the running turn, its arguments padded.
fn call_frame(id: &str, call_id: &str, turn: &str, pad: usize) -> String {
    json!({"id": id, "method": "item/tool/call", "params": {
        "threadId": THREAD, "turnId": turn, "callId": call_id, "namespace": "swbpipe", "tool": "swbpipe_solve",
        "arguments": {"pad": "a".repeat(pad)},
    }})
    .to_string()
}

fn turn_completed(turn: &'static str) -> impl FnOnce() -> Value {
    move || json!({"threadId": THREAD, "turn": {"id": turn, "status": "completed"}})
}

/// `pending_calls` calls with their largest arguments, the host reading the
/// events three at a time, then each answered with the largest text and its
/// frame read; then again, and the turn ends with calls pending.
async fn calls_answered(limits: EmbeddedLimits, s: &mut Session, turn: &'static str, round: usize) {
    let pending = limits.pending_calls as usize;
    let pad = limits.argument_bytes as usize - r#"{"pad":""}"#.len();
    let mut calls = Vec::new();
    for n in 0..pending {
        feed(&s.host, || call_frame(&format!("{turn}-{round}-{n}"), &format!("call-{turn}-{round}-{n}"), turn, pad)).await;
        if n % 3 == 2 || n == pending - 1 {
            while let Ok(event) = s.events.try_recv() {
                let EmbeddedEvent::Call { call, arguments, .. } = event else { panic!("a call") };
                assert_eq!(arguments.len(), limits.argument_bytes as usize);
                calls.push(call);
            }
        }
    }
    assert_eq!(calls.len(), pending);
    let text = host(|| "t".repeat(limits.result_bytes as usize));
    for call in &calls {
        s.engine.answer(call, true, &text).unwrap();
        let frame = s.host.from_engine.recv().await.unwrap();
        host(|| drop(frame));
    }
    host(|| drop((calls, text)));
}

pub(super) async fn scenarios(limits: EmbeddedLimits) {
    scenario("recorded call, answered with 65,515 bytes", &limits, || async {
        let lines = host(|| support::recording("embedded_call"));
        support::replay(lines, &METER).await;
    })
    .await;

    scenario("recorded call, interrupted", &limits, || async {
        let lines = host(|| support::recording("embedded_interrupt"));
        support::replay(lines, &METER).await;
    })
    .await;

    scenario("calls pending and answered, at their largest", &limits, || async {
        let mut s = Session::open(limits).await;
        run_turn(&s.engine, &mut s.host, "turn-1").await;
        for round in 0..3 {
            calls_answered(limits, &mut s, "turn-1", round).await;
        }
        // The turn ends with calls pending; the next turn's record is new.
        for n in 0..limits.pending_calls as usize {
            feed(&s.host, || call_frame(&format!("left-{n}"), &format!("call-left-{n}"), "turn-1", 1000)).await;
            drain(&mut s.events);
        }
        note(&s.host, "turn/completed", turn_completed("turn-1")).await;
        drain(&mut s.events);
        run_turn(&s.engine, &mut s.host, "turn-2").await;
        calls_answered(limits, &mut s, "turn-2", 0).await;
        s.engine.close();
        assert_eq!(closed(&mut s.events).await, None);
        s.finish().await;
    })
    .await;

    scenario("a full record of refused requests, then one more", &limits, || async {
        let mut s = Session::open(limits).await;
        run_turn(&s.engine, &mut s.host, "turn-1").await;
        let request = |n: u32| {
            // Ids and callIds of 256 bytes, the longest identities; no namespace, so refused.
            let (id, call_id) = (format!("r{n:0>255}"), format!("c{n:0>255}"));
            json!({"id": id, "method": "item/tool/call", "params": {"threadId": THREAD, "turnId": "turn-1", "callId": call_id}}).to_string()
        };
        for n in 0..limits.calls_per_turn {
            feed(&s.host, || request(n)).await;
            let refusal = written(&mut s.host).await;
            assert_eq!(host(|| refusal["error"]["code"].clone()), -32601);
            host(|| drop(refusal));
            assert!(matches!(s.events.try_recv(), Ok(EmbeddedEvent::Refused { .. })));
        }
        feed(&s.host, || request(limits.calls_per_turn)).await;
        assert_eq!(closed(&mut s.events).await, Some(Fault::TooManyRequests));
        s.finish().await;
    })
    .await;
}
