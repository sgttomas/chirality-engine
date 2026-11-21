//! The adapter against the real engine and the pinned Codex, with no window:
//! Tauri's mock runtime carries each call through IPC as the interface would.
//! Codex runs in a scratch home with every outside host unreachable; no
//! sign-in, no model turn. Skipped when the pinned Codex is absent.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use chirality_engine::{CodexStatus, EngineConfig, Event, Project, Snapshot};
use serde_json::{Value, json};
use tauri::ipc::{CallbackFn, InvokeBody};
use tauri::test::{INVOKE_KEY, get_ipc_response, mock_builder, mock_context, noop_assets};
use tauri::webview::InvokeRequest;
use tauri::{Listener, Manager, WebviewWindow, WebviewWindowBuilder};

const CODEX: &str = "/Users/ryan/dev/chirality-t3/scratch/engine/supplier/rust-v0.171.0/pkg/bin/codex";
const SCRATCH: &str = "/Users/ryan/dev/chirality-t3/scratch/engine/desktop/headless";

fn call(webview: &WebviewWindow<tauri::test::MockRuntime>, cmd: &str, args: Value) -> Result<Value, Value> {
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

#[test]
fn the_interface_reaches_the_real_engine_without_a_window() {
    if !Path::new(CODEX).is_file() {
        eprintln!("skipped: the pinned Codex is not at {CODEX}");
        return;
    }
    let root = PathBuf::from(SCRATCH).join(format!("run-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    for dir in ["home", "state", "project/notes"] {
        std::fs::create_dir_all(root.join(dir)).unwrap();
    }
    std::fs::write(root.join("project/notes/brief.md"), "# Brief\n").unwrap();
    // Codex inherits these: every outside host is unreachable.
    for name in ["HTTPS_PROXY", "https_proxy", "HTTP_PROXY", "http_proxy", "ALL_PROXY", "all_proxy"] {
        unsafe { std::env::set_var(name, "http://127.0.0.1:9") };
    }
    let config = EngineConfig {
        codex_bin: CODEX.into(),
        codex_home: Some(root.join("home")),
        state_dir: root.join("state"),
        workflow_roots: Vec::new(),
        codex_settings: vec!["analytics.enabled=false".into()],
        tools: None,
    };

    let app = super::commands(mock_builder()).build(mock_context(noop_assets())).unwrap();
    let events = Arc::new(Mutex::new(Vec::<Event>::new()));
    let seen = events.clone();
    app.listen_any("engine", move |e| seen.lock().unwrap().push(serde_json::from_str(e.payload()).unwrap()));
    super::start(app.handle(), config);
    let webview = WebviewWindowBuilder::new(&app, "main", Default::default()).build().unwrap();

    let snapshot: Snapshot = serde_json::from_value(call(&webview, "snapshot", json!({})).unwrap()).unwrap();
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut ready = matches!(snapshot.codex, CodexStatus::Ready { .. });
    while !ready && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(100));
        let snapshot: Snapshot = serde_json::from_value(call(&webview, "snapshot", json!({})).unwrap()).unwrap();
        ready = matches!(snapshot.codex, CodexStatus::Ready { .. });
    }
    assert!(ready, "Codex did not become ready");

    let project: Project = serde_json::from_value(call(&webview, "add_project", json!({ "path": root.join("project") })).unwrap()).unwrap();
    let found = call(&webview, "find_files", json!({ "project": project.id, "query": "brief" })).unwrap();
    assert!(found.to_string().contains("notes/brief.md"), "{found}");
    let text = call(&webview, "read_file", json!({ "project": project.id, "path": "notes/brief.md" })).unwrap();
    assert_eq!(text, json!("# Brief\n"));
    assert!(call(&webview, "read_thread", json!({ "thread": "no-such-thread" })).is_err());

    std::thread::sleep(Duration::from_millis(300));
    let projects_event = events.lock().unwrap().iter().any(|e| matches!(e, Event::Projects { projects } if projects.iter().any(|p| p.id == project.id)));
    assert!(projects_event, "the projects event was not forwarded");

    if let Some(Ok(engine)) = app.state::<super::Started>().engine.get() {
        tauri::async_runtime::block_on(engine.shutdown());
    }
    let _ = std::fs::remove_dir_all(&root);
}
