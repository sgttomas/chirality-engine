//! Turns admitted one at a time per thread, against the scripted Codex: two
//! sends on an idle thread, a Stop while a turn is starting, a refused start
//! beside a later one, a fork while a start is in flight, and a read sent
//! before a rename. And shutdown, which returns once Codex is reaped.

use std::path::Path;

use serde_json::{Value, json};

use super::fake::{Wire, start_engine};
use crate::Engine;
use crate::api::*;

fn thread(cwd: &Path, name: &str) -> Value {
    json!({"id": "t", "forkedFromId": null, "parentThreadId": null, "preview": "", "model": "m", "reasoningEffort": null,
           "updatedAt": 1, "status": {"type": "idle"}, "cwd": cwd.display().to_string(), "name": name, "turns": []})
}

fn turn(id: &str) -> Value {
    json!({"turn": {"id": id, "items": [], "status": "inProgress", "error": null}})
}

fn message(text: &str) -> Message {
    Message { text: text.into(), attachments: vec![], workflow: None }
}

fn settings(model: &str) -> TurnSettings {
    TurnSettings { model: model.into(), effort: None, access: Access::Ask }
}

/// An engine with one idle thread `t` (titled "Old") in its project.
async fn idle_thread(name: &str) -> (Engine, Wire, std::path::PathBuf) {
    let (engine, _wires, mut wire, _codex, project) = start_engine(name, None).await;
    let adding = tokio::spawn({
        let engine = engine.clone();
        let project = project.clone();
        async move { engine.add_project(project).await }
    });
    wire.answer("thread/list", json!({"data": [thread(&project, "Old")], "nextCursor": null})).await;
    adding.await.unwrap().unwrap();
    (engine, wire, project)
}

fn send(engine: &Engine, text: &'static str, model: &'static str) -> tokio::task::JoinHandle<Result<()>> {
    let engine = engine.clone();
    tokio::spawn(async move { engine.send(&ThreadId("t".into()), message(text), settings(model)).await })
}

fn summary(engine: &Engine) -> ThreadSummary {
    engine.snapshot().threads.into_iter().find(|t| t.id.0 == "t").unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn two_sends_on_an_idle_thread_start_one_turn_and_steer_it() {
    let (engine, mut wire, project) = idle_thread("admission-two").await;
    let first = send(&engine, "one", "m");
    wire.answer("thread/resume", json!({"thread": thread(&project, "Old")})).await;
    let start = wire.next().await;
    assert_eq!(start["method"], "turn/start");
    let second = send(&engine, "two", "m");
    // The second waits for the first; it neither resumes nor starts a turn of its own.
    wire.quiet().await;
    wire.send(json!({"id": start["id"], "result": turn("turn-1")}));
    let steer = wire.next().await;
    assert_eq!((steer["method"].as_str(), steer["params"]["expectedTurnId"].as_str()), (Some("turn/steer"), Some("turn-1")));
    wire.send(json!({"id": steer["id"], "result": {}}));
    first.await.unwrap().unwrap();
    second.await.unwrap().unwrap();
    engine.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stop_while_a_turn_is_starting_stops_that_turn() {
    let (engine, mut wire, project) = idle_thread("admission-stop").await;
    let sending = send(&engine, "one", "m");
    wire.answer("thread/resume", json!({"thread": thread(&project, "Old")})).await;
    let start = wire.next().await;
    let stopping = tokio::spawn({
        let engine = engine.clone();
        async move { engine.interrupt(&ThreadId("t".into())).await }
    });
    wire.quiet().await;
    wire.send(json!({"id": start["id"], "result": turn("turn-1")}));
    let interrupt = wire.next().await;
    assert_eq!((interrupt["method"].as_str(), interrupt["params"]["turnId"].as_str()), (Some("turn/interrupt"), Some("turn-1")));
    wire.send(json!({"id": interrupt["id"], "result": {}}));
    sending.await.unwrap().unwrap();
    stopping.await.unwrap().unwrap();
    engine.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_refused_start_keeps_the_settings_last_taken_and_a_later_start_has_its_own() {
    let (engine, mut wire, project) = idle_thread("admission-refused").await;
    let taken = summary(&engine).settings;
    let refused = send(&engine, "one", "a");
    wire.answer("thread/resume", json!({"thread": thread(&project, "Old")})).await;
    let start = wire.next().await;
    assert_eq!(summary(&engine).settings.unwrap().model, "a", "the summary shows the turn being started");
    let later = send(&engine, "two", "b");
    wire.quiet().await;
    wire.send(json!({"id": start["id"], "error": {"code": -32000, "message": "refused"}}));
    let start = wire.next().await;
    assert_eq!((start["method"].as_str(), start["params"]["model"].as_str()), (Some("turn/start"), Some("b")));
    assert_eq!(summary(&engine).settings.unwrap().model, "b");
    assert!(refused.await.unwrap().is_err());
    wire.send(json!({"id": start["id"], "error": {"code": -32000, "message": "refused too"}}));
    assert!(later.await.unwrap().is_err());
    assert_eq!(summary(&engine).settings, taken, "neither refused start is kept");
    engine.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_fork_while_a_turn_starts_takes_the_settings_last_taken() {
    let (engine, mut wire, project) = idle_thread("admission-fork").await;
    let sending = send(&engine, "one", "speculative");
    wire.answer("thread/resume", json!({"thread": thread(&project, "Old")})).await;
    let start = wire.next().await;
    let forking = tokio::spawn({
        let engine = engine.clone();
        async move { engine.fork(&ThreadId("t".into()), &TurnId("turn-0".into())).await }
    });
    let mut forked = thread(&project, "Old");
    forked["id"] = json!("f");
    forked["forkedFromId"] = json!("t");
    wire.answer("thread/fork", json!({"thread": forked})).await;
    let fork = forking.await.unwrap().unwrap();
    let fork = engine.snapshot().threads.into_iter().find(|t| t.id == fork).unwrap();
    assert_eq!(fork.settings.unwrap().model, "m");
    wire.send(json!({"id": start["id"], "error": {"code": -32000, "message": "refused"}}));
    assert!(sending.await.unwrap().is_err());
    engine.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_read_sent_before_a_rename_does_not_undo_it() {
    let (engine, mut wire, project) = idle_thread("admission-rename").await;
    let reading = tokio::spawn({
        let engine = engine.clone();
        async move { engine.read_thread(&ThreadId("t".into())).await }
    });
    let read = wire.next().await;
    assert_eq!(read["method"], "thread/read");
    let renaming = tokio::spawn({
        let engine = engine.clone();
        async move { engine.rename(&ThreadId("t".into()), "New".into()).await }
    });
    wire.answer("thread/name/set", json!({})).await;
    renaming.await.unwrap().unwrap();
    // Codex answered the read before it took the rename.
    wire.send(json!({"id": read["id"], "result": {"thread": thread(&project, "Old")}}));
    wire.answer("thread/turns/list", json!({"data": [], "nextCursor": null})).await;
    let view = reading.await.unwrap().unwrap();
    assert_eq!(view.summary.title.as_deref(), Some("New"));
    assert_eq!(summary(&engine).title.as_deref(), Some("New"));
    engine.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shutdown_returns_once_codex_is_reaped() {
    let (connector, mut wires) = super::fake::FakeCodex::new();
    connector.reap_after(std::time::Duration::from_millis(600));
    let root = std::env::temp_dir().join(format!("chirality-reaping-{}", std::process::id()));
    let config = EngineConfig {
        codex_bin: "codex".into(),
        codex_home: Some(root.join("home")),
        state_dir: root.join("state"),
        workflow_roots: Vec::new(),
        codex_settings: Vec::new(),
        tools: None,
    };
    let engine = Engine::start_with(config, connector, None).await.unwrap();
    let wire = wires.recv().await.unwrap();
    let asked = std::time::Instant::now();
    engine.shutdown().await;
    assert!(wire.is_ended());
    assert!(asked.elapsed() >= std::time::Duration::from_millis(600), "returned before Codex was reaped");
    let _ = std::fs::remove_dir_all(root);
}
