//! A stub model on loopback for the headless test: answers Codex's Responses
//! API with server-sent events, so a turn runs with no account and no network.
//! `reply` gives the items for a request (its JSON body).

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;

use serde_json::{Value, json};

pub type Reply = Arc<dyn Fn(&Value) -> Vec<Value> + Send + Sync>;

/// Starts the stub; its base URL for Codex's `openai_base_url`.
pub fn start(reply: Reply) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let reply = reply.clone();
            std::thread::spawn(move || serve(stream, &reply));
        }
    });
    format!("http://127.0.0.1:{port}/v1")
}

fn serve(mut stream: TcpStream, reply: &Reply) {
    let mut reader = BufReader::new(stream.try_clone().unwrap());
    let mut head = Vec::new();
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).unwrap_or(0) == 0 {
            return;
        }
        if line == "\r\n" {
            break;
        }
        head.push(line.trim_end().to_string());
    }
    let header = |name: &str| head.iter().find_map(|l| l.split_once(':').filter(|(k, _)| k.trim().eq_ignore_ascii_case(name)).map(|(_, v)| v.trim().to_string()));
    let request_line = head.first().cloned().unwrap_or_default();
    let mut body = vec![0; header("content-length").and_then(|l| l.parse().ok()).unwrap_or(0)];
    let _ = reader.read_exact(&mut body);
    // No websocket: Codex falls back to HTTP for the session.
    let status = if header("upgrade").is_some() { Some(426) } else if !(request_line.starts_with("POST") && request_line.contains("/responses")) { Some(404) } else { None };
    if let Some(status) = status {
        let _ = stream.write_all(format!("HTTP/1.1 {status} Stub\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{{}}").as_bytes());
        return;
    }
    let request: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    let mut out = String::from("HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n");
    let mut event = |name: &str, data: Value| out.push_str(&format!("event: {name}\ndata: {data}\n\n"));
    event("response.created", json!({"type": "response.created", "response": {"id": "resp"}}));
    for (index, item) in reply(&request).into_iter().enumerate() {
        event("response.output_item.added", json!({"type": "response.output_item.added", "output_index": index, "item": item}));
        event("response.output_item.done", json!({"type": "response.output_item.done", "output_index": index, "item": item}));
    }
    let usage = json!({"input_tokens": 1, "input_tokens_details": {"cached_tokens": 0}, "output_tokens": 1, "output_tokens_details": {"reasoning_tokens": 0}, "total_tokens": 2});
    event("response.completed", json!({"type": "response.completed", "response": {"id": "resp", "usage": usage}}));
    let _ = stream.write_all(out.as_bytes());
}

pub fn message(text: &str) -> Value {
    json!({"type": "message", "role": "assistant", "id": format!("msg_{}", text.len()), "content": [{"type": "output_text", "text": text, "annotations": []}]})
}

pub fn call(call_id: &str, namespace: &str, name: &str, arguments: Value) -> Value {
    json!({"type": "function_call", "namespace": namespace, "name": name, "call_id": call_id, "id": format!("fc_{call_id}"), "arguments": arguments.to_string()})
}

/// The output Codex returned for `call_id`, if the request carries it.
pub fn output<'a>(request: &'a Value, call_id: &str) -> Option<&'a Value> {
    request["input"].as_array()?.iter().find(|e| e["call_id"] == call_id && e["type"].as_str().is_some_and(|t| t.ends_with("_output"))).map(|e| &e["output"])
}
