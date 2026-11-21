//! The application's tool calls under way, across Codex sessions.
//!
//! A request is taken once per (session, request id), before anything is
//! sent for it, and a call is admitted only while its session's connection is
//! open and its thread is open, decided and registered under the same lock as
//! closing and cancelling. It is answered once unless cancelled first. Answering and cancelling take the same lock and check
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
#[derive(Clone)]
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
    /// The last session whose connection ended; it is never opened again.
    ended: u64,
    running: HashMap<Key, Running>,
    /// Requests taken in the current session, so one Codex sends again is answered once.
    taken: HashSet<Key>,
    /// Threads archived, deleted or closed, each with whether Codex's own
    /// notice of it has come (so a later event on the thread is a new life).
    closed: HashMap<String, bool>,
    next_token: u64,
}

/// What a request asks, decided before it is taken.
pub(crate) enum Ask {
    /// The engine has no tools.
    Unsupported,
    /// A call that does not decode.
    Invalid,
    /// A tool the application does not have, by its full name.
    Unknown(String),
    /// One of the application's tools.
    Call { place: Place, call: Box<ToolCall>, handler: Arc<dyn ToolHandler> },
}

/// What the engine sends for a request it has taken.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Taken {
    /// Already taken, or its connection is closed: nothing.
    Nothing,
    /// An error: the engine takes no such request.
    Unsupported,
    /// An error: the call does not decode.
    Invalid,
    /// A failed answer, at once.
    Refused(String),
    /// The handler's answer, when it comes.
    Running,
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
        if generation <= state.ended {
            return;
        }
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

    /// This session's connection has ended: nothing more is taken from it,
    /// even requests it delivered before ending.
    pub fn close_session(&self, generation: u64) {
        let mut state = self.state.lock().unwrap();
        state.ended = state.ended.max(generation);
        if state.open == Some(generation) {
            state.open = None;
        }
        state.running.retain(|(g, _), running| {
            let keep = *g != generation;
            if !keep {
                running.work.abort();
            }
            keep
        });
    }

    /// The thread is gone from Codex: its calls are cancelled and none is
    /// admitted. `noticed` when Codex's own notice of it is the reason.
    pub fn close_thread(&self, thread: &str, noticed: bool) {
        let mut state = self.state.lock().unwrap();
        let seen = state.closed.entry(thread.to_string()).or_insert(false);
        *seen |= noticed;
        state.running.retain(|_, running| {
            let keep = running.place.thread != thread;
            if !keep {
                running.work.abort();
            }
            keep
        });
    }

    /// The engine has loaded the thread again.
    pub fn reopen_thread(&self, thread: &str) {
        self.state.lock().unwrap().closed.remove(thread);
    }

    /// Codex reports the thread live again (started, unarchived, a new turn).
    /// An event before Codex's own notice of the closing came before the
    /// closing, so it does not reopen.
    pub fn thread_live(&self, thread: &str) {
        let mut state = self.state.lock().unwrap();
        if state.closed.get(thread) == Some(&true) {
            state.closed.remove(thread);
        }
    }

    /// Takes a request once per session and decides it; a call is registered
    /// and its handler started in the same step.
    pub fn take(&self, to: ReplyTo, thread: Option<&str>, ask: Ask, answer: fn(ToolOutput) -> Value) -> Taken {
        let key = (to.generation, to.id.to_string());
        let mut state = self.state.lock().unwrap();
        if state.open != Some(to.generation) || !state.taken.insert(key.clone()) {
            return Taken::Nothing;
        }
        let (place, call, handler) = match ask {
            Ask::Unsupported => return Taken::Unsupported,
            Ask::Invalid => return Taken::Invalid,
            _ if thread.is_some_and(|t| state.closed.contains_key(t)) => return Taken::Refused("The thread is closed.".into()),
            Ask::Unknown(name) => return Taken::Refused(format!("This application has no tool {name}.")),
            Ask::Call { place, call, handler } => (place, call, handler),
        };
        state.next_token += 1;
        let token = state.next_token;
        // The handler is invoked inside the task, so a panic building its future is caught too.
        let work = tokio::spawn(async move { handler.call(*call).await });
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
        Taken::Running
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
    use tokio::sync::mpsc;

    use crate::transport::{Connection, Stopper};

    struct NoStop;
    impl Stopper for NoStop {
        fn stop(&self) {}
        fn kill(&self) {}
    }

    /// A client whose frames the test reads.
    fn client() -> (Arc<RpcClient>, mpsc::UnboundedReceiver<String>) {
        let (out_tx, out_rx) = mpsc::unbounded_channel();
        let (_in_tx, in_rx) = mpsc::unbounded_channel::<String>();
        let (client, _, _) = RpcClient::start(Connection { outgoing: out_tx, incoming: in_rx, stopper: Box::new(NoStop) }, None, None);
        (client, out_rx)
    }

    fn to(client: &Arc<RpcClient>, generation: u64, id: i64) -> ReplyTo {
        ReplyTo { client: client.clone(), generation, id: json!(id) }
    }

    fn place(thread: &str) -> Place {
        Place { thread: thread.into(), turn: "turn".into(), item: "call_0".into() }
    }

    fn call(thread: &str, handler: Arc<dyn ToolHandler>) -> Ask {
        let call = ToolCall { thread: ThreadId(thread.into()), project: None, turn: TurnId("turn".into()), item: ItemId("call_0".into()), namespace: None, tool: "t".into(), arguments: json!({}) };
        Ask::Call { place: place(thread), call: Box::new(call), handler }
    }

    fn answer(output: ToolOutput) -> Value {
        json!({ "success": output.success })
    }

    /// Puts a call in the registry as `take` would, held until aborted.
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
        let old = hold(&calls, 1, 0, "a");
        // Codex dies; the next one numbers its requests from zero again.
        calls.close();
        calls.open(2);
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
        let (client, _out) = client();
        let calls = Calls::default();
        let unknown = || Ask::Unknown("estimate.x".into());
        assert_eq!(calls.take(to(&client, 1, 3), Some("a"), unknown(), answer), Taken::Nothing, "no session yet");
        calls.open(1);
        assert_eq!(calls.take(to(&client, 1, 3), Some("a"), Ask::Unsupported, answer), Taken::Unsupported);
        assert_eq!(calls.take(to(&client, 1, 3), Some("a"), Ask::Unsupported, answer), Taken::Nothing);
        calls.close_thread("a", false);
        assert_eq!(calls.take(to(&client, 1, 4), Some("a"), unknown(), answer), Taken::Refused("The thread is closed.".into()));
        // A new turn queued from before Codex's own notice does not reopen it; one after does.
        calls.thread_live("a");
        assert_eq!(calls.take(to(&client, 1, 5), Some("a"), unknown(), answer), Taken::Refused("The thread is closed.".into()));
        calls.close_thread("a", true);
        calls.thread_live("a");
        assert_eq!(calls.take(to(&client, 1, 6), Some("a"), unknown(), answer), Taken::Refused("This application has no tool estimate.x.".into()));
        assert_eq!(calls.take(to(&client, 2, 7), Some("a"), unknown(), answer), Taken::Nothing, "not the current session");
    }

    #[tokio::test]
    async fn a_closed_session_takes_nothing_more() {
        let (client, _out) = client();
        let calls = Calls::default();
        calls.open(1);
        hold(&calls, 1, 0, "a");
        calls.close_session(1);
        assert_eq!(calls.running(), 0);
        assert_eq!(calls.take(to(&client, 1, 1), Some("a"), Ask::Unsupported, answer), Taken::Nothing);
        // A connection that ended before its session was published is not opened.
        calls.close_session(2);
        calls.open(2);
        assert_eq!(calls.take(to(&client, 2, 1), Some("a"), Ask::Unsupported, answer), Taken::Nothing);
    }

    #[tokio::test]
    async fn a_cancelled_call_is_not_answered() {
        let calls = Calls::default();
        calls.open(1);
        let token = hold(&calls, 1, 0, "a");
        calls.cancel(|p| p.thread == "a");
        assert!(!calls.commit(&(1, "0".into()), token, || panic!("answered after cancellation")));
    }

    /// Taking a call and closing its thread at the same moment: whichever
    /// comes first, no call of that thread survives to answer.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn closing_a_thread_and_taking_its_call_at_once_leaves_no_answer() {
        let (client, mut out) = client();
        let gate = Arc::new(tokio::sync::Notify::new());
        let runtime = tokio::runtime::Handle::current();
        for round in 0..300 {
            let calls = Calls::default();
            calls.open(1);
            let handler: Arc<dyn ToolHandler> = Arc::new({
                let gate = gate.clone();
                move |_: ToolCall| {
                    let gate = gate.clone();
                    async move {
                        gate.notified().await;
                        ToolOutput::text("late")
                    }
                }
            });
            let barrier = Arc::new(std::sync::Barrier::new(2));
            let taking = std::thread::spawn({
                let (calls, barrier, runtime, to) = (calls.clone(), barrier.clone(), runtime.clone(), to(&client, 1, round));
                move || {
                    let _entered = runtime.enter();
                    barrier.wait();
                    calls.take(to, Some("a"), call("a", handler), answer)
                }
            });
            let closing = std::thread::spawn({
                let (calls, barrier) = (calls.clone(), barrier.clone());
                move || {
                    barrier.wait();
                    calls.close_thread("a", false);
                }
            });
            taking.join().unwrap();
            closing.join().unwrap();
            assert_eq!(calls.running(), 0, "round {round}: a call outlived its thread's closing");
        }
        gate.notify_waiters();
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        assert!(out.try_recv().is_err(), "a call of a closed thread was answered");
    }
}
