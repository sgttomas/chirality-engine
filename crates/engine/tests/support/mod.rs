//! Replays a session Codex recorded under SWBPIPE's configuration through the
//! embedded engine, as SWBPIPE's host would drive it, and SWBPIPE's rule.

#![allow(dead_code)]

use std::path::Path;

use chirality_engine::embedded::*;
use serde_json::Value;
use tokio::sync::mpsc;

pub const LIMITS: EmbeddedLimits =
    EmbeddedLimits { pending_calls: 16, calls_per_turn: 1024, argument_bytes: 65_536, result_bytes: 65_536, event_bytes: 256 * 1024 };

/// One line between the client and Codex, as recorded.
pub enum Line {
    Outbound(String),
    Inbound(String),
}

pub fn recording(scenario: &str) -> Vec<Line> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/codex").join(format!("{scenario}.ndjson"));
    let text = std::fs::read_to_string(&path).unwrap();
    let mut lines = Vec::new();
    for entry in text.lines().map(|l| serde_json::from_str::<Value>(l).unwrap()) {
        let line = entry["line"].as_str().map(str::to_owned);
        match (entry["type"].as_str(), line) {
            (Some("outbound"), Some(line)) => lines.push(Line::Outbound(line)),
            (Some("inbound"), Some(line)) => lines.push(Line::Inbound(line)),
            (Some("transcript_start"), _) => assert_eq!(entry["version"], "0.171.0"),
            (Some("runtime_exit"), _) => assert_eq!(entry["code"], 0),
            _ => panic!("{entry}"),
        }
    }
    lines
}

fn outbound(lines: &[Line], method: &str) -> Vec<Value> {
    lines
        .iter()
        .filter_map(|l| match l {
            Line::Outbound(l) => Some(serde_json::from_str::<Value>(l).unwrap()),
            Line::Inbound(_) => None,
        })
        .filter(|frame| frame["method"] == method)
        .collect()
}

/// SWBPIPE's configuration as the recorded `thread/start` carried it.
pub fn config(lines: &[Line], transport: HostTransport) -> EmbeddedConfig {
    let start = &outbound(lines, "thread/start")[0]["params"];
    let namespace = &start["dynamicTools"][0];
    EmbeddedConfig {
        transport,
        work_dir: start["cwd"].as_str().unwrap().into(),
        namespace: namespace["name"].as_str().unwrap().into(),
        namespace_description: namespace["description"].as_str().unwrap().into(),
        tools: namespace["tools"].to_string(),
        limits: LIMITS,
    }
}

pub fn options(lines: &[Line]) -> ThreadOptions {
    let start = &outbound(lines, "thread/start")[0]["params"];
    let text = |key: &str| start[key].as_str().map(str::to_owned);
    assert_eq!((&start["sandbox"], &start["approvalPolicy"]), (&Value::from("read-only"), &Value::from("never")));
    ThreadOptions {
        model: text("model"),
        provider: text("modelProvider"),
        base_instructions: text("baseInstructions"),
        policy: ThreadPolicy { sandbox: SandboxMode::ReadOnly, approval: ApprovalPolicy::Never },
    }
}

pub fn texts(lines: &[Line]) -> Vec<String> {
    outbound(lines, "turn/start").iter().map(|t| t["params"]["input"][0]["text"].as_str().unwrap().to_owned()).collect()
}

/// SWBPIPE's rule: whether an event ends the session.
pub fn ends_session(event: &EmbeddedEvent) -> bool {
    match event {
        EmbeddedEvent::RemoteControl { status } => *status != RemoteControl::Disabled,
        EmbeddedEvent::Notification { method, item, status, .. } => match method {
            Method::ThreadStarted | Method::TurnStarted | Method::TurnCompleted => false,
            Method::ThreadStatus => !matches!(status.as_deref(), Some("active" | "idle")),
            Method::ItemStarted | Method::ItemCompleted => {
                !matches!(item.as_ref().and_then(|i| i.item_type.as_deref()), Some("userMessage" | "agentMessage" | "reasoning" | "dynamicToolCall"))
            }
            Method::AgentMessageDelta | Method::ReasoningSummaryDelta | Method::ReasoningSummaryPart | Method::ReasoningDelta => false,
            Method::TokenUsage | Method::RateLimits | Method::Retrying => false,
            Method::ThreadClosed | Method::Error | Method::Warning | Method::ConfigWarning => true,
        },
        EmbeddedEvent::Refused { .. } | EmbeddedEvent::Unheld { .. } | EmbeddedEvent::Unmapped { .. } | EmbeddedEvent::Closed { .. } => true,
    }
}

/// An event's name, for comparing sequences.
pub fn name(event: &EmbeddedEvent) -> String {
    match event {
        EmbeddedEvent::Notification { method, item, status, .. } => {
            let detail = item.as_ref().and_then(|i| i.item_type.clone()).or(status.clone());
            format!("{method:?}{}", detail.map(|d| format!(" {d}")).unwrap_or_default())
        }
        other => format!("{other:?}").split_whitespace().next().unwrap().to_owned(),
    }
}

pub struct Host {
    pub to_engine: mpsc::Sender<String>,
    pub from_engine: mpsc::Receiver<String>,
}

pub fn transport() -> (HostTransport, Host) {
    let (to_engine, from_codex) = mpsc::channel(64);
    let (to_codex, from_engine) = mpsc::channel(64);
    (HostTransport { from_codex, to_codex }, Host { to_engine, from_engine })
}

/// How a run is measured; [`UNMEASURED`] when it is not.
pub struct Meter {
    /// Runs `f` as the host: outside what the engine is charged for.
    pub pause: fn(&mut dyn FnMut()),
    /// The engine owns this allocation, made as the host, from now on.
    pub adopt: fn(*const u8, usize),
    /// The engine has finished, and only its handles hold it.
    pub finished: fn(),
}

pub const UNMEASURED: Meter = Meter { pause: |f| f(), adopt: |_, _| {}, finished: || {} };

impl Meter {
    /// The configuration's strings, the engine's from `new`.
    pub fn adopt_config(&self, config: &EmbeddedConfig) {
        for text in [&config.tools, &config.namespace, &config.namespace_description] {
            (self.adopt)(text.as_ptr(), text.capacity());
        }
        (self.adopt)(config.work_dir.as_os_str().as_encoded_bytes().as_ptr(), config.work_dir.capacity());
    }

    /// Hands Codex's frame to the engine, and waits until the engine has
    /// taken it, so no more than one frame is counted waiting.
    pub async fn feed(&self, host: &Host, frame: String) -> bool {
        (self.adopt)(frame.as_ptr(), frame.capacity());
        let (mut frame, mut sent) = (Some(frame), false);
        (self.pause)(&mut || sent = frame.take().is_some_and(|frame| host.to_engine.try_send(frame).is_ok()));
        if sent {
            while host.to_engine.capacity() < host.to_engine.max_capacity() && !host.to_engine.is_closed() {
                tokio::task::yield_now().await;
            }
            for _ in 0..4 {
                tokio::task::yield_now().await;
            }
        }
        sent
    }
}

/// Drives the recorded session through the engine as SWBPIPE's host would:
/// connect, wait for `Disabled`, open the thread, send each recorded text
/// once the turn before has completed, then close. Codex's side is fed from
/// the recording; every frame the engine writes is checked against it.
/// Returns the events, each checked against SWBPIPE's rule.
pub async fn replay(lines: Vec<Line>, meter: &Meter) -> Vec<EmbeddedEvent> {
    let pause = meter.pause;
    let mut built = None;
    pause(&mut || {
        let (transport, host) = transport();
        built = Some((config(&lines, transport), options(&lines), texts(&lines), host));
    });
    let (config, options, texts, mut host) = built.unwrap();
    meter.adopt_config(&config);
    let (engine, mut events) = EmbeddedEngine::new(config).unwrap();
    let mut seen = Vec::new();
    let script = async {
        engine.connect().await.unwrap();
        let mut next = async || {
            let event = events.recv().await.unwrap();
            let ends = ends_session(&event);
            pause(&mut || assert!(!ends, "{event:?} ends the session"));
            let name = event_name(&event);
            pause(&mut || seen.push(event.clone()));
            name
        };
        while next().await != "remoteControl" {}
        let opened = engine.open_thread(&options).await.unwrap();
        assert_eq!(opened.ephemeral, Reported::Value(true));
        assert_eq!(opened.environments, Reported::Value(0));
        for text in &texts {
            engine.send_text(text).await.unwrap();
            while next().await != "turnCompleted" {}
        }
        engine.close();
        let closed = events.recv().await;
        assert_eq!(closed, Some(EmbeddedEvent::Closed { fault: None }));
        assert_eq!(events.recv().await, None);
        engine.finished().await;
        (meter.finished)();
    };
    let codex = async {
        for line in lines {
            match line {
                Line::Inbound(line) => assert!(meter.feed(&host, line).await),
                Line::Outbound(expected) => {
                    let sent = host.from_engine.recv().await.unwrap();
                    pause(&mut || assert_eq!(serde_json::from_str::<Value>(&sent).unwrap(), serde_json::from_str::<Value>(&expected).unwrap()));
                }
            }
        }
        assert_eq!(host.from_engine.recv().await, None, "close is the end of to_codex");
    };
    tokio::join!(script, codex);
    drop((engine, events, host));
    seen
}

/// A short, allocation-free name of an event's kind and method.
fn event_name(event: &EmbeddedEvent) -> &'static str {
    match event {
        EmbeddedEvent::RemoteControl { .. } => "remoteControl",
        EmbeddedEvent::Notification { method: Method::TurnCompleted, .. } => "turnCompleted",
        EmbeddedEvent::Notification { .. } => "notification",
        EmbeddedEvent::Refused { .. } => "refused",
        EmbeddedEvent::Unheld { .. } => "unheld",
        EmbeddedEvent::Unmapped { .. } => "unmapped",
        EmbeddedEvent::Closed { .. } => "closed",
    }
}
