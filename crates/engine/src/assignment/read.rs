//! `read`: another thread as its agent can take it in, without waking that
//! thread or queuing anything there. Built from the thread as the engine reads
//! it from Codex (`thread/read` and its turns), keeping nothing.
//!
//! The answer starts with the thread's title, project, status and the last
//! turn's error; then its items, newest first, each cut to a size and the whole
//! answer to about 16 KB. Each item has its number in the thread (oldest first),
//! and `before` pages to the items before a number.

use serde::Deserialize;
use serde_json::{Value, json};

use crate::api::*;

/// The most an answer holds, in bytes of JSON.
pub(crate) const ANSWER_LIMIT: usize = 16 * 1024;
/// The most of one text an item holds, in characters.
const TEXT_LIMIT: usize = 2000;
/// The most of a command's output or a tool's result an item holds, in characters.
const OUTPUT_LIMIT: usize = 600;
const DEFAULT_ITEMS: usize = 20;
const MOST_ITEMS: usize = 100;

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
    /// Only items numbered below this.
    pub before: Option<usize>,
    /// How many items at most.
    pub limit: Option<usize>,
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

fn tool_status(status: ToolCallStatus) -> &'static str {
    match status {
        ToolCallStatus::Running => "running",
        ToolCallStatus::Completed => "completed",
        ToolCallStatus::Failed => "failed",
    }
}

/// One item as the reader gets it, or none for what the view leaves out.
fn entry(body: &ItemBody, view: View, last_reply: bool) -> Option<Value> {
    let activity = view == View::Activity;
    Some(match body {
        ItemBody::UserMessage { text, from: Some(from), .. } => {
            let kind = if from.report.is_some() { "report" } else { "message" };
            json!({ "kind": kind, "from": { "thread": from.thread.0, "title": from.title }, "text": cut(text, TEXT_LIMIT) })
        }
        ItemBody::UserMessage { text, .. } => json!({ "kind": "person", "text": cut(text, TEXT_LIMIT) }),
        ItemBody::AgentMessage { text } if last_reply || activity => json!({ "kind": "reply", "text": cut(text, TEXT_LIMIT) }),
        _ if !activity => return None,
        ItemBody::Command { command, output, exit_code, .. } => {
            json!({ "kind": "command", "command": cut(command, OUTPUT_LIMIT), "exitCode": exit_code, "output": cut(output, OUTPUT_LIMIT) })
        }
        ItemBody::FileChange { files } => json!({ "kind": "fileChange", "files": files.iter().map(|f| f.path.display().to_string()).collect::<Vec<_>>() }),
        ItemBody::ToolCall { server, tool, status, result, .. } => json!({
            "kind": "tool",
            "name": server.as_ref().map_or_else(|| tool.clone(), |s| format!("{s}.{tool}")),
            "status": tool_status(*status),
            "result": result.as_deref().map(|r| cut(r, OUTPUT_LIMIT)),
        }),
        ItemBody::ThreadTool { tool, thread, status, .. } => {
            json!({ "kind": "tool", "name": format!("{}.{}", super::NAMESPACE, serde_json::to_value(tool).ok()?.as_str()?), "thread": thread.as_ref().map(|t| &t.0), "status": tool_status(*status) })
        }
        ItemBody::SubagentTool { tool, status, .. } => json!({ "kind": "tool", "name": serde_json::to_value(tool).ok()?, "status": tool_status(*status) }),
        ItemBody::Subagent { thread, path, activity } => json!({ "kind": "subagent", "thread": thread.0, "path": path, "activity": serde_json::to_value(activity).ok()? }),
        ItemBody::WebSearch { query } => json!({ "kind": "webSearch", "query": cut(query, OUTPUT_LIMIT) }),
        ItemBody::Error { message } => json!({ "kind": "error", "text": cut(message, OUTPUT_LIMIT) }),
        ItemBody::AgentMessage { .. } | ItemBody::Reasoning { .. } | ItemBody::Plan { .. } => return None,
    })
}

/// The answer to a read of the thread `view`, whose project is named `project`.
pub(crate) fn answer(thread: &ThreadView, project: Option<&str>, args: &ReadArgs) -> Value {
    let summary = &thread.summary;
    let error = thread.turns.last().and_then(|t| match &t.state {
        TurnState::Failed { message } => Some(message.clone()),
        _ => None,
    });
    let error = error.or_else(|| match &summary.status {
        ThreadStatus::Failed { message } => Some(message.clone()),
        _ => None,
    });
    // Every item the view keeps, oldest first; a turn's final reply is its last agent message.
    let mut items: Vec<(usize, Value)> = Vec::new();
    for turn in &thread.turns {
        let last = turn.items.iter().rposition(|i| matches!(i.body, ItemBody::AgentMessage { .. }));
        for (at, item) in turn.items.iter().enumerate() {
            if let Some(mut value) = entry(&item.body, args.view, Some(at) == last) {
                let n = items.len() + 1;
                value["n"] = json!(n);
                items.push((n, value));
            }
        }
        if let TurnState::Failed { message } = &turn.state {
            let n = items.len() + 1;
            items.push((n, json!({ "kind": "turnFailed", "text": cut(message, OUTPUT_LIMIT), "n": n })));
        }
    }
    let mut answer = json!({
        "thread": summary.id.0,
        "title": summary.title,
        "project": project,
        "status": status_word(&summary.status),
        "error": error,
        "items": [],
        "more": false,
    });
    let limit = args.limit.unwrap_or(DEFAULT_ITEMS).clamp(1, MOST_ITEMS);
    let mut size = answer.to_string().len() + 64;
    let mut shown = Vec::new();
    let mut older = items.into_iter().rev().filter(|(n, _)| args.before.is_none_or(|b| *n < b)).peekable();
    while let Some((n, value)) = older.next() {
        let length = value.to_string().len() + 1;
        if shown.len() == limit || size + length > ANSWER_LIMIT {
            answer["more"] = json!(true);
            answer["before"] = json!(n + 1);
            break;
        }
        size += length;
        shown.push(value);
        if older.peek().is_none() {
            break;
        }
    }
    answer["items"] = Value::Array(shown);
    answer
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(id: &str, body: ItemBody) -> Item {
        Item { id: ItemId(id.into()), body, done: true }
    }

    fn view(turns: Vec<Turn>) -> ThreadView {
        let summary = ThreadSummary {
            id: ThreadId("t".into()),
            project: ProjectId("p".into()),
            title: Some("Piles".into()),
            status: ThreadStatus::Idle,
            unread: false,
            forked_from: None,
            subagent: None,
            assigned_by: None,
            updated_at: 0,
            settings: None,
        };
        ThreadView { summary, turns, seq: 0 }
    }

    fn args(view: View, before: Option<usize>, limit: Option<usize>) -> ReadArgs {
        ReadArgs { thread: "t".into(), view, before, limit }
    }

    fn user(text: &str) -> ItemBody {
        ItemBody::UserMessage { text: text.into(), attachments: vec![], workflow: None, threads: vec![], from: None }
    }

    fn reply(text: &str) -> ItemBody {
        ItemBody::AgentMessage { text: text.into() }
    }

    #[test]
    fn messages_are_the_messages_in_and_each_final_reply_newest_first() {
        let command = ItemBody::Command { command: "ls".into(), cwd: "/p".into(), output: "a\nb".into(), exit_code: Some(0) };
        let turns = vec![
            Turn { id: TurnId("1".into()), items: vec![item("a", user("Check.")), item("b", reply("Looking.")), item("c", command), item("d", reply("Fine."))], state: TurnState::Completed },
            Turn { id: TurnId("2".into()), items: vec![item("e", user("Again."))], state: TurnState::Failed { message: "boom".into() } },
        ];
        let read = answer(&view(turns.clone()), Some("Bridge"), &args(View::Messages, None, None));
        assert_eq!((read["title"].as_str(), read["project"].as_str(), read["status"].as_str(), read["error"].as_str()), (Some("Piles"), Some("Bridge"), Some("idle"), Some("boom")));
        let kinds: Vec<&str> = read["items"].as_array().unwrap().iter().map(|i| i["kind"].as_str().unwrap()).collect();
        assert_eq!(kinds, ["turnFailed", "person", "reply", "person"]);
        assert_eq!(read["items"][2]["text"], "Fine.");
        let activity = answer(&view(turns), None, &args(View::Activity, None, None));
        let kinds: Vec<&str> = activity["items"].as_array().unwrap().iter().map(|i| i["kind"].as_str().unwrap()).collect();
        assert_eq!(kinds, ["turnFailed", "person", "reply", "command", "reply", "person"]);
        assert_eq!(activity["items"][3]["output"], "a\nb");
    }

    #[test]
    fn an_answer_is_cut_to_its_size_and_pages_back() {
        let long = "x".repeat(5000);
        let turns = (0..20).map(|n| Turn { id: TurnId(n.to_string()), items: vec![item("u", user(&long)), item("r", reply(&format!("Reply {n}.")))], state: TurnState::Completed }).collect();
        let thread = view(turns);
        let first = answer(&thread, None, &args(View::Messages, None, None));
        assert!(first.to_string().len() <= ANSWER_LIMIT, "{}", first.to_string().len());
        assert_eq!(first["more"], true);
        let shown = first["items"].as_array().unwrap();
        assert_eq!(shown[0]["text"], "Reply 19.");
        assert!(shown[1]["text"].as_str().unwrap().ends_with("… [cut: 3000 more characters]"));
        let before = first["before"].as_u64().unwrap() as usize;
        assert_eq!(before, shown.last().unwrap()["n"].as_u64().unwrap() as usize);
        let next = answer(&thread, None, &args(View::Messages, Some(before), Some(2)));
        let numbers: Vec<u64> = next["items"].as_array().unwrap().iter().map(|i| i["n"].as_u64().unwrap()).collect();
        assert_eq!(numbers, [before as u64 - 1, before as u64 - 2]);
        let last = answer(&thread, None, &args(View::Messages, Some(3), None));
        assert_eq!((last["items"].as_array().unwrap().len(), &last["more"]), (2, &json!(false)));
    }
}
