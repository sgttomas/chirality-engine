//! `read`: another thread as its agent can take it in, without waking that
//! thread or queuing anything there. Read from Codex a few turns at a time,
//! newest first (`thread/turns/list`), until the answer is full; nothing is kept.
//!
//! The answer starts with the thread's title, project, status and last error;
//! then its items newest first. Every text in it is cut to a size, and the
//! whole to about 16 KB: the first item of a page is cut further until it fits,
//! so every page shows something or ends. `before` (a cursor an answer gives)
//! goes on from where that answer stopped: from a turn and an item in it, found
//! however many turns have come since.

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

/// A turn's items as the reader gets them, each built when asked for, by its place in the turn (a
/// failed turn's failure after its last item).
struct TurnItems<'a> {
    turn: &'a p::Turn,
    view: View,
    /// A completed turn's final reply: its last message that is not commentary.
    reply: Option<usize>,
    /// Its latest message.
    latest: Option<usize>,
}

impl<'a> TurnItems<'a> {
    fn new(turn: &'a p::Turn, view: View) -> Self {
        let messages = || turn.items.iter().enumerate().filter(|(_, item)| matches!(item, p::ThreadItem::AgentMessage { .. }));
        let not_commentary = |i: &(usize, &p::ThreadItem)| !matches!(i.1, p::ThreadItem::AgentMessage { phase: Some(phase), .. } if phase == "commentary");
        let reply = if turn.status == p::TurnStatus::Completed { messages().rfind(not_commentary).map(|(i, _)| i) } else { None };
        TurnItems { turn, view, reply, latest: messages().next_back().map(|(i, _)| i) }
    }

    /// The places, newest first.
    fn places(&self) -> impl Iterator<Item = usize> + use<> {
        (0..=self.turn.items.len()).rev()
    }

    fn entry(&self, i: usize, at: Limits) -> Option<Value> {
        let activity = self.view == View::Activity;
        let Some(item) = self.turn.items.get(i) else {
            return match (&self.turn.status, &self.turn.error) {
                (p::TurnStatus::Failed, Some(error)) => Some(json!({ "kind": "turnFailed", "text": cut(&error.message, at.output) })),
                _ => None,
            };
        };
        Some(match item {
            p::ThreadItem::AgentMessage { text, phase, .. } => {
                let phase = phase.as_deref().map(|p| cut(p, at.name));
                if Some(i) == self.reply {
                    json!({ "kind": "reply", "text": cut(text, at.text) })
                } else if self.reply.is_none() && Some(i) == self.latest {
                    // The latest word of a turn with no final reply: how that turn stands says what it is.
                    json!({ "kind": "update", "turn": turn_word(&self.turn.status), "phase": phase, "text": cut(text, at.text) })
                } else if activity {
                    json!({ "kind": "message", "phase": phase, "text": cut(text, at.text) })
                } else {
                    return None;
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
            _ => other(&crate::mapping::item(item, true)?.body, activity, at)?,
        })
    }
}

/// Any other item, from its body: messages in always, the work in the activity view.
fn other(body: &ItemBody, activity: bool, at: Limits) -> Option<Value> {
    Some(match body {
        ItemBody::UserMessage { text, from: Some(from), .. } => {
            let kind = if from.report.is_some() { "report" } else { "message" };
            json!({ "kind": kind, "from": { "thread": cut(&from.thread.0, at.name), "title": from.title.as_deref().map(|t| cut(t, at.name)) }, "text": cut(text, at.text) })
        }
        ItemBody::UserMessage { text, .. } => json!({ "kind": "person", "text": cut(text, at.text) }),
        _ if !activity => return None,
        ItemBody::ToolCall { server, tool, arguments, status, result, .. } => {
            let name = server.as_ref().map_or_else(|| tool.clone(), |s| format!("{s}.{tool}"));
            json!({
                "kind": "tool",
                "name": cut(&name, at.name),
                "arguments": cut(arguments, at.output),
                "status": tool_status(*status),
                "result": result.as_deref().map(|r| cut(r, at.output)),
            })
        }
        ItemBody::ThreadTool { tool, thread, text, status } => {
            let name = format!("{}.{}", super::NAMESPACE, serde_json::to_value(tool).ok()?.as_str()?);
            json!({
                "kind": "tool",
                "name": name,
                "thread": thread.as_ref().map(|t| cut(&t.0, at.name)),
                "text": text.as_deref().map(|t| cut(t, at.text)),
                "status": tool_status(*status),
            })
        }
        ItemBody::SubagentTool { tool, status, .. } => json!({ "kind": "tool", "name": serde_json::to_value(tool).ok()?, "status": tool_status(*status) }),
        ItemBody::Subagent { thread, path, activity } => json!({ "kind": "subagent", "thread": cut(&thread.0, at.name), "path": cut(path, at.name), "activity": serde_json::to_value(activity).ok()? }),
        ItemBody::WebSearch { query } => json!({ "kind": "webSearch", "query": cut(query, at.output) }),
        ItemBody::Error { message } => json!({ "kind": "error", "text": cut(message, at.output) }),
        _ => return None,
    })
}

fn size(value: &Value) -> usize {
    value.to_string().len() + 1
}

/// The first item of an answer, cut further until it fits in `room` (or, if it cannot, said to be too large).
fn fit(items: &TurnItems, at: usize, mut value: Value, room: usize) -> Value {
    let mut limits = FULL;
    while size(&value) > room && limits != LEAST {
        limits = limits.halved();
        value = items.entry(at, limits).unwrap_or(value);
    }
    if size(&value) > room { json!({ "kind": value["kind"].clone(), "text": "[too large to show]" }) } else { value }
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
    let answer = json!({
        "thread": cut(&header.thread.0, FULL.name),
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
    let finish = |mut answer: Value, shown: Vec<Value>, cursor: Option<Cursor>| {
        if let Some(cursor) = cursor {
            answer["more"] = json!(true);
            answer["before"] = json!(serde_json::to_string(&cursor).unwrap_or_default());
        }
        answer["items"] = Value::Array(shown);
        answer
    };
    // Going on from a cursor: the turns newer than its own were shown already (however many came
    // since, on however many pages), and of its own, the items from its place on.
    let mut page = start.page;
    let mut anchor = start.turn.map(|turn| (turn, start.item));
    // The last turn read whole, and the page it is on.
    let mut last: Option<(Option<String>, String)> = None;
    for _ in 0..MOST_PAGES {
        let (turns, next) = fetch(page.clone()).await.ok_or("Codex could not give the thread's turns.")?;
        for turn in &turns {
            let before = match &anchor {
                Some((t, item)) if *t == turn.id => {
                    let item = *item;
                    anchor = None;
                    Some(item)
                }
                Some(_) => continue,
                None => None,
            };
            let items = TurnItems::new(turn, args.view);
            for at in items.places().filter(|at| before.is_none_or(|b| *at < b)) {
                let Some(value) = items.entry(at, FULL) else { continue };
                if shown.len() == limit || !shown.is_empty() && used + size(&value) > ANSWER_LIMIT {
                    let cursor = Cursor { page: page.clone(), turn: Some(turn.id.clone()), item: at + 1 };
                    return Ok(finish(answer, shown, Some(cursor)));
                }
                // The first item is cut until it fits, so every answer shows something or ends.
                let value = if shown.is_empty() { fit(&items, at, value, ANSWER_LIMIT.saturating_sub(used)) } else { value };
                used += size(&value);
                shown.push(value);
            }
            last = Some((page.clone(), turn.id.clone()));
        }
        match next {
            Some(next) if !turns.is_empty() => page = Some(next),
            _ if anchor.is_some() => return Err("The turn `before` names is no longer in the thread.".into()),
            _ => return Ok(finish(answer, shown, None)),
        }
    }
    // As many pages as one read looks through: the next goes on after the last turn read, or, its
    // turn not found yet, looks on from the next page.
    let cursor = match (anchor, last) {
        (Some((turn, item)), _) => Cursor { page, turn: Some(turn), item },
        (None, Some((page, turn))) => Cursor { page, turn: Some(turn), item: 0 },
        (None, None) => Cursor { page, turn: None, item: 0 },
    };
    Ok(finish(answer, shown, Some(cursor)))
}

#[cfg(test)]
#[path = "read_tests.rs"]
mod tests;
