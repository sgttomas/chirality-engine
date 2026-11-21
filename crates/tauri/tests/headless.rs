//! The adapter against the real engine and the pinned Codex, with no window and
//! no application: Tauri's mock runtime carries each call through IPC as the
//! interface would. Codex runs in a scratch home with every outside host
//! unreachable; no sign-in, no model turn. `CHIRALITY_TEST_CODEX` names the
//! pinned Codex (skipped without it); `CHIRALITY_TEST_SCRATCH` the folder for
//! the run (a folder in the system's temporary directory by default).

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use chirality_engine_tauri::chirality_engine::{
    AppTool, AppTools, CodexStatus, EngineConfig, EngineError, Event, Numbered, Project, Snapshot, ToolCall, ToolNamespace, ToolOutput,
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
    let Some(codex) = std::env::var_os("CHIRALITY_TEST_CODEX").map(PathBuf::from).filter(|p| p.is_file()) else {
        eprintln!("skipped: set CHIRALITY_TEST_CODEX to the pinned Codex");
        return;
    };
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
    let found = call(&webview, "engine:find_files", json!({ "project": project.id, "query": "brief" })).unwrap();
    assert!(found.to_string().contains("notes/brief.md"), "{found}");
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
