//! Replays a session Codex recorded under SWBPIPE's configuration through the
//! embedded engine, as SWBPIPE's host would drive it, and SWBPIPE's rule.

#![allow(dead_code)]

use std::future::Future;
use std::path::Path;
use std::pin::Pin;
use std::task::{Context, Waker};

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
        EmbeddedEvent::Call { .. } => false,
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
    /// The engine has finished, its handles still hold it, and the host has
    /// let go of the engine's output.
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

/// A method the host has called and Codex has not yet answered.
type Pending<'e> = Pin<Box<dyn Future<Output = ()> + 'e>>;

/// Drives the recorded session through the engine as SWBPIPE's host would,
/// each action when the recording shows the client's next frame: connect,
/// open the thread, send each text, interrupt, and answer each call from its
/// `Call` event. Codex's side is fed from the recording, one frame taken
/// before the next; every frame the engine writes is checked against it, and
/// every event against SWBPIPE's rule. At the end, every call is retired and
/// answering it is `Invalid`; then the engine closes. Returns the events.
pub async fn replay(lines: Vec<Line>, meter: &Meter) -> Vec<EmbeddedEvent> {
    let pause = meter.pause;
    let mut built = None;
    pause(&mut || {
        let (transport, host) = transport();
        built = Some((config(&lines, transport), options(&lines), host));
    });
    let (config, options, mut host) = built.unwrap();
    meter.adopt_config(&config);
    let (engine, mut events) = EmbeddedEngine::new(config).unwrap();
    let (mut seen, mut pending) = (Vec::new(), Vec::<Pending>::new());
    for line in lines {
        match line {
            Line::Inbound(line) => assert!(meter.feed(&host, line).await),
            Line::Outbound(expected) => {
                let mut frame = Value::Null;
                pause(&mut || frame = serde_json::from_str(&expected).unwrap());
                if let Some(action) = act(&engine, &options, &frame, &seen, meter) {
                    pending.push(action);
                }
                poll_all(&mut pending);
                let sent = host.from_engine.try_recv().expect("the engine wrote the recorded frame");
                pause(&mut || assert_eq!(serde_json::from_str::<Value>(&sent).unwrap(), frame));
                assert_eq!(sent.len(), expected.len(), "the recorded frame, byte for byte in length");
            }
        }
        poll_all(&mut pending);
        while let Ok(event) = events.try_recv() {
            let ends = ends_session(&event);
            pause(&mut || assert!(!ends, "{event:?} ends the session"));
            pause(&mut || seen.push(event.clone()));
        }
    }
    assert!(pending.is_empty(), "every method was answered");
    drop(pending);
    for event in &seen {
        if let EmbeddedEvent::Call { call, .. } = event {
            assert!(!engine.call_live(call), "retired by the end");
            assert_eq!(engine.answer(call, true, "late"), Err(EmbeddedError::Invalid { reason: "the call is not live" }));
        }
    }
    engine.close();
    assert_eq!(events.recv().await, Some(EmbeddedEvent::Closed { fault: None }));
    assert_eq!(events.recv().await, None);
    engine.finished().await;
    assert_eq!(host.from_engine.recv().await, None, "nothing more was written, and close is the end of to_codex");
    // The host's output goes before the engine's holding is sampled; the
    // engine's handles stay.
    drop(host);
    (meter.finished)();
    drop((engine, events));
    seen
}

/// The host's action for the client's next recorded frame: a method called,
/// a call answered, or nothing (the engine writes `initialized` and
/// refusals itself).
fn act<'e>(engine: &'e EmbeddedEngine, options: &'e ThreadOptions, frame: &Value, seen: &[EmbeddedEvent], meter: &Meter) -> Option<Pending<'e>> {
    let mut action: Option<Pending<'e>> = None;
    let mut boxed = |future: Pending<'e>| action = Some(future);
    match frame["method"].as_str() {
        Some("initialize") => (meter.pause)(&mut || boxed(Box::pin(async { engine.connect().await.unwrap() }))),
        Some("thread/start") => (meter.pause)(&mut || {
            boxed(Box::pin(async {
                let opened = engine.open_thread(options).await.unwrap();
                assert_eq!((opened.ephemeral, opened.environments), (Reported::Value(true), Reported::Value(0)));
            }))
        }),
        Some("turn/start") => {
            let text = frame["params"]["input"][0]["text"].as_str().unwrap().to_owned();
            (meter.pause)(&mut || {
                let text = text.clone();
                boxed(Box::pin(async move {
                    engine.send_text(&text).await.unwrap();
                }))
            })
        }
        Some("turn/interrupt") => (meter.pause)(&mut || boxed(Box::pin(async { engine.interrupt().await.unwrap() }))),
        Some(_) => {}
        None if frame.get("result").is_some() => {
            let call = seen.iter().find_map(|e| match e {
                EmbeddedEvent::Call { call, .. } if rpc_id(&call.rpc_id) == frame["id"] => Some(call),
                _ => None,
            });
            let call = call.expect("the call answered was a Call event");
            assert!(engine.call_live(call), "the host checks the call is live");
            let result = &frame["result"];
            let text = result["contentItems"][0]["text"].as_str().unwrap();
            engine.answer(call, result["success"].as_bool().unwrap(), text).unwrap();
        }
        None => {}
    }
    action
}

fn rpc_id(id: &RpcId) -> Value {
    match id {
        RpcId::Integer(n) => Value::from(*n),
        RpcId::Text(text) => Value::from(text.as_str()),
    }
}

/// Polls each pending method once, dropping those Codex has answered.
fn poll_all(pending: &mut Vec<Pending>) {
    let mut cx = Context::from_waker(Waker::noop());
    pending.retain_mut(|future| future.as_mut().poll(&mut cx).is_pending());
}
