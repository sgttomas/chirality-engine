//! A stub model on loopback: answers the Responses API with scripted
//! server-sent events, so the real Codex runs a turn with no account and no
//! network.
//!
//! The script for a request is picked by a marker `STUB[key]` in the last user
//! message, so turns are keyed on content, never on request order.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

/// What a script sees of a request: the outputs Codex returned for calls the
/// stub made since the last user message, by call id.
pub struct Request {
    pub outputs: HashMap<String, String>,
}

pub enum Reply {
    Items(Vec<Value>),
    /// Starts a response and never finishes it, until Codex hangs up.
    Hang,
}

pub type Script = Arc<dyn Fn(&Request) -> Reply + Send + Sync>;

pub struct Stub {
    pub port: u16,
    scripts: Arc<Mutex<HashMap<String, Script>>>,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for Stub {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl Stub {
    pub async fn start() -> Stub {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("loopback listener");
        let port = listener.local_addr().unwrap().port();
        let scripts: Arc<Mutex<HashMap<String, Script>>> = Arc::default();
        let task = tokio::spawn({
            let scripts = scripts.clone();
            async move {
                while let Ok((stream, _)) = listener.accept().await {
                    tokio::spawn(serve(stream, scripts.clone()));
                }
            }
        });
        Stub { port, scripts, task }
    }

    pub fn base_url(&self) -> String {
        format!("http://127.0.0.1:{}/v1", self.port)
    }

    pub fn script(&self, key: &str, script: Script) {
        self.scripts.lock().unwrap().insert(key.to_string(), script);
    }
}

async fn serve(mut stream: TcpStream, scripts: Arc<Mutex<HashMap<String, Script>>>) {
    let Some((head, body)) = read_request(&mut stream).await else { return };
    let mut lines = head.lines();
    let request_line = lines.next().unwrap_or_default().to_string();
    let headers: Vec<(String, String)> = lines.filter_map(|l| l.split_once(':')).map(|(k, v)| (k.trim().to_ascii_lowercase(), v.trim().to_string())).collect();
    let mut parts = request_line.split_whitespace();
    let (method, path) = (parts.next().unwrap_or_default(), parts.next().unwrap_or_default().split('?').next().unwrap_or_default());
    if method == "GET" && path.ends_with("/responses") && headers.iter().any(|(k, _)| k == "upgrade") {
        // No websocket: Codex falls back to HTTP for the session.
        return plain(&mut stream, 426).await;
    }
    if method != "POST" || !path.ends_with("/responses") {
        return plain(&mut stream, 404).await;
    }
    let Ok(body) = serde_json::from_slice::<Value>(&body) else { return plain(&mut stream, 400).await };
    let (text, outputs) = turn_view(&body);
    let key = text.split_once("STUB[").and_then(|(_, rest)| rest.split_once(']')).map(|(k, _)| k.to_string());
    let script = key.as_ref().and_then(|k| scripts.lock().unwrap().get(k).cloned());
    let reply = match script {
        Some(script) => script(&Request { outputs }),
        None => Reply::Items(vec![message("msg_unscripted", "No script for this turn.")]),
    };
    let head = "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nCache-Control: no-cache\r\nConnection: close\r\n\r\n";
    if stream.write_all(head.as_bytes()).await.is_err() {
        return;
    }
    let response_id = format!("resp_{}", key.unwrap_or_default());
    let _ = stream.write_all(&sse("response.created", json!({"type": "response.created", "response": {"id": response_id}}))).await;
    match reply {
        Reply::Hang => {
            let _ = stream.flush().await;
            let mut buffer = [0u8; 64];
            let _ = tokio::time::timeout(Duration::from_secs(120), stream.read(&mut buffer)).await;
        }
        Reply::Items(items) => {
            for (index, item) in items.iter().enumerate() {
                let mut events = vec![sse("response.output_item.added", json!({"type": "response.output_item.added", "output_index": index, "item": item}))];
                if item["type"] == "message" {
                    let text = item["content"][0]["text"].clone();
                    events.push(sse("response.output_text.delta", json!({"type": "response.output_text.delta", "output_index": index, "content_index": 0, "delta": text, "item_id": item["id"]})));
                }
                events.push(sse("response.output_item.done", json!({"type": "response.output_item.done", "output_index": index, "item": item})));
                for event in events {
                    let _ = stream.write_all(&event).await;
                }
            }
            let usage = json!({"input_tokens": 1, "input_tokens_details": {"cached_tokens": 0}, "output_tokens": 1, "output_tokens_details": {"reasoning_tokens": 0}, "total_tokens": 2});
            let _ = stream.write_all(&sse("response.completed", json!({"type": "response.completed", "response": {"id": response_id, "usage": usage}}))).await;
            let _ = stream.flush().await;
        }
    }
}

async fn read_request(stream: &mut TcpStream) -> Option<(String, Vec<u8>)> {
    let mut buffer = Vec::new();
    let mut chunk = [0u8; 16 * 1024];
    let head_end = loop {
        let n = stream.read(&mut chunk).await.ok()?;
        if n == 0 {
            return None;
        }
        buffer.extend_from_slice(&chunk[..n]);
        if let Some(at) = buffer.windows(4).position(|w| w == b"\r\n\r\n") {
            break at;
        }
    };
    let head = String::from_utf8_lossy(&buffer[..head_end]).into_owned();
    let length = head
        .lines()
        .filter_map(|l| l.split_once(':'))
        .find(|(k, _)| k.trim().eq_ignore_ascii_case("content-length"))
        .and_then(|(_, v)| v.trim().parse::<usize>().ok())
        .unwrap_or(0);
    let mut body = buffer[head_end + 4..].to_vec();
    while body.len() < length {
        let n = stream.read(&mut chunk).await.ok()?;
        if n == 0 {
            break;
        }
        body.extend_from_slice(&chunk[..n]);
    }
    Some((head, body))
}

async fn plain(stream: &mut TcpStream, status: u16) {
    let response = format!("HTTP/1.1 {status} Stub\r\nContent-Type: application/json\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{{}}");
    let _ = stream.write_all(response.as_bytes()).await;
}

fn sse(event: &str, data: Value) -> Vec<u8> {
    format!("event: {event}\ndata: {data}\n\n").into_bytes()
}

/// The text of the last user message, and the call outputs that follow it.
fn turn_view(body: &Value) -> (String, HashMap<String, String>) {
    let entries = body["input"].as_array().cloned().unwrap_or_default();
    let Some(last) = entries.iter().rposition(|e| e["role"] == "user" && e.get("type").and_then(Value::as_str).unwrap_or("message") == "message") else {
        return (String::new(), HashMap::new());
    };
    let text = match &entries[last]["content"] {
        Value::String(s) => s.clone(),
        Value::Array(parts) => parts.iter().filter_map(|p| p["text"].as_str()).collect::<Vec<_>>().join("\n"),
        _ => String::new(),
    };
    let outputs = entries[last + 1..]
        .iter()
        .filter(|e| e["type"].as_str().is_some_and(|t| t.ends_with("_output")))
        .filter_map(|e| {
            let output = match &e["output"] {
                Value::String(s) => s.clone(),
                other => other.to_string(),
            };
            Some((e["call_id"].as_str()?.to_string(), output))
        })
        .collect();
    (text, outputs)
}

// ---------- what scripts return ----------

pub fn message(id: &str, text: &str) -> Value {
    json!({"type": "message", "role": "assistant", "id": id, "content": [{"type": "output_text", "text": text, "annotations": []}]})
}

pub fn shell_call(call_id: &str, command: &str, escalate: bool) -> Value {
    let mut args = json!({"cmd": command});
    if escalate {
        args["sandbox_permissions"] = json!("require_escalated");
        args["justification"] = json!("Run the invented fixture command?");
    }
    json!({"type": "function_call", "name": "exec_command", "call_id": call_id, "id": format!("fc_{call_id}"), "arguments": args.to_string()})
}

pub fn patch_call(call_id: &str, patch: &str) -> Value {
    json!({"type": "custom_tool_call", "name": "apply_patch", "call_id": call_id, "id": format!("ctc_{call_id}"), "input": patch, "status": "completed"})
}

pub fn question_call(call_id: &str, questions: Value) -> Value {
    json!({"type": "function_call", "name": "request_user_input", "call_id": call_id, "id": format!("fc_{call_id}"), "arguments": json!({"questions": questions}).to_string()})
}

/// A reply of `text`.
pub fn says(key: &str, text: &str) -> Script {
    let (id, text) = (format!("msg_{key}"), text.to_string());
    Arc::new(move |_| Reply::Items(vec![message(&id, &text)]))
}

/// One call; once Codex returns its output, a closing message, which quotes
/// the output when `quote` (outputs that hold times or ids are not quoted, so
/// recordings stay the same from run to run).
pub fn calls(key: &str, call: Value, quote: bool) -> Script {
    let key = key.to_string();
    Arc::new(move |request| {
        let call_id = call["call_id"].as_str().unwrap_or_default();
        match request.outputs.get(call_id) {
            Some(output) if quote => Reply::Items(vec![message(&format!("msg_{key}_done"), &format!("Done: {}", first_line(output)))]),
            Some(_) => Reply::Items(vec![message(&format!("msg_{key}_done"), "Done.")]),
            None => Reply::Items(vec![call.clone()]),
        }
    })
}

pub fn hangs() -> Script {
    Arc::new(|_| Reply::Hang)
}

fn first_line(text: &str) -> String {
    text.lines().next().unwrap_or_default().chars().take(200).collect()
}
