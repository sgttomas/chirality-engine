//! What the engine produces from recorded transcripts, written as JSON for the
//! interface package's tests (`packages/ui`): the snapshot before, every event, the snapshot
//! after, each thread view the scenario read with the number of events
//! before it, and the workflows the engine finds in the scenario's project. `interface_data` checks the committed files still match;
//! `CHIRALITY_EXPORT=1` writes them again.

use std::path::PathBuf;
use std::sync::Mutex;

use serde_json::{Value, json};

use crate::api::{Event, ThreadView};

use super::harness::{Ctx, Tape};
use super::scenarios;

fn out_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../packages/ui/src/testing/engine-data")
}

/// Times the engine stamped during this run (not recorded ones) become one fixed time.
fn fix_times(value: &mut Value, since: i64) {
    match value {
        Value::Object(map) => {
            for (key, v) in map.iter_mut() {
                if key == "updatedAt" && v.as_i64().is_some_and(|t| t >= since) {
                    *v = json!(1_795_000_000);
                } else {
                    fix_times(v, since);
                }
            }
        }
        Value::Array(items) => items.iter_mut().for_each(|v| fix_times(v, since)),
        _ => {}
    }
}

/// Consecutive summaries of one thread with no read between them become the
/// last. Each replaces the one before it whole, and whether the engine sends
/// one or two for a turn's start depends only on whether Codex's answer or its
/// notification is handled first.
fn collapse(events: Vec<Event>, reads: Vec<(usize, ThreadView)>) -> (Vec<Event>, Vec<(usize, ThreadView)>) {
    let mut kept: Vec<Event> = Vec::new();
    // kept.len() after the first i events.
    let mut count = vec![0];
    for (i, event) in events.into_iter().enumerate() {
        let read_between = reads.iter().any(|(at, _)| *at == i);
        let same_thread = matches!((kept.last(), &event), (Some(Event::Thread { thread: a }), Event::Thread { thread: b }) if a.id == b.id);
        if same_thread && !read_between {
            *kept.last_mut().unwrap() = event;
        } else {
            kept.push(event);
        }
        count.push(kept.len());
    }
    let reads = reads.into_iter().map(|(at, view)| (count[at], view)).collect();
    (kept, reads)
}

/// Consecutive removals with no read between them are put in thread order: a
/// thread archived with its subagents is removed once Codex answers, and each
/// subagent when Codex's notice of it is handled, in either order.
fn order_removals(events: &mut [Event], reads: &[(usize, ThreadView)]) {
    let removed = |e: &Event| matches!(e, Event::ThreadRemoved { .. });
    let mut start = 0;
    while start < events.len() {
        let mut end = start;
        while end < events.len() && removed(&events[end]) && (end == start || !reads.iter().any(|(at, _)| *at == end)) {
            end += 1;
        }
        events[start..end].sort_by_key(|e| match e {
            Event::ThreadRemoved { thread } => thread.0.clone(),
            _ => String::new(),
        });
        start = end.max(start + 1);
    }
}

/// The data's own numbering: the snapshot before is 0, each event one more,
/// the snapshot after the last, a read the events before it. The engine's own
/// numbers count the start-up events too and skip what `collapse` merged.
fn number(value: &mut Value) {
    let count = value["events"].as_array().map_or(0, Vec::len);
    value["before"]["seq"] = json!(0);
    value["after"]["seq"] = json!(count);
    for read in value["reads"].as_array_mut().into_iter().flatten() {
        read["view"]["seq"] = read["afterEvents"].clone();
    }
}

async fn capture(name: &str) -> Value {
    let since = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() as i64 - 5;
    let mut ctx = Ctx::replay(name).await;
    let before = ctx.engine.snapshot();
    ctx.tape = Some(Mutex::new(Tape { rx: ctx.engine.subscribe(), events: Vec::new(), reads: Vec::new() }));
    match name {
        "text_turn" => scenarios::text_turn(&mut ctx).await,
        "workflow_turn" => scenarios::workflow_turn(&mut ctx).await,
        "command_approved" => scenarios::command_approved(&mut ctx).await,
        "file_change" => scenarios::file_change(&mut ctx).await,
        "question" => scenarios::question(&mut ctx).await,
        "interrupt" => scenarios::interrupt(&mut ctx).await,
        "fork" => scenarios::fork(&mut ctx).await,
        "app_tool" => scenarios::app_tool(&mut ctx).await,
        "subagent" => scenarios::subagent(&mut ctx).await,
        "assignment" => scenarios::assignment(&mut ctx).await,
        "thread_refs" => scenarios::thread_refs(&mut ctx).await,
        "assignment_read" => scenarios::assignment_read(&mut ctx).await,
        other => panic!("no scenario {other}"),
    }
    let after = ctx.engine.snapshot();
    let mut tape = ctx.tape.take().unwrap().into_inner().unwrap();
    tape.drain();
    let (mut events, reads) = collapse(tape.events, tape.reads);
    order_removals(&mut events, &reads);
    let reads: Vec<Value> = reads.iter().map(|(at, view)| json!({ "afterEvents": at, "view": view })).collect();
    let root = ctx.run.root.display().to_string();
    // Project ids are derived from the run's folder; name them by position instead.
    let projects: Vec<String> = after.projects.iter().map(|p| p.id.0.clone()).collect();
    let mut workflows = Vec::new();
    for project in &after.projects {
        // The project's own, not a Library edition a run may name.
        workflows.extend(ctx.engine.workflows(&project.id).await.unwrap().into_iter().filter(|w| w.source == "project"));
    }
    ctx.finish(name).await;
    let data = json!({ "before": before, "events": events, "after": after, "reads": reads, "workflows": workflows });
    // The run's temporary folder becomes a fixed one, so the files are stable.
    let mut text = serde_json::to_string(&data).unwrap().replace(&root, "/work/run");
    for (i, id) in projects.iter().enumerate() {
        text = text.replace(&format!("\"{id}\""), &format!("\"project-{}\"", i + 1));
    }
    let mut value: Value = serde_json::from_str(&text).unwrap();
    fix_times(&mut value, since);
    // In the engine's order again, now that the times are fixed: which second a thread's last change
    // fell in is not the scenario's.
    for snapshot in ["before", "after"] {
        if let Some(Value::Array(threads)) = value.get_mut(snapshot).and_then(|s| s.get_mut("threads")) {
            threads.sort_by(|a, b| b["updatedAt"].as_i64().cmp(&a["updatedAt"].as_i64()).then_with(|| a["id"].as_str().cmp(&b["id"].as_str())));
        }
    }
    number(&mut value);
    value
}

const SCENARIOS: [&str; 12] = ["text_turn", "workflow_turn", "command_approved", "file_change", "question", "interrupt", "fork", "app_tool", "subagent", "assignment", "thread_refs", "assignment_read"];

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn interface_data() {
    let write = std::env::var_os("CHIRALITY_EXPORT").is_some();
    std::fs::create_dir_all(out_dir()).unwrap();
    for name in SCENARIOS {
        let data = serde_json::to_string_pretty(&capture(name).await).unwrap() + "\n";
        let path = out_dir().join(format!("{name}.json"));
        if write {
            std::fs::write(&path, data).unwrap();
        } else {
            let committed = std::fs::read_to_string(&path).unwrap_or_default();
            assert!(committed == data, "{} is out of date: CHIRALITY_EXPORT=1 cargo test -p chirality-engine interface_data", path.display());
        }
    }
}
