//! The embedded engine's public types (`docs/embedding.md`).

use std::path::PathBuf;

use serde::Serialize;
use tokio::sync::mpsc;

use super::wire;

/// What [`super::EmbeddedEngine::new`] takes; it refuses values outside the
/// design's bounds and keeps nothing it refuses.
pub struct EmbeddedConfig {
    pub transport: HostTransport,
    pub work_dir: PathBuf,
    pub namespace: String,
    pub namespace_description: String,
    /// The namespace's `tools` array as JSON, sent as is.
    pub tools: String,
    pub limits: EmbeddedLimits,
}

/// The host's transport: one message per frame, no newline.
pub struct HostTransport {
    pub from_codex: mpsc::Receiver<String>,
    pub to_codex: mpsc::Sender<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EmbeddedLimits {
    /// 1..=16.
    pub pending_calls: u32,
    /// 1..=1,024 requests from Codex, refused ones included.
    pub calls_per_turn: u32,
    /// 1..=65,536 of a call's raw arguments.
    pub argument_bytes: u32,
    /// 1..=65,536 of an answer's text, in UTF-8.
    pub result_bytes: u32,
    /// 256 KiB..=16 MiB of unread events' strings.
    pub event_bytes: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum EmbeddedError {
    /// Nothing was written, and nothing changed.
    #[error("invalid: {reason}")]
    Invalid { reason: &'static str },
    #[error("Codex refused ({code}): {message}")]
    Codex { code: i64, message: String },
    #[error("the embedded engine is closed")]
    Closed { fault: Option<Fault> },
}

/// Why the engine closed itself: a fixed code, never supplier text.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Fault {
    InvalidFrame,
    RepeatedId,
    TooManyRequests,
    WrongTurn,
    OverBudget,
    InputEnded,
    OutputClosed,
    OutputFull,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum EmbeddedEvent {
    RemoteControl { status: RemoteControl },
    Notification { method: Method, turn: Option<TurnId>, item: Option<ItemRef>, status: Option<String>, json: String },
    Refused { method: String, thread: Option<String>, json: String },
    Unheld { method: String, thread: String, json: String },
    Unmapped { method: String, thread: Option<String>, json: String },
    Closed { fault: Option<Fault> },
}

/// An item a notification names; deltas carry no type.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ItemRef {
    pub id: String,
    pub item_type: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum RemoteControl {
    Disabled,
    Connecting,
    Connected,
    Errored,
    Other,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Method {
    ThreadStarted,
    ThreadStatus,
    ThreadClosed,
    TokenUsage,
    RateLimits,
    TurnStarted,
    TurnCompleted,
    ItemStarted,
    ItemCompleted,
    AgentMessageDelta,
    ReasoningSummaryDelta,
    ReasoningSummaryPart,
    ReasoningDelta,
    Retrying,
    Error,
    Warning,
    ConfigWarning,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize)]
#[serde(transparent)]
pub struct ThreadId(pub String);

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize)]
#[serde(transparent)]
pub struct TurnId(pub String);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ThreadOptions {
    pub model: Option<String>,
    pub provider: Option<String>,
    pub base_instructions: Option<String>,
    pub policy: ThreadPolicy,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ThreadPolicy {
    pub sandbox: SandboxMode,
    pub approval: ApprovalPolicy,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SandboxMode {
    ReadOnly,
    WorkspaceWrite,
    DangerFullAccess,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ApprovalPolicy {
    Untrusted,
    OnRequest,
    Never,
}

/// Each as Codex reported it in the `thread/start` reply.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OpenedThread {
    /// `result.thread.id`, an identity.
    pub thread: ThreadId,
    /// `result.approvalPolicy`.
    pub approval_policy: Reported<String>,
    /// `result.sandbox.type`.
    pub sandbox_type: Reported<String>,
    /// `result.sandbox.networkAccess`.
    pub network_access: Reported<bool>,
    /// `result.thread.ephemeral`.
    pub ephemeral: Reported<bool>,
    /// `result.thread.environments`, counted, not kept.
    pub environments: Reported<u32>,
    /// `result.model`.
    pub model: Reported<String>,
    /// `result.modelProvider`.
    pub model_provider: Reported<String>,
    /// `result.cwd`.
    pub cwd: Reported<String>,
}

/// `Absent`: missing or `null`. `Other`: present but not of this type, a
/// string over 256 bytes once unescaped (1,024 for `cwd`), or a constant with
/// an escape.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Reported<T> {
    Absent,
    Value(T),
    Other,
}

/// The Responses API namespaces Codex reserves (0.171.0, `validate_dynamic_tools`).
const RESERVED: [&str; 14] = [
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

impl EmbeddedConfig {
    /// Checks the configuration against the design's bounds and Codex's own
    /// rules; the tools' names when it holds.
    pub(crate) fn validate(&self) -> Result<Vec<String>, &'static str> {
        let limits = &self.limits;
        let ranges = [
            (limits.pending_calls, 1, 16),
            (limits.calls_per_turn, 1, 1024),
            (limits.argument_bytes, 1, 65_536),
            (limits.result_bytes, 1, 65_536),
            (limits.event_bytes, 256 * 1024, 16 * 1024 * 1024),
        ];
        if ranges.iter().any(|(value, low, high)| !(low..=high).contains(&value)) {
            return Err("a limit is out of range");
        }
        let namespace = &self.namespace;
        if !is_name(namespace, 64) || is_mcp(namespace) || RESERVED.contains(&namespace.as_str()) || namespace.capacity() > 64 {
            return Err("the namespace is not one Codex takes");
        }
        if self.namespace_description.chars().count() > 1024 || self.namespace_description.capacity() > 4096 {
            return Err("the namespace description is over 1,024 characters");
        }
        let work_dir = self.work_dir.to_str().unwrap_or_default();
        if !self.work_dir.is_absolute() || work_dir.is_empty() || work_dir.len() > 1024 || self.work_dir.capacity() > 1024 {
            return Err("work_dir is not an absolute UTF-8 path of at most 1,024 bytes");
        }
        if self.tools.capacity() > 128 * 1024 || !wire::prescan(&self.tools) {
            return Err("tools is not JSON of at most 128 KiB");
        }
        let mut names: Vec<String> = Vec::with_capacity(64);
        let mut bad = false;
        let count = wire::elements(wire::trim(&self.tools), &mut |tool| {
            let mut buf = [0; 128];
            let name = wire::pick(tool.get(), ["name"]).and_then(|[name]| name).and_then(|name| wire::unescape(name, &mut buf));
            match name {
                Some(name) if names.len() < 64 && is_name(name, 128) && !is_mcp(name) && !names.iter().any(|n| n == name) => names.push(name.to_owned()),
                _ => bad = true,
            }
        });
        match count {
            Some(1..=64) if !bad => Ok(names),
            _ => Err("tools is not an array of 1 to 64 objects with unique names Codex takes"),
        }
    }
}

fn is_name(name: &str, max: usize) -> bool {
    !name.is_empty() && name.len() <= max && name.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'))
}

fn is_mcp(name: &str) -> bool {
    name == "mcp" || name.starts_with("mcp__")
}

impl SandboxMode {
    pub(crate) const fn wire(self) -> &'static str {
        match self {
            SandboxMode::ReadOnly => "read-only",
            SandboxMode::WorkspaceWrite => "workspace-write",
            SandboxMode::DangerFullAccess => "danger-full-access",
        }
    }
}

impl ApprovalPolicy {
    pub(crate) const fn wire(self) -> &'static str {
        match self {
            ApprovalPolicy::Untrusted => "untrusted",
            ApprovalPolicy::OnRequest => "on-request",
            ApprovalPolicy::Never => "never",
        }
    }
}
