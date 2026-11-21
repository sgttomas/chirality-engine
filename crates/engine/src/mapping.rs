//! From Codex's protocol to the engine's API, and back for what the person
//! sends: messages, access levels and answers.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde_json::{Value, json};

use crate::api::*;
use crate::assignment::marker::{self, Marker};
use crate::protocol::{self as p, ThreadItem, UserInput};

// ---------- what the person sends ----------

const FILE_PREFIX: &str = "Attached file: ";
const WORKFLOW_OPEN: &str = "<workflow>\n";

/// A picked workflow, read from its `WORKFLOW.md`.
pub(crate) struct PickedWorkflow {
    pub id: WorkflowId,
    pub name: String,
    pub body: String,
}

/// The turn input for a message: another thread's mark when it sent it, the
/// text, then each attachment, then a picked workflow as its own text, the
/// way Codex sends a picked skill.
pub(crate) fn message_input(message: &Message, workflow: Option<&PickedWorkflow>, from: Option<&Marker>) -> Vec<UserInput> {
    let mut input: Vec<UserInput> = from.map(|m| UserInput::text(marker::render(m))).into_iter().collect();
    input.push(UserInput::text(message.text.clone()));
    for attachment in &message.attachments {
        match attachment {
            Attachment::Image { path } => input.push(UserInput::LocalImage { path: path.display().to_string() }),
            Attachment::File { path } => input.push(UserInput::text(format!("{FILE_PREFIX}{}", path.display()))),
        }
    }
    if let Some(w) = workflow {
        input.push(UserInput::text(format!("{WORKFLOW_OPEN}<name>{}</name>\n<path>{}</path>\n{}\n</workflow>", w.name, w.id.0, w.body.trim_end())));
    }
    input
}

/// A thread's title from Codex's preview, which joins the first message's
/// parts: the text comes first (after another thread's mark), so it is what is kept.
pub(crate) fn title_from_preview(preview: &str) -> Option<String> {
    let preview = marker::from_preview(preview).1;
    let end = [FILE_PREFIX, WORKFLOW_OPEN].iter().filter_map(|marker| preview.find(marker)).min().unwrap_or(preview.len());
    Some(preview[..end].trim().to_string()).filter(|t| !t.is_empty())
}

fn user_message(content: &[p::InputInfo]) -> ItemBody {
    let mut texts = Vec::new();
    let mut attachments = Vec::new();
    let mut workflow = None;
    let from = match content.first() {
        Some(p::InputInfo::Text { text }) => marker::part(text),
        _ => None,
    };
    for part in content.iter().skip(from.is_some() as usize) {
        match part {
            p::InputInfo::Text { text } if text.starts_with(WORKFLOW_OPEN) => {
                workflow = text
                    .lines()
                    .find_map(|l| l.strip_prefix("<path>").and_then(|l| l.strip_suffix("</path>")))
                    .map(|path| WorkflowId(path.to_string()));
            }
            p::InputInfo::Text { text } => match text.strip_prefix(FILE_PREFIX) {
                Some(path) if !path.contains('\n') => attachments.push(Attachment::File { path: path.into() }),
                _ => texts.push(text.as_str()),
            },
            p::InputInfo::LocalImage { path } => attachments.push(Attachment::Image { path: path.into() }),
            p::InputInfo::Image | p::InputInfo::Other => {}
        }
    }
    ItemBody::UserMessage { text: texts.join("\n"), attachments, workflow, from: from.as_ref().map(Marker::from) }
}

// ---------- items, turns, threads ----------

pub(crate) fn item(item: &ThreadItem, done: bool) -> Option<Item> {
    let (id, body) = match item {
        ThreadItem::UserMessage { id, content } => (id, user_message(content)),
        ThreadItem::AgentMessage { id, text } | ThreadItem::Plan { id, text } => (id, ItemBody::AgentMessage { text: text.clone() }),
        ThreadItem::Reasoning { id, summary } => (id, ItemBody::Reasoning { summary: summary.join("\n\n") }),
        ThreadItem::CommandExecution { id, command, cwd, aggregated_output, exit_code } => (
            id,
            ItemBody::Command { command: command.clone(), cwd: cwd.into(), output: aggregated_output.clone().unwrap_or_default(), exit_code: *exit_code },
        ),
        ThreadItem::FileChange { id, changes } => (id, ItemBody::FileChange { files: file_diffs(changes) }),
        ThreadItem::McpToolCall { id, server, tool, arguments, result, error, status } => {
            let (text, images) = result.as_ref().map(mcp_result).unwrap_or_default();
            let body = ItemBody::ToolCall {
                server: Some(server.clone()),
                tool: tool.clone(),
                arguments: arguments.to_string(),
                status: tool_status(*status, error.is_some(), done),
                result: error.as_ref().map(|e| e.message.clone()).or(text),
                images,
            };
            (id, body)
        }
        ThreadItem::DynamicToolCall { id, namespace, tool, arguments, status, content_items } if namespace.as_deref() == Some(crate::assignment::NAMESPACE) => {
            let (result, _) = dynamic_result(content_items.as_deref().unwrap_or_default());
            let body = crate::assignment::tools::item(tool, arguments, result.as_deref(), tool_status(*status, false, done))?;
            (id, body)
        }
        ThreadItem::DynamicToolCall { id, namespace, tool, arguments, status, content_items } => {
            let (result, images) = dynamic_result(content_items.as_deref().unwrap_or_default());
            let body = ItemBody::ToolCall { server: namespace.clone(), tool: tool.clone(), arguments: arguments.to_string(), status: tool_status(*status, false, done), result, images };
            (id, body)
        }
        ThreadItem::WebSearch { id, query } => (id, ItemBody::WebSearch { query: query.clone() }),
        ThreadItem::SubAgentActivity { id, kind, agent_thread_id, agent_path } => {
            let activity = match kind.as_str() {
                "started" => SubagentActivity::Started,
                "interacted" => SubagentActivity::Messaged,
                "interrupted" => SubagentActivity::Interrupted,
                "completed" => SubagentActivity::Finished,
                _ => return None,
            };
            (id, ItemBody::Subagent { thread: ThreadId(agent_thread_id.clone()), path: agent_path.clone(), activity })
        }
        ThreadItem::CollabAgentToolCall { id, tool, status, receiver_thread_ids, prompt } => {
            let tool = serde_json::from_value(Value::String(tool.clone())).ok()?;
            let status = match status.as_str() {
                "inProgress" => ToolCallStatus::Running,
                "completed" => ToolCallStatus::Completed,
                _ => ToolCallStatus::Failed,
            };
            let threads = receiver_thread_ids.iter().cloned().map(ThreadId).collect();
            (id, ItemBody::SubagentTool { tool, status, threads, prompt: prompt.clone() })
        }
        ThreadItem::Other => return None,
    };
    Some(Item { id: ItemId(id.clone()), body, done })
}

/// An application tool's answer: its text and images (`data:` URLs); any
/// other item (audio) is kept as its JSON.
fn dynamic_result(items: &[Value]) -> (Option<String>, Vec<String>) {
    let mut texts = Vec::new();
    let mut images = Vec::new();
    for item in items {
        match (item["type"].as_str(), item["text"].as_str(), item["imageUrl"].as_str()) {
            (Some("inputText"), Some(text), _) => texts.push(text.to_string()),
            (Some("inputImage"), _, Some(url)) => images.push(url.to_string()),
            _ => texts.push(item.to_string()),
        }
    }
    ((!texts.is_empty()).then(|| texts.join("\n")), images)
}

/// An MCP result's text and images (`data:` URLs). Text blocks are joined;
/// other blocks, and structured content, are kept as JSON.
fn mcp_result(result: &Value) -> (Option<String>, Vec<String>) {
    let Some(blocks) = result.get("content").and_then(Value::as_array) else {
        return (Some(result.to_string()), Vec::new());
    };
    let mut texts = Vec::new();
    let mut images = Vec::new();
    for block in blocks {
        match (block["type"].as_str(), block["text"].as_str(), block["data"].as_str(), block["mimeType"].as_str()) {
            (Some("text"), Some(text), _, _) => texts.push(text.to_string()),
            (Some("image"), _, Some(data), Some(mime)) => images.push(format!("data:{mime};base64,{data}")),
            _ => texts.push(block.to_string()),
        }
    }
    if let Some(structured) = result.get("structuredContent").filter(|v| !v.is_null()) {
        texts.push(structured.to_string());
    }
    ((!texts.is_empty()).then(|| texts.join("\n")), images)
}

/// Codex's status for a tool call; for history without one, what is known.
fn tool_status(status: Option<p::ToolStatusInfo>, error: bool, done: bool) -> ToolCallStatus {
    match status {
        Some(p::ToolStatusInfo::InProgress) => ToolCallStatus::Running,
        Some(p::ToolStatusInfo::Completed) => ToolCallStatus::Completed,
        Some(p::ToolStatusInfo::Failed) => ToolCallStatus::Failed,
        _ if error => ToolCallStatus::Failed,
        _ if done => ToolCallStatus::Completed,
        _ => ToolCallStatus::Running,
    }
}

pub(crate) fn file_diffs(changes: &[p::FileChangeInfo]) -> Vec<FileDiff> {
    changes.iter().map(|c| FileDiff { path: c.path.clone().into(), diff: c.diff.clone() }).collect()
}

pub(crate) fn turn_state(turn: &p::Turn) -> TurnState {
    match turn.status {
        p::TurnStatus::InProgress => TurnState::Running,
        p::TurnStatus::Completed => TurnState::Completed,
        p::TurnStatus::Interrupted => TurnState::Interrupted,
        p::TurnStatus::Failed => TurnState::Failed { message: turn.error.as_ref().map(|e| e.message.clone()).unwrap_or_else(|| "The turn failed".into()) },
    }
}

pub(crate) fn turn(turn: &p::Turn) -> Turn {
    Turn { id: TurnId(turn.id.clone()), items: turn.items.iter().filter_map(|i| item(i, true)).collect(), state: turn_state(turn) }
}

pub(crate) fn plan(turn: &str, steps: &[p::PlanStepInfo]) -> Item {
    let steps = steps
        .iter()
        .map(|s| PlanStep {
            text: s.step.clone(),
            status: match s.status.as_str() {
                "completed" => PlanStepStatus::Completed,
                "inProgress" => PlanStepStatus::InProgress,
                _ => PlanStepStatus::Pending,
            },
        })
        .collect();
    Item { id: ItemId(format!("plan-{turn}")), body: ItemBody::Plan { steps }, done: true }
}

pub(crate) fn model(info: &p::ModelInfo) -> Model {
    Model {
        id: info.id.clone(),
        name: info.display_name.clone(),
        efforts: info.supported_reasoning_efforts.iter().map(|e| Effort(e.reasoning_effort.clone())).collect(),
        default_effort: info.default_reasoning_effort.clone().map(Effort),
        is_default: info.is_default,
    }
}

pub(crate) fn account(info: Option<p::AccountInfo>) -> Account {
    match info {
        None => Account::SignedOut,
        Some(p::AccountInfo::Chatgpt { email, plan_type }) => Account::SignedIn { email, plan: plan_type },
        Some(p::AccountInfo::Other) => Account::SignedIn { email: None, plan: None },
    }
}

// ---------- access ----------

/// Codex's own presets: read only, ask (Codex's default), full access.
pub(crate) fn access_policy(access: Access) -> (Value, Value) {
    match access {
        Access::ReadOnly => (json!("on-request"), json!({ "type": "readOnly", "networkAccess": false })),
        Access::Ask => (
            json!("on-request"),
            json!({ "type": "workspaceWrite", "writableRoots": [], "networkAccess": false, "excludeTmpdirEnvVar": false, "excludeSlashTmp": false }),
        ),
        Access::FullAccess => (json!("never"), json!({ "type": "dangerFullAccess" })),
    }
}

// ---------- the person's part ----------

fn decode<T: serde::de::DeserializeOwned>(value: &Value) -> Option<T> {
    serde_json::from_value(value.clone()).ok()
}

pub(crate) const COMMAND_APPROVAL: &str = "item/commandExecution/requestApproval";
pub(crate) const FILE_APPROVAL: &str = "item/fileChange/requestApproval";
pub(crate) const PERMISSIONS_APPROVAL: &str = "item/permissions/requestApproval";
pub(crate) const QUESTION: &str = "item/tool/requestUserInput";

/// A server request the person answers, as the API shows it; `None` for one
/// the engine does not show (it is answered as unsupported).
pub(crate) fn request(method: &str, params: &Value, file_changes: impl Fn(&str) -> Vec<FileDiff>) -> Option<(ThreadId, Request)> {
    match method {
        COMMAND_APPROVAL => {
            let r: p::CommandApproval = decode(params)?;
            Some((ThreadId(r.thread_id), Request::Command { command: r.command.unwrap_or_default(), cwd: r.cwd.unwrap_or_default().into(), reason: r.reason }))
        }
        FILE_APPROVAL => {
            let r: p::FileChangeApproval = decode(params)?;
            Some((ThreadId(r.thread_id), Request::FileChange { files: file_changes(&r.item_id), reason: r.reason }))
        }
        PERMISSIONS_APPROVAL => {
            let r: p::PermissionsApproval = decode(params)?;
            Some((ThreadId(r.thread_id), Request::Permissions { reason: r.reason }))
        }
        QUESTION => {
            let r: p::UserInputRequest = decode(params)?;
            let questions = r
                .questions
                .into_iter()
                .map(|q| Question {
                    id: q.id,
                    header: Some(q.header).filter(|h| !h.is_empty()),
                    text: q.question,
                    options: q.options.unwrap_or_default().into_iter().map(|o| o.label).collect(),
                    free_text: q.is_other,
                })
                .collect();
            Some((ThreadId(r.thread_id), Request::Question { questions }))
        }
        _ => None,
    }
}

/// The reply Codex expects for `answer`, and whether the turn is to stop too.
pub(crate) fn reply(method: &str, params: &Value, answer: &Answer) -> std::result::Result<(Value, bool), EngineError> {
    let invalid = || EngineError::Invalid { message: "that answer does not fit this request".into() };
    match method {
        COMMAND_APPROVAL | FILE_APPROVAL => {
            if let (Answer::AllowForSession, Some(amendment)) = (answer, standing_amendment(params)) {
                return Ok((json!({ "decision": amendment }), false));
            }
            let decision = match answer {
                Answer::Allow => "accept",
                Answer::AllowForSession => "acceptForSession",
                Answer::Decline => "decline",
                Answer::Cancel => "cancel",
                Answer::Answers { .. } => return Err(invalid()),
            };
            Ok((json!({ "decision": decision }), false))
        }
        PERMISSIONS_APPROVAL => {
            let asked = params.get("permissions").cloned().unwrap_or(json!({}));
            let granted = |scope| json!({ "permissions": strip_nulls(asked.clone()), "scope": scope });
            match answer {
                Answer::Allow => Ok((granted("turn"), false)),
                Answer::AllowForSession => Ok((granted("session"), false)),
                Answer::Decline => Ok((json!({ "permissions": {}, "scope": "turn" }), false)),
                Answer::Cancel => Ok((json!({ "permissions": {}, "scope": "turn" }), true)),
                Answer::Answers { .. } => Err(invalid()),
            }
        }
        QUESTION => match answer {
            Answer::Answers { answers } => {
                let mut by_id: BTreeMap<String, p::AnswerList> = BTreeMap::new();
                for (id, text) in answers {
                    by_id.entry(id.clone()).or_insert_with(|| p::AnswerList { answers: Vec::new() }).answers.push(text.clone());
                }
                Ok((serde_json::to_value(p::UserInputAnswers { answers: by_id }).expect("answers serialise"), false))
            }
            Answer::Decline => Ok((json!({ "answers": {} }), false)),
            Answer::Cancel => Ok((json!({ "answers": {} }), true)),
            Answer::Allow | Answer::AllowForSession => Err(invalid()),
        },
        _ => Err(invalid()),
    }
}

/// When Codex offers no `acceptForSession` for a command but offers to allow
/// it from now on (an exec-policy amendment), that is what "for this session"
/// can be.
fn standing_amendment(params: &Value) -> Option<Value> {
    let offered = params.get("availableDecisions")?.as_array()?;
    if offered.iter().any(|d| d == "acceptForSession") {
        return None;
    }
    offered.iter().find(|d| d.get("acceptWithExecpolicyAmendment").is_some()).cloned()
}

/// A granted profile leaves out what was not asked for.
fn strip_nulls(value: Value) -> Value {
    match value {
        Value::Object(map) => Value::Object(map.into_iter().filter(|(_, v)| !v.is_null()).map(|(k, v)| (k, strip_nulls(v))).collect()),
        other => other,
    }
}

pub(crate) fn same_folder(a: &str, b: &std::path::Path) -> bool {
    let a = PathBuf::from(a);
    a == b || a.canonicalize().ok().zip(b.canonicalize().ok()).is_some_and(|(x, y)| x == y)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_message_round_trips_through_codex_input() {
        let message = Message {
            text: "Go".into(),
            attachments: vec![Attachment::File { path: "/p/a.md".into() }, Attachment::Image { path: "/p/i.png".into() }],
            workflow: Some(WorkflowId("/lib/w/WORKFLOW.md".into())),
        };
        let picked = PickedWorkflow { id: WorkflowId("/lib/w/WORKFLOW.md".into()), name: "w".into(), body: "# W\nDo it.\n".into() };
        let input = message_input(&message, Some(&picked), None);
        let content: Vec<p::InputInfo> = serde_json::from_value(serde_json::to_value(&input).unwrap()).unwrap();
        assert_eq!(user_message(&content), ItemBody::UserMessage { text: "Go".into(), attachments: message.attachments.clone(), workflow: message.workflow.clone(), from: None });
        // From another thread: its mark first, which is not part of the text, nor of the title.
        let mark = Marker { from: ThreadId("lead".into()), title: Some("Lead".into()), report: Some(TurnEnd::Completed) };
        let input = message_input(&message, Some(&picked), Some(&mark));
        let content: Vec<p::InputInfo> = serde_json::from_value(serde_json::to_value(&input).unwrap()).unwrap();
        let ItemBody::UserMessage { text, from, workflow, .. } = user_message(&content) else { panic!() };
        assert_eq!((text.as_str(), from, workflow), ("Go", Some(mark.from()), message.workflow.clone()));
        assert_eq!(title_from_preview(&format!("{}Check the piles{FILE_PREFIX}/p/a.md", marker::render(&mark))).as_deref(), Some("Check the piles"));
    }

    #[test]
    fn tool_results_keep_their_text_images_and_status() {
        let mcp = json!({"type": "mcpToolCall", "id": "m", "server": "files", "tool": "read", "arguments": {}, "status": "completed", "error": null,
            "result": {"content": [{"type": "text", "text": "a"}, {"type": "image", "data": "AA", "mimeType": "image/png"}, {"type": "resource", "uri": "x"}], "structuredContent": null}});
        let ItemBody::ToolCall { status, result, images, .. } = done_body(mcp) else { panic!() };
        assert_eq!((status, result.as_deref(), images), (ToolCallStatus::Completed, Some("a\n{\"type\":\"resource\",\"uri\":\"x\"}"), vec!["data:image/png;base64,AA".to_string()]));
        let dynamic = json!({"type": "dynamicToolCall", "id": "d", "namespace": "estimate", "tool": "plot", "arguments": {}, "status": "completed", "success": true,
            "contentItems": [{"type": "inputImage", "imageUrl": "data:image/png;base64,BB"}]});
        let ItemBody::ToolCall { status, result, images, .. } = done_body(dynamic) else { panic!() };
        assert_eq!((status, result, images), (ToolCallStatus::Completed, None, vec!["data:image/png;base64,BB".to_string()]));
        let audio = json!({"type": "dynamicToolCall", "id": "d", "namespace": "estimate", "tool": "say", "arguments": {}, "status": "completed", "success": true,
            "contentItems": [{"type": "inputText", "text": "said"}, {"type": "inputAudio", "audioUrl": "data:audio/wav;base64,CC"}]});
        let ItemBody::ToolCall { result, .. } = done_body(audio) else { panic!() };
        assert_eq!(result.as_deref(), Some("said\n{\"audioUrl\":\"data:audio/wav;base64,CC\",\"type\":\"inputAudio\"}"));
        let structured = json!({"type": "mcpToolCall", "id": "m", "server": "files", "tool": "stat", "arguments": {}, "status": "completed", "error": null,
            "result": {"content": [{"type": "text", "text": "ok"}], "structuredContent": {"size": 3}}});
        let ItemBody::ToolCall { result, .. } = done_body(structured) else { panic!() };
        assert_eq!(result.as_deref(), Some("ok\n{\"size\":3}"));
        let running = json!({"type": "dynamicToolCall", "id": "d", "namespace": null, "tool": "t", "arguments": {}, "status": "inProgress", "contentItems": null});
        assert!(matches!(item_body(running), ItemBody::ToolCall { status: ToolCallStatus::Running, .. }));
    }

    #[test]
    fn subagent_items_name_the_subagent() {
        let started = json!({"type": "subAgentActivity", "id": "call_spawn", "kind": "started", "agentThreadId": "c", "agentPath": "/root/worker", "model": "m"});
        assert_eq!(done_body(started), ItemBody::Subagent { thread: ThreadId("c".into()), path: "/root/worker".into(), activity: SubagentActivity::Started });
        let wait = json!({"type": "collabAgentToolCall", "id": "call_wait", "tool": "wait", "status": "inProgress", "senderThreadId": "p", "receiverThreadIds": [], "prompt": null, "agentsStates": {}});
        assert_eq!(item_body(wait), ItemBody::SubagentTool { tool: SubagentTool::Wait, status: ToolCallStatus::Running, threads: vec![], prompt: None });
        let unknown = json!({"type": "subAgentActivity", "id": "x", "kind": "renamed", "agentThreadId": "c", "agentPath": "/root/worker"});
        assert!(item(&serde_json::from_value(unknown).unwrap(), true).is_none());
    }

    fn item_body(value: Value) -> ItemBody {
        item(&serde_json::from_value(value).unwrap(), false).unwrap().body
    }

    fn done_body(value: Value) -> ItemBody {
        item(&serde_json::from_value(value).unwrap(), true).unwrap().body
    }

    #[test]
    fn answers_map_to_codex_replies() {
        let q = reply(QUESTION, &json!({}), &Answer::Answers { answers: vec![("a".into(), "Yes".into()), ("a".into(), "Also".into())] }).unwrap();
        assert_eq!(q, (json!({"answers": {"a": {"answers": ["Yes", "Also"]}}}), false));
        assert_eq!(reply(COMMAND_APPROVAL, &json!({}), &Answer::AllowForSession).unwrap().0, json!({"decision": "acceptForSession"}));
        let asked = json!({"permissions": {"network": {"enabled": true}, "fileSystem": null}});
        assert_eq!(reply(PERMISSIONS_APPROVAL, &asked, &Answer::Allow).unwrap().0, json!({"permissions": {"network": {"enabled": true}}, "scope": "turn"}));
        assert!(reply(FILE_APPROVAL, &json!({}), &Answer::Answers { answers: vec![] }).is_err());
        let amendment = json!({"acceptWithExecpolicyAmendment": {"execpolicy_amendment": ["ls"]}});
        let offered = json!({"availableDecisions": ["accept", amendment, "cancel"]});
        assert_eq!(reply(COMMAND_APPROVAL, &offered, &Answer::AllowForSession).unwrap().0, json!({"decision": amendment}));
    }
}
