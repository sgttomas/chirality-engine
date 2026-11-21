use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use chirality_engine::EngineConfig;
use tauri::test::{mock_builder, mock_context, noop_assets};

/// The interface's client names exactly these commands.
#[test]
fn the_interface_client_names_the_same_commands() {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../packages/ui/src/engine/commands.ts");
    let text = std::fs::read_to_string(path).unwrap();
    let list = text.split_once('[').and_then(|(_, rest)| rest.split_once(']')).map(|(list, _)| list).unwrap();
    let names: Vec<&str> = list.split(',').map(|n| n.trim().trim_matches('"')).filter(|n| !n.is_empty()).collect();
    assert_eq!(names, super::COMMANDS);
}

#[test]
fn an_application_command_named_like_an_engine_command_is_refused() {
    let refused = super::checked::<tauri::test::MockRuntime, _>(&["estimate_total", "my_app :: send"], |_| false).err();
    assert_eq!(refused, Some(super::Shadowed("send".into())));
    assert_eq!(refused.unwrap().to_string(), "the application's command `send` has the name of an engine command; give it another name");
    assert!(super::checked::<tauri::test::MockRuntime, _>(&["estimate_total"], |_| false).is_ok());
}

/// Codex processes this test process has started and not yet reaped.
fn codex_children() -> String {
    let out = std::process::Command::new("pgrep").args(["-P", &std::process::id().to_string()]).output().unwrap();
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// Quit while the engine is still starting: the exit is held without
/// blocking the event thread, the start is waited for and shut down, Codex is
/// reaped, and only then does the app exit, with the code asked for. With
/// `CHIRALITY_TEST_CODEX` the real Codex starts; without it the start fails.
#[test]
fn quitting_holds_the_exit_until_the_engine_has_shut_down() {
    let codex = std::env::var_os("CHIRALITY_TEST_CODEX").map(std::path::PathBuf::from).filter(|p| p.is_file());
    let real = codex.is_some();
    let root = std::env::temp_dir().join(format!("chirality-exit-hook-{}", std::process::id()));
    for dir in ["home", "state"] {
        std::fs::create_dir_all(root.join(dir)).unwrap();
    }
    let config = EngineConfig {
        codex_bin: codex.unwrap_or_else(|| root.join("no-codex")),
        codex_home: Some(root.join("home")),
        state_dir: root.join("state"),
        workflow_roots: Vec::new(),
        codex_settings: vec!["analytics.enabled=false".into()],
        tools: None,
    };
    let exits = Arc::new(Mutex::new(Vec::<i32>::new()));
    let seen = exits.clone();
    let plugin = super::plugin_exiting_with(move |_| Ok(config), move |_| Arc::new(move |code| seen.lock().unwrap().push(code)));
    let app = mock_builder().plugin(plugin).invoke_handler(crate::commands![]).build(mock_context(noop_assets())).unwrap();

    if real {
        // Codex has been spawned; the engine's start is still under way.
        let spawned = Instant::now() + Duration::from_secs(10);
        while codex_children().is_empty() && Instant::now() < spawned {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_ne!(codex_children(), "", "the engine starts Codex when the app starts");
    }
    let asked = Instant::now();
    assert!(super::exit_requested(app.handle(), Some(3)), "the exit is held");
    assert!(asked.elapsed() < Duration::from_millis(100), "without waiting on the event thread");
    assert!(super::exit_requested(app.handle(), Some(3)), "and held again while shutting down");
    let deadline = Instant::now() + Duration::from_secs(30);
    while exits.lock().unwrap().is_empty() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(*exits.lock().unwrap(), [3], "the app exits once, with the code asked for");
    if real {
        assert_eq!(codex_children(), "", "Codex was reaped before the exit");
    }
    assert!(!super::exit_requested(app.handle(), Some(3)), "the exit asked for after shutting down goes ahead");
    let _ = std::fs::remove_dir_all(&root);
}
