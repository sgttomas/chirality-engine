//! The engine's public contract: every type an application sees.
//!
//! Applications read a [`Snapshot`], follow [`Event`]s to keep it current, and
//! act through [`crate::Engine`]'s methods. TypeScript bindings for these types
//! are generated (`cargo test -p chirality-engine export_bindings`), so an
//! interface never builds or parses engine data by hand.
//!
//! These types are ours, not Codex's: the engine maps the app-server protocol
//! into them, so an application never depends on the protocol's shape.

use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use ts_rs::TS;

macro_rules! id {
    ($($name:ident),*) => {$(
        #[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, TS)]
        #[ts(export)]
        pub struct $name(pub String);
    )*};
}
id!(ProjectId, ThreadId, TurnId, ItemId, RequestId, WorkflowId);

/// How the engine is set up by the application that embeds it.
#[derive(Clone, Debug)]
pub struct EngineConfig {
    /// The pinned `codex` binary.
    pub codex_bin: PathBuf,
    /// The Codex home; `None` uses the person's own (`$CODEX_HOME` or `~/.codex`).
    pub codex_home: Option<PathBuf>,
    /// Where the engine keeps its own small state (the projects list).
    pub state_dir: PathBuf,
    /// Folders searched for workflows, besides each project's own.
    pub workflow_roots: Vec<PathBuf>,
    /// Extra `-c key=value` settings passed to `codex app-server`.
    pub codex_settings: Vec<String>,
    /// The application's own tools, given to the agent in every thread the engine starts.
    pub tools: Option<AppTools>,
}

// ---------- the application's tools ----------

/// Tools the application gives the agent, answered by its [`ToolHandler`].
///
/// Codex fixes a thread's tools when the thread starts and keeps them with it:
/// a resumed or forked thread has the tools it was started with, and a thread
/// started without them never has them.
///
/// [`crate::Engine::start`] refuses names Codex refuses and its own
/// namespaces. A catalog Codex refuses otherwise (an argument schema it
/// cannot use) makes [`crate::Engine::start_thread`] fail with Codex's own
/// message. Under some Codex settings Codex has further tools of its own in
/// namespaces of their own, and it leaves out, without saying, an
/// application's tool whose namespace and name are the same as one of those.
///
/// Subagents: the engine keeps Codex from giving a new subagent the
/// application's tools. A subagent Codex starts from a forked copy of its
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

/// Everything an interface shows, at one moment.
#[derive(Clone, Debug, Default, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct Snapshot {
    pub codex: CodexStatus,
    pub account: Account,
    pub models: Vec<Model>,
    pub projects: Vec<Project>,
    pub threads: Vec<ThreadSummary>,
    pub pending: Vec<PendingRequest>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(tag = "state", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum CodexStatus {
    #[default]
    Starting,
    Ready { version: String },
    Failed { message: String },
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(tag = "state", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum Account {
    #[default]
    Unknown,
    SignedOut,
    /// A ChatGPT sign-in is under way; the person finishes it at `url`.
    SigningIn { url: String },
    SignedIn { email: Option<String>, plan: Option<String> },
}

/// A model Codex offers, with the reasoning levels it accepts.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct Model {
    pub id: String,
    pub name: String,
    pub efforts: Vec<Effort>,
    pub default_effort: Option<Effort>,
    pub is_default: bool,
}

/// A reasoning level, as Codex names it (`low`, `medium`, `high`, `xhigh`, ...).
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct Effort(pub String);

/// Codex's own access levels, nothing added.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub enum Access {
    /// Read-only sandbox; asks before anything else.
    ReadOnly,
    /// Writes inside the project; asks before going further.
    #[default]
    Ask,
    /// No sandbox, no asking.
    FullAccess,
}

/// What a turn runs with. Sent on every turn, so a thread can change them.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct TurnSettings {
    pub model: String,
    pub effort: Option<Effort>,
    pub access: Access,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct Project {
    pub id: ProjectId,
    pub name: String,
    pub path: PathBuf,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct ThreadSummary {
    pub id: ThreadId,
    pub project: ProjectId,
    pub title: Option<String>,
    pub status: ThreadStatus,
    /// Something happened the person has not looked at yet.
    pub unread: bool,
    /// The thread this one was forked from.
    pub forked_from: Option<ThreadId>,
    /// Seconds since the Unix epoch.
    #[ts(type = "number")]
    pub updated_at: i64,
    pub settings: Option<TurnSettings>,
}

/// One status per thread, highest first: what the person most needs to know.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(tag = "state", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum ThreadStatus {
    NeedsApproval,
    NeedsAnswer,
    Working,
    Failed { message: String },
    Idle,
}

/// What the person sends.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct Message {
    pub text: String,
    pub attachments: Vec<Attachment>,
    /// A workflow the agent is to follow, from [`Workflow::id`].
    pub workflow: Option<WorkflowId>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(tag = "kind", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum Attachment {
    Image { path: PathBuf },
    /// A file in or outside the project, referenced so the agent reads it.
    File { path: PathBuf },
}

/// A thread's full contents, for showing it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct ThreadView {
    pub summary: ThreadSummary,
    pub turns: Vec<Turn>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct Turn {
    pub id: TurnId,
    pub items: Vec<Item>,
    pub state: TurnState,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(tag = "state", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum TurnState {
    Running,
    Completed,
    Interrupted,
    Failed { message: String },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct Item {
    pub id: ItemId,
    pub body: ItemBody,
    pub done: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(tag = "kind", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum ItemBody {
    UserMessage { text: String, attachments: Vec<Attachment>, workflow: Option<WorkflowId> },
    /// Markdown.
    AgentMessage { text: String },
    Reasoning { summary: String },
    Plan { steps: Vec<PlanStep> },
    Command { command: String, cwd: PathBuf, output: String, exit_code: Option<i32> },
    FileChange { files: Vec<FileDiff> },
    /// An MCP tool (`server` names it) or an application's tool (`server` is
    /// its namespace). `result` is its text; `images` are `data:` URLs.
    ToolCall { server: Option<String>, tool: String, arguments: String, status: ToolCallStatus, result: Option<String>, images: Vec<String> },
    WebSearch { query: String },
    Error { message: String },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub enum ToolCallStatus {
    Running,
    Completed,
    /// The tool failed, or the call was cancelled before it answered.
    Failed,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct PlanStep {
    pub text: String,
    pub status: PlanStepStatus,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub enum PlanStepStatus {
    Pending,
    InProgress,
    Completed,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct FileDiff {
    pub path: PathBuf,
    /// Unified diff.
    pub diff: String,
}

/// Something Codex waits on the person for.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct PendingRequest {
    pub id: RequestId,
    pub thread: ThreadId,
    pub request: Request,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(tag = "kind", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum Request {
    Command { command: String, cwd: PathBuf, reason: Option<String> },
    FileChange { files: Vec<FileDiff>, reason: Option<String> },
    Permissions { reason: Option<String> },
    Question { questions: Vec<Question> },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct Question {
    pub id: String,
    pub header: Option<String>,
    pub text: String,
    pub options: Vec<String>,
    /// The person may write their own answer.
    pub free_text: bool,
}

/// The person's answer to a [`PendingRequest`].
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(tag = "kind", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum Answer {
    Allow,
    AllowForSession,
    Decline,
    /// Decline and stop the turn.
    Cancel,
    /// One entry per question answered, by [`Question::id`].
    Answers { answers: Vec<(String, String)> },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct Workflow {
    pub id: WorkflowId,
    pub name: String,
    pub summary: String,
    /// Where it was found: a workflow root, or the project's own.
    pub source: String,
    pub path: PathBuf,
}

/// A change to the [`Snapshot`] or to a thread being shown.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(tag = "type", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum Event {
    Codex { status: CodexStatus },
    Account { account: Account },
    Models { models: Vec<Model> },
    Projects { projects: Vec<Project> },
    Thread { thread: ThreadSummary },
    ThreadRemoved { thread: ThreadId },
    TurnStarted { thread: ThreadId, turn: TurnId },
    TurnEnded { thread: ThreadId, turn: TurnId, state: TurnState },
    /// An item appeared or changed; the whole item, so no state is lost.
    Item { thread: ThreadId, turn: TurnId, item: Item },
    /// Text appended to an item while it streams.
    ItemDelta { thread: ThreadId, turn: TurnId, item: ItemId, text: String },
    Pending { request: PendingRequest },
    Resolved { request: RequestId },
}

/// Why an engine call failed, in words an interface can show.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS, thiserror::Error)]
#[ts(export)]
#[serde(tag = "kind", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum EngineError {
    #[error("Codex is not running: {message}")]
    CodexUnavailable { message: String },
    #[error("{message}")]
    Codex { message: String },
    #[error("not found: {what}")]
    NotFound { what: String },
    #[error("{message}")]
    Invalid { message: String },
}

pub type Result<T> = std::result::Result<T, EngineError>;
