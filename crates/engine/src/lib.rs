//! Chirality engine: a built-in agent for applications, over the Codex app-server.
//!
//! An application creates one [`Engine`], reads [`Engine::snapshot`], follows
//! [`Engine::subscribe`] and acts through the methods below. See `SPEC.md`.

pub mod api;

mod account;
mod calls;
mod events;
mod mapping;
mod projects;
mod protocol;
mod rpc;
mod session;
mod store;
mod threads;
mod tools;
mod transport;
mod turns;
mod workflows;

#[cfg(test)]
mod testkit;

pub use api::*;

use std::path::PathBuf;
use std::sync::Arc;

use tokio::sync::broadcast;

use session::Inner;
use transport::{Launch, ProcessConnector};

/// The running engine. Cheap to clone; every clone is the same engine.
#[derive(Clone)]
pub struct Engine {
    inner: Arc<Inner>,
}

/// An engine still starting that no caller has yet: shut down if dropped so.
struct Unowned(Option<Engine>);

impl Drop for Unowned {
    fn drop(&mut self) {
        if let Some(engine) = self.0.take()
            && let Ok(runtime) = tokio::runtime::Handle::try_current()
        {
            runtime.spawn(async move { engine.shutdown().await });
        }
    }
}

impl Engine {
    /// Starts Codex and reads the account, models, projects and threads:
    /// [`Engine::new`], then [`Engine::connect`].
    pub async fn start(config: EngineConfig) -> Result<Engine> {
        Engine::new(config)?.connected().await
    }

    /// Connects, and hands the engine over once it has; an engine never handed
    /// over (its start failed, or the caller gave up on it) is shut down.
    async fn connected(self) -> Result<Engine> {
        let mut unowned = Unowned(Some(self.clone()));
        self.connect().await?;
        unowned.0 = None;
        Ok(self)
    }

    /// The engine with its projects, before Codex has started: it can be
    /// shut down from here on, which ends a Codex still starting.
    pub fn new(config: EngineConfig) -> Result<Engine> {
        if let Some(tools) = &config.tools {
            tools::validate(tools)?;
        }
        let launch = Launch { bin: config.codex_bin.clone(), home: config.codex_home.clone(), settings: transport::settings(&config), env: None, cwd: None };
        Ok(Engine { inner: Inner::new(config, Arc::new(ProcessConnector { launch }), None)? })
    }

    /// Starts Codex and reads the account, models and threads, once: every
    /// caller waits for the same start, and has its result.
    pub async fn connect(&self) -> Result<()> {
        self.inner.connect().await
    }

    #[cfg(test)]
    pub(crate) async fn start_with(config: EngineConfig, connector: Arc<dyn transport::Connector>, tap: Option<rpc::Tap>) -> Result<Engine> {
        Engine { inner: Inner::new(config, connector, tap)? }.connected().await
    }

    #[cfg(test)]
    pub(crate) fn inner(&self) -> &Inner {
        &self.inner
    }

    /// Ends Codex (one still starting too) and every turn in it, once; every
    /// caller returns when that is done.
    pub async fn shutdown(&self) {
        self.inner.shutdown().await
    }

    pub fn snapshot(&self) -> Snapshot {
        self.inner.store.snapshot()
    }

    /// Every change from now on, in order and numbered ([`Numbered`]); a
    /// receiver that lags has missed some and reads the snapshot again.
    pub fn subscribe(&self) -> broadcast::Receiver<Numbered> {
        self.inner.store.subscribe()
    }

    // Account

    /// Starts a ChatGPT sign-in; the account becomes `SigningIn { url }`.
    pub async fn sign_in(&self) -> Result<()> {
        self.inner.sign_in().await
    }

    pub async fn cancel_sign_in(&self) -> Result<()> {
        self.inner.cancel_sign_in().await
    }

    pub async fn sign_out(&self) -> Result<()> {
        self.inner.sign_out().await
    }

    // Projects

    pub async fn add_project(&self, path: PathBuf) -> Result<Project> {
        self.inner.add_project(path).await
    }

    pub async fn remove_project(&self, project: &ProjectId) -> Result<()> {
        self.inner.remove_project(project).await
    }

    // Workflows

    /// The workflows available in a project: the configured roots and its own.
    pub async fn workflows(&self, project: &ProjectId) -> Result<Vec<Workflow>> {
        self.inner.workflows(project)
    }

    // Threads

    pub async fn read_thread(&self, thread: &ThreadId) -> Result<ThreadView> {
        self.inner.read_thread(thread).await
    }

    /// Starts a thread in a project and sends its first message.
    pub async fn start_thread(&self, project: &ProjectId, message: Message, settings: TurnSettings) -> Result<ThreadId> {
        self.inner.start_thread(project, message, settings).await
    }

    /// Sends a message; starts a turn, or steers the running one. A subagent
    /// takes none: Codex takes its messages only from its parent's agent.
    pub async fn send(&self, thread: &ThreadId, message: Message, settings: TurnSettings) -> Result<()> {
        self.inner.send(thread, message, settings).await
    }

    /// Stops the thread's running turn; a subagent's too (its parent's agent is not told).
    pub async fn interrupt(&self, thread: &ThreadId) -> Result<()> {
        self.inner.interrupt(thread).await
    }

    /// A new thread holding this one's history up to and including `through`.
    pub async fn fork(&self, thread: &ThreadId, through: &TurnId) -> Result<ThreadId> {
        self.inner.fork(thread, through).await
    }

    /// Removes the turns from `from` onward from the conversation. Codex does
    /// not undo their file changes.
    pub async fn revert(&self, thread: &ThreadId, from: &TurnId) -> Result<()> {
        self.inner.revert(thread, from).await
    }

    pub async fn rename(&self, thread: &ThreadId, title: String) -> Result<()> {
        self.inner.rename(thread, title).await
    }

    /// Archives the thread; Codex archives its subagents with it.
    pub async fn archive(&self, thread: &ThreadId) -> Result<()> {
        self.inner.archive(thread).await
    }

    /// The person has looked at the thread.
    pub fn mark_read(&self, thread: &ThreadId) {
        self.inner.mark_read(thread)
    }

    // The person's part

    pub async fn answer(&self, request: &RequestId, answer: Answer) -> Result<()> {
        self.inner.answer(request, answer).await
    }

    /// A text file inside the project folder, for a preview; a path outside it is refused.
    pub async fn read_file(&self, project: &ProjectId, path: PathBuf) -> Result<String> {
        self.inner.read_file(project, path)
    }

    /// Files in a project matching `query`, for `@` references.
    pub async fn find_files(&self, project: &ProjectId, query: String) -> Result<Vec<PathBuf>> {
        self.inner.find_files(project, query).await
    }
}
