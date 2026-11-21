//! The part of the Codex app-server protocol (0.171.0) the engine uses, as
//! serde types of our own. Fields we do not use are left out; incoming types
//! accept what Codex adds.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

// ---------- handshake ----------

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InitializeParams {
    pub client_info: ClientInfo,
    pub capabilities: Capabilities,
}

#[derive(Serialize)]
pub struct ClientInfo {
    pub name: &'static str,
    pub title: &'static str,
    pub version: &'static str,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Capabilities {
    pub experimental_api: bool,
    pub request_attestation: bool,
    pub opt_out_notification_methods: Vec<&'static str>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InitializeResponse {
    pub user_agent: String,
}

// ---------- account and models ----------

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountResponse {
    pub account: Option<AccountInfo>,
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum AccountInfo {
    Chatgpt { email: Option<String>, #[serde(rename = "planType")] plan_type: Option<String> },
    #[serde(other)]
    Other,
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum LoginResponse {
    #[serde(rename_all = "camelCase")]
    Chatgpt { login_id: String, auth_url: String },
    #[serde(other)]
    Other,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LoginCompleted {
    pub login_id: Option<String>,
    pub success: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelListResponse {
    pub data: Vec<ModelInfo>,
    pub next_cursor: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelInfo {
    pub id: String,
    pub display_name: String,
    #[serde(default)]
    pub hidden: bool,
    #[serde(default)]
    pub supported_reasoning_efforts: Vec<EffortOption>,
    pub default_reasoning_effort: Option<String>,
    #[serde(default)]
    pub is_default: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EffortOption {
    pub reasoning_effort: String,
}

// ---------- threads ----------

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Thread {
    pub id: String,
    pub forked_from_id: Option<String>,
    pub parent_thread_id: Option<String>,
    #[serde(default)]
    pub preview: String,
    pub model: Option<String>,
    pub reasoning_effort: Option<String>,
    #[serde(default)]
    pub updated_at: i64,
    pub status: ThreadStatus,
    pub cwd: String,
    pub name: Option<String>,
    #[serde(default)]
    pub turns: Vec<Turn>,
    /// A subagent's nickname, as Codex gives it.
    pub agent_nickname: Option<String>,
    /// Where the thread came from; a subagent's names its task path.
    #[serde(default)]
    pub source: Value,
}

impl Thread {
    /// A subagent's task path (`/root/worker`).
    pub fn agent_path(&self) -> Option<String> {
        self.source.pointer("/subAgent/thread_spawn/agent_path").and_then(Value::as_str).map(str::to_string)
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum ThreadStatus {
    NotLoaded,
    Idle,
    SystemError,
    Active,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Turn {
    pub id: String,
    #[serde(default, deserialize_with = "lenient_items")]
    pub items: Vec<ThreadItem>,
    pub status: TurnStatus,
    pub error: Option<TurnError>,
}

/// Items that do not decode are left out, so one odd item never loses its turn.
fn lenient_items<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<Vec<ThreadItem>, D::Error> {
    let values = Vec::<Value>::deserialize(deserializer)?;
    Ok(values.into_iter().filter_map(|v| serde_json::from_value(v).ok()).collect())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum TurnStatus {
    Completed,
    Interrupted,
    Failed,
    InProgress,
}

#[derive(Clone, Debug, Deserialize)]
pub struct TurnError {
    pub message: String,
}

#[derive(Deserialize)]
pub struct ThreadResponse {
    pub thread: Thread,
}

/// `thread/resume`: the thread, and the sandbox Codex resumed it with.
#[derive(Deserialize)]
pub struct ResumeResponse {
    #[allow(dead_code)]
    pub thread: Thread,
    pub sandbox: Option<Value>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadListResponse {
    pub data: Vec<Thread>,
    pub next_cursor: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TurnsListResponse {
    pub data: Vec<Turn>,
    pub next_cursor: Option<String>,
}

#[derive(Deserialize)]
pub struct TurnResponse {
    pub turn: Turn,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadStartParams {
    pub cwd: String,
    pub model: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dynamic_tools: Option<Vec<DynamicToolSpec>>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadListParams {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwd: Option<Vec<String>>,
    /// Every subagent below this thread, at any depth (a project's listing has none).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ancestor_thread_id: Option<String>,
    pub limit: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
}

// ---------- items ----------

#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum ThreadItem {
    UserMessage { id: String, content: Vec<InputInfo> },
    AgentMessage { id: String, text: String },
    Plan { id: String, text: String },
    Reasoning { id: String, #[serde(default)] summary: Vec<String> },
    #[serde(rename_all = "camelCase")]
    CommandExecution { id: String, command: String, cwd: String, aggregated_output: Option<String>, exit_code: Option<i32> },
    FileChange { id: String, changes: Vec<FileChangeInfo> },
    McpToolCall {
        id: String,
        server: String,
        tool: String,
        arguments: Value,
        result: Option<Value>,
        error: Option<TurnError>,
        #[serde(default)]
        status: Option<ToolStatusInfo>,
    },
    #[serde(rename_all = "camelCase")]
    DynamicToolCall {
        id: String,
        namespace: Option<String>,
        tool: String,
        arguments: Value,
        #[serde(default)]
        status: Option<ToolStatusInfo>,
        /// `inputText`, `inputImage`, `inputAudio`; kept whole so none is lost.
        content_items: Option<Vec<Value>>,
    },
    WebSearch { id: String, query: String },
    /// The agent started a subagent, messaged one, stopped one, or one finished.
    #[serde(rename_all = "camelCase")]
    SubAgentActivity { id: String, kind: String, agent_thread_id: String, agent_path: String },
    /// One of the agent's own tools for its subagents (a wait, a failed start).
    #[serde(rename_all = "camelCase")]
    CollabAgentToolCall {
        id: String,
        tool: String,
        status: String,
        #[serde(default)]
        receiver_thread_ids: Vec<String>,
        prompt: Option<String>,
    },
    #[serde(other)]
    Other,
}

/// An MCP or dynamic tool call's status.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ToolStatusInfo {
    InProgress,
    Completed,
    Failed,
    #[serde(other)]
    Other,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum InputInfo {
    Text { text: String },
    LocalImage { path: String },
    Image,
    #[serde(other)]
    Other,
}

#[derive(Clone, Debug, Deserialize)]
pub struct FileChangeInfo {
    pub path: String,
    #[serde(default)]
    pub diff: String,
}

// ---------- turns ----------

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum UserInput {
    Text { text: String, text_elements: Vec<Value> },
    LocalImage { path: String },
}

impl UserInput {
    pub fn text(text: impl Into<String>) -> UserInput {
        UserInput::Text { text: text.into(), text_elements: Vec::new() }
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TurnStartParams {
    pub thread_id: String,
    pub input: Vec<UserInput>,
    pub model: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
    pub approval_policy: Value,
    pub approvals_reviewer: &'static str,
    pub sandbox_policy: Value,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TurnSteerParams {
    pub thread_id: String,
    pub expected_turn_id: String,
    pub input: Vec<UserInput>,
}

// ---------- notifications ----------

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadNote {
    pub thread_id: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TurnNote {
    pub thread_id: String,
    pub turn: Turn,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ItemNote {
    pub thread_id: String,
    pub turn_id: String,
    pub item: ThreadItem,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeltaNote {
    pub thread_id: String,
    pub turn_id: String,
    pub item_id: String,
    pub delta: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanNote {
    pub thread_id: String,
    pub turn_id: String,
    pub plan: Vec<PlanStepInfo>,
}

#[derive(Deserialize)]
pub struct PlanStepInfo {
    pub step: String,
    pub status: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StatusNote {
    pub thread_id: String,
    pub status: ThreadStatus,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NameNote {
    pub thread_id: String,
    pub thread_name: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ErrorNote {
    pub thread_id: String,
    pub turn_id: String,
    pub error: TurnError,
    #[serde(default)]
    pub will_retry: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResolvedNote {
    pub request_id: Value,
}

// ---------- the server's requests ----------

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CommandApproval {
    pub thread_id: String,
    pub command: Option<String>,
    pub cwd: Option<String>,
    pub reason: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileChangeApproval {
    pub thread_id: String,
    pub item_id: String,
    pub reason: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PermissionsApproval {
    pub thread_id: String,
    pub reason: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UserInputRequest {
    pub thread_id: String,
    pub questions: Vec<QuestionInfo>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QuestionInfo {
    pub id: String,
    #[serde(default)]
    pub header: String,
    pub question: String,
    #[serde(default)]
    pub is_other: bool,
    pub options: Option<Vec<OptionInfo>>,
}

#[derive(Deserialize)]
pub struct OptionInfo {
    pub label: String,
}

#[derive(Serialize)]
pub struct UserInputAnswers {
    pub answers: BTreeMap<String, AnswerList>,
}

#[derive(Serialize)]
pub struct AnswerList {
    pub answers: Vec<String>,
}

// ---------- the application's tools ----------

/// A `thread/start` `dynamicTools` entry. The engine gives its tools in a
/// namespace only, so none clashes with one of Codex's own.
#[derive(Serialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum DynamicToolSpec {
    Namespace { name: String, description: String, tools: Vec<NamespaceTool> },
}

/// A namespace holds functions only.
#[derive(Serialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum NamespaceTool {
    Function(FunctionSpec),
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FunctionSpec {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
}

/// `item/tool/call`.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DynamicToolCall {
    pub thread_id: String,
    pub turn_id: String,
    pub call_id: String,
    pub namespace: Option<String>,
    pub tool: String,
    #[serde(default)]
    pub arguments: Value,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DynamicToolResponse {
    pub content_items: Vec<ToolContentItem>,
    pub success: bool,
}

#[derive(Serialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum ToolContentItem {
    InputText { text: String },
    #[serde(rename_all = "camelCase")]
    InputImage { image_url: String },
}

// ---------- files ----------

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FuzzyParams {
    pub query: String,
    pub roots: Vec<String>,
    pub cancellation_token: Option<String>,
}

#[derive(Deserialize)]
pub struct FuzzyResponse {
    pub files: Vec<FuzzyFile>,
}

#[derive(Deserialize)]
pub struct FuzzyFile {
    pub root: String,
    pub path: String,
}
