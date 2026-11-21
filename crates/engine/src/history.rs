//! A thread's turns as Codex keeps them: a page at a time for a paginated
//! thread, whole for a legacy one. Codex keeps a new thread's history only once
//! it has written the thread's first message, which can be after it answered
//! `turn/start`; until then it refuses to read the history.

use std::time::Duration;

use serde_json::json;

use crate::api::*;
use crate::protocol::{self as p, HistoryMode};
use crate::rpc::RpcError;
use crate::session::{Session, engine_error};

const PAGE: u32 = 100;
/// How long a read waits, at most, for Codex to keep a new thread's history.
const WAITS: [u64; 6] = [50, 100, 200, 400, 800, 1600];

/// The thread's turns, with the number of the last event before the read
/// that returned them. On a thread loaded in this Codex, a refusal of the kind
/// Codex gives before it has kept the thread's first message is read again,
/// for a while.
pub(crate) async fn read(session: &Session, thread: &ThreadId, mode: HistoryMode, seq: impl Fn() -> u64) -> Result<(Vec<Turn>, u64)> {
    let mut waits = WAITS.iter();
    loop {
        let at = seq();
        match turns(session, thread, mode).await {
            Ok(turns) => return Ok((turns, at)),
            Err(e) if not_kept_yet(&e) => match waits.next() {
                Some(ms) if session.is_loaded(thread) => tokio::time::sleep(Duration::from_millis(*ms)).await,
                _ => return Err(EngineError::NotReady { message: "Codex has not kept this thread's history yet.".into() }),
            },
            Err(e) => return Err(engine_error(e)),
        }
    }
}

/// Codex's answers to a history read before it has kept the thread's first
/// message: -32600 (the thread is not written yet) or -32601 (its store has
/// no record of the thread to page). For a thread in the engine's snapshot,
/// neither has another cause: it is not archived, and its id is Codex's own.
fn not_kept_yet(error: &RpcError) -> bool {
    matches!(error, RpcError::Server { code: -32600 | -32601, .. })
}

async fn turns(session: &Session, thread: &ThreadId, mode: HistoryMode) -> std::result::Result<Vec<Turn>, RpcError> {
    if mode == HistoryMode::Legacy {
        let whole: p::ThreadResponse = session.client.request("thread/read", json!({ "threadId": thread.0, "includeTurns": true })).await?;
        return Ok(whole.thread.turns.iter().map(crate::mapping::turn).collect());
    }
    let mut turns = Vec::new();
    let mut cursor: Option<String> = None;
    loop {
        let params = json!({ "threadId": thread.0, "cursor": cursor, "limit": PAGE, "sortDirection": "asc", "itemsView": "full" });
        let page: p::TurnsListResponse = session.client.request("thread/turns/list", params).await?;
        let empty = page.data.is_empty();
        turns.extend(page.data.iter().map(crate::mapping::turn));
        match page.next_cursor {
            Some(next) if !empty => cursor = Some(next),
            _ => return Ok(turns),
        }
    }
}
