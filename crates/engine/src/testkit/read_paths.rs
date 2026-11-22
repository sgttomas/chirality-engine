//! A read, against the scripted Codex: with no failure in the engine's memory
//! (as after a restart), the header's is the latest turn's in history.

use serde_json::json;

use super::assignment_paths::{engine_with, id, thread, turn};
use crate::assignment::read::{ReadArgs, View};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_failure_read_is_the_latest_turns_when_the_engine_saw_none() {
    let (engine, _wires, mut wire, _) = engine_with("read-failure", vec![thread("a", "", "lead"), thread("b", "", "worker")]).await;
    let reading = tokio::spawn({
        let engine = engine.clone();
        async move { engine.inner().read_for(&id("a"), ReadArgs { thread: "b".into(), view: View::Messages, before: None, limit: None }).await }
    });
    let mut failed = turn("t1", "failed", json!([]));
    failed["error"] = json!({"message": "The model was not reachable."});
    let latest = wire.next().await;
    assert_eq!((latest["params"]["limit"].as_u64(), latest["params"]["itemsView"].as_str()), (Some(1), Some("notLoaded")), "{latest}");
    wire.send(json!({"id": latest["id"], "result": {"data": [failed.clone()], "nextCursor": null}}));
    wire.answer("thread/turns/list", json!({"data": [failed], "nextCursor": null})).await;
    let read = reading.await.unwrap().unwrap();
    assert_eq!((read["error"].as_str(), read["items"][0]["kind"].as_str()), (Some("The model was not reachable."), Some("turnFailed")), "{read}");
    engine.shutdown().await;
}
