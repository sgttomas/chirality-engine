//! The line channel to one Codex app-server process: start, stop, and the
//! signal that it has ended.
//!
//! A [`Connector`] makes a [`Connection`] each time the engine (re)starts
//! Codex. The production connector runs `codex app-server`; tests use a replay
//! connector that plays a recorded transcript through the same interface.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::{mpsc, oneshot};

/// One live connection: lines out, lines in. `incoming` ends when Codex ends.
pub(crate) struct Connection {
    pub outgoing: mpsc::UnboundedSender<String>,
    pub incoming: mpsc::UnboundedReceiver<String>,
    pub stopper: Box<dyn Stopper>,
}

/// Ends a connection's process.
pub(crate) trait Stopper: Send + Sync {
    /// Asks the process to end, and ends it if it does not.
    fn stop(&self);
    /// Ends the process at once, as if it had died.
    fn kill(&self);
}

pub(crate) type ConnectFuture = std::pin::Pin<Box<dyn Future<Output = Result<Connection, String>> + Send>>;

/// Makes a fresh connection to Codex.
pub(crate) trait Connector: Send + Sync {
    fn connect(&self) -> ConnectFuture;
}

/// How the engine launches `codex app-server`.
#[derive(Clone, Debug)]
pub(crate) struct Launch {
    pub bin: PathBuf,
    pub home: Option<PathBuf>,
    /// `-c key=value` settings, in order.
    pub settings: Vec<String>,
    /// Replaces the inherited environment when set (tests run with a closed one).
    pub env: Option<Vec<(String, String)>>,
    pub cwd: Option<PathBuf>,
}

/// The settings every engine session needs from Codex.
pub(crate) const SESSION_SETTINGS: &[&str] = &["features.default_mode_request_user_input=true"];

impl Launch {
    pub fn args(&self) -> Vec<String> {
        let mut args = Vec::new();
        for setting in SESSION_SETTINGS.iter().map(|s| s.to_string()).chain(self.settings.iter().cloned()) {
            args.push("-c".into());
            args.push(setting);
        }
        args.push("app-server".into());
        args
    }

    /// `PATH` with the package's `codex-path` folder (which holds `rg`) first.
    fn path_var(&self, inherited: Option<&str>) -> Option<String> {
        let tools = package_tools(&self.bin)?;
        let rest = inherited.unwrap_or("/usr/bin:/bin:/usr/sbin:/sbin");
        Some(format!("{}:{rest}", tools.display()))
    }
}

/// `<package>/codex-path` for a binary at `<package>/bin/codex`, if present.
fn package_tools(bin: &Path) -> Option<PathBuf> {
    let package = bin.canonicalize().ok()?.parent()?.parent()?.to_path_buf();
    let tools = package.join("codex-path");
    tools.is_dir().then_some(tools)
}

pub(crate) struct ProcessConnector {
    pub launch: Launch,
}

impl Connector for ProcessConnector {
    fn connect(&self) -> ConnectFuture {
        let launch = self.launch.clone();
        Box::pin(async move { spawn(&launch) })
    }
}

fn spawn(launch: &Launch) -> Result<Connection, String> {
    let mut command = Command::new(&launch.bin);
    command.args(launch.args());
    match &launch.env {
        Some(env) => {
            command.env_clear();
            for (key, value) in env {
                command.env(key, value);
            }
            let path = env.iter().find(|(k, _)| k == "PATH").map(|(_, v)| v.as_str());
            if let Some(path) = launch.path_var(path) {
                command.env("PATH", path);
            }
        }
        None => {
            if let Some(path) = launch.path_var(std::env::var("PATH").ok().as_deref()) {
                command.env("PATH", path);
            }
        }
    }
    if let Some(home) = &launch.home {
        command.env("CODEX_HOME", home);
    }
    if let Some(cwd) = &launch.cwd {
        command.current_dir(cwd);
    }
    command.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).kill_on_drop(true);
    let mut child = command.spawn().map_err(|e| format!("cannot start {}: {e}", launch.bin.display()))?;
    let stdin = child.stdin.take().expect("piped stdin");
    let stdout = child.stdout.take().expect("piped stdout");
    let stderr = child.stderr.take().expect("piped stderr");

    let (out_tx, mut out_rx) = mpsc::unbounded_channel::<String>();
    let (in_tx, in_rx) = mpsc::unbounded_channel::<String>();
    let (stop_tx, stop_rx) = oneshot::channel::<bool>();

    // Writer: lines to stdin; dropping the sender closes stdin, which ends Codex.
    tokio::spawn(async move {
        let mut stdin = stdin;
        while let Some(line) = out_rx.recv().await {
            let mut bytes = line.into_bytes();
            bytes.push(b'\n');
            if stdin.write_all(&bytes).await.is_err() || stdin.flush().await.is_err() {
                break;
            }
        }
    });
    // Reader: lines from stdout until it ends.
    tokio::spawn(async move {
        let mut lines = BufReader::new(stdout).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            let line = line.trim_end_matches('\r');
            if !line.is_empty() && in_tx.send(line.to_string()).is_err() {
                break;
            }
        }
    });
    // Stderr is drained so Codex never blocks on it; its tail is not kept.
    tokio::spawn(async move {
        let mut lines = BufReader::new(stderr).lines();
        while let Ok(Some(_)) = lines.next_line().await {}
    });
    tokio::spawn(supervise(child, stop_rx));
    Ok(Connection { outgoing: out_tx, incoming: in_rx, stopper: Box::new(ProcessStopper(Arc::new(std::sync::Mutex::new(Some(stop_tx))))) })
}

/// Waits for the child; on a stop request, gives it two seconds after its
/// input closes, then kills it.
async fn supervise(mut child: Child, stop: oneshot::Receiver<bool>) {
    tokio::select! {
        _ = child.wait() => {}
        request = stop => {
            let gentle = request.unwrap_or(true);
            if gentle && tokio::time::timeout(Duration::from_secs(2), child.wait()).await.is_ok() {
                return;
            }
            let _ = child.start_kill();
            let _ = child.wait().await;
        }
    }
}

struct ProcessStopper(Arc<std::sync::Mutex<Option<oneshot::Sender<bool>>>>);

impl Stopper for ProcessStopper {
    fn stop(&self) {
        if let Some(tx) = self.0.lock().unwrap().take() {
            let _ = tx.send(true);
        }
    }
    fn kill(&self) {
        if let Some(tx) = self.0.lock().unwrap().take() {
            let _ = tx.send(false);
        }
    }
}
