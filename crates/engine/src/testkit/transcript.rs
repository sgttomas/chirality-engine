//! Recorded transcripts: the ordered frames between the engine and the real
//! Codex, one JSON entry per line, scrubbed of paths, ids and times.
//!
//! `expect_outbound` is a frame the engine must send, `emit_inbound` one Codex
//! sent, and `runtime_exit` marks Codex ending (mid-file, a restart).

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const VERSION: &str = "0.171.0";

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Entry {
    TranscriptStart { protocol: String, version: String, scenario: String },
    ExpectOutbound { label: String, frame: Value },
    EmitInbound { label: String, frame: Value },
    RuntimeExit,
}

pub fn fixture_path(scenario: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures").join("codex").join(format!("{scenario}.ndjson"))
}

pub fn read(scenario: &str) -> Vec<Entry> {
    let path = fixture_path(scenario);
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    text.lines().filter(|l| !l.trim().is_empty()).map(|l| serde_json::from_str(l).unwrap_or_else(|e| panic!("{}: {e}: {l}", path.display()))).collect()
}

pub fn write(scenario: &str, entries: &[Entry]) {
    let path = fixture_path(scenario);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let mut text = String::new();
    for entry in entries {
        text.push_str(&serde_json::to_string(entry).unwrap());
        text.push('\n');
    }
    std::fs::write(&path, text).unwrap();
}

/// Swaps a run's real paths for placeholders and back.
#[derive(Clone, Debug)]
pub struct Places {
    /// (placeholder, real), longest real path first.
    pairs: Vec<(String, String)>,
}

impl Places {
    pub fn new(mut pairs: Vec<(String, String)>) -> Places {
        pairs.sort_by_key(|p| std::cmp::Reverse(p.1.len()));
        Places { pairs }
    }

    pub fn to_placeholders(&self, text: &str) -> String {
        self.pairs.iter().fold(text.to_string(), |t, (place, real)| t.replace(real.as_str(), place))
    }

    pub fn to_real(&self, text: &str) -> String {
        let mut pairs = self.pairs.clone();
        pairs.sort_by_key(|p| std::cmp::Reverse(p.0.len()));
        pairs.iter().fold(text.to_string(), |t, (place, real)| t.replace(place.as_str(), real))
    }

    pub fn map_strings(&self, value: Value, f: &impl Fn(&Places, &str) -> String) -> Value {
        match value {
            Value::String(s) => Value::String(f(self, &s)),
            Value::Array(items) => Value::Array(items.into_iter().map(|v| self.map_strings(v, f)).collect()),
            Value::Object(map) => Value::Object(map.into_iter().map(|(k, v)| (f(self, &k), self.map_strings(v, f))).collect()),
            other => other,
        }
    }
}

/// Scrubs a whole recording in order, so an id keeps one placeholder
/// throughout and frames the engine echoes still match.
pub struct Scrubber {
    places: Places,
    uuid: Regex,
    time: Regex,
    day: Regex,
    ids: HashMap<String, String>,
}

impl Scrubber {
    pub fn new(places: Places) -> Scrubber {
        Scrubber {
            places,
            uuid: Regex::new(r"[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}").unwrap(),
            time: Regex::new(r"\d{4}-\d{2}-\d{2}T\d{2}[:-]\d{2}[:-]\d{2}(\.\d+)?Z?").unwrap(),
            day: Regex::new(r"sessions/\d{4}/\d{2}/\d{2}").unwrap(),
            ids: HashMap::new(),
        }
    }

    pub fn scrub(&mut self, value: Value) -> Value {
        self.walk(None, value)
    }

    fn walk(&mut self, key: Option<&str>, value: Value) -> Value {
        match value {
            Value::String(s) => match key {
                Some("serverName") => Value::String("replay-host".into()),
                Some("userAgent") => Value::String(format!("{} (replay)", s.split_whitespace().next().unwrap_or_default())),
                _ => Value::String(self.text(&s)),
            },
            Value::Number(_) if key.is_some_and(is_time_key) => Value::from(0),
            Value::Array(items) => Value::Array(items.into_iter().map(|v| self.walk(None, v)).collect()),
            Value::Object(map) => Value::Object(map.into_iter().map(|(k, v)| (self.text(&k), self.walk(Some(&k), v))).collect()),
            other => other,
        }
    }

    fn text(&mut self, text: &str) -> String {
        let text = self.places.to_placeholders(text);
        let text = self.time.replace_all(&text, "1970-01-01T00:00:00Z").into_owned();
        let text = self.day.replace_all(&text, "sessions/1970/01/01").into_owned();
        let ids = &mut self.ids;
        self.uuid
            .replace_all(&text, |c: &regex::Captures| {
                let next = ids.len() + 1;
                ids.entry(c[0].to_string()).or_insert_with(|| format!("00000000-0000-0000-0000-{next:012}")).clone()
            })
            .into_owned()
    }
}

fn is_time_key(key: &str) -> bool {
    key.ends_with("At") || key.ends_with("AtMs") || key.ends_with("Ms") || key.ends_with("_ms") || key.ends_with("_at")
}

/// Frames compare by value, so key order does not matter.
pub fn same(expected: &Value, actual: &Value) -> bool {
    expected == actual
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn scrubbing_is_consistent_and_reversible_for_paths() {
        let places = Places::new(vec![("<project>".into(), "/tmp/run/project".into()), ("<root>".into(), "/tmp/run".into())]);
        let mut scrubber = Scrubber::new(places.clone());
        let id = "01a1f77b-98fc-79b0-ba35-59daa5dfba53";
        let a = scrubber.scrub(json!({"thread": {"id": id, "cwd": "/tmp/run/project", "createdAt": 17}, "x": "/tmp/run/home/2026-11-20T05:43:54.877Z"}));
        let b = scrubber.scrub(json!({"threadId": id, "states": {id: 1}}));
        assert_eq!(a, json!({"thread": {"id": "00000000-0000-0000-0000-000000000001", "cwd": "<project>", "createdAt": 0}, "x": "<root>/home/1970-01-01T00:00:00Z"}));
        assert_eq!(b, json!({"threadId": "00000000-0000-0000-0000-000000000001", "states": {"00000000-0000-0000-0000-000000000001": 1}}));
        assert_eq!(places.to_real("<project>/a"), "/tmp/run/project/a");
    }
}
