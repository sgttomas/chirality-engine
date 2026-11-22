//! The embedded engine over a scripted host: startup, the gates, the routes,
//! the turn rule, and close on every path.

use std::future::{Future, poll_fn};
use std::pin::pin;
use std::task::Poll;

use serde_json::{Value, json};
use tokio::sync::mpsc;

use super::*;

pub(super) const LIMITS: EmbeddedLimits =
    EmbeddedLimits { pending_calls: 16, calls_per_turn: 1024, argument_bytes: 65_536, result_bytes: 65_536, event_bytes: 256 * 1024 };
pub(super) const TOOLS: &str = r#"[{"type":"function","name":"swbpipe_solve","description":"Solve.","inputSchema":{"type":"object"}}]"#;
pub(super) const THREAD: &str = "thread-1";

pub(super) struct Host {
    pub(super) to_engine: Option<mpsc::Sender<String>>,
    pub(super) from_engine: mpsc::Receiver<String>,
}

impl Host {
    pub(super) async fn next(&mut self) -> Value {
        let frame = self.from_engine.recv().await.expect("a frame");
        assert!(!frame.ends_with('\n'));
        serde_json::from_str(&frame).unwrap()
    }

    pub(super) async fn send(&self, frame: impl ToString) {
        self.send_as_is(frame.to_string()).await;
    }

    pub(super) async fn send_as_is(&self, frame: String) {
        self.to_engine.as_ref().unwrap().send(frame).await.unwrap();
    }

    pub(super) fn reply(&self, id: &Value, result: Value) -> String {
        json!({"id": id, "result": result}).to_string()
    }
}

pub(super) fn config(transport: HostTransport) -> EmbeddedConfig {
    EmbeddedConfig {
        transport,
        work_dir: "/scratch/work".into(),
        namespace: "swbpipe".into(),
        namespace_description: "SWBPIPE tools.".into(),
        tools: TOOLS.into(),
        limits: LIMITS,
    }
}

pub(super) fn host_with(capacity: usize, edit: impl FnOnce(&mut EmbeddedConfig)) -> (EmbeddedEngine, EmbeddedEvents, Host) {
    let (to_engine, from_codex) = mpsc::channel(64);
    let (to_codex, from_engine) = mpsc::channel(capacity);
    let mut config = config(HostTransport { from_codex, to_codex });
    edit(&mut config);
    let (engine, events) = EmbeddedEngine::new(config).unwrap();
    (engine, events, Host { to_engine: Some(to_engine), from_engine })
}

pub(super) const OPTIONS: ThreadOptions = ThreadOptions {
    model: None,
    provider: None,
    base_instructions: None,
    policy: ThreadPolicy { sandbox: SandboxMode::ReadOnly, approval: ApprovalPolicy::Never },
};

pub(super) fn policy() -> ThreadOptions {
    OPTIONS
}

/// Polls `future` once.
pub(super) async fn poll_once<F: Future>(future: Pin<&mut F>) -> Poll<F::Output> {
    let mut future = Some(future);
    poll_fn(|cx| Poll::Ready(future.take().unwrap().poll(cx))).await
}

pub(super) async fn settle() {
    for _ in 0..8 {
        tokio::task::yield_now().await;
    }
}

/// Connected, remote control `Disabled`, and the thread held.
pub(super) async fn opened() -> (EmbeddedEngine, EmbeddedEvents, Host) {
    let (engine, mut events, mut host) = host_with(64, |_| {});
    let ((), opened) = tokio::join!(async { engine.connect().await.unwrap() }, async {
        let init = host.next().await;
        host.send(host.reply(&init["id"], json!({}))).await;
        host.send(json!({"method": "remoteControl/status/changed", "params": {"status": "disabled"}})).await;
        assert_eq!(host.next().await, json!({"method": "initialized"}));
    });
    let () = opened;
    let (thread, ()) = tokio::join!(engine.open_thread(&OPTIONS), async {
        let start = host.next().await;
        host.send(host.reply(&start["id"], json!({"thread": {"id": THREAD}}))).await;
    });
    assert_eq!(thread.unwrap().thread.0, THREAD);
    assert_eq!(events.recv().await, Some(EmbeddedEvent::RemoteControl { status: RemoteControl::Disabled }));
    (engine, events, host)
}

/// A turn running on the held thread.
pub(super) async fn running(engine: &EmbeddedEngine, host: &mut Host, turn: &str) {
    let (started, ()) = tokio::join!(engine.send_text("hi"), async {
        let start = host.next().await;
        assert_eq!(start["params"], json!({"threadId": THREAD, "input": [{"type": "text", "text": "hi", "text_elements": []}]}));
        host.send(host.reply(&start["id"], json!({"turn": {"id": turn, "status": "inProgress"}}))).await;
    });
    assert_eq!(started.unwrap().0, turn);
}

pub(super) fn note(method: &str, params: Value) -> String {
    json!({"method": method, "params": params}).to_string()
}

pub(super) async fn closed_with(events: &mut EmbeddedEvents) -> Option<Fault> {
    loop {
        match events.recv().await.expect("Closed before None") {
            EmbeddedEvent::Closed { fault } => return fault,
            _ => continue,
        }
    }
}

#[test]
fn the_handles_cross_tasks() {
    fn send_sync<T: Send + Sync>() {}
    fn finished<T: Future<Output = ()> + Clone + Send + 'static>() {}
    send_sync::<EmbeddedEngine>();
    send_sync::<EmbeddedEvents>();
    finished::<Finished>();
}

#[test]
fn an_event_fits_its_ring_slot() {
    assert!(std::mem::size_of::<EmbeddedEvent>() <= budget::RING_SLOT as usize, "{}", std::mem::size_of::<EmbeddedEvent>());
}

#[test]
fn events_serialize_tagged_and_camel_case() {
    let event = EmbeddedEvent::Notification {
        method: Method::ItemStarted,
        turn: Some(TurnId("t".into())),
        item: Some(ItemRef { id: "i".into(), item_type: Some("agentMessage".into()) }),
        status: None,
        json: "{}".into(),
    };
    let expected =
        json!({"kind": "notification", "method": "itemStarted", "turn": "t", "item": {"id": "i", "itemType": "agentMessage"}, "status": null, "json": "{}"});
    assert_eq!(serde_json::to_value(&event).unwrap(), expected);
    assert_eq!(serde_json::to_value(EmbeddedEvent::Closed { fault: Some(Fault::WrongTurn) }).unwrap(), json!({"kind": "closed", "fault": "wrongTurn"}));
}

#[test]
fn new_refuses_what_codex_or_the_bounds_would_and_keeps_nothing() {
    let refused = |edit: &dyn Fn(&mut EmbeddedConfig)| {
        let (_, from_codex) = mpsc::channel(1);
        let (to_codex, _) = mpsc::channel(1);
        let mut config = config(HostTransport { from_codex, to_codex });
        edit(&mut config);
        matches!(EmbeddedEngine::new(config), Err(EmbeddedError::Invalid { .. }))
    };
    assert!(!refused(&|_| {}));
    let namespaces = ["", "mcp", "mcp__x", "web", "python_user_visible", "a b", "a.b", &"n".repeat(65)];
    for namespace in namespaces {
        assert!(refused(&|c| c.namespace = namespace.into()), "{namespace:?}");
    }
    assert!(!refused(&|c| c.namespace = "mcpx".into()));
    assert!(refused(&|c| c.namespace = String::with_capacity(65) + "swbpipe"), "a short string with a large capacity");
    assert!(!refused(&|c| c.namespace_description = "é".repeat(1024)));
    assert!(refused(&|c| c.namespace_description = "d".repeat(1025)));
    assert!(refused(&|c| c.namespace_description = String::with_capacity(4097)));
    let tool = |name: &str| format!("{{\"name\":\"{name}\",\"inputSchema\":{{}}}}");
    let many = |n: usize| format!("[{}]", (0..n).map(|i| tool(&format!("t{i}"))).collect::<Vec<_>>().join(","));
    assert!(!refused(&|c| c.tools = many(64)));
    let bad_tools = [
        "[]".to_string(),
        many(65),
        format!("[{},{}]", tool("a"), tool("a")),
        format!("[{}]", tool("mcp__a")),
        format!("[{}]", tool("mcpLookup")),
        format!("[\n  {}\n]", tool("a")),
        format!("[{}\r]", tool("a")),
        format!("[{{\"name\":\"a\",\"x\":{}{}}}]", "[".repeat(wire::TOOLS_DEPTH - 1), "]".repeat(wire::TOOLS_DEPTH - 1)),
        format!("[{}]", tool(&"a".repeat(129))),
        "{}".into(),
        "[1]".into(),
        "[{\"name\":1}]".into(),
        "[".into(),
    ];
    for tools in bad_tools {
        assert!(refused(&|c| c.tools = tools.clone()), "{tools}");
    }
    assert!(!refused(&|c| c.tools = format!("[{}]", tool("\\u0061b"))), "a name is read unescaped");
    assert!(!refused(&|c| c.tools = format!("[ {}\t]", tool("a"))), "spaces and tabs keep it on one line");
    let deepest = format!("[{{\"name\":\"a\",\"x\":{}{}}}]", "[".repeat(wire::TOOLS_DEPTH - 2), "]".repeat(wire::TOOLS_DEPTH - 2));
    assert!(!refused(&|c| c.tools = deepest.clone()), "tools at the depth thread/start leaves them");
    assert!(refused(&|c| c.tools = String::with_capacity(128 * 1024 + 1) + TOOLS));
    for work_dir in ["relative", "", &format!("/{}", "w".repeat(1024))] {
        assert!(refused(&|c| c.work_dir = work_dir.into()), "{work_dir}");
    }
    let limits: [fn(&mut EmbeddedLimits); 10] = [
        |l| l.pending_calls = 0,
        |l| l.pending_calls = 17,
        |l| l.calls_per_turn = 0,
        |l| l.calls_per_turn = 1025,
        |l| l.argument_bytes = 0,
        |l| l.argument_bytes = 65_537,
        |l| l.result_bytes = 0,
        |l| l.result_bytes = 65_537,
        |l| l.event_bytes = 256 * 1024 - 1,
        |l| l.event_bytes = 16 * 1024 * 1024 + 1,
    ];
    for limit in limits {
        assert!(refused(&|c| limit(&mut c.limits)));
    }
    assert_eq!(EmbeddedEngine::max_bytes(&LIMITS), embedded_max_bytes(&LIMITS));
}

#[tokio::test]
async fn connect_writes_initialize_once_even_after_a_cancelled_call() {
    let (engine, _events, mut host) = host_with(8, |_| {});
    {
        let connect = pin!(engine.connect());
        assert!(poll_once(connect).await.is_pending());
    }
    let init = host.next().await;
    assert_eq!(
        init,
        json!({"id": 1, "method": "initialize", "params": {"clientInfo": {"name": "chirality-embedded", "version": env!("CARGO_PKG_VERSION")}, "capabilities": {"experimentalApi": true}}})
    );
    assert!(matches!(engine.connect().await, Err(EmbeddedError::Invalid { .. })));
    assert!(matches!(engine.open_thread(&policy()).await, Err(EmbeddedError::Invalid { reason: "already in flight" | "not connected" })));
    host.send(host.reply(&init["id"], json!({}))).await;
    assert_eq!(host.next().await, json!({"method": "initialized"}), "written when the reply is applied, though its caller left");
}

#[tokio::test]
async fn open_thread_reports_what_codex_did() {
    let (engine, _events, mut host) = host_with(8, |_| {});
    let ((), ()) = tokio::join!(async { engine.connect().await.unwrap() }, async {
        let init = host.next().await;
        host.send(host.reply(&init["id"], json!({}))).await;
        host.next().await;
    });
    let long_cwd = format!("/{}", "c".repeat(1024));
    let options = ThreadOptions { model: Some("m".into()), provider: Some("p".into()), base_instructions: Some("b".into()), ..policy() };
    let (opened, ()) = tokio::join!(engine.open_thread(&options), async {
        let start = host.next().await;
        let params = &start["params"];
        assert_eq!(
            (&params["model"], &params["modelProvider"], &params["baseInstructions"], &params["cwd"]),
            (&json!("m"), &json!("p"), &json!("b"), &json!("/scratch/work"))
        );
        assert_eq!(
            (&params["sandbox"], &params["approvalPolicy"], &params["ephemeral"], &params["environments"]),
            (&json!("read-only"), &json!("never"), &json!(true), &json!([]))
        );
        assert_eq!(
            params["dynamicTools"],
            json!([{"type": "namespace", "name": "swbpipe", "description": "SWBPIPE tools.", "tools": serde_json::from_str::<Value>(TOOLS).unwrap()}])
        );
        let reply = format!(
            r#"{{"id":{},"result":{{"thread":{{"id":"th\"1","ephemeral":true,"environments":[{{}},{{}}]}},"approvalPolicy":"ne~u0076er","sandbox":{{"type":"readOnly","networkAccess":false}},"model":"m~u00e9","modelProvider":null,"cwd":"{long_cwd}"}}}}"#,
            start["id"]
        )
        .replace('~', "\\");
        host.send(reply).await;
    });
    let opened = opened.unwrap();
    assert_eq!(opened.thread.0, "th\"1");
    assert_eq!(opened.approval_policy, Reported::Other, "a constant with an escape");
    assert_eq!((opened.sandbox_type, opened.network_access), (Reported::Value("readOnly".into()), Reported::Value(false)));
    assert_eq!((opened.ephemeral, opened.environments), (Reported::Value(true), Reported::Value(2)));
    assert_eq!((opened.model, opened.model_provider, opened.cwd), (Reported::Value("mé".into()), Reported::Absent, Reported::Other));
    assert!(matches!(engine.open_thread(&policy()).await, Err(EmbeddedError::Invalid { .. })));
}

/// Connected, before a thread is held.
pub(super) async fn connected(capacity: usize) -> (EmbeddedEngine, EmbeddedEvents, Host) {
    let (engine, events, mut host) = host_with(capacity, |_| {});
    let ((), ()) = tokio::join!(async { engine.connect().await.unwrap() }, async {
        let init = host.next().await;
        host.send(host.reply(&init["id"], json!({}))).await;
        host.next().await;
    });
    (engine, events, host)
}

#[tokio::test]
async fn policy_facts_are_absent_other_or_known() {
    let other = Reported::<String>::Other;
    let cases = [
        (json!(null), json!(null), Reported::Absent, Reported::Absent, Reported::Absent),
        (json!("readOnly"), json!("never"), other.clone(), Reported::Other, Reported::Value("never".to_string())),
        (json!({"type": "futureSandbox", "networkAccess": true}), json!("futurePolicy"), other.clone(), Reported::Value(true), other.clone()),
        (json!({"type": "externalSandbox"}), json!({"granular": {}}), Reported::Value("externalSandbox".to_string()), Reported::Absent, other.clone()),
    ];
    for (sandbox, approval, kind, network, policy) in cases {
        let (engine, _events, mut host) = connected(8).await;
        let (opened, ()) = tokio::join!(engine.open_thread(&OPTIONS), async {
            let start = host.next().await;
            host.send(host.reply(&start["id"], json!({"thread": {"id": "t"}, "sandbox": sandbox, "approvalPolicy": approval}))).await;
        });
        let opened = opened.unwrap();
        assert_eq!((opened.sandbox_type, opened.network_access, opened.approval_policy), (kind, network, policy));
    }
}

#[tokio::test]
async fn a_thread_start_reply_without_an_identity_faults() {
    let (engine, mut events, mut host) = host_with(8, |_| {});
    let ((), ()) = tokio::join!(async { engine.connect().await.unwrap() }, async {
        let init = host.next().await;
        host.send(host.reply(&init["id"], json!({}))).await;
        host.next().await;
    });
    let (opened, ()) = tokio::join!(engine.open_thread(&OPTIONS), async {
        let start = host.next().await;
        host.send(host.reply(&start["id"], json!({"thread": {"id": "a\u{1}"}}))).await;
    });
    assert_eq!(opened, Err(EmbeddedError::Closed { fault: Some(Fault::InvalidFrame) }));
    assert_eq!(closed_with(&mut events).await, Some(Fault::InvalidFrame));
    assert_eq!(events.recv().await, None);
}

#[tokio::test]
async fn send_text_waits_for_remote_control_disabled() {
    let (engine, mut events, mut host) = opened().await;
    host.send(note("remoteControl/status/changed", json!({"status": "connecting"}))).await;
    assert_eq!(events.recv().await, Some(EmbeddedEvent::RemoteControl { status: RemoteControl::Connecting }));
    assert_eq!(engine.remote_control(), Some(RemoteControl::Connecting));
    assert_eq!(engine.send_text("hi").await, Err(EmbeddedError::Invalid { reason: "remote control is not disabled" }));
    host.send(note("remoteControl/status/changed", json!({"status": "pairing"}))).await;
    assert_eq!(events.recv().await, Some(EmbeddedEvent::RemoteControl { status: RemoteControl::Other }));
    host.send(note("remoteControl/status/changed", json!({"status": "disabled"}))).await;
    events.recv().await;
    running(&engine, &mut host, "turn-1").await;
    assert_eq!(engine.send_text("again").await, Err(EmbeddedError::Invalid { reason: "a turn runs" }), "no steering, no queue");
    host.send(note("turn/completed", json!({"threadId": THREAD, "turn": {"id": "turn-1", "status": "completed"}}))).await;
    settle().await;
    running(&engine, &mut host, "turn-2").await;
}

#[tokio::test]
async fn the_turn_rule() {
    let (engine, mut events, mut host) = opened().await;
    // With a `turn/start` in flight, the first new turn is admitted; it ended
    // before the reply, so it never runs.
    let mut send = pin!(engine.send_text("hi"));
    assert!(poll_once(send.as_mut()).await.is_pending());
    let start = host.next().await;
    host.send(note("turn/started", json!({"threadId": THREAD, "turn": {"id": "new", "status": "inProgress"}}))).await;
    host.send(note("turn/completed", json!({"threadId": THREAD, "turn": {"id": "new", "status": "completed"}}))).await;
    host.send(host.reply(&start["id"], json!({"turn": {"id": "new", "status": "inProgress"}}))).await;
    assert_eq!(send.await.unwrap().0, "new");
    for method in [Method::TurnStarted, Method::TurnCompleted] {
        assert!(matches!(events.recv().await, Some(EmbeddedEvent::Notification { method: m, .. }) if m == method));
    }
    // The last ended turn is still admitted.
    host.send(note("thread/tokenUsage/updated", json!({"threadId": THREAD, "turnId": "new"}))).await;
    assert!(matches!(events.recv().await, Some(EmbeddedEvent::Notification { method: Method::TokenUsage, .. })));
    running(&engine, &mut host, "second").await;
    // Any other turn faults.
    host.send(note("item/agentMessage/delta", json!({"threadId": THREAD, "turnId": "stray", "itemId": "i"}))).await;
    assert_eq!(closed_with(&mut events).await, Some(Fault::WrongTurn));
}

#[tokio::test]
async fn a_turn_completed_still_in_progress_faults() {
    let (engine, mut events, mut host) = opened().await;
    running(&engine, &mut host, "t").await;
    host.send(note("turn/completed", json!({"threadId": THREAD, "turn": {"id": "t", "status": "inProgress"}}))).await;
    assert_eq!(closed_with(&mut events).await, Some(Fault::InvalidFrame));
}

#[tokio::test]
async fn the_routes_in_order() {
    let (engine, mut events, mut host) = opened().await;
    running(&engine, &mut host, "t").await;
    let frames = [
        note("thread/status/changed", json!({"threadId": "other", "status": {"type": "active"}})),
        note("thread/status/changed", json!({"threadId": THREAD, "conversationId": "other", "status": {"type": "active"}})),
        note("thread/status/changed", json!({"threadId": THREAD, "status": {"type": "active"}})),
        note("item/started", json!({"threadId": THREAD, "turnId": "t", "item": {"id": "i\"1", "type": "agentMessage"}})),
        note("item/started", json!({"threadId": THREAD, "turnId": "t", "item": {"id": "i", "type": "comm\\u0061nd"}})).replace("\\\\u", "\\u"),
        note("item/reasoning/summaryTextDelta", json!({"threadId": THREAD, "turnId": "t", "itemId": "r"})),
        note("error", json!({"threadId": THREAD, "turnId": "t", "willRetry": true})),
        note("error", json!({"threadId": THREAD, "turnId": "t", "willRetry": false})),
        note("warning", json!({"threadId": null, "message": "w"})),
        note("configWarning", json!({})),
        note("account/rateLimits/updated", json!({"rateLimits": {}})),
        r#"{"method":"turn\/started","params":{"threadId":"thread-1","turn":{"id":"t","status":"inProgress"}}}"#.to_string(),
        note("item/fileChange/outputDelta", json!({"threadId": THREAD, "turnId": "t"})),
        r#"{"method":"rawResponseItem/completed"}"#.to_string(),
        note("thread/closed", json!({"threadId": THREAD})),
    ];
    for frame in &frames {
        host.send(frame).await;
    }
    let mut seen = Vec::new();
    for _ in &frames {
        seen.push(match events.recv().await.unwrap() {
            EmbeddedEvent::Unheld { method, thread, .. } => format!("unheld {method} {thread}"),
            EmbeddedEvent::Unmapped { method, thread, .. } => {
                format!("unmapped {method} {thread:?}")
            }
            EmbeddedEvent::Notification { method, turn, item, status, json } => {
                serde_json::from_str::<Value>(&json).unwrap();
                format!("{method:?} {:?} {:?} {status:?}", turn.map(|t| t.0), item.map(|i| (i.id, i.item_type)))
            }
            other => panic!("{other:?}"),
        });
    }
    assert_eq!(
        seen,
        [
            "unheld thread/status/changed other",
            "unheld thread/status/changed other",
            "ThreadStatus None None Some(\"active\")",
            "ItemStarted Some(\"t\") Some((\"i\\\"1\", Some(\"agentMessage\"))) None",
            "unmapped item/started Some(\"thread-1\")",
            "ReasoningSummaryDelta Some(\"t\") Some((\"r\", None)) None",
            "Retrying Some(\"t\") None None",
            "Error Some(\"t\") None None",
            "Warning None None None",
            "ConfigWarning None None None",
            "RateLimits None None None",
            "unmapped turn/started Some(\"thread-1\")",
            "unmapped item/fileChange/outputDelta Some(\"thread-1\")",
            "unmapped rawResponseItem/completed None",
            "ThreadClosed None None None",
        ]
    );
    assert_eq!(engine.send_text("hi").await, Err(EmbeddedError::Invalid { reason: "no open thread" }));
}

#[tokio::test]
async fn before_a_thread_is_held_any_thread_is_unheld() {
    let (engine, mut events, mut host) = host_with(8, |_| {});
    let ((), ()) = tokio::join!(async { engine.connect().await.unwrap() }, async {
        let init = host.next().await;
        host.send(host.reply(&init["id"], json!({}))).await;
        host.next().await;
    });
    host.send(note("thread/started", json!({"thread": {"id": "early"}}))).await;
    assert!(matches!(events.recv().await, Some(EmbeddedEvent::Unheld { thread, .. }) if thread == "early"));
}

#[tokio::test]
async fn requests_are_refused_and_answered_so() {
    let (engine, mut events, mut host) = opened().await;
    running(&engine, &mut host, "t").await;
    host.send(json!({"id": 0, "method": "item/tool/call", "params": {"threadId": THREAD, "turnId": "t", "callId": "c"}})).await;
    host.send(r#"{"id":"r\"x","method":"item/commandExecution/requestApproval"}"#).await;
    assert_eq!(host.next().await, json!({"id": 0, "error": {"code": -32601, "message": "not supported by this client"}}));
    assert_eq!(host.next().await, json!({"id": "r\"x", "error": {"code": -32601, "message": "not supported by this client"}}));
    let refused = events.recv().await.unwrap();
    assert!(matches!(&refused, EmbeddedEvent::Refused { method, thread: Some(thread), .. } if method == "item/tool/call" && thread == THREAD), "{refused:?}");
    assert_eq!(events.recv().await, Some(EmbeddedEvent::Refused { method: "item/commandExecution/requestApproval".into(), thread: None, json: "{}".into() }));
}

#[tokio::test]
async fn codex_errors_are_unescaped_and_bounded() {
    let (engine, mut events, mut host) = opened().await;
    let (sent, ()) = tokio::join!(engine.send_text("hi"), async {
        let start = host.next().await;
        host.send(format!(r#"{{"id":{},"error":{{"code":-32600,"message":"no \"turn\"\n"}}}}"#, start["id"])).await;
    });
    assert_eq!(sent, Err(EmbeddedError::Codex { code: -32600, message: "no \"turn\"\n".into() }));
    let (sent, ()) = tokio::join!(engine.send_text("hi"), async {
        let start = host.next().await;
        host.send(json!({"id": start["id"], "error": {"code": 1, "message": "m".repeat(1025)}})).await;
    });
    assert_eq!(sent, Err(EmbeddedError::Closed { fault: Some(Fault::InvalidFrame) }));
    assert_eq!(closed_with(&mut events).await, Some(Fault::InvalidFrame));
}

#[tokio::test]
async fn a_reply_waits_in_its_slot_and_a_dropped_caller_keeps_it_in_flight() {
    let (engine, _events, mut host) = opened().await;
    running(&engine, &mut host, "t").await;
    // Collected late: in flight until taken.
    let mut stop = pin!(engine.interrupt());
    assert!(poll_once(stop.as_mut()).await.is_pending());
    let interrupt = host.next().await;
    assert_eq!(interrupt["params"], json!({"threadId": THREAD, "turnId": "t"}));
    host.send(host.reply(&interrupt["id"], json!({}))).await;
    settle().await;
    for _ in 0..3 {
        assert_eq!(engine.interrupt().await, Err(EmbeddedError::Invalid { reason: "already in flight" }));
    }
    assert_eq!(stop.await, Ok(()));
    // Dropped before its reply: still in flight until the reply is discarded.
    {
        let stop = pin!(engine.interrupt());
        assert!(poll_once(stop).await.is_pending());
    }
    let interrupt = host.next().await;
    assert_eq!(engine.interrupt().await, Err(EmbeddedError::Invalid { reason: "already in flight" }));
    host.send(host.reply(&interrupt["id"], json!({}))).await;
    settle().await;
    let (stopped, ()) = tokio::join!(engine.interrupt(), async {
        let interrupt = host.next().await;
        host.send(host.reply(&interrupt["id"], json!({}))).await;
    });
    assert_eq!(stopped, Ok(()));
}

#[tokio::test]
async fn a_response_not_in_flight_faults() {
    let (_engine, mut events, host) = opened().await;
    host.send(json!({"id": 99, "result": {}})).await;
    assert_eq!(closed_with(&mut events).await, Some(Fault::InvalidFrame));
}

#[tokio::test]
async fn any_notification_naming_a_turn_follows_the_rule() {
    let (engine, mut events, mut host) = opened().await;
    running(&engine, &mut host, "t").await;
    host.send(note("turn/plan/updated", json!({"threadId": THREAD, "turnId": "t"}))).await;
    assert!(matches!(events.recv().await, Some(EmbeddedEvent::Unmapped { .. })), "an admitted turn, unlisted");
    host.send(note("turn/plan/updated", json!({"threadId": THREAD, "turnId": "stray"}))).await;
    assert_eq!(closed_with(&mut events).await, Some(Fault::WrongTurn));
    let (engine, mut events, mut host) = opened().await;
    running(&engine, &mut host, "t").await;
    host.send(note("turn/diff/updated", json!({"threadId": THREAD, "turn": {"id": "stray"}}))).await;
    assert_eq!(closed_with(&mut events).await, Some(Fault::WrongTurn));
}

#[tokio::test]
async fn a_turn_start_reply_naming_another_turn_than_the_one_seen_faults() {
    let (engine, mut events, mut host) = opened().await;
    let (sent, ()) = tokio::join!(engine.send_text("hi"), async {
        let start = host.next().await;
        host.send(note("turn/started", json!({"threadId": THREAD, "turn": {"id": "seen", "status": "inProgress"}}))).await;
        host.send(host.reply(&start["id"], json!({"turn": {"id": "other", "status": "inProgress"}}))).await;
    });
    assert_eq!(sent, Err(EmbeddedError::Closed { fault: Some(Fault::WrongTurn) }));
    assert_eq!(closed_with(&mut events).await, Some(Fault::WrongTurn));
}

#[tokio::test]
async fn a_thread_closed_before_the_turn_start_reply_admits_no_turn() {
    let (engine, mut events, mut host) = opened().await;
    let (sent, ()) = tokio::join!(engine.send_text("hi"), async {
        let start = host.next().await;
        host.send(note("thread/closed", json!({"threadId": THREAD}))).await;
        host.send(host.reply(&start["id"], json!({"turn": {"id": "t", "status": "inProgress"}}))).await;
    });
    assert_eq!(sent.unwrap().0, "t");
    assert!(matches!(events.recv().await, Some(EmbeddedEvent::Notification { method: Method::ThreadClosed, .. })));
    assert_eq!(engine.interrupt().await, Err(EmbeddedError::Invalid { reason: "no open thread" }));
    assert_eq!(engine.send_text("hi").await, Err(EmbeddedError::Invalid { reason: "no open thread" }));
}

#[tokio::test]
async fn interrupt_is_invalid_once_the_thread_closed_mid_turn() {
    let (engine, mut events, mut host) = opened().await;
    running(&engine, &mut host, "t").await;
    host.send(note("thread/closed", json!({"threadId": THREAD}))).await;
    events.recv().await;
    assert_eq!(engine.interrupt().await, Err(EmbeddedError::Invalid { reason: "no open thread" }));
    assert!(host.from_engine.try_recv().is_err(), "nothing written for the revoked thread");
}
