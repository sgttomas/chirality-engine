//! The application's tools a scenario gives the engine: an invented estimate
//! check that answers from its arguments, and one that never answers, so a
//! cancelled call can be seen to be dropped.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::json;

use crate::api::*;

pub const NAMESPACE: &str = "estimate";
pub const CHECK: &str = "check_estimate";
pub const WAIT: &str = "wait_for_network";

/// The scenarios run with the application's tools; every other runs without, as before.
pub fn for_scenario(name: &str) -> Option<Arc<TestTools>> {
    matches!(name, "app_tool" | "app_tool_kept" | "app_tool_cancelled").then(|| Arc::new(TestTools::default()))
}

#[derive(Default)]
pub struct TestTools {
    pub calls: Mutex<Vec<ToolCall>>,
    /// Calls whose futures were dropped unanswered.
    pub dropped: AtomicUsize,
}

/// Counts a future dropped before it finished.
struct Unfinished(Arc<TestTools>);

impl Drop for Unfinished {
    fn drop(&mut self) {
        self.0.dropped.fetch_add(1, Ordering::SeqCst);
    }
}

impl TestTools {
    pub fn app_tools(self: &Arc<Self>) -> AppTools {
        let tools = self.clone();
        let handler = move |call: ToolCall| {
            let tools = tools.clone();
            async move {
                tools.calls.lock().unwrap().push(call.clone());
                match call.tool.as_str() {
                    CHECK => {
                        let total = call.arguments["total"].as_f64().unwrap_or_default();
                        if total > 0.0 { ToolOutput::text(format!("The estimate of {total} checks out.")) } else { ToolOutput::failure("The estimate has no total.") }
                    }
                    WAIT => {
                        let _unfinished = Unfinished(tools.clone());
                        std::future::pending::<ToolOutput>().await
                    }
                    other => ToolOutput::failure(format!("No tool {other}.")),
                }
            }
        };
        AppTools {
            namespace: ToolNamespace { name: NAMESPACE.into(), description: "The project's estimate.".into() },
            tools: vec![
                AppTool {
                    name: CHECK.into(),
                    description: "Check the estimate's total.".into(),
                    input_schema: json!({"type": "object", "properties": {"total": {"type": "number"}}, "required": ["total"], "additionalProperties": false}),
                },
                AppTool { name: WAIT.into(), description: "Wait for the network to be built.".into(), input_schema: json!({"type": "object", "properties": {}}) },
            ],
            handler: Arc::new(handler),
        }
    }

    /// Waits until the handler has had `n` calls.
    pub async fn called(&self, n: usize) -> Vec<ToolCall> {
        for _ in 0..3000 {
            let calls = self.calls.lock().unwrap().clone();
            if calls.len() >= n {
                return calls;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        panic!("the tool handler was not called {n} times");
    }
}
