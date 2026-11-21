//! The desktop wrapper's Rust side: one command per engine method, and every
//! engine event forwarded to the interface as one `engine` event. Nothing else.

use std::path::PathBuf;

use chirality_engine::{
    Answer, Engine, EngineConfig, Message, Project, ProjectId, RequestId, Result, Snapshot, ThreadId,
    ThreadView, TurnId, TurnSettings, Workflow,
};
use tauri::{AppHandle, Builder, Emitter, Manager, Runtime, State};
use tokio::sync::OnceCell;

/// The engine, started once; every command waits for the same start.
struct Started {
    config: EngineConfig,
    engine: OnceCell<Result<Engine>>,
}

async fn engine(s: &Started) -> Result<Engine> {
    s.engine.get_or_init(|| Engine::start(s.config.clone())).await.clone()
}

type S<'a> = State<'a, Started>;

#[tauri::command]
async fn snapshot(s: S<'_>) -> Result<Snapshot> {
    Ok(engine(&s).await?.snapshot())
}
#[tauri::command]
async fn sign_in(s: S<'_>) -> Result<()> {
    engine(&s).await?.sign_in().await
}
#[tauri::command]
async fn cancel_sign_in(s: S<'_>) -> Result<()> {
    engine(&s).await?.cancel_sign_in().await
}
#[tauri::command]
async fn sign_out(s: S<'_>) -> Result<()> {
    engine(&s).await?.sign_out().await
}
#[tauri::command]
async fn add_project(s: S<'_>, path: PathBuf) -> Result<Project> {
    engine(&s).await?.add_project(path).await
}
#[tauri::command]
async fn remove_project(s: S<'_>, project: ProjectId) -> Result<()> {
    engine(&s).await?.remove_project(&project).await
}
#[tauri::command]
async fn workflows(s: S<'_>, project: ProjectId) -> Result<Vec<Workflow>> {
    engine(&s).await?.workflows(&project).await
}
#[tauri::command]
async fn read_thread(s: S<'_>, thread: ThreadId) -> Result<ThreadView> {
    engine(&s).await?.read_thread(&thread).await
}
#[tauri::command]
async fn start_thread(s: S<'_>, project: ProjectId, message: Message, settings: TurnSettings) -> Result<ThreadId> {
    engine(&s).await?.start_thread(&project, message, settings).await
}
#[tauri::command]
async fn send(s: S<'_>, thread: ThreadId, message: Message, settings: TurnSettings) -> Result<()> {
    engine(&s).await?.send(&thread, message, settings).await
}
#[tauri::command]
async fn interrupt(s: S<'_>, thread: ThreadId) -> Result<()> {
    engine(&s).await?.interrupt(&thread).await
}
#[tauri::command]
async fn fork(s: S<'_>, thread: ThreadId, through: TurnId) -> Result<ThreadId> {
    engine(&s).await?.fork(&thread, &through).await
}
#[tauri::command]
async fn revert(s: S<'_>, thread: ThreadId, from: TurnId) -> Result<()> {
    engine(&s).await?.revert(&thread, &from).await
}
#[tauri::command]
async fn rename(s: S<'_>, thread: ThreadId, title: String) -> Result<()> {
    engine(&s).await?.rename(&thread, title).await
}
#[tauri::command]
async fn archive(s: S<'_>, thread: ThreadId) -> Result<()> {
    engine(&s).await?.archive(&thread).await
}
#[tauri::command]
async fn mark_read(s: S<'_>, thread: ThreadId) -> Result<()> {
    engine(&s).await?.mark_read(&thread);
    Ok(())
}
#[tauri::command]
async fn answer(s: S<'_>, request: RequestId, answer: Answer) -> Result<()> {
    engine(&s).await?.answer(&request, answer).await
}
#[tauri::command]
async fn read_file(s: S<'_>, project: ProjectId, path: PathBuf) -> Result<String> {
    engine(&s).await?.read_file(&project, path).await
}
#[tauri::command]
async fn find_files(s: S<'_>, project: ProjectId, query: String) -> Result<Vec<PathBuf>> {
    engine(&s).await?.find_files(&project, query).await
}

/// The engine's setup on this machine. `CHIRALITY_CODEX_BIN` and
/// `CHIRALITY_WORKFLOW_ROOTS` (a `:`-separated list) override the defaults.
fn config(app: &tauri::App) -> std::result::Result<EngineConfig, Box<dyn std::error::Error>> {
    let codex_bin = match std::env::var_os("CHIRALITY_CODEX_BIN") {
        Some(path) => PathBuf::from(path),
        None => app.path().resource_dir()?.join("codex"),
    };
    let workflow_roots = std::env::var_os("CHIRALITY_WORKFLOW_ROOTS")
        .map(|roots| std::env::split_paths(&roots).collect())
        .unwrap_or_default();
    Ok(EngineConfig {
        codex_bin,
        codex_home: None,
        state_dir: app.path().app_data_dir()?,
        workflow_roots,
        codex_settings: Vec::new(),
        tools: None,
    })
}

/// Forwards every engine event, in order, for as long as the app runs.
async fn forward<R: Runtime>(app: AppHandle<R>) {
    let Ok(engine) = engine(&app.state::<Started>()).await else { return };
    let mut events = engine.subscribe();
    loop {
        match events.recv().await {
            Ok(event) => {
                let _ = app.emit("engine", event);
            }
            Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
            Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
        }
    }
}

/// The engine's commands.
fn commands<R: Runtime>(builder: Builder<R>) -> Builder<R> {
    builder.invoke_handler(tauri::generate_handler![
        snapshot,
        sign_in,
        cancel_sign_in,
        sign_out,
        add_project,
        remove_project,
        workflows,
        read_thread,
        start_thread,
        send,
        interrupt,
        fork,
        revert,
        rename,
        archive,
        mark_read,
        answer,
        read_file,
        find_files,
    ])
}

/// Gives the app its engine and starts forwarding the engine's events.
fn start<R: Runtime>(app: &AppHandle<R>, config: EngineConfig) {
    app.manage(Started { config, engine: OnceCell::new() });
    tauri::async_runtime::spawn(forward(app.clone()));
}

pub fn run() {
    commands(Builder::default())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_opener::init())
        .setup(|app| {
            start(app.handle(), config(app)?);
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("the app starts")
        .run(|app, event| {
            if let (tauri::RunEvent::Exit, Some(Ok(engine))) = (event, app.state::<Started>().engine.get()) {
                tauri::async_runtime::block_on(engine.shutdown());
            }
        });
}

#[cfg(test)]
mod headless;
