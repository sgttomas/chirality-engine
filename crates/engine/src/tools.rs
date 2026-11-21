//! The application's tools: given to Codex when a thread starts, and each call
//! Codex sends (`item/tool/call`) answered by the application's handler.
//!
//! Codex cancels a call when its item completes unanswered, its turn ends, its
//! thread is archived, deleted or closed, or Codex stops; the engine then
//! drops the handler's future and sends nothing (`calls.rs`).

use std::collections::HashSet;
use std::sync::Arc;

use serde_json::Value;

use crate::api::*;
use crate::calls::{Admission, Place, ReplyTo};
use crate::protocol as p;
use crate::session::{Inner, Session};

pub(crate) const TOOL_CALL: &str = "item/tool/call";

/// The application's tools as `thread/start` takes them.
pub(crate) fn specs(tools: &AppTools) -> Vec<p::DynamicToolSpec> {
    let functions = tools
        .tools
        .iter()
        .map(|t| p::NamespaceTool::Function(p::FunctionSpec { name: t.name.clone(), description: t.description.clone(), input_schema: t.input_schema.clone() }))
        .collect();
    vec![p::DynamicToolSpec::Namespace { name: tools.namespace.name.clone(), description: tools.namespace.description.clone(), tools: functions }]
}

/// Namespaces Codex keeps: for the Responses API (0.171.0, `validate_dynamic_tools`),
/// and for its own tools (`clock`, and the multi-agent ones), whose names
/// would win over an application's.
const RESERVED_NAMESPACES: [&str; 17] = [
    "clock",
    "collaboration",
    "multi_agent_v1",
    "api_tool",
    "browser",
    "computer",
    "container",
    "file_search",
    "functions",
    "image_gen",
    "multi_tool_use",
    "python",
    "python_user_visible",
    "submodel_delegator",
    "terminal",
    "tool_search",
    "web",
];

/// What Codex 0.171.0 refuses at `thread/start`, refused when the engine
/// starts instead. The tools are always in a namespace, so none can clash with
/// one of Codex's own (which Codex skips without saying).
pub(crate) fn validate(tools: &AppTools) -> Result<()> {
    let invalid = |message: String| Err(EngineError::Invalid { message });
    let identifier = |name: &str, max: usize| !name.is_empty() && name.chars().count() <= max && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-');
    let reserved = |name: &str| name == "mcp" || name.starts_with("mcp__");
    let ns = &tools.namespace;
    if !identifier(&ns.name, 64) || reserved(&ns.name) || RESERVED_NAMESPACES.contains(&ns.name.as_str()) {
        return invalid(format!("tool namespace {:?} must be at most 64 letters, digits, _ or -, and not one Codex keeps", ns.name));
    }
    if ns.description.chars().count() > 1024 {
        return invalid("the tool namespace's description must be at most 1024 characters".into());
    }
    if tools.tools.is_empty() {
        return invalid("there must be at least one tool".into());
    }
    let mut seen = HashSet::new();
    for tool in &tools.tools {
        if !identifier(&tool.name, 128) || reserved(&tool.name) {
            return invalid(format!("tool name {:?} must be at most 128 letters, digits, _ or -, and not mcp", tool.name));
        }
        if !seen.insert(tool.name.as_str()) {
            return invalid(format!("tool {:?} is given twice", tool.name));
        }
        if !schema_supported(&tool.input_schema) {
            return invalid(format!("tool {:?} needs a JSON Schema of an object for its arguments", tool.name));
        }
    }
    Ok(())
}

/// Arguments are an object, so their schema describes one: Codex 0.171.0
/// refuses some other schemas and makes do with the rest.
pub(crate) fn schema_supported(schema: &Value) -> bool {
    let Some(schema) = schema.as_object() else { return false };
    match schema.get("type") {
        None => true,
        Some(Value::String(kind)) => kind == "object",
        Some(Value::Array(kinds)) => kinds.iter().any(|k| k == "object"),
        Some(_) => false,
    }
}

/// Whether a call names one of the configured tools: a thread resumed from a
/// shared Codex home can carry another application's.
fn offers(tools: &AppTools, namespace: Option<&str>, tool: &str) -> bool {
    namespace == Some(tools.namespace.name.as_str()) && tools.tools.iter().any(|t| t.name == tool)
}

/// What the model sees of an answer.
fn response(output: ToolOutput) -> Value {
    let content_items = output
        .content
        .into_iter()
        .map(|c| match c {
            ToolContent::Text(text) => p::ToolContentItem::InputText { text },
            ToolContent::Image(image_url) => p::ToolContentItem::InputImage { image_url },
        })
        .collect();
    serde_json::to_value(p::DynamicToolResponse { content_items, success: output.success }).expect("a tool response serialises")
}

impl Inner {
    /// Answers a call to the application's tools through its handler, once
    /// per request; a call to a tool it does not have, or on a thread gone
    /// from Codex, is answered as failed at once.
    pub(crate) fn on_tool_call(&self, session: Arc<Session>, id: Value, params: Value, tools: &AppTools) {
        let Ok(call) = serde_json::from_value::<p::DynamicToolCall>(params) else {
            let _ = session.client.respond_error(id, -32602, "invalid tool call");
            return;
        };
        let refuse = |text: String| {
            let _ = session.client.respond(id.clone(), response(ToolOutput::failure(text)));
        };
        match self.tool_calls.admit(session.generation, &id, &call.thread_id) {
            Admission::Ignore => return,
            Admission::Refuse => return refuse("The thread is closed.".into()),
            Admission::Run => {}
        }
        if !offers(tools, call.namespace.as_deref(), &call.tool) {
            let name = [call.namespace.as_deref(), Some(call.tool.as_str())].into_iter().flatten().collect::<Vec<_>>().join(".");
            return refuse(format!("This application has no tool {name}."));
        }
        let place = Place { thread: call.thread_id.clone(), turn: call.turn_id.clone(), item: call.call_id.clone() };
        let thread = self.store.owner(ThreadId(call.thread_id));
        let project = self.store.thread(&thread).map(|e| e.project);
        let call = ToolCall {
            thread,
            project,
            turn: TurnId(call.turn_id),
            item: ItemId(call.call_id),
            namespace: call.namespace,
            tool: call.tool,
            arguments: call.arguments,
        };
        let to = ReplyTo { client: session.client.clone(), generation: session.generation, id };
        self.tool_calls.run(to, place, call, tools.handler.clone(), response);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    pub(crate) fn tools(namespace: &str, names: &[&str]) -> AppTools {
        AppTools {
            namespace: ToolNamespace { name: namespace.into(), description: "The estimate.".into() },
            tools: names.iter().map(|n| AppTool { name: n.to_string(), description: "Does it.".into(), input_schema: json!({"type": "object"}) }).collect(),
            handler: Arc::new(|_: ToolCall| async { ToolOutput::text("ok") }),
        }
    }

    #[test]
    fn tools_are_given_as_codex_takes_them() {
        let spec = serde_json::to_value(specs(&tools("estimate", &["check"]))).unwrap();
        let function = json!({"type": "function", "name": "check", "description": "Does it.", "inputSchema": {"type": "object"}});
        assert_eq!(spec, json!([{"type": "namespace", "name": "estimate", "description": "The estimate.", "tools": [function]}]));
    }

    #[test]
    fn catalogs_codex_refuses_are_refused() {
        assert!(validate(&tools("estimate", &["check", "build-network"])).is_ok());
        for bad in [tools("estimate", &["check", "check"]), tools("estimate", &["check estimate"]), tools("estimate", &["mcp__x"]), tools("estimate", &[])] {
            assert!(validate(&bad).is_err(), "{bad:?}");
        }
        for namespace in ["a.b", "", "web", "functions", "clock", "mcp", "mcp__x", &"n".repeat(65)] {
            assert!(validate(&tools(namespace, &["check"])).is_err(), "{namespace}");
        }
        let mut long = tools("estimate", &["check"]);
        long.namespace.description = "d".repeat(1025);
        assert!(validate(&long).is_err());
        // The cases checked against the pinned Codex (`testkit::catalog`).
        for (namespace, name, schema, ok) in crate::testkit::catalog::cases() {
            let mut tools = tools(namespace, &[name]);
            tools.tools[0].input_schema = schema.clone();
            assert_eq!(validate(&tools).is_ok(), ok, "{namespace}.{name} {schema}");
        }
    }

    #[test]
    fn only_the_configured_tools_are_offered() {
        let tools = tools("estimate", &["check"]);
        assert!(offers(&tools, Some("estimate"), "check"));
        assert!(!offers(&tools, Some("other"), "check"));
        assert!(!offers(&tools, None, "check"));
        assert!(!offers(&tools, Some("estimate"), "build"));
    }

    #[test]
    fn answers_carry_text_images_and_success() {
        let output = ToolOutput { success: false, content: vec![ToolContent::Text("no".into()), ToolContent::Image("data:image/png;base64,AA".into())] };
        assert_eq!(
            response(output),
            json!({"contentItems": [{"type": "inputText", "text": "no"}, {"type": "inputImage", "imageUrl": "data:image/png;base64,AA"}], "success": false})
        );
    }
}
