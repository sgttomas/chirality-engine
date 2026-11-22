//! Recording and replaying transcripts through the engine's own client.
//!
//! The recorder taps every frame the production client sends and receives
//! while it drives the real Codex. The replay connector stands in for the
//! Codex process: it checks each frame the engine sends against the next
//! `expect_outbound`, in order, and plays each `emit_inbound` back once every
//! frame before it was sent. A request the engine sends may come a little
//! before its place (tasks running at once send in either order); its id is
//! not compared, and Codex's answer goes back with the id it was sent with.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::Value;
use tokio::sync::mpsc;

use super::transcript::{Entry, Places, Scrubber, VERSION, same};
use crate::rpc::{Direction, Tap};
use crate::transport::{ConnectFuture, Connection, Connector, Stopper};

// ---------- recording ----------

#[derive(Default)]
struct Log {
    entries: Vec<Entry>,
    /// Methods of requests by id, each way, to label their answers.
    sent: HashMap<String, String>,
    received: HashMap<String, String>,
}

pub struct Recorder {
    log: Arc<Mutex<Log>>,
    stopping: Arc<AtomicBool>,
}

impl Recorder {
    pub fn new() -> Recorder {
        Recorder { log: Arc::default(), stopping: Arc::default() }
    }

    pub fn tap(&self) -> Tap {
        let log = self.log.clone();
        let stopping = self.stopping.clone();
        Arc::new(move |direction, line| {
            if stopping.load(Ordering::SeqCst) {
                return;
            }
            let Ok(frame) = serde_json::from_str::<Value>(line) else { return };
            let mut log = log.lock().unwrap();
            let id = frame.get("id").filter(|v| !v.is_null()).map(Value::to_string);
            let method = frame.get("method").and_then(Value::as_str).map(str::to_string);
            let label = match (&method, &id, direction) {
                (Some(m), Some(id), Direction::Out) => {
                    log.sent.insert(id.clone(), m.clone());
                    m.clone()
                }
                (Some(m), Some(id), Direction::In) => {
                    log.received.insert(id.clone(), m.clone());
                    m.clone()
                }
                (Some(m), None, _) => m.clone(),
                (None, Some(id), Direction::Out) => log.received.get(id).cloned().unwrap_or_default(),
                (None, Some(id), Direction::In) => log.sent.get(id).cloned().unwrap_or_default(),
                (None, None, _) => String::new(),
            };
            log.entries.push(match direction {
                Direction::Out => Entry::ExpectOutbound { label, frame },
                Direction::In => Entry::EmitInbound { label, frame },
            });
        })
    }

    /// Wraps a connector so a Codex that ends is recorded as `runtime_exit`.
    pub fn connector(&self, inner: Arc<dyn Connector>) -> Arc<dyn Connector> {
        Arc::new(RecordingConnector { inner, log: self.log.clone(), stopping: self.stopping.clone() })
    }

    /// Stops recording; what follows (the engine shutting Codex down) is not part of the scenario.
    pub fn stop(&self) {
        self.stopping.store(true, Ordering::SeqCst);
    }

    /// The scrubbed transcript.
    pub fn finish(&self, scenario: &str, places: Places) -> Vec<Entry> {
        let log = self.log.lock().unwrap();
        let mut scrubber = Scrubber::new(places);
        let mut out = vec![Entry::TranscriptStart { protocol: "codex.app-server".into(), version: VERSION.into(), scenario: scenario.into() }];
        for entry in &log.entries {
            out.push(match entry.clone() {
                Entry::ExpectOutbound { label, frame } => Entry::ExpectOutbound { label, frame: scrubber.scrub(frame) },
                Entry::EmitInbound { label, frame } => Entry::EmitInbound { label, frame: scrubber.scrub(frame) },
                other => other,
            });
        }
        out
    }
}

struct RecordingConnector {
    inner: Arc<dyn Connector>,
    log: Arc<Mutex<Log>>,
    stopping: Arc<AtomicBool>,
}

impl Connector for RecordingConnector {
    fn connect(&self) -> ConnectFuture {
        let (inner, log, stopping) = (self.inner.clone(), self.log.clone(), self.stopping.clone());
        Box::pin(async move {
            let Connection { outgoing, mut incoming, stopper, ended } = inner.connect().await?;
            let (tx, rx) = mpsc::unbounded_channel();
            tokio::spawn(async move {
                while let Some(line) = incoming.recv().await {
                    if tx.send(line).is_err() {
                        break;
                    }
                }
                if !stopping.load(Ordering::SeqCst) {
                    log.lock().unwrap().entries.push(Entry::RuntimeExit);
                }
            });
            Ok(Connection { outgoing, incoming: rx, stopper, ended })
        })
    }
}

// ---------- replaying ----------

/// How far ahead of the next expected frame a request the engine sends may be found: tasks that
/// run at once (a report's look at history, a delivery's turn) send in either order.
const LOOKAHEAD: usize = 24;

struct Cursor {
    entries: Vec<Entry>,
    /// The first entry not yet played or matched.
    at: usize,
    /// Expected frames after `at` already matched.
    matched: HashSet<usize>,
    failure: Option<String>,
}

impl Cursor {
    fn skip_matched(&mut self) {
        while self.matched.remove(&self.at) {
            self.at += 1;
        }
    }
}

pub struct Replay {
    cursor: Arc<Mutex<Cursor>>,
    places: Places,
}

impl Replay {
    pub fn new(entries: Vec<Entry>, places: Places) -> Arc<Replay> {
        let entries = entries.into_iter().filter(|e| !matches!(e, Entry::TranscriptStart { .. })).collect();
        Arc::new(Replay { cursor: Arc::new(Mutex::new(Cursor { entries, at: 0, matched: HashSet::new(), failure: None })), places })
    }

    pub fn failure(&self) -> Option<String> {
        self.cursor.lock().unwrap().failure.clone()
    }

    /// Fails unless every expected frame was sent and none differed.
    pub fn finish(&self) {
        let cursor = self.cursor.lock().unwrap();
        if let Some(failure) = &cursor.failure {
            panic!("replay: {failure}");
        }
        let left: Vec<&str> = cursor.entries[cursor.at..]
            .iter()
            .enumerate()
            .filter(|(n, _)| !cursor.matched.contains(&(cursor.at + n)))
            .filter_map(|(_, e)| match e {
                Entry::ExpectOutbound { label, .. } => Some(label.as_str()),
                _ => None,
            })
            .collect();
        assert!(left.is_empty(), "replay: the engine never sent {left:?}");
    }
}

/// A request the engine sends (not an answer to Codex, not a notification).
fn is_request(frame: &Value) -> bool {
    frame.get("method").is_some() && frame.get("id").is_some_and(|id| !id.is_null())
}

/// The frame without its id.
fn without_id(frame: &Value) -> Value {
    let mut frame = frame.clone();
    if let Some(map) = frame.as_object_mut() {
        map.remove("id");
    }
    frame
}

/// Whether `actual` is the expected frame: the engine's requests whatever their ids (which depend on
/// the order it sent them in), anything else exactly.
fn matches(expected: &Value, actual: &Value) -> bool {
    if is_request(expected) && is_request(actual) { same(&without_id(expected), &without_id(actual)) } else { same(expected, actual) }
}

impl Connector for Replay {
    fn connect(&self) -> ConnectFuture {
        let (cursor, places) = (self.cursor.clone(), self.places.clone());
        Box::pin(async move {
            let (out_tx, out_rx) = mpsc::unbounded_channel();
            let (in_tx, in_rx) = mpsc::unbounded_channel();
            tokio::spawn(play(cursor, places, out_rx, in_tx));
            Ok(Connection { outgoing: out_tx, incoming: in_rx, stopper: Box::new(NoProcess), ended: Default::default() })
        })
    }
}

struct NoProcess;

impl Stopper for NoProcess {
    fn stop(&self) {}
    fn kill(&self) {}
}

async fn play(cursor: Arc<Mutex<Cursor>>, places: Places, mut out: mpsc::UnboundedReceiver<String>, inbound: mpsc::UnboundedSender<String>) {
    let fail = |message: String| {
        let mut c = cursor.lock().unwrap();
        c.failure.get_or_insert(message);
    };
    // The id each recorded request of the engine's was sent with, for Codex's answer.
    let mut ids: HashMap<String, Value> = HashMap::new();
    loop {
        let entry = {
            let mut c = cursor.lock().unwrap();
            if c.failure.is_some() {
                return;
            }
            c.skip_matched();
            c.entries.get(c.at).cloned()
        };
        let advance = || cursor.lock().unwrap().at += 1;
        match entry {
            None => {
                // The transcript is over; anything more the engine sends is extra.
                if let Some(line) = out.recv().await {
                    fail(format!("an extra frame after the transcript ended: {line}"));
                }
                return;
            }
            Some(Entry::EmitInbound { mut frame, .. }) => {
                advance();
                if frame.get("method").is_none()
                    && let Some(id) = frame.get("id").map(Value::to_string)
                    && let Some(sent) = ids.remove(&id)
                {
                    frame["id"] = sent;
                }
                let frame = places.map_strings(frame, &|p, s| p.to_real(s));
                let _ = inbound.send(frame.to_string());
            }
            Some(Entry::RuntimeExit) => {
                advance();
                return;
            }
            Some(Entry::ExpectOutbound { label, frame: expected }) => match tokio::time::timeout(Duration::from_secs(15), out.recv()).await {
                Ok(Some(line)) => {
                    let actual: Value = serde_json::from_str(&line).unwrap_or(Value::Null);
                    let actual = places.map_strings(actual, &|p, s| p.to_placeholders(s));
                    let found = {
                        let mut c = cursor.lock().unwrap();
                        let at = c.at;
                        let end = (at + LOOKAHEAD).min(c.entries.len());
                        let found = (at..end).find(|n| {
                            !c.matched.contains(n)
                                && matches!(&c.entries[*n], Entry::ExpectOutbound { frame, .. } if (*n == at || is_request(&actual)) && matches(frame, &actual))
                        });
                        if let Some(n) = found {
                            c.matched.insert(n);
                            if let Entry::ExpectOutbound { frame, .. } = &c.entries[n]
                                && let (Some(recorded), Some(sent)) = (frame.get("id"), actual.get("id"))
                                && is_request(frame)
                            {
                                ids.insert(recorded.to_string(), sent.clone());
                            }
                        }
                        found
                    };
                    if found.is_none() {
                        fail(format!("frame {label} differs\nexpected: {expected}\n  actual: {actual}"));
                        // Codex is not to seem to die: hold the connection until the engine ends.
                        while out.recv().await.is_some() {}
                        return;
                    }
                }
                Ok(None) => return,
                Err(_) => {
                    fail(format!("the engine did not send {label}: {expected}"));
                    return;
                }
            },
            Some(Entry::TranscriptStart { .. }) => advance(),
        }
    }
}
