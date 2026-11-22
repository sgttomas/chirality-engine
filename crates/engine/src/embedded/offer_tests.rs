//! The namespace offer: under SWBPIPE's recorded configuration
//! (`fixtures/codex/swbpipe-namespace-offer.json`), the provider request
//! Codex makes for the embedded engine's thread offers the namespace only,
//! with exactly its declared tools. The request recorded from the pinned
//! Codex (`swbpipe-namespace-request.json`) is checked offline; with
//! `CHIRALITY_TEST_CODEX` set, a fresh one is made and compared with it.

use std::path::Path;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;
use tokio::sync::mpsc;

use super::*;
use crate::testkit::harness::{Run, codex, scratch};
use crate::testkit::stub::{Reply, Stub, message};

fn fixture(name: &str) -> Value {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/codex").join(name);
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

/// A provider request's `tools` offer the namespace only: no built-in, and
/// the namespace's declared tools exactly, in order. Any addition fails.
fn offers_the_namespace_only(tools: &Value, offer: &Value) {
    assert_eq!(offer["builtIns"], json!([]));
    let tools = tools.as_array().expect("tools");
    assert_eq!(tools.len(), 1, "one tool, the namespace: {tools:?}");
    let namespace = &tools[0];
    assert_eq!((&namespace["type"], &namespace["name"]), (&json!("namespace"), &offer["namespace"]));
    let declared: Vec<_> = namespace["tools"].as_array().expect("its tools").iter().map(|t| (t["type"].clone(), t["name"].clone())).collect();
    let expected: Vec<_> = offer["declaredTools"].as_array().unwrap().iter().map(|n| (json!("function"), n.clone())).collect();
    assert_eq!(declared, expected);
}

#[test]
fn the_recorded_request_offers_the_namespace_only() {
    offers_the_namespace_only(&fixture("swbpipe-namespace-request.json"), &fixture("swbpipe-namespace-offer.json"));
}

#[test]
fn any_addition_fails() {
    let offer = fixture("swbpipe-namespace-offer.json");
    let request = fixture("swbpipe-namespace-request.json");
    let mut built_in = request.clone();
    built_in.as_array_mut().unwrap().push(json!({"type": "function", "name": "exec_command"}));
    let mut tool = request.clone();
    tool[0]["tools"].as_array_mut().unwrap().push(json!({"type": "function", "name": "swbpipe_extra"}));
    let mut search = request;
    search.as_array_mut().unwrap().insert(0, json!({"type": "web_search"}));
    for added in [built_in, tool, search] {
        assert!(std::panic::catch_unwind(|| offers_the_namespace_only(&added, &offer)).is_err(), "{added}");
    }
}

const MODEL: &str = "gpt-6.2-sol";
const INSTRUCTIONS: &str = "You are the embedded agent.";

/// The stand-in tools the recording was made with: SWBPIPE's names.
fn stand_ins(offer: &Value) -> String {
    let tools: Vec<_> = offer["declaredTools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|name| {
            let name = name.as_str().unwrap();
            json!({"type": "function", "name": name, "description": format!("Stand-in for {name}."),
                   "inputSchema": {"type": "object", "properties": {}, "additionalProperties": false}})
        })
        .collect();
    Value::from(tools).to_string()
}

/// The session's root: the scratch Codex home with SWBPIPE's `config.toml`,
/// the catalogue, and the work folder.
fn session(root: &Path, offer: &Value, port: u16) -> (std::path::PathBuf, std::path::PathBuf) {
    let fill = |text: &str| {
        let pairs = [("<root>", root.to_str().unwrap()), ("<port>", &port.to_string()), ("<model>", MODEL), ("<window>", "128000"), ("<text>", INSTRUCTIONS)];
        pairs.iter().fold(text.to_owned(), |text, (key, value)| text.replace(key, value))
    };
    let entry: Value = serde_json::from_str(&fill(&offer["catalogueEntry"].to_string())).unwrap();
    std::fs::write(root.join("catalog.json"), json!({"models": [entry]}).to_string()).unwrap();
    std::fs::write(root.join("home/config.toml"), fill(offer["config"].as_str().unwrap())).unwrap();
    (root.join("home"), root.join("project"))
}

/// SWBPIPE's rule (as in `tests/support`): whether an event ends the session.
fn ends_session(event: &EmbeddedEvent) -> bool {
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

/// The next event, failing on one that ends the session.
async fn next(events: &mut EmbeddedEvents) -> EmbeddedEvent {
    let event = events.recv().await.expect("an event");
    assert!(!ends_session(&event), "an ending event: {event:?}");
    event
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_fresh_request_offers_what_was_recorded() {
    let Some(codex) = codex() else { return };
    let offer = fixture("swbpipe-namespace-offer.json");
    let stub = Stub::start().await;
    stub.script("offer", Arc::new(|_| Reply::Items(vec![message("msg_offer", "Done.")])));
    let run = Run::new(&scratch(), "embedded-offer");
    let (codex_home, work) = session(&run.root, &offer, stub.port);
    let dir = |name: &str| run.root.join(name);
    let mut child = Command::new("timeout")
        .args(["60".as_ref(), codex.as_os_str(), "app-server".as_ref(), "--strict-config".as_ref(), "--listen".as_ref(), "stdio://".as_ref()])
        .env_clear()
        .envs([("HOME", dir("userhome")), ("CODEX_HOME", codex_home), ("TMPDIR", dir("tmp")), ("PATH", "/usr/bin:/bin".into()), ("LANG", "en_US.UTF-8".into())])
        .envs([("SWBPIPE_AGENT_RELAY_CAPABILITY", "stub-capability"), ("CODEX_INTERNAL_APP_SERVER_REMOTE_CONTROL_DISABLED", "1")])
        .current_dir(&work)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .expect("the pinned Codex");
    let (to_engine, from_codex) = mpsc::channel(64);
    let (to_codex, mut from_engine) = mpsc::channel::<String>(64);
    let (mut stdin, stdout) = (child.stdin.take().unwrap(), child.stdout.take().unwrap());
    let reader = tokio::spawn(async move {
        let mut lines = BufReader::new(stdout).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            if to_engine.send(line).await.is_err() {
                break;
            }
        }
    });
    let writer = tokio::spawn(async move {
        while let Some(frame) = from_engine.recv().await {
            if stdin.write_all(format!("{frame}\n").as_bytes()).await.is_err() {
                break;
            }
        }
    });
    let config = EmbeddedConfig {
        transport: HostTransport { from_codex, to_codex },
        work_dir: work.clone(),
        namespace: offer["namespace"].as_str().unwrap().into(),
        namespace_description: "SWBPIPE tools.".into(),
        tools: stand_ins(&offer),
        limits: tests::LIMITS,
    };
    let (engine, mut events) = EmbeddedEngine::new(config).unwrap();
    let options = ThreadOptions {
        model: Some(MODEL.into()),
        provider: Some("swbpipe_relay".into()),
        base_instructions: Some(INSTRUCTIONS.into()),
        policy: ThreadPolicy { sandbox: SandboxMode::ReadOnly, approval: ApprovalPolicy::Never },
    };
    let turn = tokio::time::timeout(Duration::from_secs(60), async {
        engine.connect().await.unwrap();
        while next(&mut events).await != (EmbeddedEvent::RemoteControl { status: RemoteControl::Disabled }) {}
        engine.open_thread(&options).await.unwrap();
        engine.send_text("STUB[offer]").await.unwrap();
        loop {
            if let EmbeddedEvent::Notification { method: Method::TurnCompleted, status, .. } = next(&mut events).await {
                return status;
            }
        }
    })
    .await;
    engine.close();
    writer.abort();
    reader.abort();
    let exited = tokio::time::timeout(Duration::from_secs(10), child.wait()).await;
    if exited.is_err() {
        child.kill().await.unwrap();
    }
    assert_eq!(turn.expect("the turn ended in time").as_deref(), Some("completed"));
    // Every request the model saw, retries included.
    let bodies = std::mem::take(&mut *stub.bodies.lock().unwrap());
    assert!(!bodies.is_empty(), "a provider request");
    for body in &bodies {
        assert_eq!(body["tools"], fixture("swbpipe-namespace-request.json"), "the recorded request");
        offers_the_namespace_only(&body["tools"], &offer);
    }
    let _ = std::fs::remove_dir_all(&run.root);
}
