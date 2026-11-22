//! The engine's running core: one Codex session at a time, started,
//! initialised, watched, restarted when it dies, and stopped.

use std::collections::HashMap;
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
/// How long shutdown waits for Codex to be reaped: its two seconds to end, then the kill.
const REAP_TIMEOUT: Duration = Duration::from_secs(5);
/// More deaths than this within [`DEATH_WINDOW`] and the engine stops restarting.
const MAX_DEATHS: usize = 3;
const DEATH_WINDOW: Duration = Duration::from_secs(60);
/// The engine's status, and what it says to every request, once it has shut down.
const SHUT_DOWN: &str = "the engine has shut down";

/// Work the engine does once, as its own task; every caller waits for the same result.
pub(crate) struct Once<T>(Mutex<Option<tokio::sync::watch::Receiver<Option<T>>>>);

impl<T> Default for Once<T> {
    fn default() -> Self {
        Once(Mutex::new(None))
    }
}

impl<T: Clone + Send + Sync + 'static> Once<T> {
    /// The result of `work`, begun by the first caller; `None` if its task ended without one.
    pub async fn run<F: Future<Output = T> + Send + 'static>(&self, work: impl FnOnce() -> F) -> Option<T> {
        let mut done = {
            let mut slot = self.0.lock().unwrap();
            match &*slot {
                Some(done) => done.clone(),
                None => {
                    let (tx, done) = tokio::sync::watch::channel(None);
                    let work = work();
                    tokio::spawn(async move {
                        let _ = tx.send(Some(work.await));
                    });
                    *slot = Some(done.clone());
                    done
                }
            }
        };
        done.wait_for(Option::is_some).await.ok().and_then(|v| v.clone())
    }
}

/// One connection to one Codex process.
pub(crate) struct Session {
    pub client: Arc<RpcClient>,
    pub generation: u64,
    stopper: Arc<dyn Stopper>,
    /// Threads this process has started, resumed or forked, with the access
    /// Codex reported when it resumed or forked one.
    loaded: Mutex<HashMap<ThreadId, Option<Access>>>,
    /// Threads the engine started in this process and has not yet read, with
    /// the number of the last event before each started: Codex may not have
    /// kept their first message.
    unkept: Mutex<HashMap<ThreadId, u64>>,
}

impl Session {
    /// `None` if the thread is not loaded, else the access Codex loaded it with, if it said.
    pub fn loaded_access(&self, thread: &ThreadId) -> Option<Option<Access>> {
        self.loaded.lock().unwrap().get(thread).copied()
    }

    pub fn mark_loaded(&self, thread: &ThreadId, access: Option<Access>) {
        self.loaded.lock().unwrap().insert(thread.clone(), access);
    }

    /// Codex has unloaded the thread; the next turn resumes it.
    pub fn forget_loaded(&self, thread: &ThreadId) {
        self.loaded.lock().unwrap().remove(thread);
        self.unkept.lock().unwrap().remove(thread);
    }

    /// The engine has started the thread, after event number `seq`.
    pub fn mark_unkept(&self, thread: &ThreadId, seq: u64) {
        self.unkept.lock().unwrap().insert(thread.clone(), seq);
    }

    /// The number before the thread started, while it has not been read.
    pub fn unkept_since(&self, thread: &ThreadId) -> Option<u64> {
        self.unkept.lock().unwrap().get(thread).copied()
    }

    pub fn mark_kept(&self, thread: &ThreadId) {
        self.unkept.lock().unwrap().remove(thread);
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
    /// The Codex being started, before it answers `initialize`: shutdown ends it.
    opening: Mutex<Option<Arc<dyn Stopper>>>,
    connected: Once<Result<()>>,
    shut: Once<()>,
    generation: AtomicU64,
    shutting_down: AtomicBool,
    /// Connection attempts under way, which shutdown waits for.
    attempts: tokio::sync::watch::Sender<usize>,
    /// The ChatGPT sign-in under way, to cancel it.
    pub login: Mutex<Option<String>>,
    /// The application's tool calls being answered.
    pub tool_calls: crate::calls::Calls,
    pub admission: crate::turns::Admission,
    /// Messages between threads waiting to be delivered, and assignments closed.
    pub assignments: crate::assignment::Assignments,
}

impl Inner {
    /// The engine with its projects, before Codex: [`Inner::connect`] starts it.
    pub fn new(config: EngineConfig, connector: Arc<dyn Connector>, tap: Option<Tap>) -> Result<Arc<Inner>> {
        let projects = crate::projects::load(&config.state_dir)?;
        let inner = Arc::new(Inner {
            store: Store::new(),
            config,
            connector,
            tap,
            current: Mutex::new(None),
            opening: Mutex::new(None),
            connected: Once::default(),
            shut: Once::default(),
            generation: AtomicU64::new(0),
            shutting_down: AtomicBool::new(false),
            attempts: tokio::sync::watch::channel(0).0,
            login: Mutex::new(None),
            tool_calls: Default::default(),
            admission: Default::default(),
            assignments: Default::default(),
        });
        inner.store.set_projects(projects);
        Ok(inner)
    }

    /// Starts Codex and reads the account, models and each project's threads,
    /// once: every caller waits for the same start.
    pub async fn connect(self: &Arc<Self>) -> Result<()> {
        let inner = self.clone();
        let started = self.connected.run(|| async move {
            let incoming = inner.open().await?;
            inner.read_account().await;
            inner.read_models().await;
            for project in inner.store.projects() {
                let _ = inner.list_threads(&project).await;
            }
            if inner.shutting_down.load(Ordering::SeqCst) {
                // Shut down while it read: not started.
                return Err(EngineError::CodexUnavailable { message: SHUT_DOWN.into() });
            }
            tokio::spawn(inner.clone().run(incoming));
            Ok(())
        });
        started.await.unwrap_or_else(|| Err(EngineError::CodexUnavailable { message: "the engine's start ended".into() }))
    }

    pub fn session(&self) -> Result<Arc<Session>> {
        self.current.lock().unwrap().clone().ok_or_else(|| EngineError::CodexUnavailable {
            message: match self.store.codex() {
                CodexStatus::Failed { message } => message,
                _ => "Codex is starting".into(),
            },
        })
    }

    /// Stops Codex, once; every caller waits for the same shutdown. It ends
    /// the running Codex and one still starting (each has two seconds, then is
    /// killed), and returns once no connection attempt is under way, no
    /// application tool call of any session is still running, and each Codex
    /// has been reaped (waiting at most [`REAP_TIMEOUT`] for that). Under the
    /// same lock as a session's publication and an attempt's start.
    pub async fn shutdown(self: &Arc<Self>) {
        let inner = self.clone();
        self.shut.run(|| async move { inner.shut_down().await }).await;
    }

    async fn shut_down(&self) {
        let reaped = {
            let mut current = self.current.lock().unwrap();
            self.shutting_down.store(true, Ordering::SeqCst);
            self.store.set_codex(CodexStatus::Failed { message: SHUT_DOWN.into() });
            let mut reaped = Vec::new();
            if let Some(session) = current.take() {
                session.client.close();
                session.stopper.stop();
                reaped.push(session.stopper.reaped());
            }
            if let Some(opening) = self.opening.lock().unwrap().take() {
                opening.stop();
                reaped.push(opening.reaped());
            }
            self.tool_calls.close();
            reaped
        };
        let _ = self.attempts.subscribe().wait_for(|n| *n == 0).await;
        self.tool_calls.idle().await;
        for reaped in reaped {
            let _ = tokio::time::timeout(REAP_TIMEOUT, reaped).await;
        }
    }

    /// A connection attempt, counted until it ends; none starts after shutdown.
    fn begin_attempt(&self) -> Result<Attempt<'_>> {
        let _current = self.current.lock().unwrap();
        if self.shutting_down.load(Ordering::SeqCst) {
            return Err(EngineError::CodexUnavailable { message: SHUT_DOWN.into() });
        }
        self.attempts.send_modify(|n| *n += 1);
        Ok(Attempt(&self.attempts))
    }

    pub fn is_shut_down(&self) -> bool {
        self.shutting_down.load(Ordering::SeqCst)
    }

    /// Sets Codex's status unless the engine has shut down, whose status is the last.
    fn set_status(&self, status: CodexStatus) {
        let _current = self.current.lock().unwrap();
        if !self.shutting_down.load(Ordering::SeqCst) {
            self.store.set_codex(status);
        }
    }

    /// Ends the Codex process as if it had died.
    #[cfg(test)]
    pub fn kill_codex(&self) {
        if let Some(session) = self.current.lock().unwrap().as_ref() {
            session.stopper.kill();
        }
    }

    async fn open(&self) -> Result<mpsc::UnboundedReceiver<Incoming>> {
        let _attempt = self.begin_attempt()?;
        self.set_status(CodexStatus::Starting);
        let failed = |message: String| {
            self.set_status(CodexStatus::Failed { message: message.clone() });
            EngineError::CodexUnavailable { message }
        };
        let connection = self.connector.connect().await.map_err(failed)?;
        let generation = self.generation.fetch_add(1, Ordering::SeqCst) + 1;
        // Requests the connection delivered before ending are taken from no more.
        let calls = self.tool_calls.clone();
        let on_end: Box<dyn FnOnce() + Send> = Box::new(move || calls.close_session(generation));
        let (client, incoming, stopper) = RpcClient::start(connection, self.tap.clone(), Some(on_end));
        let stopper: Arc<dyn Stopper> = Arc::from(stopper);
        let refused = {
            let _current = self.current.lock().unwrap();
            let refused = self.shutting_down.load(Ordering::SeqCst);
            if !refused {
                *self.opening.lock().unwrap() = Some(stopper.clone());
            }
            refused
        };
        if refused {
            // Shutdown began as this Codex was spawned; it waits for this attempt, so for the reaping too.
            client.close();
            stopper.kill();
            let _ = tokio::time::timeout(REAP_TIMEOUT, stopper.reaped()).await;
            return Err(EngineError::CodexUnavailable { message: SHUT_DOWN.into() });
        }
        let _opening = Opening(&self.opening);
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
        let refused = {
            let mut current = self.current.lock().unwrap();
            if self.shutting_down.load(Ordering::SeqCst) {
                client.close();
                stopper.kill();
                Some(stopper.reaped())
            } else {
                *current = Some(Arc::new(Session { client, generation, stopper, loaded: Mutex::default(), unkept: Mutex::default() }));
                self.tool_calls.open(generation);
                self.store.set_codex(CodexStatus::Ready { version: version(&response.user_agent) });
                None
            }
        };
        if let Some(reaped) = refused {
            // Shutdown waits for this attempt, so for this Codex to be reaped too.
            let _ = tokio::time::timeout(REAP_TIMEOUT, reaped).await;
            return Err(EngineError::CodexUnavailable { message: SHUT_DOWN.into() });
        }
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
                self.set_status(CodexStatus::Failed { message: "Codex keeps stopping".into() });
                return;
            }
            match self.open().await {
                Ok(next) => incoming = next,
                Err(_) => return,
            }
            // What Codex did while the engine could not hear it (a subagent it started, a
            // thread it ended) is in its listing, as at the start.
            for project in self.store.projects() {
                let _ = self.list_threads(&project).await;
            }
        }
    }

    /// What a dead process leaves: its requests can no longer be answered and
    /// its running turns have ended.
    fn after_death(&self, generation: u64) {
        self.store.resolve(|p| p.generation == generation);
        self.tool_calls.close();
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

/// Clears the Codex being started when its start ends, however it ends.
struct Opening<'a>(&'a Mutex<Option<Arc<dyn Stopper>>>);

impl Drop for Opening<'_> {
    fn drop(&mut self) {
        self.0.lock().unwrap().take();
    }
}

struct Attempt<'a>(&'a tokio::sync::watch::Sender<usize>);

impl Drop for Attempt<'_> {
    fn drop(&mut self) {
        self.0.send_modify(|n| *n -= 1);
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
