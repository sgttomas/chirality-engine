//! The embedded engine (`docs/embedding.md`): one thread over the host's
//! transport, the host's tools only, calls through the event stream, and
//! bounded memory. It shares none of the ordinary engine's structure.

mod budget;
mod calls;
mod consumer;
mod types;
mod wire;

use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::{Arc, Mutex, MutexGuard};
use std::task::{Context, Poll, Waker};

use tokio::sync::mpsc::{self, error::TryRecvError, error::TrySendError};
use tokio::task::JoinHandle;

pub use budget::Finished;
pub use types::*;

use budget::{Charge, FRAME_TEXT, Ledger, REPLY_SLOTS, Ring, class};
use consumer::{Held, Turns};
use types::ToolNames;
use wire::Writer;

/// Each method's slot.
const INITIALIZE: usize = 0;
const THREAD_START: usize = 1;
const TURN_START: usize = 2;
const TURN_INTERRUPT: usize = 3;

/// The most the engine holds with these limits ([`EmbeddedEngine::max_bytes`]).
pub const fn embedded_max_bytes(limits: &EmbeddedLimits) -> u64 {
    budget::max_bytes(limits)
}

/// The engine. Not `Clone`; dropping it closes.
pub struct EmbeddedEngine {
    shared: Arc<Shared>,
}

/// The engine's events, in wire order; `None` after `Closed`.
pub struct EmbeddedEvents {
    shared: Arc<Shared>,
}

impl EmbeddedEngine {
    /// The engine over the host's transport, once its input is valid; nothing
    /// refused is kept.
    pub fn new(config: EmbeddedConfig) -> Result<(EmbeddedEngine, EmbeddedEvents), EmbeddedError> {
        config.check_limits().map_err(|reason| EmbeddedError::Invalid { reason })?;
        // The ledger first, so validation's one allocation, its nesting stack,
        // is charged; the names are checked on the stack.
        let ledger = Ledger::new(&config.limits);
        let names = {
            let _nesting = ledger.charge(budget::VALIDATION_NESTING).map_err(|_| invalid("limits exceed the budget"))?;
            config.validate().map_err(|reason| EmbeddedError::Invalid { reason })?
        };
        let shared = Arc::new(Shared::new(config, ledger, &names).map_err(|_| invalid("limits exceed the budget"))?);
        Ok((EmbeddedEngine { shared: shared.clone() }, EmbeddedEvents { shared }))
    }

    /// The most the engine holds with these limits; the host reserves it
    /// before `new`.
    pub const fn max_bytes(limits: &EmbeddedLimits) -> u64 {
        budget::max_bytes(limits)
    }

    /// Writes `initialize` and starts the consumer; once only, even after a
    /// cancelled first call.
    pub async fn connect(&self) -> Result<(), EmbeddedError> {
        let wait = {
            let (mut out, mut st) = self.shared.lock();
            if st.closed {
                return Err(closed(&st));
            }
            if st.connect_called {
                return Err(invalid("connect was called"));
            }
            let Some(HostTransport { from_codex, to_codex }) = st.transport.take() else {
                return Err(invalid("no transport"));
            };
            st.connect_called = true;
            out.to_codex = Some(to_codex);
            let id = st.next_id;
            let sent = out.send(&self.shared.ledger, |w| {
                w.raw("{\"id\":").integer(id).raw(",\"method\":\"initialize\",\"params\":{\"clientInfo\":{\"name\":\"chirality-embedded\",\"version\":");
                w.string(env!("CARGO_PKG_VERSION")).raw("},\"capabilities\":{\"experimentalApi\":true}}}");
            });
            if let Err(unsent) = sent {
                // Codex's frames are freed before the close can finish.
                drop(from_codex);
                return Err(self.shared.unsent(&mut out, &mut st, unsent));
            }
            st.next_id += 1;
            st.slots[INITIALIZE] = Slot::waiting(id);
            self.shared.ledger.consumer_started();
            let input = consumer::Input::new(from_codex, self.shared.ledger.clone());
            st.consumer = Some(tokio::spawn(consumer::consume(self.shared.clone(), input)));
            Wait { shared: &self.shared, slot: INITIALIZE, done: false }
        };
        match wait.await? {
            Reply::Codex { code, message } => Err(EmbeddedError::Codex { code, message }),
            _ => Ok(()),
        }
    }

    /// Starts the one ephemeral thread with the policy given; once only.
    pub async fn open_thread(&self, options: &ThreadOptions) -> Result<OpenedThread, EmbeddedError> {
        let wait = self.shared.call(THREAD_START, |out, st, ledger, id| {
            if st.open_called {
                return Err(invalid("open_thread was called"));
            }
            if !st.initialized {
                return Err(invalid("not connected"));
            }
            let config = st.config.as_ref().ok_or(invalid("no configuration"))?;
            let offer = types::Offer {
                namespace: &st.namespace,
                description: &config.description,
                tools: &config.tools,
                work_dir: config.work_dir.to_str().unwrap_or_default(),
            };
            debug_assert_eq!(id, types::OPEN_THREAD_ID, "the size new checked");
            let sent = out.send(ledger, |w| types::thread_start(w, id, &offer, options));
            if sent.is_ok() {
                st.open_called = true;
                st.config = None;
            }
            Ok(sent)
        })?;
        match wait.await? {
            Reply::Opened(opened) => Ok(opened),
            reply => Err(codex(reply)),
        }
    }

    /// Starts a turn with `text`, only while remote control is `Disabled`, a
    /// thread is held and open, and nothing runs, waits or is in flight.
    pub async fn send_text(&self, text: &str) -> Result<TurnId, EmbeddedError> {
        let wait = self.shared.call(TURN_START, |out, st, ledger, id| {
            if st.remote != Some(RemoteControl::Disabled) {
                return Err(invalid("remote control is not disabled"));
            }
            let thread = st.thread.as_ref().filter(|t| t.open).ok_or(invalid("no open thread"))?;
            if st.turns.running.is_some() {
                return Err(invalid("a turn runs"));
            }
            if st.slots.iter().any(|s| !matches!(s, Slot::Free)) {
                return Err(invalid("a request is in flight"));
            }
            debug_assert!(st.record.as_ref().is_none_or(|r| r.pending().is_empty()), "no call pending without a turn");
            let sent = out.send(ledger, |w| {
                w.raw("{\"id\":").integer(id).raw(",\"method\":\"turn/start\",\"params\":{\"threadId\":").string(thread.id.as_str());
                w.raw(",\"input\":[{\"type\":\"text\",\"text\":").string(text).raw(",\"text_elements\":[]}]}}");
            });
            if sent.is_ok() {
                st.turns.starting();
            }
            Ok(sent)
        })?;
        match wait.await? {
            Reply::Started(turn) => Ok(turn),
            reply => Err(codex(reply)),
        }
    }

    /// Asks Codex to stop the running turn; returns on Codex's reply.
    pub async fn interrupt(&self) -> Result<(), EmbeddedError> {
        let wait = self.shared.call(TURN_INTERRUPT, |out, st, ledger, id| {
            let Some(thread) = st.thread.as_ref().filter(|t| t.open) else {
                return Err(invalid("no open thread"));
            };
            let Some(turn) = st.turns.running.as_ref() else {
                return Err(invalid("no turn runs"));
            };
            Ok(out.send(ledger, |w| {
                w.raw("{\"id\":").integer(id).raw(",\"method\":\"turn/interrupt\",\"params\":{\"threadId\":").string(thread.id.as_str());
                w.raw(",\"turnId\":").string(turn.as_str()).raw("}}");
            }))
        })?;
        match wait.await? {
            Reply::Interrupted => Ok(()),
            reply => Err(codex(reply)),
        }
    }

    /// Whether `call` is still pending: the host checks when it dequeues a
    /// `Call`, before it starts work.
    pub fn call_live(&self, call: &CallRef) -> bool {
        self.shared.call_live(call)
    }

    /// Answers a pending call. `Invalid` for a call retired, by close too
    /// (nothing is written), or over `result_bytes` or a frame (the call
    /// stays pending); after close, `Closed` for a call never issued.
    pub fn answer(&self, call: &CallRef, success: bool, text: &str) -> Result<(), EmbeddedError> {
        self.shared.answer(call, success, text)
    }

    /// The latest remote-control status Codex reported.
    pub fn remote_control(&self) -> Option<RemoteControl> {
        self.shared.lock_state().remote
    }

    /// Resolves once the consumer has ended and every charge outside the
    /// fixed row is released.
    pub fn finished(&self) -> Finished {
        Finished::new(self.shared.clone())
    }

    /// Closes: ends `to_codex`, fails pending callers, and frees every buffer
    /// but the fixed row. Idempotent; the first fault wins.
    pub fn close(&self) {
        let (mut out, mut st) = self.shared.lock();
        self.shared.close_step(&mut out, &mut st, None);
    }
}

impl Drop for EmbeddedEngine {
    fn drop(&mut self) {
        self.close();
    }
}

impl EmbeddedEvents {
    pub async fn recv(&mut self) -> Option<EmbeddedEvent> {
        std::future::poll_fn(|cx| self.shared.next_event(Some(cx))).await
    }

    pub fn try_recv(&mut self) -> Result<EmbeddedEvent, TryRecvError> {
        match self.shared.next_event(None) {
            Poll::Ready(Some(event)) => Ok(event),
            Poll::Ready(None) => Err(TryRecvError::Disconnected),
            Poll::Pending => Err(TryRecvError::Empty),
        }
    }
}

/// A caller's wait for its method's reply. Dropped before the reply, it
/// leaves the method in flight until the reply comes and is discarded.
struct Wait<'e> {
    shared: &'e Shared,
    slot: usize,
    done: bool,
}

impl Future for Wait<'_> {
    type Output = Result<Reply, EmbeddedError>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.get_mut();
        let mut st = this.shared.lock_state();
        if st.closed {
            this.done = true;
            return Poll::Ready(Err(closed(&st)));
        }
        match &mut st.slots[this.slot] {
            Slot::Waiting { waker, .. } => {
                if !waker.as_ref().is_some_and(|w| w.will_wake(cx.waker())) {
                    *waker = Some(cx.waker().clone());
                }
                Poll::Pending
            }
            slot => {
                this.done = true;
                match std::mem::replace(slot, Slot::Free) {
                    Slot::Ready { reply, .. } => Poll::Ready(Ok(reply)),
                    _ => Poll::Ready(Err(closed(&st))),
                }
            }
        }
    }
}

impl Drop for Wait<'_> {
    fn drop(&mut self) {
        if self.done {
            return;
        }
        let mut st = self.shared.lock_state();
        let slot = &mut st.slots[self.slot];
        match slot {
            Slot::Waiting { abandoned, waker, .. } => {
                *abandoned = true;
                *waker = None;
            }
            Slot::Ready { .. } => *slot = Slot::Free,
            Slot::Free => {}
        }
    }
}

impl Shared {
    fn lock(&self) -> (MutexGuard<'_, Output>, MutexGuard<'_, State>) {
        let out = self.output.lock().unwrap_or_else(|e| e.into_inner());
        (out, self.lock_state())
    }

    fn lock_state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Checks and writes a method's request under both locks, then waits in
    /// its slot. `write` returns whether its frame went out.
    fn call(
        &self,
        slot: usize,
        write: impl FnOnce(&mut Output, &mut State, &Arc<Ledger>, i64) -> Result<Result<(), Unsent>, EmbeddedError>,
    ) -> Result<Wait<'_>, EmbeddedError> {
        let (mut out, mut st) = self.lock();
        if st.closed {
            return Err(closed(&st));
        }
        if st.in_flight(slot) {
            return Err(invalid("already in flight"));
        }
        let id = st.next_id;
        if let Err(unsent) = write(&mut out, &mut st, &self.ledger, id)? {
            return Err(self.unsent(&mut out, &mut st, unsent));
        }
        st.next_id += 1;
        st.slots[slot] = Slot::waiting(id);
        Ok(Wait { shared: self, slot, done: false })
    }

    /// A frame not sent: too long is `Invalid`; a fault closes.
    fn unsent(&self, out: &mut Output, st: &mut State, unsent: Unsent) -> EmbeddedError {
        match unsent {
            Unsent::TooLong => invalid("the frame is over 131,071 bytes"),
            Unsent::Fault(fault) => {
                self.close_step(out, st, Some(fault));
                closed(st)
            }
        }
    }

    /// The close step, with both locks held: synchronous, awaiting nothing.
    fn close_step(&self, out: &mut Output, st: &mut State, fault: Option<Fault>) {
        if st.closed {
            return;
        }
        st.closed = true;
        st.fault = fault;
        out.to_codex = None;
        st.transport = None;
        for slot in &mut st.slots {
            if let Slot::Waiting { waker: Some(waker), .. } = std::mem::replace(slot, Slot::Free) {
                waker.wake();
            }
        }
        st.config = None;
        out.writer = None;
        st.record = None;
        st.ring = None;
        if let Some(consumer) = st.consumer.take() {
            consumer.abort();
        }
        st.terminal = Terminal::Closed(fault);
        if let Some(reader) = st.reader.take() {
            reader.wake();
        }
        self.ledger.mark_closed();
    }

    /// The next event; `Closed` once closed, then `None`.
    fn next_event(&self, cx: Option<&mut Context<'_>>) -> Poll<Option<EmbeddedEvent>> {
        let mut st = self.lock_state();
        if let Some(event) = st.ring.as_mut().and_then(Ring::pop) {
            return Poll::Ready(Some(event));
        }
        match st.terminal {
            Terminal::Closed(fault) => {
                st.terminal = Terminal::Read;
                Poll::Ready(Some(EmbeddedEvent::Closed { fault }))
            }
            Terminal::Read => Poll::Ready(None),
            Terminal::Open => {
                if let Some(cx) = cx {
                    st.reader = Some(cx.waker().clone());
                }
                Poll::Pending
            }
        }
    }
}

// ---------- the engine's state ----------

/// The control block: the ledger, then the two locks, always taken writer
/// first, then state.
struct Shared {
    ledger: Arc<Ledger>,
    output: Mutex<Output>,
    state: Mutex<State>,
}

/// The writer and the host's `to_codex`, under the writer lock.
struct Output {
    writer: Option<(Writer, Charge)>,
    to_codex: Option<mpsc::Sender<String>>,
}

/// Why a frame was not sent.
enum Unsent {
    TooLong,
    Fault(Fault),
}

struct State {
    closed: bool,
    fault: Option<Fault>,
    /// The whole transport, until `connect`.
    transport: Option<HostTransport>,
    /// Tools, description and `work_dir`, until `open_thread` writes them.
    config: Option<Config>,
    namespace: String,
    /// The namespace's tools, for admission.
    tool_names: Vec<String>,
    limits: EmbeddedLimits,
    /// The turn's requests and the calls pending.
    record: Option<calls::Record>,
    /// Calls admitted in the session: the next call's generation.
    calls_issued: u64,
    connect_called: bool,
    open_called: bool,
    initialized: bool,
    next_id: i64,
    slots: [Slot; REPLY_SLOTS],
    remote: Option<RemoteControl>,
    thread: Option<Held>,
    turns: Turns,
    ring: Option<Ring>,
    reader: Option<Waker>,
    terminal: Terminal,
    consumer: Option<JoinHandle<()>>,
}

struct Config {
    tools: String,
    description: String,
    work_dir: PathBuf,
    _charge: Charge,
}

/// A method's slot: free, holding its RPC id until the reply, or holding the
/// reply until its future takes it.
enum Slot {
    Free,
    Waiting { id: i64, waker: Option<Waker>, abandoned: bool },
    Ready { reply: Reply, _charge: Option<Charge> },
}

enum Reply {
    Connected,
    Opened(OpenedThread),
    Started(TurnId),
    Interrupted,
    Codex { code: i64, message: String },
}

/// The terminal slot: `Closed` once closed, until it is read.
#[derive(Clone, Copy)]
enum Terminal {
    Open,
    Closed(Option<Fault>),
    Read,
}

impl Shared {
    fn new(config: EmbeddedConfig, ledger: Arc<Ledger>, names: &ToolNames) -> Result<Shared, Fault> {
        let EmbeddedConfig { transport, work_dir, namespace, namespace_description, tools, limits } = config;
        // In the fixed row.
        let tool_names = names.iter().map(str::to_owned).collect();
        let held = class(tools.capacity() as u64) + class(namespace_description.capacity() as u64) + class(work_dir.capacity() as u64);
        let config = Config { tools, description: namespace_description, work_dir, _charge: ledger.charge(held)? };
        let writer_charge = ledger.charge(class(FRAME_TEXT as u64))?;
        let writer = Writer::new();
        let ring = Ring::new(&ledger, budget::ring_slots(&limits) as usize)?;
        let record = calls::Record::new(&ledger, limits.calls_per_turn)?;
        let state = State {
            closed: false,
            fault: None,
            transport: Some(transport),
            config: Some(config),
            namespace,
            tool_names,
            limits,
            record: Some(record),
            calls_issued: 0,
            connect_called: false,
            open_called: false,
            initialized: false,
            next_id: 1,
            slots: [const { Slot::Free }; REPLY_SLOTS],
            remote: None,
            thread: None,
            turns: Turns::default(),
            ring: Some(ring),
            reader: None,
            terminal: Terminal::Open,
            consumer: None,
        };
        let output = Output { writer: Some((writer, writer_charge)), to_codex: None };
        Ok(Shared { ledger, output: Mutex::new(output), state: Mutex::new(state) })
    }
}

impl Output {
    /// Builds a frame in the writer buffer and sends a copy, charged until
    /// `try_send` takes it.
    fn send(&mut self, ledger: &Arc<Ledger>, build: impl FnOnce(&mut Writer)) -> Result<(), Unsent> {
        let (Some((writer, _)), Some(to_codex)) = (self.writer.as_mut(), self.to_codex.as_ref()) else {
            return Err(Unsent::Fault(Fault::OutputClosed));
        };
        build(writer.begin());
        let text = writer.text().ok_or(Unsent::TooLong)?;
        let charge = ledger.charge(class(text.len() as u64)).map_err(Unsent::Fault)?;
        // The copy is freed before its charge is released.
        let (fault, copy) = match to_codex.try_send(text.to_owned()) {
            Ok(()) => return Ok(()),
            Err(TrySendError::Full(copy)) => (Fault::OutputFull, copy),
            Err(TrySendError::Closed(copy)) => (Fault::OutputClosed, copy),
        };
        drop(copy);
        drop(charge);
        Err(Unsent::Fault(fault))
    }
}

impl State {
    fn in_flight(&self, slot: usize) -> bool {
        !matches!(self.slots[slot], Slot::Free)
    }
}

impl Slot {
    fn waiting(id: i64) -> Slot {
        Slot::Waiting { id, waker: None, abandoned: false }
    }

    fn waits_for(&self, rpc: i64) -> bool {
        matches!(self, Slot::Waiting { id, .. } if *id == rpc)
    }

    /// The reply arrived: held for its caller, or discarded when the caller
    /// gave up, which frees the slot.
    fn fill(&mut self, reply: Reply, charge: Option<Charge>) {
        match std::mem::replace(self, Slot::Free) {
            Slot::Waiting { waker, abandoned: false, .. } => {
                *self = Slot::Ready { reply, _charge: charge };
                if let Some(waker) = waker {
                    waker.wake();
                }
            }
            _ => {
                drop(reply);
                drop(charge);
            }
        }
    }
}

fn invalid(reason: &'static str) -> EmbeddedError {
    EmbeddedError::Invalid { reason }
}

fn closed(st: &State) -> EmbeddedError {
    EmbeddedError::Closed { fault: st.fault }
}

fn codex(reply: Reply) -> EmbeddedError {
    match reply {
        Reply::Codex { code, message } => EmbeddedError::Codex { code, message },
        _ => EmbeddedError::Closed { fault: Some(Fault::InvalidFrame) },
    }
}

#[cfg(test)]
mod bounds_tests;
#[cfg(test)]
mod calls_tests;
#[cfg(test)]
mod close_tests;
#[cfg(test)]
mod offer_tests;
#[cfg(test)]
mod tests;
