//! Reading a thread's history from Codex, and Codex's answers about a thread
//! it does not have or has not kept yet.
//!
//! Codex keeps a new thread's history only once it has written the thread's
//! first message, which can be after it answered `turn/start`; until then a
//! read of the history is refused (-32600 or -32601). So a thread the engine
//! started in this Codex, and has not read yet, is read again for a while on
//! those answers to its first page. A thread Codex does not have at all it
//! names in an invalid-request message, without a code of its own: that is
//! told by Codex's exact words, which name the thread, as Codex itself tells
//! them (`thread_processor.rs`, `thread-store/src/error.rs`).

use std::time::Duration;

use serde::de::DeserializeOwned;
use serde_json::{Value, json};

use crate::api::*;
use crate::protocol as p;
use crate::rpc::RpcError;
use crate::session::{Inner, Session, engine_error};

const PAGE: u32 = 100;
const INVALID_REQUEST: i64 = -32600;
/// What Codex answers a paged read its store cannot serve.
const UNSUPPORTED: i64 = -32601;
/// How long a read waits, at most, for Codex to keep a new thread's history.
#[cfg(not(test))]
const WAITS: [u64; 6] = [50, 100, 200, 400, 800, 1600];
#[cfg(test)]
const WAITS: [u64; 6] = [10, 20, 40, 80, 160, 320];

/// Codex's answer that it does not have the thread.
fn missing(error: &RpcError, thread: &ThreadId) -> bool {
    let RpcError::Server { code: INVALID_REQUEST, message } = error else { return false };
    let id = &thread.0;
    [format!("thread not loaded: {id}"), format!("no rollout found for thread id {id}"), format!("thread not found: {id}")].contains(message)
}

/// How a history read failed: on its first page, or on a later one.
struct Failed {
    error: RpcError,
    first_page: bool,
}

impl Inner {
    /// A request about one thread; Codex's answer that it does not have the
    /// thread takes the thread out of the snapshot and is `NotFound`.
    pub(crate) async fn thread_call<R: DeserializeOwned>(&self, session: &Session, thread: &ThreadId, method: &str, params: Value) -> Result<R> {
        match session.client.request(method, params).await {
            Err(e) if missing(&e, thread) => Err(self.gone(session, thread)),
            other => other.map_err(engine_error),
        }
    }

    /// Takes a thread out of the snapshot and this Codex's loaded threads.
    pub(crate) fn forget_thread(&self, session: &Session, thread: &ThreadId) {
        self.store.remove_thread(thread);
        self.tool_calls.close_thread(&thread.0, false);
        session.forget_loaded(thread);
    }

    fn gone(&self, session: &Session, thread: &ThreadId) -> EngineError {
        self.forget_thread(session, thread);
        EngineError::NotFound { what: format!("thread {}", thread.0) }
    }

    /// The thread's history, numbered from before the first request: every
    /// event after that number applies to it, whichever attempt returned it.
    pub(crate) async fn read_thread(&self, thread: &ThreadId) -> Result<ThreadView> {
        let session = self.session()?;
        self.known_thread(thread)?;
        let seq = self.store.seq();
        let mut waits = WAITS.iter();
        loop {
            let read_at = self.store.renames();
            let read: p::ThreadResponse = self.thread_call(&session, thread, "thread/read", json!({ "threadId": thread.0 })).await?;
            self.adopt_thread(&read.thread, Some(read_at));
            let error = match turns(&session, thread).await {
                Ok(turns) => {
                    session.mark_read(thread);
                    let summary = self.store.summary(thread).ok_or_else(|| EngineError::NotFound { what: format!("thread {}", thread.0) })?;
                    return Ok(ThreadView { summary, turns, seq });
                }
                Err(Failed { error, .. }) if missing(&error, thread) => return Err(self.gone(&session, thread)),
                Err(Failed { error: RpcError::Server { code: INVALID_REQUEST | UNSUPPORTED, .. }, first_page: true }) if session.is_fresh(thread) => {
                    // Not kept yet: still the snapshot's thread, it is read again, or said to be not ready.
                    self.known_thread(thread)?;
                    match waits.next() {
                        Some(ms) => {
                            tokio::time::sleep(Duration::from_millis(*ms)).await;
                            self.known_thread(thread)?;
                            continue;
                        }
                        None => return Err(EngineError::NotReady { message: "Codex has not kept this thread's history yet.".into() }),
                    }
                }
                Err(Failed { error, .. }) => error,
            };
            return Err(match error {
                RpcError::Server { code: UNSUPPORTED, .. } => EngineError::Unsupported { message: "Codex can't read this thread's history.".into() },
                other => engine_error(other),
            });
        }
    }
}

async fn turns(session: &Session, thread: &ThreadId) -> std::result::Result<Vec<Turn>, Failed> {
    let mut turns = Vec::new();
    let mut cursor: Option<String> = None;
    loop {
        let params = json!({ "threadId": thread.0, "cursor": cursor, "limit": PAGE, "sortDirection": "asc", "itemsView": "full" });
        let page: p::TurnsListResponse = session.client.request("thread/turns/list", params).await.map_err(|error| Failed { error, first_page: cursor.is_none() })?;
        let empty = page.data.is_empty();
        turns.extend(page.data.iter().map(crate::mapping::turn));
        match page.next_cursor {
            Some(next) if !empty => cursor = Some(next),
            _ => return Ok(turns),
        }
    }
}
