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

/// Every engine command is named `engine:<name>`, as the client calls it.
#[test]
fn each_engine_command_has_the_prefix() {
    let source = include_str!("commands.rs");
    for name in super::COMMANDS {
        assert!(source.contains(&format!("#[tauri::command(rename = \"engine:{name}\")]\nasync fn {name}(")), "{name}");
    }
    assert_eq!(source.matches("#[tauri::command").count(), super::COMMANDS.len());
}

#[test]
fn a_mark_is_the_last_number_sent() {
    assert_eq!(serde_json::to_value(super::Mark { last: 7 }).unwrap(), serde_json::json!({ "last": 7 }));
}

/// Processes running `codex` that this test process has started and not yet reaped.
fn codex_children(codex: &std::path::Path) -> String {
    let out = std::process::Command::new("pgrep").args(["-f", "-P", &std::process::id().to_string(), &codex.display().to_string()]).output().unwrap();
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// Quit while the engine is still starting: the exit is held without
/// blocking the event thread, the start is waited for and shut down, Codex is
/// reaped, and only then does the app exit, with the code asked for. With
/// `CHIRALITY_TEST_CODEX` the real Codex starts; without it the start fails.
#[test]
fn quitting_holds_the_exit_until_the_engine_has_shut_down() {
    let codex = std::env::var_os("CHIRALITY_TEST_CODEX").map(std::path::PathBuf::from).filter(|p| p.is_file());
    let real = codex.clone();
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
    let app = mock_builder().plugin(plugin).invoke_handler(crate::commands(|_| false)).build(mock_context(noop_assets())).unwrap();

    let mut ours = String::new();
    if let Some(real) = &real {
        // Codex has been spawned; the engine's start is still under way.
        let spawned = Instant::now() + Duration::from_secs(10);
        while ours.is_empty() && Instant::now() < spawned {
            std::thread::sleep(Duration::from_millis(5));
            ours = codex_children(real);
        }
        assert_ne!(ours, "", "the engine starts Codex when the app starts");
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
    // Each Codex seen is gone, not even a zombie: reaped before the exit.
    for pid in ours.split_whitespace() {
        let alive = std::process::Command::new("/bin/kill").args(["-0", pid]).stderr(std::process::Stdio::null()).status().unwrap().success();
        assert!(!alive, "Codex {pid} was not reaped before the exit");
    }
    assert!(!super::exit_requested(app.handle(), Some(3)), "the exit asked for after shutting down goes ahead");
    let _ = std::fs::remove_dir_all(&root);
}

/// A Codex that never answers `initialize`: quitting ends it (after its two
/// seconds), well before the exit's own limit, and the app exits with the code asked for.
#[test]
fn quitting_ends_a_codex_that_never_finishes_starting() {
    use std::os::unix::fs::PermissionsExt;
    let root = std::env::temp_dir().join(format!("chirality-exit-hung-{}", std::process::id()));
    std::fs::create_dir_all(root.join("home")).unwrap();
    let bin = root.join("hung-codex");
    let pid_file = root.join("pid");
    std::fs::write(&bin, format!("#!/bin/sh\necho $$ > '{}'\nexec /bin/sleep 90\n", pid_file.display())).unwrap();
    std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
    let config = EngineConfig {
        codex_bin: bin,
        codex_home: Some(root.join("home")),
        state_dir: root.join("state"),
        workflow_roots: Vec::new(),
        codex_settings: Vec::new(),
        tools: None,
    };
    let (exit_tx, exit_rx) = std::sync::mpsc::channel();
    let plugin = super::plugin_exiting_with(move |_| Ok(config), move |_| Arc::new(move |code| exit_tx.send(code).unwrap()));
    let app = mock_builder().plugin(plugin).build(mock_context(noop_assets())).unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while !pid_file.is_file() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    let pid = std::fs::read_to_string(&pid_file).unwrap();
    let asked = Instant::now();
    assert!(super::exit_requested(app.handle(), Some(9)));
    let exit = exit_rx.recv_timeout(Duration::from_secs(20));
    let took = asked.elapsed();
    let alive = std::process::Command::new("/bin/kill").args(["-0", pid.trim()]).stderr(std::process::Stdio::null()).status().unwrap().success();
    if alive {
        let _ = std::process::Command::new("/bin/kill").args(["-KILL", pid.trim()]).status();
    }
    let _ = std::fs::remove_dir_all(&root);
    assert_eq!(exit.unwrap(), 9, "the app exits with the code asked for");
    assert!(!alive, "the starting Codex was ended before the exit");
    assert!(took < Duration::from_secs(8), "quitting took {took:?}");
}
