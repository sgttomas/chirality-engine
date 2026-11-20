//! The engine's running core: one Codex session at a time, started,
//! initialised, watched, restarted when it dies, and stopped.

use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;
use tokio::sync::mpsc;

use crate::api::*;
use crate::protocol as p;
use crate::rpc::{Incoming, RpcClient, RpcError, Tap};
use crate::store::Store;
use crate::transport::{Connector, Stopper};

const INITIALIZE_TIMEOUT: Duration = Duration::from_secs(30);
/// More deaths than this within [`DEATH_WINDOW`] and the engine stops restarting.
const MAX_DEATHS: usize = 3;
const DEATH_WINDOW: Duration = Duration::from_secs(60);

/// One connection to one Codex process.
pub(crate) struct Session {
    pub client: Arc<RpcClient>,
    pub generation: u64,
    stopper: Box<dyn Stopper>,
    /// Threads this process has started, resumed or forked.
    loaded: Mutex<HashSet<ThreadId>>,
}

impl Session {
    pub fn is_loaded(&self, thread: &ThreadId) -> bool {
        self.loaded.lock().unwrap().contains(thread)
    }

    pub fn mark_loaded(&self, thread: &ThreadId) {
        self.loaded.lock().unwrap().insert(thread.clone());
    }

    pub async fn call<R: DeserializeOwned>(&self, method: &str, params: impl Serialize) -> Result<R> {
        self.client.request(method, params).await.map_err(engine_error)
    }

    pub async fn call_value(&self, method: &str, params: impl Serialize) -> Result<Value> {
        self.client.request_value(method, params).await.map_err(engine_error)
    }
}

pub(crate) fn engine_error(error: RpcError) -> EngineError {
    match error {
        RpcError::Closed => EngineError::CodexUnavailable { message: "Codex stopped".into() },
        other => EngineError::Codex { message: other.to_string() },
    }
}

pub(crate) struct Inner {
    pub store: Store,
    pub config: EngineConfig,
    connector: Arc<dyn Connector>,
    tap: Option<Tap>,
    current: Mutex<Option<Arc<Session>>>,
    generation: AtomicU64,
    shutting_down: AtomicBool,
    /// The ChatGPT sign-in under way, to cancel it.
    pub login: Mutex<Option<String>>,
    /// The application's tool calls being answered.
    pub tool_calls: crate::tools::Calls,
}

impl Inner {
    pub async fn start(config: EngineConfig, connector: Arc<dyn Connector>, tap: Option<Tap>) -> Result<Arc<Inner>> {
        let projects = crate::projects::load(&config.state_dir)?;
        let inner = Arc::new(Inner {
            store: Store::new(),
            config,
            connector,
            tap,
            current: Mutex::new(None),
            generation: AtomicU64::new(0),
            shutting_down: AtomicBool::new(false),
            login: Mutex::new(None),
            tool_calls: Default::default(),
        });
        inner.store.set_projects(projects);
        let incoming = inner.open().await?;
        inner.read_account().await;
        inner.read_models().await;
        for project in inner.store.projects() {
            let _ = inner.list_threads(&project).await;
        }
        tokio::spawn(inner.clone().run(incoming));
        Ok(inner)
    }

    pub fn session(&self) -> Result<Arc<Session>> {
        self.current.lock().unwrap().clone().ok_or_else(|| EngineError::CodexUnavailable {
            message: match self.store.codex() {
                CodexStatus::Failed { message } => message,
                _ => "Codex is starting".into(),
            },
        })
    }

    /// Stops Codex and returns once no application tool call is still running.
    /// Under the same lock as a session's publication, so none starts after.
    pub async fn shutdown(&self) {
        let mut calls = {
            let mut current = self.current.lock().unwrap();
            self.shutting_down.store(true, Ordering::SeqCst);
            if let Some(session) = current.take() {
                session.client.close();
                session.stopper.stop();
            }
            self.tool_calls.close()
        };
        while calls.join_next().await.is_some() {}
    }

    /// Ends the Codex process as if it had died.
    #[cfg(test)]
    pub fn kill_codex(&self) {
        if let Some(session) = self.current.lock().unwrap().as_ref() {
            session.stopper.kill();
        }
    }

    async fn open(&self) -> Result<mpsc::UnboundedReceiver<Incoming>> {
        self.store.set_codex(CodexStatus::Starting);
        let failed = |message: String| {
            self.store.set_codex(CodexStatus::Failed { message: message.clone() });
            EngineError::CodexUnavailable { message }
        };
        let connection = self.connector.connect().await.map_err(failed)?;
        let (client, incoming, stopper) = RpcClient::start(connection, self.tap.clone());
        let params = p::InitializeParams {
            client_info: p::ClientInfo { name: "chirality", title: "Chirality", version: env!("CARGO_PKG_VERSION") },
            capabilities: p::Capabilities { experimental_api: true, request_attestation: false, opt_out_notification_methods: vec!["turn/diff/updated"] },
        };
        let response = tokio::time::timeout(INITIALIZE_TIMEOUT, client.request::<p::InitializeResponse>("initialize", params)).await;
        let response = match response {
            Ok(Ok(response)) => response,
            Ok(Err(error)) => {
                stopper.kill();
                return Err(failed(format!("Codex did not start: {error}")));
            }
            Err(_) => {
                stopper.kill();
                return Err(failed("Codex did not start in time".into()));
            }
        };
        client.notify("initialized", None).map_err(|e| failed(e.to_string()))?;
        let generation = self.generation.fetch_add(1, Ordering::SeqCst) + 1;
        {
            let mut current = self.current.lock().unwrap();
            if self.shutting_down.load(Ordering::SeqCst) {
                client.close();
                stopper.kill();
                return Err(EngineError::CodexUnavailable { message: "the engine has shut down".into() });
            }
            *current = Some(Arc::new(Session { client, generation, stopper, loaded: Mutex::default() }));
            self.tool_calls.open(generation);
        }
        self.store.set_codex(CodexStatus::Ready { version: version(&response.user_agent) });
        Ok(incoming)
    }

    async fn run(self: Arc<Self>, mut incoming: mpsc::UnboundedReceiver<Incoming>) {
        let mut deaths: Vec<Instant> = Vec::new();
        loop {
            let generation = self.generation.load(Ordering::SeqCst);
            while let Some(message) = incoming.recv().await {
                match message {
                    Incoming::Notification { method, params } => self.on_notification(&method, params).await,
                    Incoming::Request { id, method, params } => self.on_request(generation, id, &method, params),
                }
            }
            if self.shutting_down.load(Ordering::SeqCst) {
                return;
            }
            self.current.lock().unwrap().take();
            self.after_death(generation);
            deaths.retain(|at| at.elapsed() < DEATH_WINDOW);
            deaths.push(Instant::now());
            if deaths.len() > MAX_DEATHS {
                self.store.set_codex(CodexStatus::Failed { message: "Codex keeps stopping".into() });
                return;
            }
            match self.open().await {
                Ok(next) => incoming = next,
                Err(_) => return,
            }
        }
    }

    /// What a dead process leaves: its requests can no longer be answered and
    /// its running turns have ended.
    fn after_death(&self, generation: u64) {
        self.store.resolve(|p| p.generation == generation);
        drop(self.tool_calls.close());
        let message = "Codex stopped during this turn".to_string();
        for id in self.store.thread_ids() {
            let ended = self.store.update_thread(&id, |entry| {
                let turn = entry.active_turn.take()?;
                entry.last_ended = Some(turn.clone());
                entry.failed = Some(message.clone());
                Some(turn)
            });
            if let Some(Some(turn)) = ended {
                self.store.emit(Event::TurnEnded { thread: id, turn, state: TurnState::Failed { message: message.clone() } });
            }
        }
    }

    pub async fn read_account(&self) {
        let Ok(session) = self.session() else { return };
        match session.call::<p::AccountResponse>("account/read", serde_json::json!({})).await {
            Ok(response) => self.store.set_account(crate::mapping::account(response.account)),
            Err(_) => self.store.set_account(Account::Unknown),
        }
    }

    pub async fn read_models(&self) {
        let Ok(session) = self.session() else { return };
        let mut models = Vec::new();
        let mut cursor: Option<String> = None;
        loop {
            let params = serde_json::json!({ "cursor": cursor });
            let Ok(page) = session.call::<p::ModelListResponse>("model/list", params).await else { break };
            models.extend(page.data.iter().filter(|m| !m.hidden).map(crate::mapping::model));
            match page.next_cursor {
                Some(next) => cursor = Some(next),
                None => break,
            }
        }
        self.store.set_models(models);
    }
}

/// `chirality/0.171.0 (Mac OS ...)` gives `0.171.0`.
fn version(user_agent: &str) -> String {
    let after = user_agent.split_once('/').map(|(_, rest)| rest).unwrap_or(user_agent);
    after.split_whitespace().next().unwrap_or(after).to_string()
}

#[cfg(test)]
mod tests {
    #[test]
    fn version_comes_from_the_user_agent() {
        assert_eq!(super::version("chirality/0.171.0 (Mac OS 27.2.0; arm64) unknown (chirality; 0.1.0)"), "0.171.0");
    }
}
