//! The application's tools: given to Codex when a thread starts, and each call
//! Codex sends (`item/tool/call`) answered by the application's handler.
//!
//! Every call is answered. Codex cancels a call when its turn ends or is
//! interrupted (the call's item completes, failed, without an answer); the
//! engine then drops the handler's future, as it does when Codex stops.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use serde_json::Value;
use tokio::sync::oneshot;

use crate::api::*;
use crate::protocol as p;
use crate::session::{Inner, Session};

pub(crate) const TOOL_CALL: &str = "item/tool/call";

/// The application's tools as `thread/start` takes them.
pub(crate) fn specs(tools: &AppTools) -> Vec<p::DynamicToolSpec> {
    let functions = tools
        .tools
        .iter()
        .map(|t| p::DynamicToolSpec::Function(p::FunctionSpec { name: t.name.clone(), description: t.description.clone(), input_schema: t.input_schema.clone() }))
        .collect();
    match &tools.namespace {
        Some(ns) => vec![p::DynamicToolSpec::Namespace { name: ns.name.clone(), description: ns.description.clone(), tools: functions }],
        None => functions,
    }
}

/// Names Codex would refuse, refused when the engine starts instead of at the first thread.
pub(crate) fn validate(tools: &AppTools) -> Result<()> {
    let invalid = |message: String| Err(EngineError::Invalid { message });
    let identifier = |name: &str| !name.is_empty() && name.len() <= 128 && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-');
    if let Some(ns) = &tools.namespace
        && (!identifier(&ns.name) || ns.name.len() > 64)
    {
        return invalid(format!("tool namespace {:?} must be at most 64 letters, digits, _ or -", ns.name));
    }
    let mut seen = std::collections::HashSet::new();
    for tool in &tools.tools {
        if !identifier(&tool.name) || tool.name == "mcp" || tool.name.starts_with("mcp__") {
            return invalid(format!("tool name {:?} must be letters, digits, _ or - and not mcp", tool.name));
        }
        if !seen.insert(tool.name.as_str()) {
            return invalid(format!("tool {:?} is given twice", tool.name));
        }
    }
    Ok(())
}

/// What the model sees of an answer.
fn response(output: ToolOutput) -> p::DynamicToolResponse {
    let content_items = output
        .content
        .into_iter()
        .map(|c| match c {
            ToolContent::Text(text) => p::ToolContentItem::InputText { text },
            ToolContent::Image(image_url) => p::ToolContentItem::InputImage { image_url },
        })
        .collect();
    p::DynamicToolResponse { content_items, success: output.success }
}

struct Running {
    turn: String,
    /// Dropping it cancels the call.
    _cancel: oneshot::Sender<()>,
}

/// The calls being answered, by session generation and call id.
#[derive(Clone, Default)]
pub(crate) struct Calls(Arc<Mutex<HashMap<(u64, String), Running>>>);

impl Calls {
    /// Cancels the calls `matches` picks.
    fn cancel(&self, mut matches: impl FnMut(&(u64, String), &Running) -> bool) {
        self.0.lock().unwrap().retain(|key, running| !matches(key, running));
    }

    pub fn cancel_call(&self, generation: u64, call: &str) {
        self.cancel(|(g, id), _| *g == generation && id == call);
    }

    pub fn cancel_turn(&self, generation: u64, turn: &str) {
        self.cancel(|(g, _), r| *g == generation && r.turn == turn);
    }

    /// A dead session's calls can no longer be answered.
    pub fn cancel_generation(&self, generation: u64) {
        self.cancel(|(g, _), _| *g == generation);
    }

    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.0.lock().unwrap().len()
    }
}

impl Inner {
    /// Runs the handler on a call and answers Codex with its output.
    pub(crate) fn on_tool_call(&self, session: Arc<Session>, id: Value, params: Value, handler: Arc<dyn ToolHandler>) {
        let Ok(call) = serde_json::from_value::<p::DynamicToolCall>(params) else {
            let _ = session.client.respond_error(id, -32602, "invalid tool call");
            return;
        };
        let thread = self.store.owner(ThreadId(call.thread_id));
        let project = self.store.thread(&thread).map(|e| e.project);
        let key = (session.generation, call.call_id.clone());
        let (cancel_tx, cancel_rx) = oneshot::channel();
        self.tool_calls.0.lock().unwrap().insert(key.clone(), Running { turn: call.turn_id.clone(), _cancel: cancel_tx });
        let call = ToolCall {
            thread,
            project,
            turn: TurnId(call.turn_id),
            item: ItemId(call.call_id),
            namespace: call.namespace,
            tool: call.tool,
            arguments: call.arguments,
        };
        let calls = self.tool_calls.clone();
        tokio::spawn(async move {
            let mut work = tokio::spawn(handler.call(call));
            let output = tokio::select! {
                done = &mut work => done.unwrap_or_else(|_| ToolOutput::failure("The tool failed.")),
                _ = cancel_rx => {
                    work.abort();
                    return;
                }
            };
            // Answered only while Codex still waits: a call cancelled meanwhile is gone.
            if calls.0.lock().unwrap().remove(&key).is_some() {
                let reply = serde_json::to_value(response(output)).expect("a tool response serialises");
                let _ = session.client.respond(id, reply);
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn tools(namespace: Option<&str>, names: &[&str]) -> AppTools {
        AppTools {
            namespace: namespace.map(|n| ToolNamespace { name: n.into(), description: "The estimate.".into() }),
            tools: names.iter().map(|n| AppTool { name: n.to_string(), description: "Does it.".into(), input_schema: json!({"type": "object"}) }).collect(),
            handler: Arc::new(|_: ToolCall| async { ToolOutput::text("ok") }),
        }
    }

    #[test]
    fn tools_are_given_as_codex_takes_them() {
        let spec = serde_json::to_value(specs(&tools(Some("estimate"), &["check"]))).unwrap();
        let function = json!({"type": "function", "name": "check", "description": "Does it.", "inputSchema": {"type": "object"}});
        assert_eq!(spec, json!([{"type": "namespace", "name": "estimate", "description": "The estimate.", "tools": [function.clone()]}]));
        assert_eq!(serde_json::to_value(specs(&tools(None, &["check"]))).unwrap(), json!([function]));
    }

    #[test]
    fn names_codex_refuses_are_refused() {
        assert!(validate(&tools(Some("estimate"), &["check", "build-network"])).is_ok());
        assert!(validate(&tools(None, &["check", "check"])).is_err());
        assert!(validate(&tools(None, &["check estimate"])).is_err());
        assert!(validate(&tools(None, &["mcp__x"])).is_err());
        assert!(validate(&tools(Some("a.b"), &["check"])).is_err());
    }

    #[test]
    fn answers_carry_text_images_and_success() {
        let output = ToolOutput { success: false, content: vec![ToolContent::Text("no".into()), ToolContent::Image("data:image/png;base64,AA".into())] };
        assert_eq!(
            serde_json::to_value(response(output)).unwrap(),
            json!({"contentItems": [{"type": "inputText", "text": "no"}, {"type": "inputImage", "imageUrl": "data:image/png;base64,AA"}], "success": false})
        );
    }
}
