//! The engine's public contract: every type an application sees.
//!
//! Applications read a [`Snapshot`], follow [`Event`]s to keep it current, and
//! act through [`crate::Engine`]'s methods. TypeScript bindings for these types
//! are generated (`cargo test -p chirality-engine export_bindings`), so an
//! interface never builds or parses engine data by hand.
//!
//! These types are ours, not Codex's: the engine maps the app-server protocol
//! into them, so an application never depends on the protocol's shape.

use std::path::PathBuf;

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
#[serde(tag = "state", rename_all = "camelCase")]
pub enum CodexStatus {
    #[default]
    Starting,
    Ready { version: String },
    Failed { message: String },
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(tag = "state", rename_all = "camelCase")]
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
    pub updated_at: i64,
    pub settings: Option<TurnSettings>,
}

/// One status per thread, highest first: what the person most needs to know.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(tag = "state", rename_all = "camelCase")]
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
#[serde(tag = "kind", rename_all = "camelCase")]
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
#[serde(tag = "state", rename_all = "camelCase")]
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
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum ItemBody {
    UserMessage { text: String, attachments: Vec<Attachment>, workflow: Option<WorkflowId> },
    /// Markdown.
    AgentMessage { text: String },
    Reasoning { summary: String },
    Plan { steps: Vec<PlanStep> },
    Command { command: String, cwd: PathBuf, output: String, exit_code: Option<i32> },
    FileChange { files: Vec<FileDiff> },
    ToolCall { server: Option<String>, tool: String, arguments: String, result: Option<String> },
    WebSearch { query: String },
    Error { message: String },
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
#[serde(tag = "kind", rename_all = "camelCase")]
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
#[serde(tag = "kind", rename_all = "camelCase")]
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
#[serde(tag = "type", rename_all = "camelCase")]
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
#[serde(tag = "kind", rename_all = "camelCase")]
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
