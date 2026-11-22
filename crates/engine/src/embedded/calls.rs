//! Calls: the record of a turn's requests, admission, the answers, and
//! retirement (`docs/embedding.md`).

use std::sync::Arc;

use serde_json::value::RawValue;

use super::budget::{Charge, Ledger, RECORD_ENTRY, class};
use super::consumer::{Cx, publish, unsent};
use super::types::{CallRef, EmbeddedError, EmbeddedEvent, Fault, RpcId, TurnId};
use super::wire::{self, IDENTITY};
use super::{Shared, State, closed, invalid};

/// The most calls pending at once (`pending_calls` is at most this).
const MAX_PENDING: usize = 16;

/// A request's RPC id, still borrowed.
#[derive(Clone, Copy)]
enum Key<'a> {
    Integer(i64),
    Text(&'a str),
}

impl Key<'_> {
    fn is(&self, id: &RpcId) -> bool {
        match (self, id) {
            (Key::Integer(a), RpcId::Integer(b)) => a == b,
            (Key::Text(a), RpcId::Text(b)) => a == b,
            _ => false,
        }
    }

    fn text(&self) -> &str {
        match self {
            Key::Text(text) => text,
            Key::Integer(_) => "",
        }
    }

    fn owned(&self) -> RpcId {
        match self {
            Key::Integer(n) => RpcId::Integer(*n),
            Key::Text(text) => RpcId::Text((*text).to_owned()),
        }
    }
}

/// One request: its RPC id and any valid `callId`.
struct Entry {
    rpc: RpcId,
    call_id: Option<String>,
}

const _: () = assert!(std::mem::size_of::<Entry>() as u64 <= RECORD_ENTRY);

/// Every request in the turn, preallocated at `calls_per_turn`, and the calls
/// pending, by their place in it and their generation. Its strings are freed
/// before their charge is released (fields drop in order).
pub(super) struct Record {
    entries: Vec<Entry>,
    limit: usize,
    pending: [(u16, u64); MAX_PENDING],
    pending_len: usize,
    strings: Charge,
    _buffer: Charge,
}

impl Record {
    pub(super) fn new(ledger: &Arc<Ledger>, limit: u32) -> Result<Record, Fault> {
        let buffer = ledger.charge(class(limit as u64 * RECORD_ENTRY))?;
        let strings = ledger.charge(0)?;
        let limit = limit as usize;
        Ok(Record { entries: Vec::with_capacity(limit), limit, pending: [(0, 0); MAX_PENDING], pending_len: 0, strings, _buffer: buffer })
    }

    /// Records a request; a repeated RPC id or `callId`, or a request past
    /// the count, faults.
    fn push(&mut self, rpc: Key, call_id: Option<&str>) -> Result<usize, Fault> {
        let repeated = |e: &Entry| rpc.is(&e.rpc) || call_id.is_some_and(|c| e.call_id.as_deref() == Some(c));
        if self.entries.iter().any(repeated) {
            return Err(Fault::RepeatedId);
        }
        if self.entries.len() >= self.limit {
            return Err(Fault::TooManyRequests);
        }
        self.strings.grow(class(rpc.text().len() as u64) + call_id.map_or(0, |c| class(c.len() as u64)))?;
        self.entries.push(Entry { rpc: rpc.owned(), call_id: call_id.map(str::to_owned) });
        Ok(self.entries.len() - 1)
    }

    /// A newly admitted turn starts a new record.
    pub(super) fn clear(&mut self) {
        self.pending_len = 0;
        self.entries.clear();
        self.strings.release_all();
    }

    pub(super) fn pending(&self) -> &[(u16, u64)] {
        &self.pending[..self.pending_len]
    }

    fn pend(&mut self, index: usize, generation: u64) {
        self.pending[self.pending_len] = (index as u16, generation);
        self.pending_len += 1;
    }

    /// Where `call` is among the pending calls: its generation, which no
    /// other call in the session has, and its ids.
    fn find(&self, call: &CallRef) -> Option<usize> {
        self.pending().iter().position(|&(at, generation)| {
            let entry = &self.entries[at as usize];
            generation == call.generation && entry.rpc == call.rpc_id && entry.call_id.as_deref() == Some(call.call_id.as_str())
        })
    }

    fn retire_at(&mut self, at: usize) {
        self.pending.copy_within(at + 1..self.pending_len, at);
        self.pending_len -= 1;
    }

    /// The call whose `callId` is `id`, by its item's completion.
    pub(super) fn retire_call(&mut self, id: &str) {
        let at = self.pending().iter().position(|&(at, _)| self.entries[at as usize].call_id.as_deref() == Some(id));
        if let Some(at) = at {
            self.retire_at(at);
        }
    }

    /// Every call, by its turn's end or the thread's.
    pub(super) fn retire_all(&mut self) {
        self.pending_len = 0;
    }
}

impl State {
    /// Where `call` is among the pending calls of the running turn.
    fn live(&self, call: &CallRef) -> Option<usize> {
        let running = self.turns.running.as_ref().is_some_and(|t| t.as_str() == call.turn.0);
        self.record.as_ref().filter(|_| running && !self.closed)?.find(call)
    }
}

impl Shared {
    pub(super) fn call_live(&self, call: &CallRef) -> bool {
        self.lock_state().live(call).is_some()
    }

    /// Writes the answer and retires the call; over a bound, the call stays
    /// pending and nothing is written. A call this engine issued and retired,
    /// by close too, is `Invalid`; after close, any other is `Closed`.
    pub(super) fn answer(&self, call: &CallRef, success: bool, text: &str) -> Result<(), EmbeddedError> {
        let (mut out, mut st) = self.lock();
        let Some(at) = st.live(call) else {
            let issued = call.generation < st.calls_issued;
            return Err(if st.closed && !issued { closed(&st) } else { invalid("the call is not live") });
        };
        if text.len() > st.limits.result_bytes as usize {
            return Err(invalid("the text is over result_bytes"));
        }
        let sent = out.send(&self.ledger, |w| {
            w.raw("{\"id\":");
            match &call.rpc_id {
                RpcId::Integer(id) => w.integer(*id),
                RpcId::Text(id) => w.string(id),
            };
            w.raw(",\"result\":{\"contentItems\":[{\"type\":\"inputText\",\"text\":").string(text);
            w.raw("}],\"success\":").raw(if success { "true" } else { "false" }).raw("}}");
        });
        match sent {
            Ok(()) => {
                if let Some(record) = st.record.as_mut() {
                    record.retire_at(at);
                }
                Ok(())
            }
            Err(unsent) => Err(self.unsent(&mut out, &mut st, unsent)),
        }
    }
}

// ---------- requests ----------

const PARAMS: [&str; 8] = ["callId", "namespace", "tool", "threadId", "turnId", "arguments", "thread", "conversationId"];

/// A request from Codex: recorded, then a `Call` if admitted, else `Refused`.
pub(super) fn request(cx: &mut Cx, id: &RawValue, method: &RawValue, params: Option<&RawValue>) -> Result<(), Fault> {
    let fields = params.and_then(|p| wire::pick(p.get(), PARAMS)).unwrap_or_default();
    let (mut rpc_buf, mut call_buf) = ([0; IDENTITY], [0; IDENTITY]);
    let rpc = match wire::integer(id) {
        Some(n) => Key::Integer(n),
        None => Key::Text(wire::identity(id, &mut rpc_buf).ok_or(Fault::InvalidFrame)?),
    };
    let call_id = fields[0].and_then(|c| wire::identity(c, &mut call_buf));
    // 1. Every request is recorded before dispatch; a repeat or one too many faults.
    let index = cx.st.record.as_mut().ok_or(Fault::OverBudget)?.push(rpc, call_id)?;
    // 3. An admitted call.
    let (mut tool_buf, mut turn_buf) = ([0; IDENTITY], [0; IDENTITY]);
    if let Some(call_id) = call_id
        && let Some((tool, turn, arguments)) = admit(cx.st, method, &fields, &mut tool_buf, &mut turn_buf)
    {
        let parts = [rpc.text(), call_id, turn, tool, arguments];
        // Generations never repeat in a session, so a call retired with its
        // turn is never taken for a later one with the same ids.
        let generation = cx.st.calls_issued;
        cx.st.calls_issued += 1;
        publish(cx, &parts, || EmbeddedEvent::Call {
            call: CallRef { rpc_id: rpc.owned(), call_id: call_id.to_owned(), turn: TurnId(turn.to_owned()), generation },
            tool: tool.to_owned(),
            arguments: arguments.into(),
        })?;
        cx.st.record.as_mut().ok_or(Fault::OverBudget)?.pend(index, generation);
        return Ok(());
    }
    // 4. Any other request.
    refuse(cx, id, method, params, &fields)
}

/// The tool, turn and raw arguments of an `item/tool/call` that names the
/// namespace and one of its tools, in the running turn on the held open
/// thread, with arguments present (`null` too) and within bounds, while fewer than
/// `pending_calls` are pending.
fn admit<'a>(
    st: &State,
    method: &RawValue,
    fields: &[Option<&'a RawValue>; 8],
    tool_buf: &'a mut [u8; IDENTITY],
    turn_buf: &'a mut [u8; IDENTITY],
) -> Option<(&'a str, &'a str, &'a str)> {
    let [_, namespace, tool, thread_id, turn_id, arguments, ..] = *fields;
    if wire::constant(method) != Some("item/tool/call") {
        return None;
    }
    let mut ns_buf = [0; IDENTITY];
    if namespace.and_then(|n| wire::unescape(n, &mut ns_buf)) != Some(st.namespace.as_str()) {
        return None;
    }
    let tool = tool.and_then(|t| wire::unescape(t, tool_buf)).filter(|t| st.tool_names.iter().any(|n| n == t))?;
    let held = st.thread.as_ref().filter(|t| t.open)?;
    let mut thread_buf = [0; IDENTITY];
    if thread_id.and_then(|t| wire::identity(t, &mut thread_buf)) != Some(held.id.as_str()) {
        return None;
    }
    let running = st.turns.running.as_ref()?;
    let turn = turn_id.and_then(|t| wire::identity(t, turn_buf)).filter(|t| *t == running.as_str())?;
    // Present, `null` too: its raw text.
    let arguments = arguments.map(RawValue::get)?;
    let pending = st.record.as_ref()?.pending().len();
    (arguments.len() <= st.limits.argument_bytes as usize && pending < st.limits.pending_calls as usize).then_some((tool, turn, arguments))
}

/// `Refused`, then the refusal is written.
fn refuse(cx: &mut Cx, id: &RawValue, method: &RawValue, params: Option<&RawValue>, fields: &[Option<&RawValue>; 8]) -> Result<(), Fault> {
    let mut name_buf = [0; IDENTITY];
    let name = wire::identity(method, &mut name_buf).ok_or(Fault::InvalidFrame)?;
    let json = params.map_or("{}", RawValue::get);
    let [.., thread_id, _, _, thread, conversation] = *fields;
    let thread_dot_id = thread.and_then(|t| wire::pick(t.get(), ["id"])).and_then(|[id]| id);
    // The first of them that is an identity.
    let (mut buf, mut len) = ([0; IDENTITY], None);
    for raw in [thread_id, thread_dot_id, conversation].into_iter().flatten() {
        if let Some(id) = wire::identity(raw, &mut buf) {
            len = Some(id.len());
            break;
        }
    }
    let thread = len.map(|len| std::str::from_utf8(&buf[..len]).unwrap_or_default());
    publish(cx, &[name, thread.unwrap_or_default(), json], || EmbeddedEvent::Refused {
        method: name.to_owned(),
        thread: thread.map(str::to_owned),
        json: json.to_owned(),
    })?;
    cx.out
        .send(cx.ledger, |w| _ = w.raw("{\"id\":").raw(id.get()).raw(",\"error\":{\"code\":-32601,\"message\":\"not supported by this client\"}}"))
        .map_err(unsent)
}
