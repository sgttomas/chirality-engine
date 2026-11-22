//! The embedded engine's maximum, measured: a counting allocator prices every
//! allocation the engine makes at its class (SWBPIPE's `allocation_envelope`),
//! old and new together while a buffer grows, and the peak must stay within
//! `max_bytes`. Once `finished`, with only its handles holding the engine, it
//! holds no more than the fixed row; once they drop, nothing.
//!
//! What the host makes is made paused and not counted, until the engine owns
//! it: the configuration from `new`, and each frame from when it is sent
//! (the host waits for the engine to take it before the next). Every scenario
//! runs once unmeasured first, so one-time runtime state is not counted.

mod support;

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::future::{Future, poll_fn};
use std::pin::{Pin, pin};
use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};
use std::task::Poll;

use chirality_engine::embedded::*;
use serde_json::{Value, json};
use support::{Host, LIMITS};

// ---------- the counting allocator ----------

const SLOTS: usize = 1 << 16;
const EMPTY: usize = 0;
const GONE: usize = usize::MAX;
static KEYS: [AtomicUsize; SLOTS] = [const { AtomicUsize::new(EMPTY) }; SLOTS];
static PRICES: [AtomicUsize; SLOTS] = [const { AtomicUsize::new(0) }; SLOTS];

std::thread_local! {
    static MEASURED: Cell<bool> = const { Cell::new(false) };
    static PAUSED: Cell<bool> = const { Cell::new(false) };
    static LIVE: Cell<u64> = const { Cell::new(0) };
    static PEAK: Cell<u64> = const { Cell::new(0) };
    static START: Cell<u64> = const { Cell::new(0) };
}

/// The fixed row: control block, consumer task, terminal slot, namespace, tool names.
const FIXED_ROW: u64 = 64 * 1024;

/// SWBPIPE's `allocation_envelope`: max(16, the next power of two), 0 for 0.
fn price(size: usize) -> u64 {
    if size == 0 { 0 } else { (size as u64).next_power_of_two().max(16) }
}

fn measured() -> bool {
    MEASURED.try_with(Cell::get).unwrap_or(false)
}

fn slot(ptr: usize, at: usize) -> usize {
    ((ptr >> 4).wrapping_mul(0x9e37_79b9_7f4a_7c15) >> 48).wrapping_add(at) % SLOTS
}

fn track(ptr: usize, size: usize) {
    for at in 0..SLOTS {
        let i = slot(ptr, at);
        let key = KEYS[i].load(Relaxed);
        if key == EMPTY || key == GONE {
            KEYS[i].store(ptr, Relaxed);
            PRICES[i].store(price(size) as usize, Relaxed);
            LIVE.with(|live| live.set(live.get() + price(size)));
            PEAK.with(|peak| peak.set(peak.get().max(LIVE.with(Cell::get))));
            return;
        }
    }
    std::process::abort();
}

fn untrack(ptr: usize) {
    for at in 0..SLOTS {
        let i = slot(ptr, at);
        match KEYS[i].load(Relaxed) {
            EMPTY => return,
            key if key == ptr => {
                KEYS[i].store(GONE, Relaxed);
                let priced = PRICES[i].load(Relaxed) as u64;
                LIVE.with(|live| live.set(live.get() - priced));
                return;
            }
            _ => {}
        }
    }
}

struct Counting;

// SAFETY: every call is passed to `System`; the bookkeeping allocates nothing.
// `realloc` is the provided one (allocate, copy, free), so a growing buffer's
// old and new blocks are live together.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let ptr = unsafe { System.alloc(layout) };
        if !ptr.is_null() && measured() && !PAUSED.with(Cell::get) {
            track(ptr as usize, layout.size());
        }
        ptr
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        if measured() {
            untrack(ptr as usize);
        }
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static ALLOCATOR: Counting = Counting;

fn pause(f: &mut dyn FnMut()) {
    let was = PAUSED.with(|p| p.replace(true));
    f();
    PAUSED.with(|p| p.set(was));
}

/// An allocation the host made, the engine's from now on.
fn adopt(ptr: *const u8, capacity: usize) {
    if measured() && capacity > 0 {
        track(ptr as usize, capacity);
    }
}

/// The engine has finished: with only its handles holding it, within the fixed row.
fn at_finished() {
    if measured() {
        let held = LIVE.with(Cell::get) - START.with(Cell::get);
        assert!(held <= FIXED_ROW, "{held} bytes held once finished, over the fixed row");
    }
}

const METER: support::Meter = support::Meter { pause, adopt, finished: at_finished };

/// `f`'s result, made as the host.
fn host<T>(f: impl FnOnce() -> T) -> T {
    let mut f = Some(f);
    let mut out = None;
    pause(&mut || out = Some((f.take().unwrap())()));
    out.unwrap()
}

/// Runs a scenario: its peak above where it began within the maximum, and
/// nothing left once every handle, the host's too, has dropped.
async fn scenario<F: Future<Output = ()>>(label: &str, limits: &EmbeddedLimits, run: impl FnOnce() -> F) {
    let start = LIVE.with(Cell::get);
    START.with(|s| s.set(start));
    PEAK.with(|p| p.set(start));
    // A buffer whose charge is never released holds `finished`: fail, not hang.
    let ran = tokio::time::timeout(std::time::Duration::from_secs(20), run()).await;
    assert!(ran.is_ok(), "{label}: timed out");
    if !measured() {
        return;
    }
    let (peak, left) = (PEAK.with(Cell::get) - start, LIVE.with(Cell::get) as i64 - start as i64);
    let max = EmbeddedEngine::max_bytes(limits);
    host(|| eprintln!("{label}: peak {peak} of {max}, left {left}"));
    assert!(peak <= max, "{label}: peak {peak} over {max}");
    assert!(peak > 0, "{label}: nothing counted");
    assert_eq!(left, 0, "{label}: bytes left after every handle dropped");
}

// ---------- a scripted host ----------

struct Session {
    engine: EmbeddedEngine,
    events: EmbeddedEvents,
    host: Host,
}

const THREAD: &str = "thread-1";

/// A session whose tools are large, so configuration kept past `open_thread`
/// or close would show at `finished`.
fn session(limits: EmbeddedLimits) -> Session {
    let (transport, host_side) = host(support::transport);
    let config = host(|| EmbeddedConfig {
        transport,
        work_dir: "/scratch/work".into(),
        namespace: "swbpipe".into(),
        namespace_description: "SWBPIPE tools.".into(),
        tools: format!(r#"[{{"type":"function","name":"swbpipe_solve","description":"{}","inputSchema":{{"type":"object"}}}}]"#, "d".repeat(100_000))
            .into_boxed_str()
            .into_string(),
        limits,
    });
    METER.adopt_config(&config);
    let (engine, events) = EmbeddedEngine::new(config).unwrap();
    Session { engine, events, host: host_side }
}

/// Sends a frame as the host does: its capacity its length, as the host's
/// framing gives it.
async fn feed(host: &Host, frame: impl FnOnce() -> String) {
    let frame = self::host(|| frame().into_boxed_str().into_string());
    assert!(METER.feed(host, frame).await, "the engine closed");
}

async fn written(host: &mut Host) -> Value {
    let frame = host.from_engine.recv().await.unwrap();
    self::host(|| serde_json::from_str(&frame).unwrap())
}

async fn reply(host: &mut Host, result: &'static str) {
    let id = written(host).await["id"].as_i64().unwrap();
    feed(host, || format!("{{\"id\":{id},\"result\":{result}}}")).await;
}

async fn note(host: &Host, method: &'static str, params: impl FnOnce() -> Value) {
    feed(host, || json!({"method": method, "params": params()}).to_string()).await;
}

fn drain(events: &mut EmbeddedEvents) -> usize {
    let mut n = 0;
    while events.try_recv().is_ok() {
        n += 1;
    }
    n
}

async fn closed(events: &mut EmbeddedEvents) -> Option<Fault> {
    loop {
        if let EmbeddedEvent::Closed { fault } = events.recv().await.unwrap() {
            return fault;
        }
    }
}

async fn run_turn(engine: &EmbeddedEngine, host: &mut Host, turn: &'static str) {
    let mut send = pin!(engine.send_text("hi"));
    assert!(poll_once(send.as_mut()).await.is_pending());
    let id = written(host).await["id"].as_i64().unwrap();
    feed(host, || json!({"id": id, "result": {"turn": {"id": turn, "status": "inProgress"}}}).to_string()).await;
    send.await.unwrap();
}

impl Session {
    /// Connected, remote control `Disabled`, the thread held.
    async fn open(limits: EmbeddedLimits) -> Session {
        let mut s = session(limits);
        {
            let mut connect = pin!(s.engine.connect());
            assert!(poll_once(connect.as_mut()).await.is_pending());
            reply(&mut s.host, "{}").await;
            connect.await.unwrap();
        }
        written(&mut s.host).await;
        note(&s.host, "remoteControl/status/changed", || json!({"status": "disabled"})).await;
        let options = host(|| ThreadOptions {
            model: None,
            provider: None,
            base_instructions: None,
            policy: ThreadPolicy { sandbox: SandboxMode::ReadOnly, approval: ApprovalPolicy::Never },
        });
        {
            let mut open = pin!(s.engine.open_thread(&options));
            assert!(poll_once(open.as_mut()).await.is_pending());
            reply(&mut s.host, r#"{"thread":{"id":"thread-1","ephemeral":true,"environments":[]},"model":"m","modelProvider":"p","cwd":"/scratch/work"}"#)
                .await;
            assert_eq!(open.await.unwrap().thread.0, THREAD);
        }
        drain(&mut s.events);
        s
    }

    /// Finished, holding only the fixed row; then every handle dropped.
    async fn finish(self) {
        self.engine.finished().await;
        at_finished();
        drop(self);
    }
}

async fn poll_once<F: Future>(future: Pin<&mut F>) -> Poll<F::Output> {
    let mut future = Some(future);
    poll_fn(|cx| Poll::Ready(future.take().unwrap().poll(cx))).await
}

fn nested(depth: usize) -> String {
    format!("{{\"threadId\":\"{THREAD}\",\"a\":{}{}}}", "[".repeat(depth - 1), "]".repeat(depth - 1))
}

// ---------- the scenarios ----------

#[tokio::test(flavor = "current_thread")]
async fn the_engine_stays_within_its_maximum() {
    for measured in [false, true] {
        MEASURED.with(|m| m.set(measured));
        scenarios(LIMITS).await;
    }
    MEASURED.with(|m| m.set(false));
}

async fn scenarios(limits: EmbeddedLimits) {
    scenario("recorded text turns", &limits, || async {
        let lines = host(|| support::recording("embedded_text_turn"));
        support::replay(lines, &METER).await;
    })
    .await;

    scenario("recorded retry", &limits, || async {
        let lines = host(|| support::recording("embedded_retry"));
        support::replay(lines, &METER).await;
    })
    .await;

    scenario("close before connect", &limits, || async {
        let s = session(limits);
        s.engine.close();
        s.finish().await;
    })
    .await;

    scenario("depth 127, then 128", &limits, || async {
        let mut s = Session::open(limits).await;
        feed(&s.host, || format!("{{\"method\":\"x/deep\",\"params\":{}}}", nested(126))).await;
        assert!(matches!(s.events.try_recv(), Ok(EmbeddedEvent::Unmapped { .. })));
        feed(&s.host, || format!("{{\"method\":\"x/deep\",\"params\":{}}}", nested(127))).await;
        assert_eq!(closed(&mut s.events).await, Some(Fault::InvalidFrame));
        s.finish().await;
    })
    .await;

    scenario("escaped keys, identities and constants", &limits, || async {
        let mut s = Session::open(limits).await;
        run_turn(&s.engine, &mut s.host, "turn-1").await;
        // `~` stands for a backslash.
        let escaped = [
            // An escaped identity is unescaped: the held thread and its turn.
            r#"{"method":"item/started","params":{"threadId":"thread~u002d1","turnId":"turn~u002d1","item":{"id":"item\"1\\","type":"agentMessage"}}}"#,
            // An escaped constant is not taken.
            r#"{"method":"thread/status/changed","params":{"threadId":"thread-1","status":{"type":"~u0061ctive"}}}"#,
            r#"{"method":"turn\/started","params":{"threadId":"thread-1","turn":{"id":"turn-1","status":"inProgress"}}}"#,
            // An escaped key matches nothing.
            r#"{"method":"thread/status/changed","params":{"thread~u0049d":"other","threadId":"thread-1","status":{"type":"idle"}}}"#,
            // A request with an escaped id and callId is refused.
            r#"{"id":"r~u0031","method":"item/tool/call","params":{"threadId":"thread-1","turnId":"turn-1","callId":"call_\"文\\x"}}"#,
        ];
        for frame in escaped {
            feed(&s.host, || frame.replace('~', "\\")).await;
        }
        let refusal = written(&mut s.host).await;
        assert_eq!(refusal["id"], "r1");
        let kinds: Vec<_> = host(|| std::iter::from_fn(|| s.events.try_recv().ok()).map(|e| support::name(&e)).collect());
        assert_eq!(kinds, ["ItemStarted agentMessage", "Unmapped", "Unmapped", "ThreadStatus idle", "Refused"]);
        feed(&s.host, || r#"{"m~u0065thod":"turn/started"}"#.replace('~', "\\")).await;
        assert_eq!(closed(&mut s.events).await, Some(Fault::InvalidFrame));
        s.finish().await;
    })
    .await;

    scenario("replies uncollected across repeated calls", &limits, || async {
        let mut s = Session::open(limits).await;
        run_turn(&s.engine, &mut s.host, "turn-1").await;
        for _ in 0..50 {
            let mut stop = pin!(s.engine.interrupt());
            assert!(poll_once(stop.as_mut()).await.is_pending());
            reply(&mut s.host, "{}").await;
            for _ in 0..20 {
                assert!(matches!(s.engine.interrupt().await, Err(EmbeddedError::Invalid { .. })));
                assert!(matches!(s.engine.send_text("hi").await, Err(EmbeddedError::Invalid { .. })));
            }
            stop.await.unwrap();
            // Dropped before its reply: discarded when it comes.
            {
                let stop = pin!(s.engine.interrupt());
                assert!(poll_once(stop).await.is_pending());
            }
            reply(&mut s.host, "{}").await;
        }
        s.engine.close();
        assert_eq!(closed(&mut s.events).await, None);
        s.finish().await;
    })
    .await;

    scenario("the ring filled, drained and refilled", &limits, || async {
        let mut s = Session::open(limits).await;
        let slots = limits.event_bytes as usize / 512;
        for pad in [0, 300, 0] {
            for n in 0..slots {
                note(&s.host, "account/rateLimits/updated", || json!({"n": n, "pad": "p".repeat(pad)})).await;
            }
            assert_eq!(drain(&mut s.events), slots);
        }
        for _ in 0..3 {
            for _ in 0..2 {
                note(&s.host, "account/rateLimits/updated", || json!({"pad": "p".repeat(120_000)})).await;
            }
            assert_eq!(drain(&mut s.events), 2);
        }
        for n in 0..=slots {
            note(&s.host, "account/rateLimits/updated", || json!({"n": n})).await;
        }
        assert_eq!(closed(&mut s.events).await, Some(Fault::OverBudget));
        s.finish().await;
    })
    .await;

    scenario("close mid-turn with a caller waiting and events unread", &limits, || async {
        let mut s = Session::open(limits).await;
        run_turn(&s.engine, &mut s.host, "turn-1").await;
        note(&s.host, "thread/status/changed", || json!({"threadId": THREAD, "status": {"type": "active"}})).await;
        {
            let mut stop = pin!(s.engine.interrupt());
            assert!(poll_once(stop.as_mut()).await.is_pending());
            s.engine.close();
            assert_eq!(stop.await, Err(EmbeddedError::Closed { fault: None }));
        }
        assert_eq!(closed(&mut s.events).await, None);
        s.finish().await;
    })
    .await;

    scenario("close as Codex's input ends", &limits, || async {
        let mut s = Session::open(limits).await;
        run_turn(&s.engine, &mut s.host, "turn-1").await;
        let (closed_to_engine, _) = host(|| tokio::sync::mpsc::channel(1));
        drop(std::mem::replace(&mut s.host.to_engine, closed_to_engine));
        assert_eq!(closed(&mut s.events).await, Some(Fault::InputEnded));
        s.finish().await;
    })
    .await;

    scenario("close as the host's output closes", &limits, || async {
        let mut s = Session::open(limits).await;
        let (_, gone) = host(|| tokio::sync::mpsc::channel(1));
        drop(std::mem::replace(&mut s.host.from_engine, gone));
        assert_eq!(s.engine.send_text("hi").await, Err(EmbeddedError::Closed { fault: Some(Fault::OutputClosed) }));
        s.finish().await;
    })
    .await;

    scenario("close as the host's output is full", &limits, || async {
        let mut s = Session::open(limits).await;
        run_turn(&s.engine, &mut s.host, "turn-1").await;
        let mut n = 0;
        // Refusals go out until the host's channel is full.
        while !matches!(s.events.try_recv(), Ok(EmbeddedEvent::Closed { .. })) {
            feed(&s.host, || json!({"id": n, "method": "item/tool/call", "params": {}}).to_string()).await;
            n += 1;
            assert!(n < 200);
        }
        s.finish().await;
    })
    .await;

    scenario("close by dropping the engine mid-connect", &limits, || async {
        let s = session(limits);
        let finished = s.engine.finished();
        {
            let connect = pin!(s.engine.connect());
            assert!(poll_once(connect).await.is_pending());
        }
        drop(s);
        finished.await;
        at_finished();
    })
    .await;
}
