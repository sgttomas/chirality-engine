//! The engine's Tauri adapter: one command per [`Engine`] method, every engine
//! event forwarded to the webview as one [`EVENT`], and the engine shut down
//! when the app exits. An application registers it on its builder:
//!
//! ```ignore
//! tauri::Builder::default()
//!     .plugin(chirality_engine_tauri::plugin(|app| Ok(my_config(app)?)))
//!     .invoke_handler(chirality_engine_tauri::commands(tauri::generate_handler![my_command]))
//! ```
//!
//! The application's own tools go in the [`EngineConfig`] it gives
//! [`plugin`]; its own commands reach the engine through [`engine`].
//! The interface's client for these commands is `@chirality/engine-ui`.

use std::path::PathBuf;

pub use chirality_engine;
use chirality_engine::{
    Answer, Engine, EngineConfig, EngineError, Message, Project, ProjectId, RequestId, Result, Snapshot, ThreadId, ThreadView, TurnId,
    TurnSettings, Workflow,
};
use tauri::ipc::Invoke;
use tauri::plugin::TauriPlugin;
use tauri::{AppHandle, Emitter, Manager, RunEvent, Runtime, State};
use tokio::sync::OnceCell;

/// The webview event every engine [`chirality_engine::Event`] arrives as.
pub const EVENT: &str = "engine";

/// The engine's command names; an application's own commands use other names.
pub const COMMANDS: [&str; 19] = [
    "snapshot",
    "sign_in",
    "cancel_sign_in",
    "sign_out",
    "add_project",
    "remove_project",
    "workflows",
    "read_thread",
    "start_thread",
    "send",
    "interrupt",
    "fork",
    "revert",
    "rename",
    "archive",
    "mark_read",
    "answer",
    "read_file",
    "find_files",
];

/// The engine, started once; every command waits for the same start.
struct Started {
    config: EngineConfig,
    engine: OnceCell<Result<Engine>>,
}

async fn started(s: &Started) -> Result<Engine> {
    s.engine.get_or_init(|| Engine::start(s.config.clone())).await.clone()
}

/// The app's engine, for its own commands; started on first use.
pub async fn engine<R: Runtime>(app: &AppHandle<R>) -> Result<Engine> {
    match app.try_state::<Started>() {
        Some(s) => started(&s).await,
        None => Err(EngineError::Invalid { message: "the engine plugin is not registered".into() }),
    }
}

/// Gives the app its engine, set up by `config` when the app starts: forwards
/// the engine's events and shuts it down on exit.
pub fn plugin<R, F>(config: F) -> TauriPlugin<R>
where
    R: Runtime,
    F: FnOnce(&AppHandle<R>) -> std::result::Result<EngineConfig, Box<dyn std::error::Error>> + Send + 'static,
{
    tauri::plugin::Builder::new("chirality-engine")
        .setup(move |app, _api| {
            let config = config(app)?;
            app.manage(Started { config, engine: OnceCell::new() });
            tauri::async_runtime::spawn(forward(app.clone()));
            Ok(())
        })
        .on_event(|app, event| {
            if let RunEvent::Exit = event
                && let Some(s) = app.try_state::<Started>()
                && let Some(Ok(engine)) = s.engine.get()
            {
                tauri::async_runtime::block_on(engine.shutdown());
            }
        })
        .build()
}

/// The engine's commands, then the application's own (`|_| false` when it has none).
pub fn commands<R, F>(app: F) -> impl Fn(Invoke<R>) -> bool + Send + Sync + 'static
where
    R: Runtime,
    F: Fn(Invoke<R>) -> bool + Send + Sync + 'static,
{
    move |invoke: Invoke<R>| if COMMANDS.contains(&invoke.message.command()) { handle(invoke) } else { app(invoke) }
}

fn handle<R: Runtime>(invoke: Invoke<R>) -> bool {
    let handler: fn(Invoke<R>) -> bool = tauri::generate_handler![
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
    ];
    handler(invoke)
}

/// Forwards every engine event, in order, for as long as the app runs.
async fn forward<R: Runtime>(app: AppHandle<R>) {
    let Ok(engine) = engine(&app).await else { return };
    let mut events = engine.subscribe();
    loop {
        match events.recv().await {
            Ok(event) => {
                let _ = app.emit(EVENT, event);
            }
            Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
            Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
        }
    }
}

type S<'a> = State<'a, Started>;

#[tauri::command]
async fn snapshot(s: S<'_>) -> Result<Snapshot> {
    Ok(started(&s).await?.snapshot())
}
#[tauri::command]
async fn sign_in(s: S<'_>) -> Result<()> {
    started(&s).await?.sign_in().await
}
#[tauri::command]
async fn cancel_sign_in(s: S<'_>) -> Result<()> {
    started(&s).await?.cancel_sign_in().await
}
#[tauri::command]
async fn sign_out(s: S<'_>) -> Result<()> {
    started(&s).await?.sign_out().await
}
#[tauri::command]
async fn add_project(s: S<'_>, path: PathBuf) -> Result<Project> {
    started(&s).await?.add_project(path).await
}
#[tauri::command]
async fn remove_project(s: S<'_>, project: ProjectId) -> Result<()> {
    started(&s).await?.remove_project(&project).await
}
#[tauri::command]
async fn workflows(s: S<'_>, project: ProjectId) -> Result<Vec<Workflow>> {
    started(&s).await?.workflows(&project).await
}
#[tauri::command]
async fn read_thread(s: S<'_>, thread: ThreadId) -> Result<ThreadView> {
    started(&s).await?.read_thread(&thread).await
}
#[tauri::command]
async fn start_thread(s: S<'_>, project: ProjectId, message: Message, settings: TurnSettings) -> Result<ThreadId> {
    started(&s).await?.start_thread(&project, message, settings).await
}
#[tauri::command]
async fn send(s: S<'_>, thread: ThreadId, message: Message, settings: TurnSettings) -> Result<()> {
    started(&s).await?.send(&thread, message, settings).await
}
#[tauri::command]
async fn interrupt(s: S<'_>, thread: ThreadId) -> Result<()> {
    started(&s).await?.interrupt(&thread).await
}
#[tauri::command]
async fn fork(s: S<'_>, thread: ThreadId, through: TurnId) -> Result<ThreadId> {
    started(&s).await?.fork(&thread, &through).await
}
#[tauri::command]
async fn revert(s: S<'_>, thread: ThreadId, from: TurnId) -> Result<()> {
    started(&s).await?.revert(&thread, &from).await
}
#[tauri::command]
async fn rename(s: S<'_>, thread: ThreadId, title: String) -> Result<()> {
    started(&s).await?.rename(&thread, title).await
}
#[tauri::command]
async fn archive(s: S<'_>, thread: ThreadId) -> Result<()> {
    started(&s).await?.archive(&thread).await
}
#[tauri::command]
async fn mark_read(s: S<'_>, thread: ThreadId) -> Result<()> {
    started(&s).await?.mark_read(&thread);
    Ok(())
}
#[tauri::command]
async fn answer(s: S<'_>, request: RequestId, answer: Answer) -> Result<()> {
    started(&s).await?.answer(&request, answer).await
}
#[tauri::command]
async fn read_file(s: S<'_>, project: ProjectId, path: PathBuf) -> Result<String> {
    started(&s).await?.read_file(&project, path).await
}
#[tauri::command]
async fn find_files(s: S<'_>, project: ProjectId, query: String) -> Result<Vec<PathBuf>> {
    started(&s).await?.find_files(&project, query).await
}

#[cfg(test)]
mod tests {
    /// The interface's client names exactly these commands.
    #[test]
    fn the_interface_client_names_the_same_commands() {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../packages/ui/src/engine/commands.ts");
        let text = std::fs::read_to_string(path).unwrap();
        let list = text.split_once('[').and_then(|(_, rest)| rest.split_once(']')).map(|(list, _)| list).unwrap();
        let names: Vec<&str> = list.split(',').map(|n| n.trim().trim_matches('"')).filter(|n| !n.is_empty()).collect();
        assert_eq!(names, super::COMMANDS);
    }
}
