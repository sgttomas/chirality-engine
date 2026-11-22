//! The engine's tools: `assign`, `message`, `threads` and `close`, as Codex
//! takes them, as their calls show in the conversation, and how each call is
//! answered. A call is answered as soon as its work is done: a turn it starts
//! in another thread is not waited for.

use std::sync::{Arc, Weak};

use serde::Deserialize;
use serde_json::{Value, json};

use super::NAMESPACE;
use super::delivery::{Delivery, Posted};
use super::marker::Marker;
use crate::api::*;
use crate::protocol as p;
use crate::session::Inner;

const ASSIGN: &str = "assign";
const MESSAGE: &str = "message";
const THREADS: &str = "threads";
const CLOSE: &str = "close";

const ABOUT: &str = "Work with the person's other threads. A thread you assign is an ordinary thread the person can open, \
talk to and stop; its agent works on your brief, and when a turn you gave it (by the brief or a message) ends, its last reply \
comes back to you as a message. A message from another thread is a user message that begins with \
<thread-message from=\"<thread id>\" title=\"<title>\"/>, with report=\"completed\", \"stopped\" or \"failed\" when it reports \
how a turn you gave ended; answer one with `message`. A message to a thread that is running waits for its turn to end.";

fn function(name: &str, description: &str, schema: Value) -> p::NamespaceTool {
    p::NamespaceTool::Function(p::FunctionSpec { name: name.into(), description: description.into(), input_schema: schema })
}

/// The engine's tools as `thread/start` takes them.
pub(crate) fn spec() -> p::DynamicToolSpec {
    let thread = json!({"type": "string", "description": "A thread's id, from `threads` or `assign`."});
    let tools = vec![
        function(
            ASSIGN,
            "Start a new thread with a title and a brief (its first message), in this thread's project or another, \
             and return its id. Model, reasoning effort and access are this thread's unless named; access can only be \
             the same or lower (the new thread's agent asks the person for more).",
            json!({"type": "object", "properties": {
                "title": {"type": "string", "description": "The new thread's title."},
                "brief": {"type": "string", "description": "The work, as the new thread's first message."},
                "project": {"type": "string", "description": "A project's name or path, from `threads`; this thread's project when left out."},
                "workflow": {"type": "string", "description": "A workflow's name or id, for the new thread to follow."},
                "model": {"type": "string", "description": "A model id."},
                "effort": {"type": "string", "description": "A reasoning effort: low, medium, high, xhigh."},
                "access": {"type": "string", "enum": ["readOnly", "ask", "fullAccess"], "description": "readOnly, ask or fullAccess."}
            }, "required": ["title", "brief"], "additionalProperties": false}),
        ),
        function(
            MESSAGE,
            "Send text to another thread. It starts a turn there, or waits until the turn running there ends.",
            json!({"type": "object", "properties": {"thread": thread, "text": {"type": "string"}}, "required": ["thread", "text"], "additionalProperties": false}),
        ),
        function(
            THREADS,
            "The threads you can address, in every project: each with its title, project, whether it is running, idle or \
             waiting on the person, and whether you assigned it or it assigned you.",
            json!({"type": "object", "properties": {}, "additionalProperties": false}),
        ),
        function(
            CLOSE,
            "End an assignment you made: no more reports come from that thread.",
            json!({"type": "object", "properties": {"thread": thread}, "required": ["thread"], "additionalProperties": false}),
        ),
    ];
    p::DynamicToolSpec::Namespace { name: NAMESPACE.into(), description: ABOUT.into(), tools }
}

/// A call's item in the conversation; `result` is its answer's text.
pub(crate) fn item(tool: &str, arguments: &Value, result: Option<&str>, status: ToolCallStatus) -> Option<ItemBody> {
    let text = |key: &str| arguments.get(key).and_then(Value::as_str).map(str::to_string);
    let (tool, thread, sent) = match tool {
        ASSIGN => {
            let made = result.filter(|_| status == ToolCallStatus::Completed).and_then(|r| serde_json::from_str::<Value>(r).ok());
            (ThreadTool::Assign, made.and_then(|r| r["thread"].as_str().map(|t| ThreadId(t.into()))), text("brief"))
        }
        MESSAGE => (ThreadTool::Message, text("thread").map(ThreadId), text("text")),
        THREADS => (ThreadTool::Threads, None, None),
        CLOSE => (ThreadTool::Close, text("thread").map(ThreadId), None),
        _ => return None,
    };
    Some(ItemBody::ThreadTool { tool, thread, text: sent, status })
}

/// Answers the engine's tools, each call as the engine's own task, so a call
/// Codex cancels never leaves its work half done.
pub(crate) struct EngineTools(pub Weak<Inner>);

impl ToolHandler for EngineTools {
    fn call(&self, call: ToolCall) -> ToolFuture {
        let inner = self.0.upgrade();
        Box::pin(async move {
            let Some(inner) = inner else { return ToolOutput::failure("The engine has shut down.") };
            let work = tokio::spawn(async move { inner.engine_tool(call).await });
            match work.await {
                Ok(Ok(answer)) => ToolOutput::text(answer.to_string()),
                Ok(Err(message)) => ToolOutput::failure(message),
                Err(_) => ToolOutput::failure("The tool failed."),
            }
        })
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AssignArgs {
    title: String,
    brief: String,
    project: Option<String>,
    workflow: Option<String>,
    model: Option<String>,
    effort: Option<String>,
    access: Option<Access>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct MessageArgs {
    thread: String,
    text: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CloseArgs {
    thread: String,
}

fn args<T: serde::de::DeserializeOwned>(value: Value) -> std::result::Result<T, String> {
    serde_json::from_value(value).map_err(|e| format!("Invalid arguments: {e}"))
}

fn rank(access: Access) -> u8 {
    match access {
        Access::ReadOnly => 0,
        Access::Ask => 1,
        Access::FullAccess => 2,
    }
}

/// The lower of two access levels: a call never raises one.
pub(crate) fn lower(a: Access, b: Access) -> Access {
    if rank(a) <= rank(b) { a } else { b }
}

fn status_word(status: &ThreadStatus) -> &'static str {
    match status {
        ThreadStatus::NeedsApproval | ThreadStatus::NeedsAnswer => "waiting on the person",
        ThreadStatus::Working => "running",
        ThreadStatus::Failed { .. } | ThreadStatus::Idle => "idle",
    }
}

type Answer = std::result::Result<Value, String>;

impl Inner {
    /// The engine's tools for this engine's calls.
    pub(crate) fn engine_tools(self: &Arc<Self>) -> Arc<dyn ToolHandler> {
        Arc::new(EngineTools(Arc::downgrade(self)))
    }

    async fn engine_tool(self: Arc<Self>, call: ToolCall) -> Answer {
        let caller = call.thread.clone();
        let entry = self.known_thread(&caller).map_err(|e| e.to_string())?;
        // Codex gives a subagent these tools only from a forked copy of its parent's history.
        if entry.subagent.is_some() || self.store.owner(caller.clone()) != caller {
            return Err("A subagent cannot use these tools.".into());
        }
        match call.tool.as_str() {
            ASSIGN => self.assign(&caller, args(call.arguments)?).await,
            MESSAGE => self.message(&caller, args(call.arguments)?).await,
            THREADS => Ok(self.threads(&caller)),
            CLOSE => self.close(&caller, args(call.arguments)?),
            other => Err(format!("There is no tool {NAMESPACE}.{other}.")),
        }
    }

    /// The settings the thread's turns run with; the access is a stand-in (Ask) unless known.
    pub(crate) fn settings_of(&self, thread: &ThreadId) -> Option<TurnSettings> {
        self.store.thread(thread).and_then(|e| e.starting.or(e.settings))
    }

    /// The thread's own settings, with its access read only unless the engine knows it.
    fn sender_settings(&self, thread: &ThreadId) -> Option<TurnSettings> {
        let entry = self.store.thread(thread)?;
        let known = entry.access_known || entry.starting.is_some();
        let settings = entry.starting.or(entry.settings)?;
        Some(if known { settings } else { TurnSettings { access: Access::ReadOnly, ..settings } })
    }

    fn find_project(&self, named: &str) -> std::result::Result<Project, String> {
        let projects = self.store.projects();
        projects
            .iter()
            .find(|p| p.id.0 == named || p.path.display().to_string() == named)
            .or_else(|| projects.iter().find(|p| p.name.eq_ignore_ascii_case(named)))
            .cloned()
            .ok_or_else(|| format!("There is no project {named:?}; the projects are {}.", projects.iter().map(|p| format!("{:?}", p.name)).collect::<Vec<_>>().join(", ")))
    }

    async fn assign(self: &Arc<Self>, caller: &ThreadId, args: AssignArgs) -> Answer {
        let entry = self.known_thread(caller).map_err(|e| e.to_string())?;
        let base = self.sender_settings(caller).ok_or("This thread's settings are not known yet.")?;
        let project = match &args.project {
            Some(named) => self.find_project(named)?,
            None => self.project(&entry.project).map_err(|e| e.to_string())?,
        };
        let workflow = match &args.workflow {
            Some(named) => {
                let found = self.workflows(&project.id).map_err(|e| e.to_string())?;
                let picked = found.iter().find(|w| w.id.0 == *named).or_else(|| found.iter().find(|w| w.name.eq_ignore_ascii_case(named)));
                Some(picked.ok_or_else(|| format!("There is no workflow {named:?} in {}.", project.name))?.id.clone())
            }
            None => None,
        };
        let access = args.access.map_or(base.access, |asked| lower(asked, base.access));
        let settings = TurnSettings { model: args.model.unwrap_or(base.model), effort: args.effort.map(Effort).or(base.effort), access };
        let from = Marker { from: caller.clone(), title: entry.title.clone(), report: None };
        let brief = Message { text: args.brief, attachments: Vec::new(), workflow };
        let thread = self.start_assigned(&project.id, brief, settings.clone(), from, args.title).await.map_err(|e| e.to_string())?;
        Ok(json!({ "thread": thread.0, "project": project.name, "model": settings.model, "effort": settings.effort.map(|e| e.0), "access": settings.access }))
    }

    async fn message(self: &Arc<Self>, caller: &ThreadId, args: MessageArgs) -> Answer {
        let target = ThreadId(args.thread);
        let entry = self.known_thread(&target).map_err(|e| e.to_string())?;
        if &target == caller {
            return Err("That is this thread.".into());
        }
        if entry.subagent.is_some() {
            return Err("A subagent takes its messages from the agent that started it.".into());
        }
        // The receiving thread's own settings when its turn starts; the sender's bound one whose access is not known.
        let sender = self.sender_settings(caller).ok_or("This thread's settings are not known yet.")?;
        let title = self.store.thread(caller).and_then(|e| e.title);
        let from = Marker { from: caller.clone(), title, report: None };
        let delivered = match self.post(target.clone(), Delivery::new(from, args.text, sender.access, sender)).await {
            Posted::Now => "now",
            Posted::Later => "when its running turn ends",
            Posted::Refused(message) => return Err(message),
            Posted::Gone => return Err("That thread is gone.".into()),
        };
        Ok(json!({ "thread": target.0, "delivered": delivered }))
    }

    fn threads(&self, caller: &ThreadId) -> Value {
        let snapshot = self.store.snapshot();
        let assigner = snapshot.threads.iter().find(|t| &t.id == caller).and_then(|t| t.assigned_by.clone());
        let project_name = |id: &ProjectId| snapshot.projects.iter().find(|p| &p.id == id).map(|p| p.name.clone());
        let threads: Vec<Value> = snapshot
            .threads
            .iter()
            .filter(|t| t.subagent.is_none() && &t.id != caller)
            .map(|t| {
                let relation = if t.assigned_by.as_ref() == Some(caller) {
                    Some("you assigned it")
                } else if assigner.as_ref() == Some(&t.id) {
                    Some("it assigned you")
                } else {
                    None
                };
                json!({ "id": t.id.0, "title": t.title, "project": project_name(&t.project), "status": status_word(&t.status), "relation": relation })
            })
            .collect();
        let projects: Vec<Value> = snapshot.projects.iter().map(|p| json!({ "name": p.name, "path": p.path })).collect();
        json!({ "projects": projects, "threads": threads })
    }

    fn close(&self, caller: &ThreadId, args: CloseArgs) -> Answer {
        let target = ThreadId(args.thread);
        let entry = self.known_thread(&target).map_err(|e| e.to_string())?;
        if entry.assigned_by.as_ref() != Some(caller) {
            return Err("This thread did not assign that thread.".into());
        }
        self.assignments.close(caller, &target);
        Ok(json!({ "thread": target.0, "closed": true }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_call_shows_as_the_work_with_the_other_thread() {
        let assigned = item(ASSIGN, &json!({"title": "Piles", "brief": "Check them."}), Some(r#"{"thread":"t2","access":"ask"}"#), ToolCallStatus::Completed);
        assert_eq!(assigned, Some(ItemBody::ThreadTool { tool: ThreadTool::Assign, thread: Some(ThreadId("t2".into())), text: Some("Check them.".into()), status: ToolCallStatus::Completed }));
        let running = item(ASSIGN, &json!({"title": "Piles", "brief": "Check them."}), None, ToolCallStatus::Running);
        assert!(matches!(running, Some(ItemBody::ThreadTool { thread: None, status: ToolCallStatus::Running, .. })));
        let refused = item(ASSIGN, &json!({}), Some("There is no project \"x\"."), ToolCallStatus::Failed);
        assert!(matches!(refused, Some(ItemBody::ThreadTool { thread: None, status: ToolCallStatus::Failed, .. })));
        let sent = item(MESSAGE, &json!({"thread": "t2", "text": "And the deck."}), None, ToolCallStatus::Completed);
        assert_eq!(sent, Some(ItemBody::ThreadTool { tool: ThreadTool::Message, thread: Some(ThreadId("t2".into())), text: Some("And the deck.".into()), status: ToolCallStatus::Completed }));
        assert_eq!(item("other", &json!({}), None, ToolCallStatus::Completed), None);
    }

    #[test]
    fn access_is_never_raised() {
        assert_eq!(lower(Access::FullAccess, Access::Ask), Access::Ask);
        assert_eq!(lower(Access::ReadOnly, Access::Ask), Access::ReadOnly);
        assert_eq!(lower(Access::Ask, Access::FullAccess), Access::Ask);
    }

    #[test]
    fn the_tools_are_ones_codex_takes() {
        let p::DynamicToolSpec::Namespace { name, description, tools } = spec();
        assert!(!crate::tools::RESERVED_NAMESPACES.contains(&name.as_str()) && description.chars().count() <= 1024);
        for p::NamespaceTool::Function(f) in tools {
            assert!(crate::tools::schema_supported(&f.input_schema), "{}", f.name);
        }
    }
}
