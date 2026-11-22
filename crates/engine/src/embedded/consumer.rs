//! The one consumer: each frame from Codex, handled before the next, gets the
//! first route that applies, and only it; with the turn rules.

use std::sync::Arc;

use serde_json::value::RawValue;
use tokio::sync::mpsc;

use super::budget::{FRAME_HAND, Ledger, NESTING, class, event_bytes};
use super::types::{EmbeddedEvent, Fault, ItemRef, Method, OpenedThread, RemoteControl, Reported, ThreadId, TurnId};
use super::wire::{self, CodexError, Frame, IDENTITY, LONG_TEXT};
use super::{INITIALIZE, Output, Reply, Shared, State, THREAD_START, TURN_START, Unsent};

/// An identity the engine holds, inline: the held thread and the turns.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) struct Ident {
    len: u16,
    bytes: [u8; IDENTITY],
}

impl Ident {
    fn new(id: &str) -> Ident {
        let mut bytes = [0; IDENTITY];
        bytes[..id.len()].copy_from_slice(id.as_bytes());
        Ident { len: id.len() as u16, bytes }
    }

    pub(super) fn as_str(&self) -> &str {
        std::str::from_utf8(&self.bytes[..self.len as usize]).unwrap_or_default()
    }
}

pub(super) struct Held {
    pub(super) id: Ident,
    pub(super) open: bool,
}

/// The turn rule: the running turn, the last ended one, or, with a
/// `turn/start` in flight, the first new one.
#[derive(Default)]
pub(super) struct Turns {
    pub(super) running: Option<Ident>,
    last_ended: Option<Ident>,
    starting: bool,
    first_new: Option<Ident>,
    first_new_ended: bool,
}

impl Turns {
    /// A `turn/start` went out.
    pub(super) fn starting(&mut self) {
        self.starting = true;
        self.first_new = None;
        self.first_new_ended = false;
    }

    /// Its reply: the turn runs unless it ended, or the thread closed, first.
    fn started(&mut self, id: Ident, thread_open: bool) -> Result<(), Fault> {
        self.starting = false;
        let ended = std::mem::take(&mut self.first_new_ended);
        if self.first_new.take().is_some_and(|first| first != id) {
            return Err(Fault::WrongTurn);
        }
        if !ended && thread_open {
            self.running = Some(id);
        }
        Ok(())
    }

    fn refused(&mut self) {
        self.starting = false;
        self.first_new = None;
        self.first_new_ended = false;
    }

    fn admits(&mut self, id: Ident) -> bool {
        if self.running == Some(id) || self.last_ended == Some(id) {
            return true;
        }
        match self.first_new {
            _ if !self.starting => false,
            None => {
                self.first_new = Some(id);
                true
            }
            Some(first) => first == id,
        }
    }

    fn completed(&mut self, id: Ident) {
        if self.running == Some(id) {
            self.running = None;
            self.last_ended = Some(id);
        } else if self.starting && self.first_new == Some(id) {
            self.first_new_ended = true;
            self.last_ended = Some(id);
        }
    }
}

/// The consumer task: takes each frame and handles it before the next, never
/// awaiting while it does. `end` signals its end even when it is aborted
/// before it first runs.
pub(super) async fn consume(shared: Arc<Shared>, from_codex: mpsc::Receiver<String>, end: End) {
    let _end = end;
    let mut from_codex = from_codex;
    loop {
        let Some(frame) = from_codex.recv().await else {
            let (mut out, mut st) = shared.lock();
            shared.close_step(&mut out, &mut st, Some(Fault::InputEnded));
            return;
        };
        if !shared.take_frame(frame) {
            return;
        }
    }
}

/// Signals the consumer's end, after the frame in hand and the receiver are
/// freed.
pub(super) struct End(pub(super) Arc<Ledger>);

impl Drop for End {
    fn drop(&mut self) {
        self.0.consumer_ended();
    }
}

impl Shared {
    /// Handles one frame under both locks; whether the engine is still open.
    fn take_frame(&self, frame: String) -> bool {
        let (mut out, mut st) = self.lock();
        if st.closed {
            return false;
        }
        if let Err(fault) = self.handle(&mut out, &mut st, frame) {
            self.close_step(&mut out, &mut st, Some(fault));
        }
        !st.closed
    }

    /// The frame is the engine's, at its capacity's class, until decoding
    /// ends; it is freed before its charge is released.
    fn handle(&self, out: &mut Output, st: &mut State, frame: String) -> Result<(), Fault> {
        let capacity = frame.capacity() as u64;
        if capacity > FRAME_HAND {
            return Err(Fault::OverBudget);
        }
        let hand = self.ledger.charge(class(capacity))?;
        let nesting = self.ledger.charge(NESTING)?;
        let routed = route(&mut Cx { out, st, ledger: &self.ledger }, &frame);
        drop(frame);
        drop(nesting);
        drop(hand);
        routed
    }
}

struct Cx<'a> {
    out: &'a mut Output,
    st: &'a mut State,
    ledger: &'a Arc<Ledger>,
}

fn route(cx: &mut Cx, text: &str) -> Result<(), Fault> {
    match wire::frame(text).ok_or(Fault::InvalidFrame)? {
        Frame::Response { id, reply } => respond(cx, id, reply),
        Frame::Request { id, method, params } => refuse(cx, id, method, params),
        Frame::Notification { method, params } => notification(cx, method, params),
    }
}

// ---------- responses ----------

fn respond(cx: &mut Cx, id: i64, reply: Result<&RawValue, CodexError>) -> Result<(), Fault> {
    let slot = cx.st.slots.iter().position(|s| s.waits_for(id)).ok_or(Fault::InvalidFrame)?;
    let (reply, charge) = match reply {
        Err(error) => {
            if slot == TURN_START {
                cx.st.turns.refused();
            }
            let mut buf = [0; LONG_TEXT];
            let message = wire::unescape(error.message, &mut buf).ok_or(Fault::InvalidFrame)?;
            let charge = cx.ledger.charge(class(message.len() as u64))?;
            (Reply::Codex { code: error.code, message: message.to_owned() }, Some(charge))
        }
        Ok(_) if slot == INITIALIZE => {
            cx.st.initialized = true;
            cx.out.send(cx.ledger, |w| _ = w.raw("{\"method\":\"initialized\"}")).map_err(unsent)?;
            (Reply::Connected, None)
        }
        Ok(result) if slot == THREAD_START => {
            let (opened, charge) = opened(cx, result)?;
            (Reply::Opened(opened), Some(charge))
        }
        Ok(result) if slot == TURN_START => {
            let (turn, charge) = started(cx, result)?;
            (Reply::Started(turn), Some(charge))
        }
        Ok(_) => (Reply::Interrupted, None),
    };
    cx.st.slots[slot].fill(reply, charge);
    Ok(())
}

fn unsent(unsent: Unsent) -> Fault {
    match unsent {
        Unsent::Fault(fault) => fault,
        Unsent::TooLong => Fault::OverBudget,
    }
}

/// A fact as reported, still borrowed.
enum Fact<'a> {
    Absent,
    Value(&'a str),
    Other,
}

impl Fact<'_> {
    fn bytes(&self) -> u64 {
        match self {
            Fact::Value(text) => class(text.len() as u64),
            _ => 0,
        }
    }

    fn owned(&self) -> Reported<String> {
        match self {
            Fact::Absent => Reported::Absent,
            Fact::Value(text) => Reported::Value((*text).to_owned()),
            Fact::Other => Reported::Other,
        }
    }
}

/// A value neither missing nor `null`.
fn present(raw: Option<&RawValue>) -> Option<&RawValue> {
    raw.filter(|raw| !wire::is_null(raw))
}

fn absent(raw: Option<&RawValue>) -> bool {
    present(raw).is_none()
}

/// A constant fact: taken only without escapes.
fn constant_fact(raw: Option<&RawValue>) -> Fact<'_> {
    match present(raw) {
        Some(raw) => wire::bare(raw, IDENTITY).map_or(Fact::Other, Fact::Value),
        None => Fact::Absent,
    }
}

/// A text fact, unescaped into `buf`.
fn text_fact<'b>(raw: Option<&RawValue>, buf: &'b mut [u8]) -> Fact<'b> {
    match present(raw) {
        Some(raw) => wire::unescape(raw, buf).map_or(Fact::Other, Fact::Value),
        None => Fact::Absent,
    }
}

fn reported<T>(raw: Option<&RawValue>, read: impl FnOnce(&RawValue) -> Option<T>) -> Reported<T> {
    match present(raw) {
        Some(raw) => read(raw).map_or(Reported::Other, Reported::Value),
        None => Reported::Absent,
    }
}

/// The `thread/start` reply: the thread is held, and its facts are kept.
fn opened(cx: &mut Cx, result: &RawValue) -> Result<(OpenedThread, super::Charge), Fault> {
    let [thread, approval, sandbox, model, provider, cwd] =
        wire::pick(result.get(), ["thread", "approvalPolicy", "sandbox", "model", "modelProvider", "cwd"]).unwrap_or_default();
    let [id, ephemeral, environments] = thread.and_then(|t| wire::pick(t.get(), ["id", "ephemeral", "environments"])).ok_or(Fault::InvalidFrame)?;
    let mut id_buf = [0; IDENTITY];
    let id = id.and_then(|id| wire::identity(id, &mut id_buf)).ok_or(Fault::InvalidFrame)?;
    let (sandbox_type, network_access) = match present(sandbox).map(|s| wire::pick(s.get(), ["type", "networkAccess"])) {
        None => (Fact::Absent, Reported::Absent),
        Some(Some([kind, network])) => (constant_fact(kind), reported(network, wire::boolean)),
        Some(None) => (Fact::Other, Reported::Other),
    };
    let approval = constant_fact(approval);
    let (mut model_buf, mut provider_buf, mut cwd_buf) = ([0; IDENTITY], [0; IDENTITY], [0; LONG_TEXT]);
    let model = text_fact(model, &mut model_buf);
    let provider = text_fact(provider, &mut provider_buf);
    let cwd = text_fact(cwd, &mut cwd_buf);
    let facts = [&approval, &sandbox_type, &model, &provider, &cwd];
    let charge = cx.ledger.charge(class(id.len() as u64) + facts.iter().map(|f| f.bytes()).sum::<u64>())?;
    let opened = OpenedThread {
        thread: ThreadId(id.to_owned()),
        approval_policy: approval.owned(),
        sandbox_type: sandbox_type.owned(),
        network_access,
        ephemeral: reported(ephemeral, wire::boolean),
        environments: reported(environments, |e| wire::elements(e.get(), &mut |_| {}).and_then(|n| u32::try_from(n).ok())),
        model: model.owned(),
        model_provider: provider.owned(),
        cwd: cwd.owned(),
    };
    cx.st.thread = Some(Held { id: Ident::new(id), open: true });
    Ok((opened, charge))
}

/// The `turn/start` reply: an identity `turn.id` and `status: "inProgress"`.
fn started(cx: &mut Cx, result: &RawValue) -> Result<(TurnId, super::Charge), Fault> {
    let turn = wire::pick(result.get(), ["turn"]).and_then(|[turn]| turn).ok_or(Fault::InvalidFrame)?;
    let [id, status] = wire::pick(turn.get(), ["id", "status"]).ok_or(Fault::InvalidFrame)?;
    let mut buf = [0; IDENTITY];
    let id = id.and_then(|id| wire::identity(id, &mut buf)).ok_or(Fault::InvalidFrame)?;
    if status.and_then(wire::constant) != Some("inProgress") {
        return Err(Fault::InvalidFrame);
    }
    let open = cx.st.thread.as_ref().is_some_and(|t| t.open);
    cx.st.turns.started(Ident::new(id), open)?;
    let charge = cx.ledger.charge(class(id.len() as u64))?;
    Ok((TurnId(id.to_owned()), charge))
}

// ---------- requests ----------

/// Any request is refused until admission (step 2): `Refused`, then the
/// refusal is written.
fn refuse(cx: &mut Cx, id: &RawValue, method: &RawValue, params: Option<&RawValue>) -> Result<(), Fault> {
    let mut name_buf = [0; IDENTITY];
    let name = wire::identity(method, &mut name_buf).ok_or(Fault::InvalidFrame)?;
    let json = params.map_or("{}", RawValue::get);
    let [thread_id, thread, conversation] = params.and_then(|p| wire::pick(p.get(), ["threadId", "thread", "conversationId"])).unwrap_or_default();
    let thread_dot_id = thread.and_then(|t| wire::pick(t.get(), ["id"])).and_then(|[id]| id);
    // The first of them that is an identity.
    let mut thread_buf = [0; IDENTITY];
    let thread = [thread_id, thread_dot_id, conversation].into_iter().flatten().find(|raw| wire::identity(raw, &mut [0; IDENTITY]).is_some());
    let thread = thread.and_then(|raw| wire::identity(raw, &mut thread_buf));
    publish(cx, &[name, thread.unwrap_or_default(), json], || EmbeddedEvent::Refused {
        method: name.to_owned(),
        thread: thread.map(str::to_owned),
        json: json.to_owned(),
    })?;
    cx.out
        .send(cx.ledger, |w| _ = w.raw("{\"id\":").raw(id.get()).raw(",\"error\":{\"code\":-32601,\"message\":\"not supported by this client\"}}"))
        .map_err(unsent)
}

// ---------- notifications ----------

/// What the routing table needs of a method.
#[derive(Clone, Copy)]
enum Route {
    ThreadStarted,
    ThreadClosed,
    ThreadStatus,
    TokenUsage,
    RateLimits,
    Turn(Method),
    Item(Method),
    Delta(Method),
    Error,
    Warning(Method),
}

fn table(method: &str) -> Option<Route> {
    Some(match method {
        "thread/started" => Route::ThreadStarted,
        "thread/closed" => Route::ThreadClosed,
        "thread/status/changed" => Route::ThreadStatus,
        "thread/tokenUsage/updated" => Route::TokenUsage,
        "account/rateLimits/updated" => Route::RateLimits,
        "turn/started" => Route::Turn(Method::TurnStarted),
        "turn/completed" => Route::Turn(Method::TurnCompleted),
        "item/started" => Route::Item(Method::ItemStarted),
        "item/completed" => Route::Item(Method::ItemCompleted),
        "item/agentMessage/delta" => Route::Delta(Method::AgentMessageDelta),
        "item/reasoning/summaryTextDelta" => Route::Delta(Method::ReasoningSummaryDelta),
        "item/reasoning/summaryPartAdded" => Route::Delta(Method::ReasoningSummaryPart),
        "item/reasoning/textDelta" => Route::Delta(Method::ReasoningDelta),
        "error" => Route::Error,
        "warning" => Route::Warning(Method::Warning),
        "configWarning" => Route::Warning(Method::ConfigWarning),
        _ => return None,
    })
}

/// A notification's fields, still borrowed.
struct Note<'a> {
    method: Method,
    turn: Option<&'a str>,
    item: Option<(&'a str, Option<&'a str>)>,
    status: Option<&'a str>,
}

const PARAMS: [&str; 9] = ["threadId", "thread", "conversationId", "turnId", "turn", "item", "itemId", "status", "willRetry"];

fn notification(cx: &mut Cx, method: &RawValue, params: Option<&RawValue>) -> Result<(), Fault> {
    let mut name_buf = [0; IDENTITY];
    let name = wire::identity(method, &mut name_buf).ok_or(Fault::InvalidFrame)?;
    let json = params.map_or("{}", RawValue::get);
    let fields = params.and_then(|p| wire::pick(p.get(), PARAMS)).unwrap_or_default();
    let [thread_id, thread, conversation, ..] = fields;
    let thread_dot_id = thread.and_then(|t| wire::pick(t.get(), ["id"])).and_then(|[id]| id);
    let held = cx.st.thread.as_ref().map(|h| h.id);
    // 5. A thread other than the held one's, or any before one is held.
    let mut on_held = [false; 3];
    let mut buf = [0; IDENTITY];
    for (at, field) in [thread_id, thread_dot_id, conversation].into_iter().enumerate() {
        let Some(id) = field.and_then(|f| wire::identity(f, &mut buf)) else {
            continue;
        };
        if held.is_some_and(|h| h.as_str() == id) {
            on_held[at] = true;
            continue;
        }
        return publish(cx, &[name, id, json], || EmbeddedEvent::Unheld { method: name.to_owned(), thread: id.to_owned(), json: json.to_owned() });
    }
    let constant = wire::constant(method);
    // 6. Remote control.
    let status = fields[7].filter(|s| s.get().starts_with('"'));
    if let (Some("remoteControl/status/changed"), Some(status)) = (constant, status) {
        let status = match wire::constant(status) {
            Some("disabled") => RemoteControl::Disabled,
            Some("connecting") => RemoteControl::Connecting,
            Some("connected") => RemoteControl::Connected,
            Some("errored") => RemoteControl::Errored,
            _ => RemoteControl::Other,
        };
        cx.st.remote = Some(status);
        return publish(cx, &[], || EmbeddedEvent::RemoteControl { status });
    }
    // 7 and 8. The turn rule, then the table.
    let (mut turn_buf, mut item_buf) = ([0; IDENTITY], [0; IDENTITY]);
    let note = match constant.and_then(table) {
        Some(route) => decode(cx, route, &fields, on_held, &mut turn_buf, &mut item_buf)?,
        None => None,
    };
    let Some(note) = note else {
        // 9. Anything else.
        let thread = on_held.contains(&true).then_some(held).flatten();
        let thread = thread.as_ref().map(Ident::as_str);
        return publish(cx, &[name, thread.unwrap_or_default(), json], || EmbeddedEvent::Unmapped {
            method: name.to_owned(),
            thread: thread.map(str::to_owned),
            json: json.to_owned(),
        });
    };
    let (item_id, item_type) = note.item.map_or((None, None), |(id, kind)| (Some(id), kind));
    let parts = [note.turn, item_id, item_type, note.status, Some(json)].map(Option::unwrap_or_default);
    publish(cx, &parts, || EmbeddedEvent::Notification {
        method: note.method,
        turn: note.turn.map(|t| TurnId(t.to_owned())),
        item: item_id.map(|id| ItemRef { id: id.to_owned(), item_type: item_type.map(str::to_owned) }),
        status: note.status.map(str::to_owned),
        json: json.to_owned(),
    })
}

/// Rules 7 and 8: a turn the rule forbids faults; otherwise the method's
/// required fields, or `None` when one does not decode.
fn decode<'a>(
    cx: &mut Cx,
    route: Route,
    fields: &[Option<&'a RawValue>; 9],
    on_held: [bool; 3],
    turn_buf: &'a mut [u8; IDENTITY],
    item_buf: &'a mut [u8; IDENTITY],
) -> Result<Option<Note<'a>>, Fault> {
    let [thread_id, _, _, turn_id, turn, item, item_id, status, will_retry] = *fields;
    let turn_fields = turn.and_then(|t| wire::pick(t.get(), ["id", "status"]));
    let thread_ok = match route {
        Route::RateLimits => true,
        Route::ThreadStarted => on_held[1],
        Route::Warning(_) => on_held[0] || absent(thread_id),
        _ => on_held[0],
    };
    let turn_raw = match route {
        Route::Turn(_) => turn_fields.and_then(|[id, _]| id),
        Route::TokenUsage | Route::Item(_) | Route::Delta(_) | Route::Error => turn_id,
        _ => None,
    };
    // 7. A turn named on the held thread must be one the rule admits.
    let mut turn_name = None;
    if let Some(raw) = turn_raw.filter(|r| r.get().starts_with('"'))
        && on_held[0]
    {
        let id = wire::identity(raw, turn_buf).ok_or(Fault::InvalidFrame)?;
        if !cx.st.turns.admits(Ident::new(id)) {
            return Err(Fault::WrongTurn);
        }
        turn_name = Some(id);
    }
    if !thread_ok || (turn_raw.is_some() && turn_name.is_none()) {
        return Ok(None);
    }
    let mut note = Note { method: Method::RateLimits, turn: turn_name, item: None, status: None };
    note.method = match route {
        Route::RateLimits => Method::RateLimits,
        Route::ThreadStarted => Method::ThreadStarted,
        Route::ThreadClosed => Method::ThreadClosed,
        Route::TokenUsage => Method::TokenUsage,
        Route::Warning(method) => method,
        Route::ThreadStatus => {
            let kind = status.and_then(|s| wire::pick(s.get(), ["type"])).and_then(|[kind]| kind).and_then(wire::constant);
            let Some(kind) = kind else { return Ok(None) };
            note.status = Some(kind);
            Method::ThreadStatus
        }
        Route::Turn(method) => {
            let Some(status) = turn_fields.and_then(|[_, status]| status).and_then(wire::constant) else {
                return Ok(None);
            };
            if method == Method::TurnCompleted && status == "inProgress" {
                return Err(Fault::InvalidFrame);
            }
            note.status = Some(status);
            method
        }
        Route::Item(method) => {
            let [id, kind] = item.and_then(|i| wire::pick(i.get(), ["id", "type"])).unwrap_or_default();
            let (Some(id), Some(kind)) = (id.and_then(|id| wire::identity(id, item_buf)), kind.and_then(wire::constant)) else {
                return Ok(None);
            };
            note.item = Some((id, Some(kind)));
            method
        }
        Route::Delta(method) => {
            let Some(id) = item_id.and_then(|id| wire::identity(id, item_buf)) else {
                return Ok(None);
            };
            note.item = Some((id, None));
            method
        }
        Route::Error => match will_retry.and_then(wire::boolean) {
            Some(true) => Method::Retrying,
            Some(false) => Method::Error,
            None => return Ok(None),
        },
    };
    if matches!(route, Route::Turn(_) | Route::TokenUsage | Route::Item(_) | Route::Delta(_) | Route::Error) && note.turn.is_none() {
        return Ok(None);
    }
    // Effects, applied before the event is published.
    match note.method {
        Method::TurnCompleted => cx.st.turns.completed(Ident::new(note.turn.unwrap_or_default())),
        Method::ThreadClosed => {
            if let Some(held) = cx.st.thread.as_mut() {
                held.open = false;
            }
        }
        _ => {}
    }
    Ok(Some(note))
}

// ---------- events ----------

/// Publishes an event into the ring: its strings (`parts`) charged before
/// `make` allocates them; a full ring faults.
fn publish(cx: &mut Cx, parts: &[&str], make: impl FnOnce() -> EmbeddedEvent) -> Result<(), Fault> {
    let ring = cx.st.ring.as_mut().ok_or(Fault::OverBudget)?;
    if ring.is_full() {
        return Err(Fault::OverBudget);
    }
    let charge = cx.ledger.charge_event(parts.iter().map(|p| class(p.len() as u64)).sum())?;
    let event = make();
    debug_assert_eq!(event_bytes(&event), charge_bytes(parts));
    ring.push(event, charge);
    if let Some(reader) = cx.st.reader.take() {
        reader.wake();
    }
    Ok(())
}

fn charge_bytes(parts: &[&str]) -> u64 {
    parts.iter().map(|p| class(p.len() as u64)).sum()
}
