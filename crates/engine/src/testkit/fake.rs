//! A scripted Codex for what the real one cannot be made to do on demand:
//! the test sends Codex's frames itself and reads what the engine answers.
//! The handshake, account and models are answered for it, unless held.

use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Value, json};
use tokio::sync::mpsc;

use crate::Engine;
use crate::api::{AppTools, EngineConfig};
use crate::transport::{ConnectFuture, Connection, Connector, Ended, Stopper};

/// The fake's end of the connection, and the mark that it has ended.
#[derive(Clone)]
struct Line {
    tx: Arc<Mutex<Option<mpsc::UnboundedSender<String>>>>,
    ended: Ended,
}

impl Line {
    fn lock(&self) -> std::sync::MutexGuard<'_, Option<mpsc::UnboundedSender<String>>> {
        self.tx.lock().unwrap()
    }

    /// Ends it as a transport would: marked first, while what was sent may still wait.
    fn end(&self) {
        self.ended.mark();
        self.lock().take();
    }
}

/// One connection, as the test drives it.
pub struct Wire {
    line: Line,
    from_engine: mpsc::UnboundedReceiver<Value>,
}

impl Wire {
    pub fn send(&self, frame: Value) {
        if let Some(line) = self.line.lock().as_ref() {
            let _ = line.send(frame.to_string());
        }
    }

    /// Ends the connection as if Codex died.
    pub fn kill(&self) {
        self.line.end();
    }

    /// Ends the connection with `frames` still queued behind its end, as the
    /// transport leaves lines it read before Codex's output ended.
    pub fn end_with(&self, frames: Vec<Value>) {
        self.line.ended.mark();
        if let Some(line) = self.line.lock().as_ref() {
            for frame in frames {
                let _ = line.send(frame.to_string());
            }
        }
        self.line.end();
    }

    /// Whether the engine has ended this Codex, or it died.
    pub fn is_ended(&self) -> bool {
        self.line.lock().is_none()
    }

    /// Reads the engine's next frame, which must be a `method` request, and answers it.
    pub async fn answer(&mut self, method: &str, result: Value) {
        let frame = self.next().await;
        assert_eq!(frame["method"], method, "{frame}");
        self.send(json!({ "id": frame["id"], "result": result }));
    }

    /// The next frame the engine sends, within a few seconds.
    pub async fn next(&mut self) -> Value {
        tokio::time::timeout(Duration::from_secs(5), self.from_engine.recv()).await.expect("a frame from the engine").expect("the connection is open")
    }

    /// Nothing more from the engine for a moment.
    pub async fn quiet(&mut self) {
        if let Ok(Some(frame)) = tokio::time::timeout(Duration::from_millis(300), self.from_engine.recv()).await {
            panic!("the engine sent {frame}");
        }
    }
}

pub struct FakeCodex {
    wires: mpsc::UnboundedSender<Wire>,
    /// Methods given to the test to answer instead.
    held: Arc<Mutex<HashSet<String>>>,
    /// How long a stopped connection's process takes to be reaped.
    reaping: Arc<Mutex<Duration>>,
}

impl FakeCodex {
    /// The connector, and each connection it makes, in order.
    pub fn new() -> (Arc<FakeCodex>, mpsc::UnboundedReceiver<Wire>) {
        let (wires, rx) = mpsc::unbounded_channel();
        (Arc::new(FakeCodex { wires, held: Arc::default(), reaping: Arc::default() }), rx)
    }

    /// From now on the test answers `method`, on every connection.
    pub fn hold(&self, method: &str) {
        self.held.lock().unwrap().insert(method.to_string());
    }

    /// From now on a stopped process takes `delay` to be reaped.
    pub fn reap_after(&self, delay: Duration) {
        *self.reaping.lock().unwrap() = delay;
    }
}

struct Stop(Line, Duration);

impl Stopper for Stop {
    fn stop(&self) {
        self.0.end();
    }
    fn kill(&self) {
        self.0.end();
    }
    fn reaped(&self) -> std::pin::Pin<Box<dyn Future<Output = ()> + Send>> {
        Box::pin(tokio::time::sleep(self.1))
    }
}

impl Connector for FakeCodex {
    fn connect(&self) -> ConnectFuture {
        let (out_tx, mut out_rx) = mpsc::unbounded_channel::<String>();
        let (in_tx, in_rx) = mpsc::unbounded_channel::<String>();
        let (test_tx, test_rx) = mpsc::unbounded_channel::<Value>();
        let line = Line { tx: Arc::new(Mutex::new(Some(in_tx))), ended: Ended::default() };
        let _ = self.wires.send(Wire { line: line.clone(), from_engine: test_rx });
        let answering = line.clone();
        let held = self.held.clone();
        tokio::spawn(async move {
            while let Some(text) = out_rx.recv().await {
                let frame: Value = serde_json::from_str(&text).expect("the engine sends JSON");
                let method = frame["method"].as_str().unwrap_or_default();
                let result = match method {
                    _ if held.lock().unwrap().contains(method) => None,
                    "initialize" => Some(json!({ "userAgent": "chirality/0.171.0 (fake)" })),
                    "account/read" => Some(json!({ "account": null })),
                    "model/list" => Some(json!({ "data": [], "nextCursor": null })),
                    // A thread's subagents, which these threads have none of.
                    "thread/list" if frame["params"]["ancestorThreadId"].is_string() => Some(json!({ "data": [], "nextCursor": null })),
                    "initialized" => continue,
                    _ => None,
                };
                match result {
                    Some(result) => {
                        if let Some(line) = answering.lock().as_ref() {
                            let _ = line.send(json!({ "id": frame["id"], "result": result }).to_string());
                        }
                    }
                    None => {
                        let _ = test_tx.send(frame);
                    }
                }
            }
        });
        let ended = line.ended.clone();
        let reaping = *self.reaping.lock().unwrap();
        Box::pin(async move { Ok(Connection { outgoing: out_tx, incoming: in_rx, stopper: Box::new(Stop(line, reaping)), ended }) })
    }
}

static RUN: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// An engine on the scripted Codex: the engine, the later connections, the
/// first one, the scripted Codex itself, and a project folder the engine
/// knows nothing of yet.
pub async fn start_engine(name: &str, tools: Option<AppTools>) -> (Engine, mpsc::UnboundedReceiver<Wire>, Wire, Arc<FakeCodex>, std::path::PathBuf) {
    let n = RUN.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    let root = std::env::temp_dir().join(format!("chirality-{name}-{}-{n}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    let config = EngineConfig {
        codex_bin: "codex".into(),
        codex_home: Some(root.join("home")),
        state_dir: root.join("state"),
        workflow_roots: Vec::new(),
        codex_settings: Vec::new(),
        tools,
    };
    let (connector, mut wires) = FakeCodex::new();
    let engine = Engine::start_with(config, connector.clone(), None).await.expect("the engine starts");
    let wire = wires.recv().await.expect("a connection");
    std::fs::create_dir_all(root.join("project")).unwrap();
    (engine, wires, wire, connector, root.join("project").canonicalize().unwrap())
}
