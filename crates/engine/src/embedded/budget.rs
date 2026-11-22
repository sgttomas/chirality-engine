//! Charges, their guards, and the maximum: every byte the engine holds is
//! charged before it is allocated, at SWBPIPE's prices (each allocation costs
//! its class, the next power of two of at least 16 bytes).

use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::SeqCst};

use tokio::sync::Notify;

use super::types::{EmbeddedEvent, EmbeddedLimits, Fault};

const KIB: u64 = 1024;

/// A frame's text at most (the host's 128 KiB includes the newline).
pub(crate) const FRAME_TEXT: usize = 131_071;
/// The frame in hand: its capacity's class, at most this.
pub(crate) const FRAME_HAND: u64 = 128 * KIB;
/// Decoding: serde_json's nesting stack and the facts read.
pub(crate) const DECODING: u64 = 4 * KIB;
/// serde_json's nesting stack while one value is read: at most 127 bytes,
/// growing 8, 16, ... 128, its old and new buffers live together as it grows.
pub(crate) const NESTING: u64 = 256;
/// The deepest a frame nests.
pub(crate) const DEPTH: usize = 127;
/// One event's place in the ring.
pub(crate) const RING_SLOT: u64 = 128;
/// Unread events' bytes per ring slot.
pub(crate) const EVENT_BYTES_PER_SLOT: u64 = 512;
/// The record: one preallocated entry per request, and two identities.
const RECORD_ENTRY: u64 = 56;
const RECORD_IDENTITIES: u64 = 512;
/// One reply waiting in its method's slot.
pub(crate) const REPLY: u64 = 4 * KIB;
pub(crate) const REPLY_SLOTS: usize = 4;
/// The writer buffer, and one copy being sent.
pub(crate) const WRITER: u64 = 128 * KIB;
/// Configuration until `open_thread` writes it.
pub(crate) const TOOLS: u64 = 128 * KIB;
pub(crate) const DESCRIPTION: u64 = 4 * KIB;
pub(crate) const WORK_DIR: u64 = KIB;
/// Control block, consumer task, terminal slot, namespace, tool names.
pub(crate) const FIXED: u64 = 64 * KIB;

/// The price of one allocation of `size` bytes: max(16, next power of two),
/// 0 for 0 (SWBPIPE's `allocation_envelope`).
pub const fn class(size: u64) -> u64 {
    match size {
        0 => 0,
        _ => match size.checked_next_power_of_two() {
            Some(p) if p < 16 => 16,
            Some(p) => p,
            None => u64::MAX,
        },
    }
}

/// The event ring's slots.
pub(crate) const fn ring_slots(limits: &EmbeddedLimits) -> u64 {
    limits.event_bytes as u64 / EVENT_BYTES_PER_SLOT
}

/// The most the engine holds with these limits; the host reserves it before
/// `new`.
pub const fn max_bytes(limits: &EmbeddedLimits) -> u64 {
    let calls = limits.calls_per_turn as u64;
    FRAME_HAND
        + DECODING
        + class(ring_slots(limits) * RING_SLOT)
        + limits.event_bytes as u64
        + class(calls * RECORD_ENTRY)
        + calls * RECORD_IDENTITIES
        + REPLY_SLOTS as u64 * REPLY
        + 2 * WRITER
        + TOOLS
        + DESCRIPTION
        + WORK_DIR
        + FIXED
}

/// What the engine has charged, and whether it has finished: closed, its
/// consumer ended, and nothing outside the fixed row held.
pub(crate) struct Ledger {
    used: AtomicU64,
    events: AtomicU64,
    max: u64,
    event_max: u64,
    closed: AtomicBool,
    consumer: AtomicBool,
    finished: AtomicBool,
    pub(crate) notify: Arc<Notify>,
}

impl Ledger {
    pub(crate) fn new(limits: &EmbeddedLimits) -> Arc<Ledger> {
        Arc::new(Ledger {
            used: AtomicU64::new(FIXED),
            events: AtomicU64::new(0),
            max: max_bytes(limits),
            event_max: limits.event_bytes as u64,
            closed: AtomicBool::new(false),
            consumer: AtomicBool::new(false),
            finished: AtomicBool::new(false),
            notify: Arc::new(Notify::new()),
        })
    }

    /// Charges `bytes`, or refuses a charge past the maximum.
    pub(crate) fn charge(self: &Arc<Self>, bytes: u64) -> Result<Charge, Fault> {
        add(&self.used, bytes, self.max)?;
        Ok(Charge { ledger: self.clone(), bytes, event: false })
    }

    /// Charges an unread event's strings, against `event_bytes` too.
    pub(crate) fn charge_event(self: &Arc<Self>, bytes: u64) -> Result<Charge, Fault> {
        add(&self.events, bytes, self.event_max)?;
        if let Err(fault) = add(&self.used, bytes, self.max) {
            self.events.fetch_sub(bytes, SeqCst);
            return Err(fault);
        }
        Ok(Charge { ledger: self.clone(), bytes, event: true })
    }

    /// Releases an unread event's strings, charged by [`Ledger::charge_event`]
    /// and kept by the ring since.
    pub(crate) fn release_event(&self, bytes: u64) {
        self.events.fetch_sub(bytes, SeqCst);
        self.release(bytes);
    }

    fn release(&self, bytes: u64) {
        self.used.fetch_sub(bytes, SeqCst);
        self.check_finished();
    }

    pub(crate) fn mark_closed(&self) {
        self.closed.store(true, SeqCst);
        self.check_finished();
    }

    pub(crate) fn consumer_started(&self) {
        self.consumer.store(true, SeqCst);
    }

    pub(crate) fn consumer_ended(&self) {
        self.consumer.store(false, SeqCst);
        self.check_finished();
    }

    pub(crate) fn is_finished(&self) -> bool {
        self.finished.load(SeqCst)
    }

    /// Bytes charged now, the fixed row included.
    #[cfg(test)]
    pub(crate) fn used(&self) -> u64 {
        self.used.load(SeqCst)
    }

    fn check_finished(&self) {
        let done = self.closed.load(SeqCst) && !self.consumer.load(SeqCst) && self.used.load(SeqCst) == FIXED;
        if done && !self.finished.swap(true, SeqCst) {
            self.notify.notify_waiters();
        }
    }
}

fn add(counter: &AtomicU64, bytes: u64, max: u64) -> Result<(), Fault> {
    counter.fetch_update(SeqCst, SeqCst, |used| used.checked_add(bytes).filter(|total| *total <= max)).map(|_| ()).map_err(|_| Fault::OverBudget)
}

/// A charge, released when dropped.
pub(crate) struct Charge {
    ledger: Arc<Ledger>,
    bytes: u64,
    event: bool,
}

impl Charge {
    /// Hands an event's charge to the ring, which releases it when the event
    /// is read or dropped; returns its bytes.
    pub(crate) fn into_ring(mut self) -> u64 {
        std::mem::take(&mut self.bytes)
    }

    pub(crate) fn ledger(&self) -> &Arc<Ledger> {
        &self.ledger
    }
}

impl Drop for Charge {
    fn drop(&mut self) {
        if self.bytes == 0 {
            return;
        }
        if self.event {
            self.ledger.release_event(self.bytes);
        } else {
            self.ledger.release(self.bytes);
        }
    }
}

/// The preallocated event ring, which never grows, and the unread events'
/// charged bytes.
pub(crate) struct Ring {
    events: VecDeque<EmbeddedEvent>,
    slots: usize,
    unread: u64,
    buffer: Charge,
}

impl Ring {
    pub(crate) fn new(ledger: &Arc<Ledger>, slots: usize) -> Result<Ring, Fault> {
        let buffer = ledger.charge(class(slots as u64 * RING_SLOT))?;
        Ok(Ring { events: VecDeque::with_capacity(slots), slots, unread: 0, buffer })
    }

    pub(crate) fn is_full(&self) -> bool {
        self.events.len() >= self.slots
    }

    pub(crate) fn push(&mut self, event: EmbeddedEvent, charge: Charge) {
        debug_assert!(!self.is_full());
        self.unread += charge.into_ring();
        self.events.push_back(event);
    }

    pub(crate) fn pop(&mut self) -> Option<EmbeddedEvent> {
        let event = self.events.pop_front()?;
        let bytes = event_bytes(&event);
        self.unread -= bytes;
        self.buffer.ledger().release_event(bytes);
        Some(event)
    }
}

impl Drop for Ring {
    fn drop(&mut self) {
        self.events.clear();
        self.buffer.ledger().release_event(self.unread);
    }
}

/// An event's strings at their classes, as charged.
pub(crate) fn event_bytes(event: &EmbeddedEvent) -> u64 {
    let size = |s: &String| class(s.capacity() as u64);
    let maybe = |s: &Option<String>| s.as_ref().map_or(0, size);
    match event {
        EmbeddedEvent::RemoteControl { .. } | EmbeddedEvent::Closed { .. } => 0,
        EmbeddedEvent::Notification { turn, item, status, json, .. } => {
            turn.as_ref().map_or(0, |t| size(&t.0)) + item.as_ref().map_or(0, |i| size(&i.id) + maybe(&i.item_type)) + maybe(status) + size(json)
        }
        EmbeddedEvent::Refused { method, thread, json } | EmbeddedEvent::Unmapped { method, thread, json } => size(method) + maybe(thread) + size(json),
        EmbeddedEvent::Unheld { method, thread, json } => size(method) + size(thread) + size(json),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SWBPIPE: EmbeddedLimits =
        EmbeddedLimits { pending_calls: 16, calls_per_turn: 1024, argument_bytes: 65_536, result_bytes: 65_536, event_bytes: 1024 * 1024 };

    #[test]
    fn classes_are_swbpipes() {
        assert_eq!([class(0), class(1), class(16), class(17), class(131_071), class(131_072), class(131_073)], [0, 16, 16, 32, 131_072, 131_072, 262_144]);
    }

    #[test]
    fn the_maximum_at_swbpipes_limits() {
        assert_eq!(max_bytes(&SWBPIPE), 2_515_968);
        assert_eq!(max_bytes(&SWBPIPE), 2_457 * 1024);
    }

    #[test]
    fn charges_past_the_maximum_are_refused_and_released_on_drop() {
        let ledger = Ledger::new(&SWBPIPE);
        let room = max_bytes(&SWBPIPE) - FIXED;
        let all = ledger.charge(room).unwrap();
        assert_eq!(ledger.charge(1).err(), Some(Fault::OverBudget));
        drop(all);
        assert_eq!(ledger.used(), FIXED);
        let event = ledger.charge_event(SWBPIPE.event_bytes as u64).unwrap();
        assert_eq!(ledger.charge_event(16).err(), Some(Fault::OverBudget));
        let bytes = event.into_ring();
        ledger.release_event(bytes);
        assert_eq!(ledger.used(), FIXED);
        assert!(ledger.charge_event(16).is_ok());
    }

    #[test]
    fn finished_once_closed_consumer_ended_and_released() {
        let ledger = Ledger::new(&SWBPIPE);
        let held = ledger.charge(100).unwrap();
        ledger.consumer_started();
        ledger.mark_closed();
        assert!(!ledger.is_finished());
        ledger.consumer_ended();
        assert!(!ledger.is_finished());
        drop(held);
        assert!(ledger.is_finished());
    }
}
