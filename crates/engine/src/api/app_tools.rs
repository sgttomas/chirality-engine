//! The application's own tools, which the engine gives the agent beside its
//! own (namespace `chirality`) and whose calls the application answers.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use super::{ItemId, ProjectId, ThreadId, TurnId};

/// Tools the application gives the agent, answered by its [`ToolHandler`].
///
/// Codex fixes a thread's tools when the thread starts and keeps them with it:
/// a resumed or forked thread has the tools it was started with, and a thread
/// started without them never has them.
///
/// [`crate::Engine::start`] refuses names Codex refuses, its own
/// namespaces, and the engine's (`chirality`). A catalog Codex refuses otherwise (an argument schema it
/// cannot use) makes [`crate::Engine::start_thread`] fail with Codex's own
/// message. Under some Codex settings Codex has further tools of its own in
/// namespaces of their own, and it leaves out, without saying, an
/// application's tool whose namespace and name are the same as one of those.
///
/// Subagents: the engine keeps Codex from giving a new subagent the
/// application's tools (and its own). A subagent Codex starts from a forked copy of its
/// parent's conversation can still carry them, since Codex restores a thread's
/// tools from forked history. For a subagent's thread that has them, a call
/// made just as its parent is archived can still reach the handler (Codex
/// ends the subagents after answering the archive), and Codex replaying a
/// subagent's earlier turn can let its calls through again.
#[derive(Clone)]
pub struct AppTools {
    /// The one name the agent sees the tools under, with what they are for.
    /// Always given, so no tool clashes with one of Codex's own.
    pub namespace: ToolNamespace,
    pub tools: Vec<AppTool>,
    pub handler: Arc<dyn ToolHandler>,
}

impl std::fmt::Debug for AppTools {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AppTools").field("namespace", &self.namespace).field("tools", &self.tools).finish_non_exhaustive()
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct ToolNamespace {
    /// Letters, digits, `_` and `-`.
    pub name: String,
    pub description: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct AppTool {
    /// Letters, digits, `_` and `-`; unique among the tools.
    pub name: String,
    /// What the tool does, for the agent.
    pub description: String,
    /// JSON Schema of the arguments.
    pub input_schema: serde_json::Value,
}

/// The agent's call to one of the application's tools.
#[derive(Clone, Debug, PartialEq)]
pub struct ToolCall {
    /// The thread it shows on (a subagent's call shows on its root thread).
    pub thread: ThreadId,
    /// The project of that thread, when it is one of the engine's.
    pub project: Option<ProjectId>,
    /// The turn that made the call (a subagent's own turn when a subagent made it).
    pub turn: TurnId,
    /// The call's item in the thread, [`Item::id`].
    pub item: ItemId,
    pub namespace: Option<String>,
    pub tool: String,
    /// As the agent wrote them; not checked against the schema.
    pub arguments: serde_json::Value,
}

/// The answer to a [`ToolCall`], given to the agent.
#[derive(Clone, Debug, PartialEq)]
pub struct ToolOutput {
    /// False tells the agent the tool failed.
    pub success: bool,
    pub content: Vec<ToolContent>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum ToolContent {
    Text(String),
    /// A `data:` URL; Codex refuses a remote image.
    Image(String),
}

impl ToolOutput {
    pub fn text(text: impl Into<String>) -> ToolOutput {
        ToolOutput { success: true, content: vec![ToolContent::Text(text.into())] }
    }

    pub fn failure(text: impl Into<String>) -> ToolOutput {
        ToolOutput { success: false, content: vec![ToolContent::Text(text.into())] }
    }
}

pub type ToolFuture = Pin<Box<dyn Future<Output = ToolOutput> + Send>>;

/// Answers the agent's calls to the application's tools; any `Fn(ToolCall)`
/// returning a future of [`ToolOutput`] is one.
///
/// Each call is answered with what its future gives. Codex cancels a call
/// when its turn is interrupted or ends, or Codex stops; the engine then drops
/// the future. A future that panics answers its call as failed.
///
/// A handler may now and then run, at least in part, for a call whose answer
/// never reaches the agent: Codex cancelled it, its thread was archived, or
/// Codex ended, just as the handler started. So a handler must be safe to run
/// without its answer being delivered, as anything across a process boundary
/// must be.
pub trait ToolHandler: Send + Sync {
    fn call(&self, call: ToolCall) -> ToolFuture;
}

impl<F, Fut> ToolHandler for F
where
    F: Fn(ToolCall) -> Fut + Send + Sync,
    Fut: Future<Output = ToolOutput> + Send + 'static,
{
    fn call(&self, call: ToolCall) -> ToolFuture {
        Box::pin(self(call))
    }
}

