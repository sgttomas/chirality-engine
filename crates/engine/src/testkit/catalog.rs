//! The engine's catalog check against the pinned Codex's own. Codex takes a
//! tool only if `thread/start` accepts it and the tool then reaches the model
//! (Codex skips, without saying, a tool whose schema it cannot convert, or
//! whose name one of its own already has). Ignored: it runs the pinned Codex,
//! offline, against the stub.

use std::sync::Arc;

use serde_json::{Value, json};

use super::harness::{Ctx, codex_present, settings, text};
use super::stub::{Reply, message};
use crate::api::*;

const MARK: &str = "Probe tool, marked.";

fn catalog(namespace: &str, tool: &str, schema: Value) -> AppTools {
    AppTools {
        namespace: ToolNamespace { name: namespace.into(), description: "Probe.".into() },
        tools: vec![AppTool { name: tool.into(), description: MARK.into(), input_schema: schema }],
        handler: Arc::new(|_: ToolCall| async { ToolOutput::text("ok") }),
    }
}

/// Whether Codex gives the tool to the model; when it refuses the thread, Codex's message.
async fn codex_takes(tools: AppTools) -> Result<bool> {
    let mut ctx = Ctx::live_with("catalog", false, Some(tools)).await;
    ctx.script("probe", Arc::new(|request| {
        let offered = request.body.to_string().contains(MARK);
        Reply::Items(vec![message("msg_probe", if offered { "offered" } else { "absent" })])
    }));
    let project = ctx.engine.add_project(ctx.run.project()).await.unwrap();
    let taken = match ctx.engine.start_thread(&project.id, text("STUB[probe] go"), settings(Access::Ask)).await {
        Err(error) => Err(error),
        Ok(thread) => {
            ctx.turn_ended(&thread).await;
            let view = ctx.engine.read_thread(&thread).await.unwrap();
            Ok(view.turns[0].items.iter().any(|i| i.body == ItemBody::AgentMessage { text: "offered".into() }))
        }
    };
    ctx.finish("catalog").await;
    taken
}

/// Catalogs, and whether the engine is to take them.
pub fn cases() -> Vec<(&'static str, &'static str, Value, bool)> {
    vec![
        ("probe", "t", json!({"type": "object"}), true),
        ("probe", "t", json!({"type": "object", "properties": {"a": {"type": ["string", "null"]}}, "required": ["a"]}), true),
        ("probe", "t", json!({"properties": {}}), true),
        ("probe", "t", json!({}), true),
        ("probe", "t", json!({"type": ["object", "null"]}), true),
        // Codex takes these and gives them to the model as it can.
        ("probe", "t", json!({"type": "object", "properties": {"a": {"type": "null"}}}), true),
        ("probe", "t", json!({"type": "object", "properties": {"a": {"type": "bogus"}}}), true),
        // Codex refuses these two; it takes the rest, though arguments are never one.
        ("probe", "t", json!({"type": "null"}), false),
        ("probe", "t", json!({"type": "string"}), false),
        ("probe", "t", json!({"type": "array", "items": {}}), false),
        ("probe", "t", json!({"type": "integer"}), false),
        ("probe", "t", json!(true), false),
        ("probe", "t", json!([]), false),
        ("clock", "curr_time", json!({"type": "object"}), false),
        // Codex's own tool wins (`clock` is live in this configuration); the
        // multi-agent namespaces are Codex's too when those tools are on.
        ("multi_agent_v1", "spawn_agent", json!({"type": "object"}), false),
        ("collaboration", "spawn_agent", json!({"type": "object"}), false),
    ]
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "runs the pinned Codex"]
async fn the_catalog_check_matches_codex() {
    assert!(codex_present(), "the pinned Codex is needed");
    let mut differ = Vec::new();
    for (namespace, tool, schema, expected) in cases() {
        let tools = catalog(namespace, tool, schema.clone());
        let engine = crate::tools::validate(&tools).is_ok();
        let codex = match codex_takes(tools).await {
            Ok(offered) => offered,
            // A schema Codex refuses reaches the application as Codex's own message.
            Err(EngineError::Codex { message }) => {
                assert!(message.contains("input schema is not supported for t"), "{message}");
                false
            }
            Err(other) => panic!("{other:?}"),
        };
        eprintln!("{namespace}.{tool} {schema}: codex {codex}, engine {engine}, listed {expected}");
        // The engine may refuse more than Codex (a namespace Codex keeps for itself), never less.
        if engine != expected || (engine && !codex) {
            differ.push(format!("{namespace}.{tool} {schema}"));
        }
    }
    assert!(differ.is_empty(), "verdicts differ for {differ:?}");
}
