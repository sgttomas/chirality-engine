//! Chirality engine: a built-in agent for applications, over the Codex app-server.
//!
//! An application creates one [`Engine`], reads [`Engine::snapshot`], follows
//! [`Engine::subscribe`] and acts through the methods below. See `SPEC.md`.

pub mod api;

mod account;
mod events;
mod mapping;
mod projects;
mod protocol;
mod rpc;
mod session;
mod store;
mod threads;
mod transport;
mod turns;
mod workflows;


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

impl Engine {
    /// Starts Codex and reads the account, models, projects and threads.
    pub async fn start(config: EngineConfig) -> Result<Engine> {
        let launch = Launch { bin: config.codex_bin.clone(), home: config.codex_home.clone(), settings: config.codex_settings.clone(), env: None, cwd: None };
        Engine::start_with(config, Arc::new(ProcessConnector { launch }), None).await
    }

    pub(crate) async fn start_with(config: EngineConfig, connector: Arc<dyn transport::Connector>, tap: Option<rpc::Tap>) -> Result<Engine> {
        Ok(Engine { inner: Inner::start(config, connector, tap).await? })
    }

    #[cfg(test)]
    pub(crate) fn inner(&self) -> &Inner {
        &self.inner
    }

    /// Ends Codex and every turn in it.
    pub async fn shutdown(&self) {
        self.inner.shutdown().await
    }

    pub fn snapshot(&self) -> Snapshot {
        self.inner.store.snapshot()
    }

    /// Every change from now on, in order.
    pub fn subscribe(&self) -> broadcast::Receiver<Event> {
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

    /// Sends a message; starts a turn, or steers the running one.
    pub async fn send(&self, thread: &ThreadId, message: Message, settings: TurnSettings) -> Result<()> {
        self.inner.send(thread, message, settings).await
    }

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

    /// Files in a project matching `query`, for `@` references.
    pub async fn find_files(&self, project: &ProjectId, query: String) -> Result<Vec<PathBuf>> {
        self.inner.find_files(project, query).await
    }
}
