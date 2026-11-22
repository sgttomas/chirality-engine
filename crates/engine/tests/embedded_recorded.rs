//! Sessions the pinned Codex (0.171.0) recorded under SWBPIPE's configuration,
//! through the embedded engine and SWBPIPE's rule.

mod support;

use chirality_engine::embedded::{CallRef, EmbeddedEvent, RpcId};
use serde_json::Value;

fn names(events: &[EmbeddedEvent]) -> Vec<String> {
    events.iter().map(support::name).collect()
}

const TURN: [&str; 13] = [
    "ThreadStatus active",
    "TurnStarted inProgress",
    "ItemStarted userMessage",
    "ItemCompleted userMessage",
    "ItemStarted agentMessage",
    "AgentMessageDelta",
    "AgentMessageDelta",
    "AgentMessageDelta",
    "ItemCompleted agentMessage",
    "TokenUsage",
    "RateLimits",
    "ThreadStatus idle",
    "TurnCompleted completed",
];

#[tokio::test]
async fn a_text_only_whole_turn_then_another() {
    let events = support::replay(support::recording("embedded_text_turn"), &support::UNMEASURED).await;
    let mut expected = vec!["RemoteControl".to_string(), "ThreadStarted".into()];
    expected.extend(TURN.iter().chain(TURN.iter()).map(|n| n.to_string()));
    assert_eq!(names(&events), expected);
    let EmbeddedEvent::Notification { turn: Some(first), .. } = &events[3] else { panic!() };
    let EmbeddedEvent::Notification { turn: Some(second), .. } = &events[3 + TURN.len()] else { panic!() };
    assert_ne!(first, second);
}

#[tokio::test]
async fn a_stream_cut_short_is_retried() {
    let events = support::replay(support::recording("embedded_retry"), &support::UNMEASURED).await;
    let expected = [
        "RemoteControl",
        "ThreadStarted",
        "ThreadStatus active",
        "TurnStarted inProgress",
        "ItemStarted userMessage",
        "ItemCompleted userMessage",
        "RateLimits",
        "Retrying",
        "ItemStarted agentMessage",
        "AgentMessageDelta",
        "AgentMessageDelta",
        "AgentMessageDelta",
        "ItemCompleted agentMessage",
        "TokenUsage",
        "RateLimits",
        "ThreadStatus idle",
        "TurnCompleted completed",
    ];
    assert_eq!(names(&events), expected);
}

fn call(events: &[EmbeddedEvent]) -> (&CallRef, &str, &str) {
    let calls: Vec<_> = events
        .iter()
        .filter_map(|e| match e {
            EmbeddedEvent::Call { call, tool, arguments } => Some((call, tool.as_str(), arguments.as_str())),
            _ => None,
        })
        .collect();
    assert_eq!(calls.len(), 1);
    calls[0]
}

/// A call with a provider's `callId` as Codex passes it (`call_"文\x`),
/// answered with 65,515 bytes of text in a 65,710-byte line, then the reply.
#[tokio::test]
async fn a_call_answered_whole() {
    let lines = support::recording("embedded_call");
    let answer = lines.iter().find_map(|l| match l {
        support::Line::Outbound(l) if l.starts_with(r#"{"id":0,"result""#) => Some(l.clone()),
        _ => None,
    });
    let answer: Value = serde_json::from_str(&answer.clone().unwrap()).unwrap();
    assert_eq!(answer["result"]["contentItems"][0]["text"].as_str().unwrap().len(), 65_515);
    let events = support::replay(lines, &support::UNMEASURED).await;
    let (call, tool, arguments) = call(&events);
    assert_eq!((&call.rpc_id, call.call_id.as_str(), tool), (&RpcId::Integer(0), "call_\"文\\x", "swbpipe_read_records"));
    assert_eq!(serde_json::from_str::<Value>(arguments).unwrap()["family"], "load_cases");
    let at = |name: &str| names(&events).iter().position(|n| n == name).unwrap();
    assert!(at("ItemStarted dynamicToolCall") < at("Call") && at("Call") < at("ItemCompleted dynamicToolCall"));
    assert_eq!(names(&events).last().map(String::as_str), Some("TurnCompleted completed"));
}

/// A call pending when the host stops the turn: Codex fails its item, then
/// replies, then ends the turn; the call is not live after.
#[tokio::test]
async fn a_call_pending_when_the_turn_is_interrupted() {
    let events = support::replay(support::recording("embedded_interrupt"), &support::UNMEASURED).await;
    let (call, ..) = call(&events);
    assert_eq!(call.call_id, "call_stop");
    let tail: Vec<_> = names(&events).into_iter().skip_while(|n| n != "Call").collect();
    assert_eq!(tail, ["Call", "ItemCompleted dynamicToolCall", "TokenUsage", "RateLimits", "ThreadStatus idle", "TurnCompleted interrupted"]);
}
