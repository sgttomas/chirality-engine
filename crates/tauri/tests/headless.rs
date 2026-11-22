//! The adapter against the real engine and the pinned Codex, with no window and
//! no application: Tauri's mock runtime carries each call through IPC as the
//! interface would. Codex runs in a scratch home with every outside host
//! unreachable; no sign-in, and model turns only against a stub on loopback
//! (`support`). `CHIRALITY_TEST_CODEX` names the
//! pinned Codex (skipped without it); `CHIRALITY_TEST_SCRATCH` the folder for
//! the run (a folder in the system's temporary directory by default).

mod support;

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use chirality_engine_tauri::chirality_engine::{
    AppTool, AppTools, CodexStatus, EngineConfig, EngineError, Event, ItemBody, Numbered, Project, Snapshot, ThreadId, ThreadTool, ThreadView, ToolCall,
    ToolCallStatus, ToolNamespace, ToolOutput, TurnState,
};
use chirality_engine_tauri::{EVENT, commands, engine, plugin, shutdown};
use serde_json::{Value, json};
use tauri::ipc::{CallbackFn, InvokeBody};
use tauri::test::{INVOKE_KEY, MockRuntime, get_ipc_response, mock_builder, mock_context, noop_assets};
use tauri::webview::InvokeRequest;
use tauri::{App, AppHandle, Listener, Runtime, WebviewWindow, WebviewWindowBuilder};

fn call(webview: &WebviewWindow<MockRuntime>, cmd: &str, args: Value) -> Result<Value, Value> {
    let request = InvokeRequest {
        cmd: cmd.into(),
        callback: CallbackFn(0),
        error: CallbackFn(1),
        url: "tauri://localhost".parse().unwrap(),
        body: InvokeBody::Json(args),
        headers: Default::default(),
        invoke_key: INVOKE_KEY.to_string(),
    };
    get_ipc_response(webview, request).map(|body| body.deserialize::<Value>().unwrap())
}

/// An application's own command, reaching the engine.
#[tauri::command]
async fn project_count<R: Runtime>(app: AppHandle<R>) -> Result<usize, EngineError> {
    Ok(engine(&app).await?.snapshot().projects.len())
}

/// An application's own command with the name of an engine method: the engine's are `engine:<method>`.
#[tauri::command(rename = "snapshot")]
fn own_snapshot() -> &'static str {
    "the application's snapshot"
}

fn tools(namespace: &str) -> AppTools {
    AppTools {
        namespace: ToolNamespace { name: namespace.into(), description: "The estimate.".into() },
        tools: vec![AppTool { name: "check_estimate".into(), description: "Checks it.".into(), input_schema: json!({"type": "object"}) }],
        handler: Arc::new(|_: ToolCall| async { ToolOutput::text("ok") }),
    }
}

fn config(codex: PathBuf, root: &std::path::Path, tools: Option<AppTools>) -> EngineConfig {
    EngineConfig {
        codex_bin: codex,
        codex_home: Some(root.join("home")),
        state_dir: root.join("state"),
        workflow_roots: Vec::new(),
        codex_settings: vec!["analytics.enabled=false".into()],
        tools,
    }
}

fn app(config: EngineConfig) -> App<MockRuntime> {
    mock_builder().plugin(plugin(move |_| Ok(config))).invoke_handler(commands(tauri::generate_handler![project_count, own_snapshot])).build(mock_context(noop_assets())).unwrap()
}

/// Codex processes this test process has started and not yet reaped.
fn codex_children() -> String {
    let out = std::process::Command::new("pgrep").args(["-f", "-P", &std::process::id().to_string(), "app-server"]).output().unwrap();
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// The application's tools reach the engine through the plugin: a catalog the
/// engine refuses fails its start, which the interface is told.
#[test]
fn the_applications_tools_are_given_to_the_engine() {
    let root = std::env::temp_dir().join(format!("chirality-tauri-tools-{}", std::process::id()));
    let app = app(config(root.join("no-codex"), &root, Some(tools("web"))));
    let webview = WebviewWindowBuilder::new(&app, "main", Default::default()).build().unwrap();
    let refused = call(&webview, "engine:snapshot", json!({})).unwrap_err();
    assert!(refused.to_string().contains("tool namespace \\\"web\\\""), "{refused}");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn the_interface_reaches_the_real_engine_without_a_window() {
    let Some(codex) = codex_or_skip() else { return };
    let _one = CODEX_RUNS.lock().unwrap_or_else(|e| e.into_inner());
    let scratch = std::env::var_os("CHIRALITY_TEST_SCRATCH").map(PathBuf::from).unwrap_or_else(|| std::env::temp_dir().join("chirality-engine-tests"));
    let root = scratch.join(format!("tauri-headless-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    for dir in ["home", "state", "project/notes"] {
        std::fs::create_dir_all(root.join(dir)).unwrap();
    }
    std::fs::write(root.join("project/notes/brief.md"), "# Brief\n").unwrap();
    // Codex inherits these: every outside host is unreachable.
    for name in ["HTTPS_PROXY", "https_proxy", "HTTP_PROXY", "http_proxy", "ALL_PROXY", "all_proxy"] {
        unsafe { std::env::set_var(name, "http://127.0.0.1:9") };
    }

    let app = app(config(codex.clone(), &root, Some(tools("estimate"))));
    let events = Arc::new(Mutex::new(Vec::<Value>::new()));
    let seen = events.clone();
    app.listen_any(EVENT, move |e| seen.lock().unwrap().push(serde_json::from_str(e.payload()).unwrap()));
    let webview = WebviewWindowBuilder::new(&app, "main", Default::default()).build().unwrap();

    let snapshot: Snapshot = serde_json::from_value(call(&webview, "engine:snapshot", json!({})).unwrap()).unwrap();
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut ready = matches!(snapshot.codex, CodexStatus::Ready { .. });
    while !ready && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(100));
        let snapshot: Snapshot = serde_json::from_value(call(&webview, "engine:snapshot", json!({})).unwrap()).unwrap();
        ready = matches!(snapshot.codex, CodexStatus::Ready { .. });
    }
    assert!(ready, "Codex did not become ready with the application's tools");

    let project: Project = serde_json::from_value(call(&webview, "engine:add_project", json!({ "path": root.join("project") })).unwrap()).unwrap();
    let text = call(&webview, "engine:read_file", json!({ "project": project.id, "path": "notes/brief.md" })).unwrap();
    assert_eq!(text, json!("# Brief\n"));
    assert!(call(&webview, "engine:read_thread", json!({ "thread": "no-such-thread" })).is_err());
    assert_eq!(call(&webview, "project_count", json!({})).unwrap(), json!(1), "the application's own command");
    assert_eq!(call(&webview, "snapshot", json!({})).unwrap(), json!("the application's snapshot"), "named like an engine method");
    assert!(call(&webview, "not_a_command", json!({})).is_err());

    // Past the next mark.
    std::thread::sleep(Duration::from_millis(1500));
    {
        let sent = events.lock().unwrap();
        let numbered: Vec<Numbered> = sent.iter().filter(|v| v.get("event").is_some()).map(|v| serde_json::from_value(v.clone()).unwrap()).collect();
        let projects_event = numbered.iter().any(|n| matches!(&n.event, Event::Projects { projects } if projects.iter().any(|p| p.id == project.id)));
        assert!(projects_event, "the projects event was not forwarded");
        assert!(numbered.windows(2).all(|w| w[1].seq == w[0].seq + 1), "forwarded in order, numbered without gaps");
        let last = numbered.last().unwrap().seq;
        let snapshot: Snapshot = serde_json::from_value(call(&webview, "engine:snapshot", json!({})).unwrap()).unwrap();
        assert_eq!(snapshot.seq, last, "the snapshot holds the last event forwarded");
        assert_eq!(sent.last().unwrap(), &json!({ "last": last }), "a mark follows the events, with the last number sent");
    }

    tauri::async_runtime::block_on(shutdown(app.handle()));
    assert_eq!(codex_children(), "", "Codex is reaped when shutdown returns");

    // Shut down while the engine is still starting: the start is waited for and its Codex reaped too.
    let starting = self::app(config(codex, &root, None));
    tauri::async_runtime::block_on(shutdown(starting.handle()));
    assert_eq!(codex_children(), "", "the Codex a start in flight spawned is reaped");
    let _ = std::fs::remove_dir_all(&root);
}

/// The tests that run Codex, one at a time: each counts the Codex processes left.
static CODEX_RUNS: Mutex<()> = Mutex::new(());

fn codex_or_skip() -> Option<PathBuf> {
    let codex = std::env::var_os("CHIRALITY_TEST_CODEX").map(PathBuf::from).filter(|p| p.is_file());
    if codex.is_none() {
        eprintln!("skipped: set CHIRALITY_TEST_CODEX to the pinned Codex");
    }
    codex
}

/// The engine's tools reach the agent through the adapter: a thread's agent reads another thread
/// with `chirality.read`, and gets its messages; the read shows in its conversation.
#[test]
fn an_agent_reads_another_thread_through_the_adapter() {
    let Some(codex) = codex_or_skip() else { return };
    let _one = CODEX_RUNS.lock().unwrap_or_else(|e| e.into_inner());
    let scratch = std::env::var_os("CHIRALITY_TEST_SCRATCH").map(PathBuf::from).unwrap_or_else(|| std::env::temp_dir().join("chirality-engine-tests"));
    let root = scratch.join(format!("tauri-read-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    for dir in ["home", "state", "project"] {
        std::fs::create_dir_all(root.join(dir)).unwrap();
    }
    unsafe { std::env::set_var("NO_PROXY", "127.0.0.1,localhost") };
    for name in ["HTTPS_PROXY", "https_proxy", "HTTP_PROXY", "http_proxy", "ALL_PROXY", "all_proxy"] {
        unsafe { std::env::set_var(name, "http://127.0.0.1:9") };
    }
    let peer = Arc::new(Mutex::new(String::new()));
    let heard = Arc::new(Mutex::new(None::<String>));
    let (peer_id, heard_by) = (peer.clone(), heard.clone());
    let url = support::start(Arc::new(move |request| {
        if !request["input"].to_string().contains("Read the other thread.") {
            return vec![support::message("The deck is surveyed.")];
        }
        match support::output(request, "call_read") {
            None => vec![support::call("call_read", "chirality", "read", json!({"thread": *peer_id.lock().unwrap()}))],
            Some(answer) => {
                *heard_by.lock().unwrap() = Some(answer.to_string());
                vec![support::message("Read it.")]
            }
        }
    }));
    let mut config = config(codex, &root, None);
    config.codex_settings.extend(["model_provider=\"openai\"".to_string(), format!("openai_base_url=\"{url}\"")]);
    let app = app(config);
    let events = Arc::new(Mutex::new(Vec::<Value>::new()));
    let seen = events.clone();
    app.listen_any(EVENT, move |e| seen.lock().unwrap().push(serde_json::from_str(e.payload()).unwrap()));
    let webview = WebviewWindowBuilder::new(&app, "main", Default::default()).build().unwrap();
    let project: Project = serde_json::from_value(call(&webview, "engine:add_project", json!({ "path": root.join("project") })).unwrap()).unwrap();
    let settings = json!({ "model": "gpt-6.2-sol", "effort": "low", "access": "ask" });
    let ended = |thread: &ThreadId| {
        let deadline = Instant::now() + Duration::from_secs(60);
        while Instant::now() < deadline {
            let done = events.lock().unwrap().iter().filter_map(|v| serde_json::from_value::<Numbered>(v.clone()).ok()).any(|n| matches!(&n.event, Event::TurnEnded { thread: t, .. } if t == thread));
            if done {
                return;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        panic!("the turn did not end");
    };
    let start = |text: &str| -> ThreadId {
        let message = json!({ "text": text, "attachments": [], "workflow": null, "threads": [] });
        serde_json::from_value(call(&webview, "engine:start_thread", json!({ "project": project.id, "message": message, "settings": settings })).unwrap()).unwrap()
    };
    let surveyed = start("Survey the deck.");
    ended(&surveyed);
    *peer.lock().unwrap() = surveyed.0.clone();
    let reader = start("Read the other thread.");
    ended(&reader);

    let answer: Value = serde_json::from_str(&heard.lock().unwrap().clone().expect("the agent heard the read")).unwrap();
    let answer: Value = answer.as_str().map_or(answer.clone(), |text| serde_json::from_str(text).unwrap());
    assert_eq!((answer["status"].as_str(), answer["project"].as_str()), (Some("idle"), Some("project")), "{answer}");
    let kinds: Vec<&str> = answer["items"].as_array().unwrap().iter().map(|i| i["kind"].as_str().unwrap()).collect();
    assert_eq!(kinds, ["reply", "person"], "{answer}");
    assert_eq!(answer["items"][0]["text"], "The deck is surveyed.");
    let view: ThreadView = serde_json::from_value(call(&webview, "engine:read_thread", json!({ "thread": reader })).unwrap()).unwrap();
    let read = ItemBody::ThreadTool { tool: ThreadTool::Read, thread: Some(surveyed.clone()), text: None, status: ToolCallStatus::Completed };
    assert!(view.turns[0].items.iter().any(|i| i.body == read), "{:?}", view.turns[0].items);
    assert_eq!(view.turns[0].state, TurnState::Completed);
    let peer_view: ThreadView = serde_json::from_value(call(&webview, "engine:read_thread", json!({ "thread": surveyed })).unwrap()).unwrap();
    assert_eq!(peer_view.turns.len(), 1, "the thread read was not woken");

    tauri::async_runtime::block_on(shutdown(app.handle()));
    assert_eq!(codex_children(), "", "Codex is reaped when shutdown returns");
    let _ = std::fs::remove_dir_all(&root);
}

/// An application with its own ACL manifest grants the engine's commands with
/// `permissions/engine.toml`, resolved here as Tauri resolves it.
#[test]
fn an_application_with_its_own_acl_grants_the_engine_commands_with_the_permission_file() {
    use std::collections::BTreeMap;
    use tauri_utils::acl::capability::Capability;
    use tauri_utils::acl::manifest::{Manifest, PermissionFile};
    use tauri_utils::acl::resolved::Resolved;

    // The app's permissions: the engine's file and one of its own.
    let acl = || {
        let engine: PermissionFile = toml::from_str(include_str!("../permissions/engine.toml")).unwrap();
        let own: PermissionFile = toml::from_str("[[permission]]\nidentifier = \"allow-own\"\ncommands.allow = [\"snapshot\"]\n").unwrap();
        BTreeMap::from([(tauri_utils::acl::APP_ACL_KEY.to_string(), Manifest::new(vec![engine, own], None))])
    };
    let app_with = |permissions: serde_json::Value| {
        let capability: Capability = serde_json::from_value(json!({ "identifier": "default", "windows": ["main"], "permissions": permissions })).unwrap();
        let resolved = Resolved::resolve(&acl(), BTreeMap::from([("default".to_string(), capability)]), tauri_utils::platform::Target::current()).unwrap();
        assert!(resolved.has_app_acl);
        let mut context = mock_context(noop_assets());
        *context.runtime_authority_mut() = tauri::runtime_authority!(acl(), resolved);
        let root = std::env::temp_dir().join(format!("chirality-tauri-acl-{}", std::process::id()));
        let app = mock_builder()
            .plugin(plugin(move |_| Ok(config(root.join("no-codex"), &root, Some(tools("web"))))))
            .invoke_handler(commands(tauri::generate_handler![own_snapshot]))
            .build(context)
            .unwrap();
        let webview = WebviewWindowBuilder::new(&app, "main", Default::default()).build().unwrap();
        (app, webview)
    };

    let (_app, granted) = app_with(json!(["allow-engine", "allow-own"]));
    assert_eq!(call(&granted, "snapshot", json!({})).unwrap(), json!("the application's snapshot"));
    // The engine answers (its start is refused for the `web` namespace, which the interface is told).
    let answered = call(&granted, "engine:snapshot", json!({})).unwrap_err();
    assert!(answered.to_string().contains("tool namespace"), "{answered}");

    let (_app, ungranted) = app_with(json!(["allow-own"]));
    let refused = call(&ungranted, "engine:snapshot", json!({})).unwrap_err();
    assert!(refused.to_string().contains("not allowed"), "{refused}");
}

/// The permission file names exactly the engine's commands.
#[test]
fn the_permission_file_names_every_engine_command() {
    let file: tauri_utils::acl::manifest::PermissionFile = toml::from_str(include_str!("../permissions/engine.toml")).unwrap();
    let named: Vec<String> = file.permission[0].commands.allow.clone();
    let commands: Vec<String> = chirality_engine_tauri::COMMANDS.iter().map(|c| format!("{}{c}", chirality_engine_tauri::PREFIX)).collect();
    assert_eq!(named, commands);
}
