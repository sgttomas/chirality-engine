//! The engine's Tauri adapter: one command per [`Engine`] method, named
//! `engine:<method>`; every engine event forwarded to the webview, numbered,
//! as one [`EVENT`]; and the engine shut down before the app exits. An
//! application registers it on its builder:
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

mod commands;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

pub use chirality_engine;
use chirality_engine::{Engine, EngineConfig, EngineError, Result};
use serde::Serialize;
use tauri::ipc::Invoke;
use tauri::plugin::TauriPlugin;
use tauri::{AppHandle, Emitter, Manager, RunEvent, Runtime};
use tokio::sync::broadcast::error::RecvError;

/// The webview event every engine [`chirality_engine::Numbered`] arrives as,
/// and after each burst of them a [`Mark`]. A gap in the numbers, or a mark
/// past the last number received, means some were lost: the interface reads again.
pub const EVENT: &str = "engine";

/// What the engine's command names begin with; an application's own commands do not.
pub const PREFIX: &str = "engine:";

/// The engine's commands, each named [`PREFIX`] and its name here.
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

/// Sent on [`EVENT`] at most [`MARK_EVERY`] after events: the number of the
/// last event sent to the webview, so it finds a missed last event too.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Mark {
    pub last: u64,
}

const MARK_EVERY: Duration = Duration::from_secs(1);

/// At most this long between Quit and the app's exit, if shutting the engine down hangs.
const EXIT_TIMEOUT: Duration = Duration::from_secs(15);

type Exit = Arc<dyn Fn(i32) + Send + Sync>;

/// The engine, made when the app starts (Codex starting with it); every
/// command waits for that start, and shutdown ends it, finished or not.
struct Started {
    engine: Result<Engine>,
    /// Shutting down for an exit has begun; then, it has finished.
    exiting: AtomicBool,
    exited: AtomicBool,
    exit: Exit,
}

async fn started(s: &Started) -> Result<Engine> {
    let engine = s.engine.clone()?;
    engine.connect().await?;
    Ok(engine)
}

/// The app's engine, once Codex has started, for its own commands.
pub async fn engine<R: Runtime>(app: &AppHandle<R>) -> Result<Engine> {
    match app.try_state::<Started>() {
        Some(s) => started(&s).await,
        None => Err(EngineError::Invalid { message: "the engine plugin is not registered".into() }),
    }
}

/// Shuts the engine down, ending Codex (one still starting too), and returns
/// once Codex is reaped. The plugin does this before the app exits.
pub async fn shutdown<R: Runtime>(app: &AppHandle<R>) {
    if let Some(s) = app.try_state::<Started>()
        && let Ok(engine) = &s.engine
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
            let engine = Engine::new(config);
            if let Ok(engine) = &engine {
                tauri::async_runtime::spawn(forward(app.clone(), engine.clone()));
                let engine = engine.clone();
                tauri::async_runtime::spawn(async move { engine.connect().await });
            }
            app.manage(Started { engine, exiting: AtomicBool::new(false), exited: AtomicBool::new(false), exit });
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
/// runs; after each burst, a [`Mark`] with the last number sent. One the
/// webview misses (a lagging receiver, an emit that failed) shows as a gap,
/// or as a mark past what it received, and it reads again.
async fn forward<R: Runtime>(app: AppHandle<R>, engine: Engine) {
    let mut events = engine.subscribe();
    let (mut last, mut marked) = (0, 0);
    let mut every = tokio::time::interval(MARK_EVERY);
    every.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            biased;
            received = events.recv() => match received {
                Ok(event) => {
                    last = event.seq;
                    let _ = app.emit(EVENT, event);
                }
                Err(RecvError::Lagged(missed)) => last += missed,
                Err(RecvError::Closed) => return,
            },
            _ = every.tick() => {
                if last != marked && app.emit(EVENT, Mark { last }).is_ok() {
                    marked = last;
                }
            }
        }
    }
}

/// The invoke handler: the engine's commands (named [`PREFIX`]…), and every
/// other command to the application's own `handler` (`|_| false` for none).
pub fn commands<R, F>(handler: F) -> impl Fn(Invoke<R>) -> bool + Send + Sync + 'static
where
    R: Runtime,
    F: Fn(Invoke<R>) -> bool + Send + Sync + 'static,
{
    move |invoke: Invoke<R>| if invoke.message.command().starts_with(PREFIX) { commands::handle(invoke) } else { handler(invoke) }
}

#[cfg(test)]
mod tests;
