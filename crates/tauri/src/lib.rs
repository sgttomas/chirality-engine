//! The engine's Tauri adapter: one command per [`Engine`] method, every engine
//! event forwarded to the webview as one numbered [`EVENT`], and the engine
//! shut down before the app exits. An application registers it on its builder:
//!
//! ```ignore
//! tauri::Builder::default()
//!     .plugin(chirality_engine_tauri::plugin(|app| Ok(my_config(app)?)))
//!     .invoke_handler(chirality_engine_tauri::commands![my_command])
//! ```
//!
//! The application's own tools go in the [`EngineConfig`] it gives
//! [`plugin`]; its own commands reach the engine through [`engine`].
//! The interface's client for these commands is `@chirality/engine-ui`.

mod commands;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

pub use chirality_engine;
use chirality_engine::{Engine, EngineConfig, EngineError, Result};
use tauri::ipc::Invoke;
use tauri::plugin::TauriPlugin;
use tauri::{AppHandle, Emitter, Manager, RunEvent, Runtime};
use tokio::sync::OnceCell;

/// The webview event every engine [`chirality_engine::Numbered`] arrives as.
/// A gap in the numbers means some were lost: the interface reads again.
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

/// At most this long between Quit and the app's exit, if shutting the engine down hangs.
const EXIT_TIMEOUT: Duration = Duration::from_secs(15);

type Exit = Arc<dyn Fn(i32) + Send + Sync>;

/// The engine, started once (when the app starts); every command waits for the same start.
struct Started {
    config: EngineConfig,
    engine: OnceCell<Result<Engine>>,
    /// Shutting down for an exit has begun; then, it has finished.
    exiting: AtomicBool,
    exited: AtomicBool,
    exit: Exit,
}

async fn started(s: &Started) -> Result<Engine> {
    s.engine.get_or_init(|| Engine::start(s.config.clone())).await.clone()
}

/// The app's engine, for its own commands.
pub async fn engine<R: Runtime>(app: &AppHandle<R>) -> Result<Engine> {
    match app.try_state::<Started>() {
        Some(s) => started(&s).await,
        None => Err(EngineError::Invalid { message: "the engine plugin is not registered".into() }),
    }
}

/// Shuts the engine down: waits for its start if it is still starting, then
/// for Codex to end and be reaped. The plugin does this before the app exits.
pub async fn shutdown<R: Runtime>(app: &AppHandle<R>) {
    if let Some(s) = app.try_state::<Started>()
        && let Ok(engine) = started(&s).await
    {
        engine.shutdown().await;
    }
}

/// Gives the app its engine, set up by `config` and started when the app
/// starts; forwards the engine's events, and shuts the engine down before the
/// app exits.
pub fn plugin<R, F>(config: F) -> TauriPlugin<R>
where
    R: Runtime,
    F: FnOnce(&AppHandle<R>) -> std::result::Result<EngineConfig, Box<dyn std::error::Error>> + Send + 'static,
{
    plugin_exiting_with(config, |app| Arc::new(move |code| app.exit(code)))
}

fn plugin_exiting_with<R, F>(config: F, exit: impl FnOnce(AppHandle<R>) -> Exit + Send + 'static) -> TauriPlugin<R>
where
    R: Runtime,
    F: FnOnce(&AppHandle<R>) -> std::result::Result<EngineConfig, Box<dyn std::error::Error>> + Send + 'static,
{
    tauri::plugin::Builder::new("chirality-engine")
        .setup(move |app, _api| {
            let config = config(app)?;
            let exit = exit(app.clone());
            app.manage(Started { config, engine: OnceCell::new(), exiting: AtomicBool::new(false), exited: AtomicBool::new(false), exit });
            tauri::async_runtime::spawn(forward(app.clone()));
            Ok(())
        })
        .on_event(|app, event| match event {
            RunEvent::ExitRequested { code, api, .. } => {
                if exit_requested(app, *code) {
                    api.prevent_exit();
                }
            }
            // An exit that did not ask first (none should): shut down here, bounded.
            RunEvent::Exit => {
                if let Some(s) = app.try_state::<Started>()
                    && !s.exited.load(Ordering::SeqCst)
                {
                    tauri::async_runtime::block_on(async { tokio::time::timeout(EXIT_TIMEOUT, shutdown(app)).await.ok() });
                }
            }
            _ => {}
        })
        .build()
}

/// The app is asked to exit. Until the engine is shut down the exit is held
/// (returns true) and the shutdown runs off Tauri's event thread, which keeps
/// handling events meanwhile; then the app exits with the code asked for.
fn exit_requested<R: Runtime>(app: &AppHandle<R>, code: Option<i32>) -> bool {
    let Some(s) = app.try_state::<Started>() else { return false };
    if s.exited.load(Ordering::SeqCst) {
        return false;
    }
    if !s.exiting.swap(true, Ordering::SeqCst) {
        let app = app.clone();
        tauri::async_runtime::spawn(async move {
            let _ = tokio::time::timeout(EXIT_TIMEOUT, shutdown(&app)).await;
            let s = app.state::<Started>();
            s.exited.store(true, Ordering::SeqCst);
            (s.exit)(code.unwrap_or(0));
        });
    }
    true
}

/// Forwards every engine event, numbered, in order, for as long as the app
/// runs. One the webview misses (a lagging receiver, an emit that failed)
/// leaves a gap in the numbers, which the interface reads again for.
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

/// An application command named like one of the engine's.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Shadowed(pub String);

impl std::fmt::Display for Shadowed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "the application's command `{}` has the name of an engine command; give it another name", self.0)
    }
}

impl std::error::Error for Shadowed {}

/// The engine's commands, then the application's own, named `app_commands`;
/// refused when one of those has an engine command's name. [`commands!`]
/// names them from the handler.
pub fn checked<R, F>(app_commands: &[&str], app: F) -> std::result::Result<impl Fn(Invoke<R>) -> bool + Send + Sync + 'static, Shadowed>
where
    R: Runtime,
    F: Fn(Invoke<R>) -> bool + Send + Sync + 'static,
{
    // `tauri::generate_handler!` names a command by its last path segment.
    let names = app_commands.iter().map(|path| path.rsplit("::").next().unwrap_or(path).trim());
    if let Some(name) = names.into_iter().find(|name| COMMANDS.contains(name)) {
        return Err(Shadowed(name.to_string()));
    }
    Ok(move |invoke: Invoke<R>| if COMMANDS.contains(&invoke.message.command()) { commands::handle(invoke) } else { app(invoke) })
}

/// The invoke handler: the engine's commands and the application's own
/// (`commands![]` for none). Panics, when the app is built, on an application
/// command named like an engine command.
#[macro_export]
macro_rules! commands {
    ($($command:path),* $(,)?) => {
        match $crate::checked(&[$(stringify!($command)),*], ::tauri::generate_handler![$($command),*]) {
            Ok(handler) => handler,
            Err(shadowed) => panic!("{shadowed}"),
        }
    };
}

#[cfg(test)]
mod tests;
