//! A scripted Codex for what the real one cannot be made to do on demand:
//! the test sends Codex's frames itself and reads what the engine answers.
//! The handshake, account and models are answered for it.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Value, json};
use tokio::sync::mpsc;

use crate::transport::{ConnectFuture, Connection, Connector, Stopper};

type Line = Arc<Mutex<Option<mpsc::UnboundedSender<String>>>>;

/// One connection, as the test drives it.
pub struct Wire {
    line: Line,
    from_engine: mpsc::UnboundedReceiver<Value>,
}

impl Wire {
    pub fn send(&self, frame: Value) {
        if let Some(line) = self.line.lock().unwrap().as_ref() {
            let _ = line.send(frame.to_string());
        }
    }

    /// Ends the connection as if Codex died.
    pub fn kill(&self) {
        self.line.lock().unwrap().take();
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
}

impl FakeCodex {
    /// The connector, and each connection it makes, in order.
    pub fn new() -> (Arc<FakeCodex>, mpsc::UnboundedReceiver<Wire>) {
        let (wires, rx) = mpsc::unbounded_channel();
        (Arc::new(FakeCodex { wires }), rx)
    }
}

struct Stop(Line);

impl Stopper for Stop {
    fn stop(&self) {
        self.0.lock().unwrap().take();
    }
    fn kill(&self) {
        self.0.lock().unwrap().take();
    }
}

impl Connector for FakeCodex {
    fn connect(&self) -> ConnectFuture {
        let (out_tx, mut out_rx) = mpsc::unbounded_channel::<String>();
        let (in_tx, in_rx) = mpsc::unbounded_channel::<String>();
        let (test_tx, test_rx) = mpsc::unbounded_channel::<Value>();
        let line: Line = Arc::new(Mutex::new(Some(in_tx)));
        let _ = self.wires.send(Wire { line: line.clone(), from_engine: test_rx });
        let answering = line.clone();
        tokio::spawn(async move {
            while let Some(text) = out_rx.recv().await {
                let frame: Value = serde_json::from_str(&text).expect("the engine sends JSON");
                let result = match frame["method"].as_str() {
                    Some("initialize") => Some(json!({ "userAgent": "chirality/0.171.0 (fake)" })),
                    Some("account/read") => Some(json!({ "account": null })),
                    Some("model/list") => Some(json!({ "data": [], "nextCursor": null })),
                    Some("initialized") => continue,
                    _ => None,
                };
                match result {
                    Some(result) => {
                        if let Some(line) = answering.lock().unwrap().as_ref() {
                            let _ = line.send(json!({ "id": frame["id"], "result": result }).to_string());
                        }
                    }
                    None => {
                        let _ = test_tx.send(frame);
                    }
                }
            }
        });
        Box::pin(async move { Ok(Connection { outgoing: out_tx, incoming: in_rx, stopper: Box::new(Stop(line)) }) })
    }
}
