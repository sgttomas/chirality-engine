//! Sessions the pinned Codex (0.171.0) recorded under SWBPIPE's configuration,
//! through the embedded engine and SWBPIPE's rule.

mod support;

use chirality_engine::embedded::EmbeddedEvent;

fn no_pause(f: &mut dyn FnMut()) {
    f()
}

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
    let events = support::replay(support::recording("embedded_text_turn"), &no_pause).await;
    let mut expected = vec!["RemoteControl".to_string(), "ThreadStarted".into()];
    expected.extend(TURN.iter().chain(TURN.iter()).map(|n| n.to_string()));
    assert_eq!(names(&events), expected);
    let EmbeddedEvent::Notification { turn: Some(first), .. } = &events[3] else { panic!() };
    let EmbeddedEvent::Notification { turn: Some(second), .. } = &events[3 + TURN.len()] else { panic!() };
    assert_ne!(first, second);
}

#[tokio::test]
async fn a_stream_cut_short_is_retried() {
    let events = support::replay(support::recording("embedded_retry"), &no_pause).await;
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
