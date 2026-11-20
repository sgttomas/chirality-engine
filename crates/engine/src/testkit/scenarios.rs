//! Scenarios, each run the same way live (recording) and in replay, and each
//! asserting on the engine's snapshot and events.

use serde_json::json;

use super::harness::{Ctx, LIBRARY, settings, text};
use super::stub::{calls, hangs, patch_call, question_call, says, shell_call};
use crate::api::*;

async fn project(ctx: &Ctx) -> Project {
    ctx.engine.add_project(ctx.run.project()).await.expect("the project is added")
}

async fn turns(ctx: &Ctx, thread: &ThreadId) -> Vec<Turn> {
    ctx.read_thread(thread).await.turns
}

fn agent_text(turn: &Turn) -> String {
    turn.items.iter().filter_map(|i| match &i.body {
        ItemBody::AgentMessage { text } => Some(text.clone()),
        _ => None,
    }).collect::<Vec<_>>().join("\n")
}

fn status(ctx: &Ctx, thread: &ThreadId) -> ThreadSummary {
    ctx.engine.snapshot().threads.into_iter().find(|t| &t.id == thread).expect("the thread is in the snapshot")
}

/// A thread with one finished turn saying `key`'s text.
async fn one_turn(ctx: &mut Ctx, project: &Project, key: &str, reply: &str, access: Access) -> (ThreadId, TurnId) {
    ctx.script(key, says(key, reply));
    let thread = ctx.engine.start_thread(&project.id, text(&format!("STUB[{key}] go")), settings(access)).await.expect("the thread starts");
    let (turn, state) = ctx.turn_ended(&thread).await;
    assert_eq!(state, TurnState::Completed);
    (thread, turn)
}

async fn next_turn(ctx: &mut Ctx, thread: &ThreadId, key: &str, reply: &str) -> TurnId {
    ctx.script(key, says(key, reply));
    ctx.engine.send(thread, text(&format!("STUB[{key}] go")), settings(Access::Ask)).await.expect("the message is sent");
    let (turn, state) = ctx.turn_ended(thread).await;
    assert_eq!(state, TurnState::Completed);
    turn
}

pub async fn start_and_models(ctx: &mut Ctx) {
    let snapshot = ctx.engine.snapshot();
    assert_eq!(snapshot.codex, CodexStatus::Ready { version: "0.171.0".into() });
    assert_eq!(snapshot.account, Account::SignedOut);
    let default = snapshot.models.iter().find(|m| m.is_default).expect("a default model");
    assert!(!default.efforts.is_empty());
    assert!(default.default_effort.as_ref().is_some_and(|e| default.efforts.contains(e)));
}

pub async fn text_turn(ctx: &mut Ctx) {
    std::fs::write(ctx.run.project().join("notes.md"), "notes\n").unwrap();
    let project = project(ctx).await;
    let (thread, _) = one_turn(ctx, &project, "hello", "Hello from the stub.", Access::Ask).await;
    let turns = turns(ctx, &thread).await;
    assert_eq!(turns.len(), 1);
    assert_eq!(turns[0].items[0].body, ItemBody::UserMessage { text: "STUB[hello] go".into(), attachments: vec![], workflow: None });
    assert_eq!(agent_text(&turns[0]), "Hello from the stub.");
    let summary = status(ctx, &thread);
    assert_eq!((summary.status, summary.unread, summary.project), (ThreadStatus::Idle, true, project.id.clone()));
    assert_eq!(summary.settings, Some(settings(Access::Ask)));
    ctx.engine.mark_read(&thread);
    assert!(!status(ctx, &thread).unread);
    ctx.engine.rename(&thread, "Greeting".into()).await.unwrap();
    assert_eq!(status(ctx, &thread).title.as_deref(), Some("Greeting"));
    let found = ctx.engine.find_files(&project.id, "notes".into()).await.unwrap();
    assert!(found.contains(&ctx.run.project().join("notes.md")), "{found:?}");
    assert_eq!(ctx.engine.read_file(&project.id, "notes.md".into()).await.unwrap(), "notes\n");
    assert!(ctx.engine.read_file(&project.id, ctx.run.project().join("../home")).await.is_err());
}

pub async fn workflow_turn(ctx: &mut Ctx) {
    let folder = ctx.run.project().join(".chirality/workflows/estimate");
    std::fs::create_dir_all(&folder).unwrap();
    std::fs::write(folder.join("WORKFLOW.md"), "---\nname: estimate\ndescription: Estimate the work.\n---\n# Estimate\nList the parts, then cost each.\n").unwrap();
    std::fs::write(ctx.run.project().join("scope.md"), "scope\n").unwrap();
    let project = project(ctx).await;
    let workflows = ctx.engine.workflows(&project.id).await.unwrap();
    let estimate = workflows.iter().find(|w| w.source == "project").expect("the project's workflow").clone();
    assert_eq!((estimate.name.as_str(), estimate.summary.as_str()), ("estimate", "Estimate the work."));
    ctx.script("flow", says("flow", "Following the workflow."));
    // A 1x1 PNG.
    const PNG: &[u8] = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR\0\0\0\x01\0\0\0\x01\x08\x06\0\0\0\x1f\x15\xc4\x89\0\0\0\rIDATx\x9cc\xf8\xff\xff?\0\x05\xfe\x02\xfe\xa7\x35\x81\x84\0\0\0\0IEND\xaeB`\x82";
    std::fs::write(ctx.run.project().join("sketch.png"), PNG).unwrap();
    let attachments = vec![Attachment::File { path: ctx.run.project().join("scope.md") }, Attachment::Image { path: ctx.run.project().join("sketch.png") }];
    let message = Message { text: "STUB[flow] Estimate this".into(), attachments: attachments.clone(), workflow: Some(estimate.id.clone()) };
    let thread = ctx.engine.start_thread(&project.id, message, settings(Access::Ask)).await.unwrap();
    assert_eq!(ctx.turn_ended(&thread).await.1, TurnState::Completed);
    let turns = turns(ctx, &thread).await;
    assert_eq!(turns[0].items[0].body, ItemBody::UserMessage { text: "STUB[flow] Estimate this".into(), attachments, workflow: Some(estimate.id) });
    assert_eq!(status(ctx, &thread).title.as_deref(), Some("STUB[flow] Estimate this"));
}

async fn command(ctx: &mut Ctx, key: &str, answer: Answer) -> (ThreadId, Turn) {
    let project = project(ctx).await;
    let file = format!("{key}.txt");
    ctx.script(key, calls(key, shell_call(&format!("call_{key}"), &format!("printf ok > {file}"), true), false));
    let thread = ctx.engine.start_thread(&project.id, text(&format!("STUB[{key}] write a file")), settings(Access::Ask)).await.unwrap();
    let pending = ctx.pending(&thread).await;
    match &pending.request {
        Request::Command { command, cwd, .. } => {
            assert!(command.contains(&format!("printf ok > {file}")), "{command}");
            assert_eq!(cwd, &ctx.run.project());
        }
        other => panic!("not a command approval: {other:?}"),
    }
    assert_eq!(status(ctx, &thread).status, ThreadStatus::NeedsApproval);
    ctx.engine.answer(&pending.id, answer).await.unwrap();
    assert!(ctx.engine.snapshot().pending.is_empty());
    assert_eq!(ctx.turn_ended(&thread).await.1, TurnState::Completed);
    let turn = turns(ctx, &thread).await.remove(0);
    (thread, turn)
}

fn command_item(turn: &Turn) -> (String, Option<i32>) {
    turn.items.iter().find_map(|i| match &i.body {
        ItemBody::Command { output, exit_code, .. } => Some((output.clone(), *exit_code)),
        _ => None,
    }).expect("a command item")
}

pub async fn command_approved(ctx: &mut Ctx) {
    let (_, turn) = command(ctx, "approve", Answer::Allow).await;
    assert_eq!(command_item(&turn).1, Some(0));
    if ctx.is_live() {
        assert_eq!(std::fs::read_to_string(ctx.run.project().join("approve.txt")).unwrap(), "ok");
    }
}

pub async fn command_declined(ctx: &mut Ctx) {
    let (_, turn) = command(ctx, "decline", Answer::Decline).await;
    assert!(turn.items.iter().all(|i| !matches!(i.body, ItemBody::Command { exit_code: Some(_), .. })));
    assert!(!ctx.run.project().join("decline.txt").exists());
    assert_eq!(agent_text(&turn), "Done.");
}

pub async fn file_change(ctx: &mut Ctx) {
    let project = project(ctx).await;
    let patch = "*** Begin Patch\n*** Add File: hello.txt\n+hello\n*** End Patch\n";
    ctx.script("patch", calls("patch", patch_call("call_patch", patch), false));
    let thread = ctx.engine.start_thread(&project.id, text("STUB[patch] add a file"), settings(Access::ReadOnly)).await.unwrap();
    let pending = ctx.pending(&thread).await;
    let Request::FileChange { files, .. } = &pending.request else { panic!("not a file change: {:?}", pending.request) };
    assert!(files.len() == 1 && files[0].path.ends_with("hello.txt") && files[0].diff.contains("hello"), "{files:?}");
    ctx.engine.answer(&pending.id, Answer::Allow).await.unwrap();
    assert_eq!(ctx.turn_ended(&thread).await.1, TurnState::Completed);
    let turn = turns(ctx, &thread).await.remove(0);
    let diff = turn.items.iter().find_map(|i| match &i.body {
        ItemBody::FileChange { files } => Some(files[0].diff.clone()),
        _ => None,
    });
    assert!(diff.is_some_and(|d| d.contains("hello")));
    if ctx.is_live() {
        assert_eq!(std::fs::read_to_string(ctx.run.project().join("hello.txt")).unwrap(), "hello\n");
    }
}

pub async fn question(ctx: &mut Ctx) {
    let project = project(ctx).await;
    let questions = json!([{"id": "colour", "header": "Colour", "question": "Which colour?", "options": [{"label": "Red", "description": "Warm"}, {"label": "Blue", "description": "Cool"}]}]);
    ctx.script("ask", calls("ask", question_call("call_ask", questions), true));
    let thread = ctx.engine.start_thread(&project.id, text("STUB[ask] ask me"), settings(Access::Ask)).await.unwrap();
    let pending = ctx.pending(&thread).await;
    let Request::Question { questions } = &pending.request else { panic!("not a question: {:?}", pending.request) };
    assert_eq!(questions[0].text, "Which colour?");
    assert_eq!(questions[0].options, vec!["Red".to_string(), "Blue".to_string()]);
    assert_eq!(status(ctx, &thread).status, ThreadStatus::NeedsAnswer);
    ctx.engine.answer(&pending.id, Answer::Answers { answers: vec![("colour".into(), "Blue".into())] }).await.unwrap();
    assert_eq!(ctx.turn_ended(&thread).await.1, TurnState::Completed);
    let turn = turns(ctx, &thread).await.remove(0);
    assert!(agent_text(&turn).contains("Blue"), "{}", agent_text(&turn));
}

pub async fn interrupt(ctx: &mut Ctx) {
    let project = project(ctx).await;
    ctx.script("wait", hangs());
    let thread = ctx.engine.start_thread(&project.id, text("STUB[wait] take your time"), settings(Access::Ask)).await.unwrap();
    ctx.wait("the turn to start", |e| matches!(e, Event::TurnStarted { thread: t, .. } if t == &thread).then_some(())).await;
    assert_eq!(status(ctx, &thread).status, ThreadStatus::Working);
    ctx.engine.send(&thread, text("STUB[wait] and this"), settings(Access::Ask)).await.expect("a steer");
    ctx.engine.interrupt(&thread).await.unwrap();
    assert_eq!(ctx.turn_ended(&thread).await.1, TurnState::Interrupted);
    assert_eq!(status(ctx, &thread).status, ThreadStatus::Idle);
}

pub async fn fork(ctx: &mut Ctx) {
    let project = project(ctx).await;
    let (thread, first) = one_turn(ctx, &project, "one", "One.", Access::Ask).await;
    next_turn(ctx, &thread, "two", "Two.").await;
    let fork = ctx.engine.fork(&thread, &first).await.unwrap();
    assert_eq!(status(ctx, &fork).forked_from, Some(thread.clone()));
    let forked = turns(ctx, &fork).await;
    assert_eq!(forked.len(), 1);
    assert_eq!(agent_text(&forked[0]), "One.");
    assert_eq!(turns(ctx, &thread).await.len(), 2);
    ctx.engine.archive(&fork).await.unwrap();
    assert!(ctx.engine.snapshot().threads.iter().all(|t| t.id != fork));
}

pub async fn revert(ctx: &mut Ctx) {
    let project = project(ctx).await;
    let (thread, _) = one_turn(ctx, &project, "one", "One.", Access::Ask).await;
    let second = next_turn(ctx, &thread, "two", "Two.").await;
    ctx.engine.revert(&thread, &second).await.unwrap();
    let left = turns(ctx, &thread).await;
    assert_eq!(left.len(), 1);
    assert_eq!(agent_text(&left[0]), "One.");
}

pub async fn restart(ctx: &mut Ctx) {
    let project = project(ctx).await;
    let (thread, _) = one_turn(ctx, &project, "before", "Before.", Access::Ask).await;
    ctx.kill_codex();
    ctx.wait("Codex to stop", |e| matches!(e, Event::Codex { status: CodexStatus::Starting }).then_some(())).await;
    ctx.wait("Codex to be back", |e| matches!(e, Event::Codex { status: CodexStatus::Ready { .. } }).then_some(())).await;
    next_turn(ctx, &thread, "after", "After.").await;
    let all = turns(ctx, &thread).await;
    assert_eq!(all.iter().map(agent_text).collect::<Vec<_>>(), vec!["Before.".to_string(), "After.".to_string()]);
}

/// Live only: a library workflow picked into a conversation that runs a command.
pub async fn main_path(ctx: &mut Ctx) {
    let project = project(ctx).await;
    let workflows = ctx.engine.workflows(&project.id).await.unwrap();
    let review = workflows.iter().find(|w| w.source == LIBRARY && w.name == "review").expect("the library's review workflow").clone();
    ctx.script("main", calls("main", shell_call("call_main", "printf ok > main.txt", true), false));
    let message = Message { text: "STUB[main] review this".into(), attachments: vec![], workflow: Some(review.id.clone()) };
    let thread = ctx.engine.start_thread(&project.id, message, settings(Access::Ask)).await.unwrap();
    let pending = ctx.pending(&thread).await;
    ctx.engine.answer(&pending.id, Answer::Allow).await.unwrap();
    assert_eq!(ctx.turn_ended(&thread).await.1, TurnState::Completed);
    let turn = turns(ctx, &thread).await.remove(0);
    assert!(matches!(&turn.items[0].body, ItemBody::UserMessage { workflow: Some(id), .. } if *id == review.id));
    assert_eq!(command_item(&turn).1, Some(0));
    assert_eq!(agent_text(&turn), "Done.");
}
