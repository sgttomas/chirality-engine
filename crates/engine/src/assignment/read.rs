//! `read`: another thread as its agent can take it in, without waking that
//! thread or queuing anything there. Read from Codex a few turns at a time,
//! newest first (`thread/turns/list`), until the answer is full; nothing is kept.
//!
//! The answer starts with the thread's title, project, status and last error;
//! then its items newest first. Every text in it is cut to a size, and the
//! whole to about 16 KB: the first item of a page is cut further until it fits,
//! so every page shows something or ends. `before` (a cursor an answer gives)
//! goes on from where that answer stopped.

use std::future::Future;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::api::*;
use crate::protocol as p;

/// The most an answer holds, in bytes of JSON.
pub(crate) const ANSWER_LIMIT: usize = 16 * 1024;
const DEFAULT_ITEMS: usize = 20;
const MOST_ITEMS: usize = 100;
/// Turns asked of Codex at a time, and how many such pages one read looks through at most.
pub(crate) const PAGE_TURNS: u32 = 5;
const MOST_PAGES: usize = 20;

/// How much of each kind of text an item keeps, in characters, and how many files a change lists.
#[derive(Clone, Copy, PartialEq)]
struct Limits {
    text: usize,
    output: usize,
    name: usize,
    paths: usize,
}

const FULL: Limits = Limits { text: 2000, output: 600, name: 200, paths: 20 };
const LEAST: Limits = Limits { text: 16, output: 16, name: 16, paths: 1 };

impl Limits {
    fn halved(self) -> Limits {
        let half = |n: usize, least: usize| (n / 2).max(least);
        Limits { text: half(self.text, LEAST.text), output: half(self.output, LEAST.output), name: half(self.name, LEAST.name), paths: half(self.paths, LEAST.paths) }
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub(crate) enum View {
    /// Messages in, and the agent's final replies.
    #[default]
    Messages,
    /// The work as well: commands, file changes and tool calls.
    Activity,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ReadArgs {
    pub thread: String,
    #[serde(default)]
    pub view: View,
    /// The cursor an earlier answer gave.
    pub before: Option<String>,
    /// How many items at most.
    pub limit: Option<usize>,
}

/// Where an answer stopped: the items of `turn` before `item` (the whole turn when none),
/// then the older turns, reading Codex's pages from `page`.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
struct Cursor {
    page: Option<String>,
    turn: Option<String>,
    item: usize,
}

/// What the answer starts with.
pub(crate) struct Header {
    pub thread: ThreadId,
    pub title: Option<String>,
    pub project: Option<String>,
    pub status: ThreadStatus,
    pub error: Option<String>,
}

/// `text`, cut to `limit` characters, with the cut said.
fn cut(text: &str, limit: usize) -> String {
    let count = text.chars().count();
    if count <= limit {
        return text.to_string();
    }
    let kept: String = text.chars().take(limit).collect();
    format!("{kept}… [cut: {} more characters]", count - limit)
}

fn status_word(status: &ThreadStatus) -> &'static str {
    match status {
        ThreadStatus::NeedsApproval | ThreadStatus::NeedsAnswer => "waiting on the person",
        ThreadStatus::Working => "running",
        ThreadStatus::Failed { .. } => "failed",
        ThreadStatus::Idle => "idle",
    }
}

fn turn_word(status: &p::TurnStatus) -> &'static str {
    match status {
        p::TurnStatus::InProgress => "running",
        p::TurnStatus::Completed => "completed",
        p::TurnStatus::Interrupted => "stopped",
        p::TurnStatus::Failed => "failed",
    }
}

fn tool_status(status: ToolCallStatus) -> &'static str {
    match status {
        ToolCallStatus::Running => "inProgress",
        ToolCallStatus::Completed => "completed",
        ToolCallStatus::Failed => "failed",
    }
}

/// A turn's items as the reader gets them, each with its place in the turn, newest first.
fn entries(turn: &p::Turn, view: View, at: Limits) -> Vec<(usize, Value)> {
    let activity = view == View::Activity;
    let completed = turn.status == p::TurnStatus::Completed;
    let messages = || turn.items.iter().enumerate().filter_map(|(i, item)| matches!(item, p::ThreadItem::AgentMessage { .. }).then_some((i, item)));
    // A completed turn's final reply: its last message that is not commentary.
    let last = |i: &(usize, &p::ThreadItem)| !matches!(i.1, p::ThreadItem::AgentMessage { phase: Some(phase), .. } if phase == "commentary");
    let reply = if completed { messages().filter(last).last().map(|(i, _)| i) } else { None };
    let latest = messages().last().map(|(i, _)| i);
    let mut out = Vec::new();
    for (i, item) in turn.items.iter().enumerate() {
        let value = match item {
            p::ThreadItem::AgentMessage { text, phase, .. } => {
                if Some(i) == reply {
                    json!({ "kind": "reply", "text": cut(text, at.text) })
                } else if reply.is_none() && Some(i) == latest {
                    // The latest word of a turn with no final reply: how that turn stands says what it is.
                    json!({ "kind": "update", "turn": turn_word(&turn.status), "phase": phase, "text": cut(text, at.text) })
                } else if activity {
                    json!({ "kind": "message", "phase": phase, "text": cut(text, at.text) })
                } else {
                    continue;
                }
            }
            p::ThreadItem::CommandExecution { command, aggregated_output, exit_code, status, .. } if activity => json!({
                "kind": "command",
                "command": cut(command, at.output),
                "status": status,
                "exitCode": exit_code,
                "output": cut(aggregated_output.as_deref().unwrap_or_default(), at.output),
            }),
            p::ThreadItem::FileChange { changes, status, .. } if activity => {
                let files: Vec<String> = changes.iter().take(at.paths).map(|c| cut(&c.path, at.name)).collect();
                json!({ "kind": "fileChange", "status": status, "files": files, "moreFiles": changes.len() - files.len() })
            }
            _ => match crate::mapping::item(item, true).map(|i| i.body) {
                Some(body) => match other(&body, activity, at) {
                    Some(value) => value,
                    None => continue,
                },
                None => continue,
            },
        };
        out.push((i, value));
    }
    if let (p::TurnStatus::Failed, Some(error)) = (&turn.status, &turn.error) {
        out.push((turn.items.len(), json!({ "kind": "turnFailed", "text": cut(&error.message, at.output) })));
    }
    out.reverse();
    out
}

/// Any other item, from its body: messages in always, the work in the activity view.
fn other(body: &ItemBody, activity: bool, at: Limits) -> Option<Value> {
    Some(match body {
        ItemBody::UserMessage { text, from: Some(from), .. } => {
            let kind = if from.report.is_some() { "report" } else { "message" };
            json!({ "kind": kind, "from": { "thread": from.thread.0, "title": from.title.as_deref().map(|t| cut(t, at.name)) }, "text": cut(text, at.text) })
        }
        ItemBody::UserMessage { text, .. } => json!({ "kind": "person", "text": cut(text, at.text) }),
        _ if !activity => return None,
        ItemBody::ToolCall { server, tool, status, result, .. } => {
            let name = server.as_ref().map_or_else(|| tool.clone(), |s| format!("{s}.{tool}"));
            json!({ "kind": "tool", "name": cut(&name, at.name), "status": tool_status(*status), "result": result.as_deref().map(|r| cut(r, at.output)) })
        }
        ItemBody::ThreadTool { tool, thread, status, .. } => {
            let name = format!("{}.{}", super::NAMESPACE, serde_json::to_value(tool).ok()?.as_str()?);
            json!({ "kind": "tool", "name": name, "thread": thread.as_ref().map(|t| &t.0), "status": tool_status(*status) })
        }
        ItemBody::SubagentTool { tool, status, .. } => json!({ "kind": "tool", "name": serde_json::to_value(tool).ok()?, "status": tool_status(*status) }),
        ItemBody::Subagent { thread, path, activity } => json!({ "kind": "subagent", "thread": thread.0, "path": cut(path, at.name), "activity": serde_json::to_value(activity).ok()? }),
        ItemBody::WebSearch { query } => json!({ "kind": "webSearch", "query": cut(query, at.output) }),
        ItemBody::Error { message } => json!({ "kind": "error", "text": cut(message, at.output) }),
        _ => return None,
    })
}

fn size(value: &Value) -> usize {
    value.to_string().len() + 1
}

/// The answer to a read: `header`, then items from the pages `fetch` gives (Codex's turns newest
/// first, and the cursor of the page after; `None` when Codex cannot say).
pub(crate) async fn answer<F, Fut>(header: Header, args: &ReadArgs, mut fetch: F) -> std::result::Result<Value, String>
where
    F: FnMut(Option<String>) -> Fut,
    Fut: Future<Output = Option<(Vec<p::Turn>, Option<String>)>>,
{
    let start: Cursor = match &args.before {
        Some(text) => serde_json::from_str(text).map_err(|_| "`before` is not one a read gave.".to_string())?,
        None => Cursor::default(),
    };
    let mut answer = json!({
        "thread": header.thread.0,
        "title": header.title.as_deref().map(|t| cut(t, FULL.name)),
        "project": header.project.as_deref().map(|t| cut(t, FULL.name)),
        "status": status_word(&header.status),
        "error": header.error.as_deref().map(|e| cut(e, FULL.output)),
        "items": [],
        "more": false,
    });
    let limit = args.limit.unwrap_or(DEFAULT_ITEMS).clamp(1, MOST_ITEMS);
    // Room for the cursor, which names Codex's own.
    let mut used = size(&answer) + 512;
    let mut shown: Vec<Value> = Vec::new();
    let (mut page, mut from_turn) = (start.page.clone(), start.turn.clone());
    for _ in 0..MOST_PAGES {
        let (turns, next) = fetch(page.clone()).await.ok_or("Codex could not give the thread's turns.")?;
        for turn in &turns {
            // Going on from a cursor: the turns before its own were shown already.
            let before = match &from_turn {
                Some(t) if *t != turn.id => continue,
                Some(_) => {
                    from_turn = None;
                    Some(start.item)
                }
                None => None,
            };
            for (at, value) in entries(turn, args.view, FULL) {
                if before.is_some_and(|b| at >= b) {
                    continue;
                }
                let mut value = value;
                if shown.is_empty() {
                    // The first item is cut down until it fits.
                    let mut limits = FULL;
                    while used + size(&value) > ANSWER_LIMIT && limits != LEAST {
                        limits = limits.halved();
                        value = entries(turn, args.view, limits).into_iter().find(|(i, _)| *i == at).map(|(_, v)| v).unwrap_or(value);
                    }
                } else if shown.len() == limit || used + size(&value) > ANSWER_LIMIT {
                    let cursor = Cursor { page: page.clone(), turn: Some(turn.id.clone()), item: at + 1 };
                    answer["more"] = json!(true);
                    answer["before"] = json!(serde_json::to_string(&cursor).unwrap_or_default());
                    answer["items"] = Value::Array(shown);
                    return Ok(answer);
                }
                used += size(&value);
                shown.push(value);
            }
        }
        // A cursor whose turn is no longer on its page goes on from the next.
        from_turn = None;
        match next {
            Some(next) if !turns.is_empty() => page = Some(next),
            _ => {
                answer["items"] = Value::Array(shown);
                return Ok(answer);
            }
        }
    }
    answer["more"] = json!(true);
    answer["before"] = json!(serde_json::to_string(&Cursor { page, turn: None, item: 0 }).unwrap_or_default());
    answer["items"] = Value::Array(shown);
    Ok(answer)
}

#[cfg(test)]
#[path = "read_tests.rs"]
mod tests;
