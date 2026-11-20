//! Line-delimited JSON-RPC over a [`Connection`]: requests with ids,
//! notifications, and the server's own requests, which are always answered.

use std::collections::HashMap;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex};

use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use tokio::sync::{mpsc, oneshot};

use crate::transport::Connection;

/// Which way a frame went, for the frame tap.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Direction {
    Out,
    In,
}

/// Sees every frame, in order, as raw lines. The recorder is built on it.
pub(crate) type Tap = Arc<dyn Fn(Direction, &str) + Send + Sync>;

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum RpcError {
    /// The connection ended before the answer came.
    Closed,
    /// Codex answered with an error.
    Server { code: i64, message: String },
    /// The answer did not have the expected shape.
    Decode(String),
}

impl std::fmt::Display for RpcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RpcError::Closed => write!(f, "Codex stopped"),
            RpcError::Server { message, .. } => write!(f, "{message}"),
            RpcError::Decode(message) => write!(f, "unexpected answer from Codex: {message}"),
        }
    }
}

/// What Codex sends of its own accord.
#[derive(Clone, Debug)]
pub(crate) enum Incoming {
    Notification { method: String, params: Value },
    /// Codex waits for [`RpcClient::respond`] with this `id`.
    Request { id: Value, method: String, params: Value },
}

type Reply = oneshot::Sender<Result<Value, RpcError>>;

pub(crate) struct RpcClient {
    out: Mutex<Option<mpsc::UnboundedSender<String>>>,
    next_id: AtomicI64,
    pending: Arc<Mutex<HashMap<i64, Reply>>>,
    tap: Option<Tap>,
}

impl RpcClient {
    /// Starts reading the connection. The returned receiver ends when Codex does.
    pub fn start(connection: Connection, tap: Option<Tap>) -> (Arc<RpcClient>, mpsc::UnboundedReceiver<Incoming>, Box<dyn crate::transport::Stopper>) {
        let Connection { outgoing, mut incoming, stopper } = connection;
        let pending: Arc<Mutex<HashMap<i64, Reply>>> = Arc::default();
        let client = Arc::new(RpcClient { out: Mutex::new(Some(outgoing)), next_id: AtomicI64::new(1), pending: pending.clone(), tap: tap.clone() });
        let (events_tx, events_rx) = mpsc::unbounded_channel();
        tokio::spawn(async move {
            while let Some(line) = incoming.recv().await {
                if let Some(tap) = &tap {
                    tap(Direction::In, &line);
                }
                let Ok(frame) = serde_json::from_str::<Value>(&line) else { continue };
                route(frame, &pending, &events_tx);
            }
            for (_, reply) in pending.lock().unwrap().drain() {
                let _ = reply.send(Err(RpcError::Closed));
            }
        });
        (client, events_rx, stopper)
    }

    pub async fn request<R: DeserializeOwned>(&self, method: &str, params: impl Serialize) -> Result<R, RpcError> {
        let value = self.request_value(method, params).await?;
        serde_json::from_value(value).map_err(|e| RpcError::Decode(format!("{method}: {e}")))
    }

    pub async fn request_value(&self, method: &str, params: impl Serialize) -> Result<Value, RpcError> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = oneshot::channel();
        self.pending.lock().unwrap().insert(id, tx);
        let _guard = Remove(&self.pending, id);
        let params = serde_json::to_value(params).map_err(|e| RpcError::Decode(e.to_string()))?;
        self.send(json!({ "id": id, "method": method, "params": params }))?;
        rx.await.unwrap_or(Err(RpcError::Closed))
    }

    pub fn notify(&self, method: &str, params: Option<Value>) -> Result<(), RpcError> {
        let mut frame = json!({ "method": method });
        if let Some(params) = params {
            frame["params"] = params;
        }
        self.send(frame)
    }

    pub fn respond(&self, id: Value, result: Value) -> Result<(), RpcError> {
        self.send(json!({ "id": id, "result": result }))
    }

    pub fn respond_error(&self, id: Value, code: i64, message: &str) -> Result<(), RpcError> {
        self.send(json!({ "id": id, "error": { "code": code, "message": message } }))
    }

    /// Closes Codex's input, which asks it to end.
    pub fn close(&self) {
        self.out.lock().unwrap().take();
    }

    fn send(&self, frame: Value) -> Result<(), RpcError> {
        let line = frame.to_string();
        let out = self.out.lock().unwrap();
        let sender = out.as_ref().ok_or(RpcError::Closed)?;
        if let Some(tap) = &self.tap {
            tap(Direction::Out, &line);
        }
        sender.send(line).map_err(|_| RpcError::Closed)
    }
}

/// Removes a pending entry however the request ends, so a cancelled caller
/// leaves nothing behind.
struct Remove<'a>(&'a Mutex<HashMap<i64, Reply>>, i64);

impl Drop for Remove<'_> {
    fn drop(&mut self) {
        self.0.lock().unwrap().remove(&self.1);
    }
}

fn route(mut frame: Value, pending: &Mutex<HashMap<i64, Reply>>, events: &mpsc::UnboundedSender<Incoming>) {
    let method = frame.get("method").and_then(Value::as_str).map(str::to_string);
    let id = frame.get("id").cloned().filter(|id| !id.is_null());
    let params = frame.get_mut("params").map(Value::take).unwrap_or(Value::Null);
    match (method, id) {
        (Some(method), Some(id)) => {
            let _ = events.send(Incoming::Request { id, method, params });
        }
        (Some(method), None) => {
            let _ = events.send(Incoming::Notification { method, params });
        }
        (None, Some(id)) => {
            let Some(id) = id.as_i64() else { return };
            let Some(reply) = pending.lock().unwrap().remove(&id) else { return };
            let result = match frame.get_mut("error") {
                Some(error) => Err(RpcError::Server {
                    code: error.get("code").and_then(Value::as_i64).unwrap_or(0),
                    message: error.get("message").and_then(Value::as_str).unwrap_or("error").to_string(),
                }),
                None => Ok(frame.get_mut("result").map(Value::take).unwrap_or(Value::Null)),
            };
            let _ = reply.send(result);
        }
        (None, None) => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::Stopper;

    struct NoStop;
    impl Stopper for NoStop {
        fn stop(&self) {}
        fn kill(&self) {}
    }

    fn pair() -> (Arc<RpcClient>, mpsc::UnboundedReceiver<Incoming>, mpsc::UnboundedReceiver<String>, mpsc::UnboundedSender<String>) {
        let (out_tx, out_rx) = mpsc::unbounded_channel();
        let (in_tx, in_rx) = mpsc::unbounded_channel();
        let (client, events, _) = RpcClient::start(Connection { outgoing: out_tx, incoming: in_rx, stopper: Box::new(NoStop) }, None);
        (client, events, out_rx, in_tx)
    }

    #[tokio::test]
    async fn requests_are_correlated_and_server_frames_routed() {
        let (client, mut events, mut out, inbound) = pair();
        let call = tokio::spawn({
            let client = client.clone();
            async move { client.request_value("a/b", json!({"x": 1})).await }
        });
        let sent: Value = serde_json::from_str(&out.recv().await.unwrap()).unwrap();
        assert_eq!(sent, json!({"id": 1, "method": "a/b", "params": {"x": 1}}));
        inbound.send(r#"{"method":"n","params":{"k":2}}"#.into()).unwrap();
        inbound.send(r#"{"id":"s1","method":"ask","params":{}}"#.into()).unwrap();
        inbound.send(r#"{"id":1,"result":{"ok":true}}"#.into()).unwrap();
        assert_eq!(call.await.unwrap(), Ok(json!({"ok": true})));
        assert!(matches!(events.recv().await, Some(Incoming::Notification { method, .. }) if method == "n"));
        assert!(matches!(events.recv().await, Some(Incoming::Request { id, .. }) if id == json!("s1")));
    }

    #[tokio::test]
    async fn errors_and_closing_fail_requests() {
        let (client, _events, mut out, inbound) = pair();
        let first = tokio::spawn({
            let client = client.clone();
            async move { client.request_value("x", json!({})).await }
        });
        out.recv().await.unwrap();
        inbound.send(r#"{"id":1,"error":{"code":-32600,"message":"no"}}"#.into()).unwrap();
        assert_eq!(first.await.unwrap(), Err(RpcError::Server { code: -32600, message: "no".into() }));
        let second = tokio::spawn({
            let client = client.clone();
            async move { client.request_value("y", json!({})).await }
        });
        out.recv().await.unwrap();
        drop(inbound);
        assert_eq!(second.await.unwrap(), Err(RpcError::Closed));
    }
}
