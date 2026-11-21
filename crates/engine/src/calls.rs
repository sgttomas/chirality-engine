//! The application's tool calls under way, across Codex sessions.
//!
//! A call is admitted only while its session is current and its thread open,
//! is run once per (session, request id), and is answered once unless
//! cancelled first. Answering and cancelling take the same lock and check
//! that the entry is still the call's own, so a call of a session that has
//! ended can never take or answer one of the next session, whose request ids
//! start again from zero. Every supervisor is counted until it ends, so
//! shutdown can wait for the calls of every session.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use serde_json::Value;
use tokio::sync::watch;
use tokio::task::AbortHandle;

use crate::api::*;
use crate::rpc::RpcClient;

/// Where a call is in Codex: its thread, turn and item, as Codex names them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Place {
    pub thread: String,
    pub turn: String,
    pub item: String,
}

/// A request: the session it came in on and its id there.
type Key = (u64, String);

/// Where a call's answer goes: its session's connection, and its request id there.
pub(crate) struct ReplyTo {
    pub client: Arc<RpcClient>,
    pub generation: u64,
    pub id: Value,
}

struct Running {
    /// This admission's own mark, so only its supervisor answers through it.
    token: u64,
    place: Place,
    work: AbortHandle,
}

#[derive(Default)]
struct State {
    /// The session calls are admitted for; none while Codex is down or the engine is shut down.
    open: Option<u64>,
    running: HashMap<Key, Running>,
    /// Requests taken in the current session, so one Codex sends again is answered once.
    taken: HashSet<Key>,
    /// Threads archived, deleted or closed, until the engine loads them again.
    closed: HashSet<String>,
    next_token: u64,
}

/// Whether a request is to be answered, and how.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Admission {
    /// Run the handler.
    Run,
    /// Answer it at once, as failed.
    Refuse,
    /// Already taken, or its session is over: send nothing.
    Ignore,
}

#[derive(Clone)]
pub(crate) struct Calls {
    state: Arc<Mutex<State>>,
    /// Supervisors not yet ended.
    live: Arc<watch::Sender<usize>>,
}

impl Default for Calls {
    fn default() -> Calls {
        Calls { state: Arc::default(), live: Arc::new(watch::channel(0).0) }
    }
}

/// Counts a supervisor until it ends, however it ends.
struct Live(Arc<watch::Sender<usize>>);

impl Drop for Live {
    fn drop(&mut self) {
        self.0.send_modify(|n| *n -= 1);
    }
}

impl Calls {
    /// A new session admits calls.
    pub fn open(&self, generation: u64) {
        let mut state = self.state.lock().unwrap();
        state.open = Some(generation);
        state.taken.retain(|(g, _)| *g == generation);
    }

    /// Admits no more calls and cancels the running ones.
    pub fn close(&self) {
        let mut state = self.state.lock().unwrap();
        state.open = None;
        for (_, running) in state.running.drain() {
            running.work.abort();
        }
    }

    /// Returns once every supervisor, of every session, has ended: each
    /// handler future has finished or been dropped.
    pub async fn idle(&self) {
        let _ = self.live.subscribe().wait_for(|n| *n == 0).await;
    }

    /// Cancels the calls whose place `matches`.
    pub fn cancel(&self, matches: impl Fn(&Place) -> bool) {
        self.state.lock().unwrap().running.retain(|_, running| {
            let keep = !matches(&running.place);
            if !keep {
                running.work.abort();
            }
            keep
        });
    }

    /// The thread is gone from Codex: its calls are cancelled and none is admitted.
    pub fn close_thread(&self, thread: &str) {
        self.state.lock().unwrap().closed.insert(thread.to_string());
        self.cancel(|place| place.thread == thread);
    }

    /// The engine has loaded the thread again.
    pub fn reopen_thread(&self, thread: &str) {
        self.state.lock().unwrap().closed.remove(thread);
    }

    /// Takes a request once per session; refuses it if its thread is closed.
    pub fn admit(&self, generation: u64, id: &Value, thread: &str) -> Admission {
        let mut state = self.state.lock().unwrap();
        if state.open != Some(generation) || !state.taken.insert((generation, id.to_string())) {
            return Admission::Ignore;
        }
        if state.closed.contains(thread) { Admission::Refuse } else { Admission::Run }
    }

    /// Runs `handler` on an admitted call and answers it, unless it is cancelled first.
    pub fn run(&self, to: ReplyTo, place: Place, call: ToolCall, handler: Arc<dyn ToolHandler>, answer: fn(ToolOutput) -> Value) {
        let key = (to.generation, to.id.to_string());
        let mut state = self.state.lock().unwrap();
        if state.open != Some(to.generation) {
            return;
        }
        state.next_token += 1;
        let token = state.next_token;
        // The handler is invoked inside the task, so a panic building its future is caught too.
        let work = tokio::spawn(async move { handler.call(call).await });
        state.running.insert(key.clone(), Running { token, place, work: work.abort_handle() });
        self.live.send_modify(|n| *n += 1);
        let live = Live(self.live.clone());
        let calls = self.clone();
        tokio::spawn(async move {
            let _live = live;
            let output = match work.await {
                Ok(output) => output,
                Err(error) if error.is_cancelled() => return,
                Err(_) => ToolOutput::failure("The tool failed."),
            };
            let reply = answer(output);
            calls.commit(&key, token, || {
                let _ = to.client.respond(to.id, reply);
            });
        });
    }

    /// Sends the answer if the call is still this admission's and not cancelled.
    fn commit(&self, key: &Key, token: u64, send: impl FnOnce()) -> bool {
        let mut state = self.state.lock().unwrap();
        if state.running.get(key).is_none_or(|r| r.token != token) {
            return false;
        }
        state.running.remove(key);
        send();
        true
    }

    #[cfg(test)]
    pub fn running(&self) -> usize {
        self.state.lock().unwrap().running.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn place(thread: &str) -> Place {
        Place { thread: thread.into(), turn: "turn".into(), item: "call_0".into() }
    }

    /// Puts a call in the registry as `run` would, held until aborted.
    fn hold(calls: &Calls, generation: u64, id: i64, thread: &str) -> u64 {
        let mut state = calls.state.lock().unwrap();
        state.next_token += 1;
        let token = state.next_token;
        let work = tokio::spawn(std::future::pending::<()>());
        state.running.insert((generation, json!(id).to_string()), Running { token, place: place(thread), work: work.abort_handle() });
        token
    }

    #[tokio::test]
    async fn a_call_of_an_ended_session_cannot_answer_the_next_sessions_call() {
        let calls = Calls::default();
        calls.open(1);
        assert_eq!(calls.admit(1, &json!(0), "a"), Admission::Run);
        let old = hold(&calls, 1, 0, "a");
        // Codex dies; the next one numbers its requests from zero again.
        calls.close();
        calls.open(2);
        assert_eq!(calls.admit(2, &json!(0), "a"), Admission::Run);
        let new = hold(&calls, 2, 0, "a");
        // The old call's supervisor, already past its handler, tries to answer.
        assert!(!calls.commit(&(1, "0".into()), old, || panic!("answered through the old session")));
        assert_eq!(calls.running(), 1, "the new call is still there");
        let mut sent = false;
        assert!(calls.commit(&(2, "0".into()), new, || sent = true));
        assert!(sent);
    }

    #[tokio::test]
    async fn a_request_is_taken_once_per_session_and_refused_on_a_closed_thread() {
        let calls = Calls::default();
        assert_eq!(calls.admit(1, &json!(3), "a"), Admission::Ignore, "no session yet");
        calls.open(1);
        assert_eq!(calls.admit(1, &json!(3), "a"), Admission::Run);
        assert_eq!(calls.admit(1, &json!(3), "a"), Admission::Ignore);
        calls.close_thread("a");
        assert_eq!(calls.admit(1, &json!(4), "a"), Admission::Refuse);
        assert_eq!(calls.admit(1, &json!(4), "a"), Admission::Ignore);
        calls.reopen_thread("a");
        assert_eq!(calls.admit(1, &json!(5), "a"), Admission::Run);
        assert_eq!(calls.admit(2, &json!(6), "a"), Admission::Ignore, "not the current session");
    }

    #[tokio::test]
    async fn a_cancelled_call_is_not_answered() {
        let calls = Calls::default();
        calls.open(1);
        let token = hold(&calls, 1, 0, "a");
        calls.cancel(|p| p.thread == "a");
        assert!(!calls.commit(&(1, "0".into()), token, || panic!("answered after cancellation")));
    }
}
