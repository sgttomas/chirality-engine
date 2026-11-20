//! Chirality engine: a built-in agent for applications, over the Codex app-server.
//!
//! An application creates one [`Engine`], reads [`Engine::snapshot`], follows
//! [`Engine::subscribe`] and acts through the methods below. See `SPEC.md`.

pub mod api;

pub use api::*;

use std::path::PathBuf;

use tokio::sync::broadcast;

/// The running engine. Cheap to clone; every clone is the same engine.
#[derive(Clone)]
pub struct Engine {
    _private: (),
}

#[allow(unused_variables)]
impl Engine {
    /// Starts Codex and reads the account, models, projects and threads.
    pub async fn start(config: EngineConfig) -> Result<Engine> {
        todo!()
    }

    /// Ends Codex and every turn in it.
    pub async fn shutdown(&self) {
        todo!()
    }

    pub fn snapshot(&self) -> Snapshot {
        todo!()
    }

    /// Every change from now on, in order.
    pub fn subscribe(&self) -> broadcast::Receiver<Event> {
        todo!()
    }

    // Account

    /// Starts a ChatGPT sign-in; the account becomes `SigningIn { url }`.
    pub async fn sign_in(&self) -> Result<()> {
        todo!()
    }

    pub async fn cancel_sign_in(&self) -> Result<()> {
        todo!()
    }

    pub async fn sign_out(&self) -> Result<()> {
        todo!()
    }

    // Projects

    pub async fn add_project(&self, path: PathBuf) -> Result<Project> {
        todo!()
    }

    pub async fn remove_project(&self, project: &ProjectId) -> Result<()> {
        todo!()
    }

    // Workflows

    /// The workflows available in a project: the configured roots and its own.
    pub async fn workflows(&self, project: &ProjectId) -> Result<Vec<Workflow>> {
        todo!()
    }

    // Threads

    pub async fn read_thread(&self, thread: &ThreadId) -> Result<ThreadView> {
        todo!()
    }

    /// Starts a thread in a project and sends its first message.
    pub async fn start_thread(&self, project: &ProjectId, message: Message, settings: TurnSettings) -> Result<ThreadId> {
        todo!()
    }

    /// Sends a message; starts a turn, or steers the running one.
    pub async fn send(&self, thread: &ThreadId, message: Message, settings: TurnSettings) -> Result<()> {
        todo!()
    }

    pub async fn interrupt(&self, thread: &ThreadId) -> Result<()> {
        todo!()
    }

    /// A new thread holding this one's history up to and including `through`.
    pub async fn fork(&self, thread: &ThreadId, through: &TurnId) -> Result<ThreadId> {
        todo!()
    }

    /// Removes the turns from `from` onward, undoing their file changes where Codex can.
    pub async fn revert(&self, thread: &ThreadId, from: &TurnId) -> Result<()> {
        todo!()
    }

    pub async fn rename(&self, thread: &ThreadId, title: String) -> Result<()> {
        todo!()
    }

    pub async fn archive(&self, thread: &ThreadId) -> Result<()> {
        todo!()
    }

    /// The person has looked at the thread.
    pub fn mark_read(&self, thread: &ThreadId) {
        todo!()
    }

    // The person's part

    pub async fn answer(&self, request: &RequestId, answer: Answer) -> Result<()> {
        todo!()
    }

    /// Files in a project matching `query`, for `@` references.
    pub async fn find_files(&self, project: &ProjectId, query: String) -> Result<Vec<PathBuf>> {
        todo!()
    }
}
