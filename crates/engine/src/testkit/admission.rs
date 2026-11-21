//! Turns admitted one at a time per thread, against the scripted Codex: two
//! sends on an idle thread, a Stop while a turn is starting, a send after a
//! Stop, a send its caller gave up on, a refused start beside a later one, a
//! fork while a start is in flight, and a read sent before a rename. And
//! shutdown: once, for every caller, ending a Codex still starting, and
//! returning once Codex is reaped.

use std::path::Path;

use serde_json::{Value, json};

use super::fake::{Wire, start_engine};
use crate::Engine;
use crate::api::*;

fn thread(cwd: &Path, name: &str) -> Value {
    json!({"id": "t", "forkedFromId": null, "parentThreadId": null, "preview": "", "model": "m", "reasoningEffort": null,
           "updatedAt": 1, "status": {"type": "idle"}, "cwd": cwd.display().to_string(), "name": name, "turns": [], "historyMode": "paginated"})
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
    wire.send(completed("turn-1"));
    sending.await.unwrap().unwrap();
    stopping.await.unwrap().unwrap();
    engine.shutdown().await;
}

fn completed(turn: &str) -> Value {
    json!({"method": "turn/completed", "params": {"threadId": "t", "turn": {"id": turn, "items": [], "status": "interrupted", "error": null}}})
}

fn stop(engine: &Engine) -> tokio::task::JoinHandle<Result<()>> {
    let engine = engine.clone();
    tokio::spawn(async move { engine.interrupt(&ThreadId("t".into())).await })
}

/// An idle thread with a turn `turn-1` running.
async fn running_turn(name: &str) -> (Engine, Wire) {
    let (engine, mut wire, project) = idle_thread(name).await;
    let first = send(&engine, "one", "m");
    wire.answer("thread/resume", json!({"thread": thread(&project, "Old")})).await;
    wire.answer("turn/start", turn("turn-1")).await;
    first.await.unwrap().unwrap();
    (engine, wire)
}

/// Stop, then Send: Codex answers `turn/interrupt` before it ends the turn, so
/// the send waits for the end and starts a turn of its own.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_send_after_a_stop_starts_a_turn_once_codex_has_ended_the_stopped_one() {
    let (engine, mut wire) = running_turn("admission-stop-then-send").await;
    let stopping = stop(&engine);
    let interrupt = wire.next().await;
    assert_eq!(interrupt["method"], "turn/interrupt");
    let queued = send(&engine, "next", "m");
    wire.quiet().await;
    wire.send(json!({"id": interrupt["id"], "result": {}}));
    // Codex has answered the interrupt, not yet ended the turn: nothing is sent.
    wire.quiet().await;
    wire.send(completed("turn-1"));
    let start = wire.next().await;
    assert_eq!(start["method"], "turn/start");
    wire.send(json!({"id": start["id"], "result": turn("turn-2")}));
    stopping.await.unwrap().unwrap();
    queued.await.unwrap().unwrap();
    engine.shutdown().await;
}

/// A Stop Codex never ends the turn for: after a while the Stop is reported
/// as not taken, and no end is made up; the turn still shows as running and
/// can be stopped again, and the next send goes to Codex as a start.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_stop_codex_answers_but_never_ends_is_reported_and_lets_the_thread_go() {
    let (engine, mut wire) = running_turn("admission-stop-unended").await;
    let mut events = engine.subscribe();
    let stopping = stop(&engine);
    wire.answer("turn/interrupt", json!({})).await;
    assert!(matches!(stopping.await.unwrap(), Err(EngineError::Codex { .. })), "the Stop is reported as not taken");
    assert_eq!(summary(&engine).status, ThreadStatus::Working, "the turn may still run");
    let again = stop(&engine);
    let interrupt = wire.next().await;
    assert_eq!(interrupt["method"], "turn/interrupt", "a second Stop goes to Codex");
    assert_eq!(interrupt["params"]["turnId"], "turn-1");
    wire.send(json!({"id": interrupt["id"], "result": {}}));
    assert!(again.await.unwrap().is_err());
    let next = send(&engine, "next", "m");
    let start = wire.next().await;
    assert_eq!(start["method"], "turn/start");
    while let Ok(n) = events.try_recv() {
        assert!(!matches!(n.event, Event::TurnEnded { .. }), "no end is made up");
    }
    wire.send(json!({"id": start["id"], "result": turn("turn-2")}));
    next.await.unwrap().unwrap();
    wire.send(completed("turn-1"));
    wire.send(json!({"method": "turn/started", "params": {"threadId": "t", "turn": turn("turn-2")["turn"]}}));
    wire.quiet().await;
    let steer = send(&engine, "more", "m");
    assert_eq!(wire.next().await["method"], "turn/steer", "a turn started after it is steered as usual");
    steer.abort();
    engine.shutdown().await;
}

/// Codex that holds `turn/interrupt` unanswered (until the turn's end, which
/// does not come): the Stop is bounded all the same, and the next send goes on.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_stop_codex_never_answers_is_bounded_and_the_next_send_goes_on() {
    let (engine, mut wire) = running_turn("admission-stop-unanswered").await;
    let stopping = stop(&engine);
    assert_eq!(wire.next().await["method"], "turn/interrupt");
    let queued = send(&engine, "next", "m");
    let within = std::time::Duration::from_secs(2);
    assert!(tokio::time::timeout(within, stopping).await.expect("the Stop returns").unwrap().is_err());
    assert_eq!(summary(&engine).status, ThreadStatus::Working, "the turn may still run");
    let start = tokio::time::timeout(within, wire.next()).await.expect("the send goes on");
    assert_eq!(start["method"], "turn/start");
    wire.send(json!({"id": start["id"], "result": turn("turn-1")}));
    tokio::time::timeout(within, queued).await.unwrap().unwrap().unwrap();
    engine.shutdown().await;
}

/// A read whose history answer comes after the turn it shows running has
/// ended: its number is from before the read, so the end applies on top.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_read_is_numbered_before_codex_answers_it() {
    let (engine, mut wire, project) = idle_thread("admission-read-numbered").await;
    let first = send(&engine, "one", "m");
    wire.answer("thread/resume", json!({"thread": thread(&project, "Old")})).await;
    wire.answer("turn/start", turn("turn-1")).await;
    first.await.unwrap().unwrap();
    let mut events = engine.subscribe();
    let reading = tokio::spawn({
        let engine = engine.clone();
        async move { engine.read_thread(&ThreadId("t".into())).await }
    });
    wire.answer("thread/read", json!({"thread": thread(&project, "Old")})).await;
    let list = wire.next().await;
    assert_eq!(list["method"], "thread/turns/list");
    wire.send(completed("turn-1"));
    let ended = tokio::time::timeout(std::time::Duration::from_secs(3), async {
        loop {
            let n = events.recv().await.unwrap();
            if matches!(n.event, Event::TurnEnded { .. }) {
                break n.seq;
            }
        }
    })
    .await
    .unwrap();
    // Codex's history as of the request, with the turn still running.
    wire.send(json!({"id": list["id"], "result": {"data": [turn("turn-1")["turn"]], "nextCursor": null}}));
    let view = reading.await.unwrap().unwrap();
    assert!(view.seq < ended, "the view, numbered {}, does not claim the end ({ended}) it does not hold", view.seq);
    engine.shutdown().await;
}

/// A send whose caller gives up after `turn/start` is on its way still holds
/// the thread until Codex answers, so the next send steers the turn it started.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_send_its_caller_gave_up_on_holds_the_thread_until_codex_answers() {
    let (engine, mut wire, project) = idle_thread("admission-abandoned").await;
    let first = send(&engine, "one", "model-a");
    wire.answer("thread/resume", json!({"thread": thread(&project, "Old")})).await;
    let start = wire.next().await;
    assert_eq!(start["method"], "turn/start");
    first.abort();
    assert!(first.await.unwrap_err().is_cancelled());
    let next = send(&engine, "two", "model-b");
    wire.quiet().await;
    wire.send(json!({"id": start["id"], "result": turn("turn-1")}));
    let steer = wire.next().await;
    assert_eq!((steer["method"].as_str(), steer["params"]["expectedTurnId"].as_str()), (Some("turn/steer"), Some("turn-1")));
    wire.send(json!({"id": steer["id"], "result": {}}));
    next.await.unwrap().unwrap();
    assert_eq!(summary(&engine).settings.unwrap().model, "model-a");
    engine.shutdown().await;
}

/// A Stop on an idle thread does nothing; Codex dying while a start and a Stop
/// wait ends both, and the start's settings are not kept.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_stop_while_idle_and_codex_dying_during_a_start() {
    let (engine, mut wire, project) = idle_thread("admission-death").await;
    engine.interrupt(&ThreadId("t".into())).await.unwrap();
    wire.quiet().await;
    let first = send(&engine, "one", "pending");
    wire.answer("thread/resume", json!({"thread": thread(&project, "Old")})).await;
    assert_eq!(wire.next().await["method"], "turn/start");
    let stopping = stop(&engine);
    wire.quiet().await;
    wire.kill();
    let within = std::time::Duration::from_secs(2);
    assert!(tokio::time::timeout(within, first).await.unwrap().unwrap().is_err());
    let _ = tokio::time::timeout(within, stopping).await.unwrap().unwrap();
    assert_eq!(summary(&engine).settings.unwrap().model, "m");
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn every_shutdown_returns_once_codex_is_reaped() {
    let (connector, mut wires) = super::fake::FakeCodex::new();
    connector.reap_after(std::time::Duration::from_millis(600));
    let root = std::env::temp_dir().join(format!("chirality-reaping-twice-{}", std::process::id()));
    let engine = Engine::start_with(config(&root), connector, None).await.unwrap();
    let wire = wires.recv().await.unwrap();
    let first = tokio::spawn({
        let engine = engine.clone();
        async move { engine.shutdown().await }
    });
    while !wire.is_ended() {
        tokio::task::yield_now().await;
    }
    let began = std::time::Instant::now();
    engine.shutdown().await;
    assert!(began.elapsed() >= std::time::Duration::from_millis(500), "the second returned after {:?}", began.elapsed());
    first.await.unwrap();
    let _ = std::fs::remove_dir_all(root);
}

fn config(root: &Path) -> EngineConfig {
    EngineConfig {
        codex_bin: "codex".into(),
        codex_home: Some(root.join("home")),
        state_dir: root.join("state"),
        workflow_roots: Vec::new(),
        codex_settings: Vec::new(),
        tools: None,
    }
}

/// A Codex that never answers `initialize`: the engine exists before it, and
/// shutting it down ends that Codex and the start.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shutdown_ends_a_codex_still_starting() {
    let (connector, mut wires) = super::fake::FakeCodex::new();
    connector.hold("initialize");
    let root = std::env::temp_dir().join(format!("chirality-hung-start-{}", std::process::id()));
    let engine = Engine { inner: crate::session::Inner::new(config(&root), connector, None).unwrap() };
    let starting = tokio::spawn({
        let engine = engine.clone();
        async move { engine.connect().await }
    });
    let mut wire = wires.recv().await.unwrap();
    assert_eq!(wire.next().await["method"], "initialize");
    let asked = std::time::Instant::now();
    tokio::time::timeout(std::time::Duration::from_secs(5), engine.shutdown()).await.expect("shutdown returns");
    assert!(wire.is_ended(), "the starting Codex is ended");
    assert!(asked.elapsed() < std::time::Duration::from_secs(3));
    assert!(starting.await.unwrap().is_err(), "the start fails");
    assert!(engine.connect().await.is_err(), "and stays failed");
    assert_eq!(engine.snapshot().codex, CodexStatus::Failed { message: "the engine has shut down".into() }, "the snapshot says so");
    let _ = std::fs::remove_dir_all(root);
}

/// Shutdown while the engine reads the account, after Codex answered
/// `initialize`: the start fails, and stays failed.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_start_shut_down_while_it_reads_is_not_started() {
    let (connector, mut wires) = super::fake::FakeCodex::new();
    connector.hold("account/read");
    let root = std::env::temp_dir().join(format!("chirality-shut-reading-{}", std::process::id()));
    let engine = Engine { inner: crate::session::Inner::new(config(&root), connector, None).unwrap() };
    let starting = tokio::spawn({
        let engine = engine.clone();
        async move { engine.connect().await }
    });
    let mut wire = wires.recv().await.unwrap();
    assert_eq!(wire.next().await["method"], "account/read");
    engine.shutdown().await;
    assert!(wire.is_ended());
    assert!(starting.await.unwrap().is_err(), "the start fails");
    assert!(engine.connect().await.is_err(), "and stays failed");
    assert_eq!(engine.snapshot().codex, CodexStatus::Failed { message: "the engine has shut down".into() }, "the snapshot says so");
    let _ = std::fs::remove_dir_all(root);
}

/// `Engine::start` given up on while Codex starts: the engine nobody holds
/// is shut down, and its Codex with it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_start_given_up_on_shuts_its_engine_down() {
    use std::os::unix::fs::PermissionsExt;
    use std::time::Duration;
    let root = std::env::temp_dir().join(format!("chirality-start-given-up-{}", std::process::id()));
    std::fs::create_dir_all(root.join("home")).unwrap();
    let (pid_file, proceed, bootstrapped) = (root.join("pid"), root.join("proceed"), root.join("bootstrapped"));
    let bin = root.join("supplier");
    let script = format!(
        r#"#!/bin/sh
test ! -f '{pid}' || exit 0
echo $$ > '{pid}'
IFS= read -r init || exit 0
while test ! -f '{proceed}'; do /bin/sleep 0.01; done
printf '%s\n' '{{"id":1,"result":{{"userAgent":"chirality/0.171.0 (fake)"}}}}'
IFS= read -r initialized || exit 0
IFS= read -r account || exit 0
printf '%s\n' '{{"id":2,"result":{{"account":null}}}}'
IFS= read -r models || exit 0
printf '%s\n' '{{"id":3,"result":{{"data":[],"nextCursor":null}}}}'
touch '{bootstrapped}'
while IFS= read -r more; do :; done
"#,
        pid = pid_file.display(),
        proceed = proceed.display(),
        bootstrapped = bootstrapped.display()
    );
    std::fs::write(&bin, script).unwrap();
    std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
    let starting = tokio::spawn(Engine::start(EngineConfig { codex_bin: bin, ..config(&root) }));
    let appears = |path: std::path::PathBuf| async move {
        tokio::time::timeout(Duration::from_secs(3), async {
            while !path.is_file() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .is_ok()
    };
    assert!(appears(pid_file.clone()).await);
    let pid = std::fs::read_to_string(&pid_file).unwrap().trim().to_string();
    starting.abort();
    assert!(matches!(starting.await, Err(e) if e.is_cancelled()));
    std::fs::write(&proceed, "go").unwrap();
    let _ = appears(bootstrapped).await;
    let gone = async {
        for _ in 0..300 {
            let alive = std::process::Command::new("/bin/kill").args(["-0", &pid]).stderr(std::process::Stdio::null()).status().unwrap().success();
            if !alive {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        false
    };
    let gone = gone.await;
    if !gone {
        let _ = std::process::Command::new("/bin/kill").args(["-KILL", &pid]).status();
    }
    let _ = std::fs::remove_dir_all(root);
    assert!(gone, "the Codex of a start nobody took is ended");
}
