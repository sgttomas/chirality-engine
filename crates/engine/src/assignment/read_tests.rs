use std::cell::Cell;

use super::*;

fn turn(id: &str, status: &str, items: Value) -> p::Turn {
    serde_json::from_value(json!({ "id": id, "status": status, "error": if status == "failed" { json!({"message": "boom"}) } else { json!(null) }, "items": items })).unwrap()
}

fn user(text: &str) -> Value {
    json!({"type": "userMessage", "id": "u", "content": [{"type": "text", "text": text}]})
}

fn said(id: &str, text: &str, phase: Option<&str>) -> Value {
    json!({"type": "agentMessage", "id": id, "text": text, "phase": phase})
}

fn header(title: &str) -> Header {
    Header { thread: ThreadId("t".into()), title: Some(title.into()), project: Some("Bridge".into()), status: ThreadStatus::Idle, error: None }
}

fn args(view: View, before: Option<String>, limit: Option<usize>) -> ReadArgs {
    ReadArgs { thread: "t".into(), view, before, limit }
}

/// Reads over `pages` (newest first), as Codex gives them; counts the pages asked for.
async fn read(header: Header, args: &ReadArgs, pages: &[Vec<p::Turn>], asked: &Cell<usize>) -> Value {
    let fetch = |cursor: Option<String>| {
        asked.set(asked.get() + 1);
        let at: usize = cursor.map_or(0, |c| c.parse().unwrap());
        let page = pages.get(at).cloned().map(|turns| (turns, (at + 1 < pages.len()).then(|| (at + 1).to_string())));
        async move { page }
    };
    answer(header, args, fetch).await.unwrap()
}

fn kinds(answer: &Value) -> Vec<String> {
    answer["items"].as_array().unwrap().iter().map(|i| i["kind"].as_str().unwrap().to_string()).collect()
}

#[tokio::test]
async fn only_a_completed_turns_last_word_is_its_final_reply() {
    let pages = vec![vec![
        turn("4", "failed", json!([user("Fail."), said("a", "Trying.", None)])),
        turn("3", "interrupted", json!([user("Stop."), said("a", "Let me look first.", Some("commentary"))])),
        turn("2", "inProgress", json!([user("Go on."), said("a", "Halfway.", None)])),
        turn("1", "completed", json!([user("Check."), said("a", "Looking.", Some("commentary")), said("b", "Fine.", Some("final_answer")), said("c", "Noted.", Some("commentary"))])),
    ]];
    let read = read(header("Piles"), &args(View::Messages, None, None), &pages, &Cell::new(0)).await;
    assert_eq!(kinds(&read), ["turnFailed", "update", "person", "update", "person", "update", "person", "reply", "person"]);
    let items = read["items"].as_array().unwrap();
    assert_eq!((items[1]["turn"].as_str(), items[3]["turn"].as_str(), items[5]["turn"].as_str()), (Some("failed"), Some("stopped"), Some("running")));
    assert_eq!(items[7]["text"], "Fine.");
}

#[tokio::test]
async fn activity_says_how_each_piece_of_work_went() {
    let command = |id: &str, status: &str| json!({"type": "commandExecution", "id": id, "command": "ls", "cwd": "/p", "aggregatedOutput": null, "exitCode": null, "status": status});
    let change = |id: &str, status: &str| json!({"type": "fileChange", "id": id, "changes": [{"path": "/p/a.md", "diff": ""}], "status": status});
    let pages = vec![vec![turn("1", "completed", json!([user("Do it."), command("c1", "declined"), command("c2", "inProgress"), change("f1", "failed"), change("f2", "completed"), said("r", "Done.", None)]))]];
    let read = read(header("Piles"), &args(View::Activity, None, None), &pages, &Cell::new(0)).await;
    let statuses: Vec<(&str, &str)> = read["items"].as_array().unwrap().iter().filter_map(|i| Some((i["kind"].as_str()?, i["status"].as_str()?))).collect();
    assert_eq!(statuses, [("fileChange", "completed"), ("fileChange", "failed"), ("command", "inProgress"), ("command", "declined")]);
}

#[tokio::test]
async fn every_field_is_cut_the_header_too() {
    let title = "t".repeat(30_000);
    let paths: Vec<Value> = (0..600).map(|n| json!({"path": format!("/p/{}{n}.md", "d".repeat(300)), "diff": ""})).collect();
    let pages = vec![vec![turn("1", "completed", json!([{"type": "fileChange", "id": "f", "changes": paths, "status": "completed"}]))]];
    let mut h = header(&title);
    h.error = Some("e".repeat(30_000));
    let read = read(h, &args(View::Activity, None, None), &pages, &Cell::new(0)).await;
    assert!(read.to_string().len() <= ANSWER_LIMIT, "{}", read.to_string().len());
    assert!(read["title"].as_str().unwrap().ends_with("[cut: 29800 more characters]"));
    assert!(read["error"].as_str().unwrap().ends_with("[cut: 29400 more characters]"));
    let change = &read["items"][0];
    assert_eq!((change["files"].as_array().unwrap().len(), change["moreFiles"].as_u64()), (20, Some(580)));
    assert!(change["files"][0].as_str().unwrap().contains("[cut:"));
}

#[tokio::test]
async fn a_first_item_too_big_is_cut_until_it_fits() {
    // Each of these characters takes six bytes in JSON.
    let heavy = "\u{1}".repeat(20_000);
    let pages = vec![vec![turn("2", "completed", json!([user(&heavy), said("r", &heavy, None)])), turn("1", "completed", json!([user("First.")]))]];
    let mut h = header(&heavy);
    h.error = Some(heavy.clone());
    let read = read(h, &args(View::Messages, None, None), &pages, &Cell::new(0)).await;
    assert!(read.to_string().len() <= ANSWER_LIMIT, "{}", read.to_string().len());
    assert_eq!(read["items"].as_array().unwrap().len(), 1, "the first item, cut to fit");
    assert_eq!(read["more"], true);
}

#[tokio::test]
async fn paging_back_shows_every_item_once_and_ends() {
    let long = "x".repeat(5000);
    let pages: Vec<Vec<p::Turn>> =
        (0..4).map(|page| (0..5).map(|n| turn(&format!("{page}-{n}"), "completed", json!([user(&long), said("r", &format!("Reply {page}-{n}."), None)]))).collect()).collect();
    let (asked, mut before, mut seen) = (Cell::new(0), None, Vec::new());
    for _ in 0..100 {
        let read = read(header("Piles"), &args(View::Messages, before.clone(), Some(7)), &pages, &asked).await;
        assert!(read.to_string().len() <= ANSWER_LIMIT);
        let items = read["items"].as_array().unwrap();
        assert!(!items.is_empty(), "every page shows something");
        seen.extend(items.iter().map(|i| i["text"].as_str().unwrap().to_string()));
        match read["before"].as_str() {
            Some(next) if read["more"] == true => before = Some(next.to_string()),
            _ => break,
        }
    }
    assert_eq!(seen.len(), 40, "two items for each of 20 turns");
    let replies: Vec<&String> = seen.iter().filter(|t| t.starts_with("Reply")).collect();
    assert_eq!((replies.first().map(|s| s.as_str()), replies.last().map(|s| s.as_str())), (Some("Reply 0-0."), Some("Reply 3-4.")));
}

#[tokio::test]
async fn a_full_answer_reads_no_further() {
    let pages: Vec<Vec<p::Turn>> = (0..4).map(|page| (0..5).map(|n| turn(&format!("{page}-{n}"), "completed", json!([user("Go."), said("r", "Done.", None)]))).collect()).collect();
    let asked = Cell::new(0);
    let read = read(header("Piles"), &args(View::Messages, None, Some(1)), &pages, &asked).await;
    assert_eq!((kinds(&read), asked.get(), &read["more"]), (vec!["reply".to_string()], 1, &json!(true)));
    let bad = answer(header("Piles"), &args(View::Messages, Some("7".into()), None), |_| async { None }).await;
    assert!(bad.is_err());
}

/// Reads over `turns` (newest first), which Codex gives five at a time from an offset.
async fn read_flat(args: &ReadArgs, turns: &[p::Turn]) -> std::result::Result<Value, String> {
    let fetch = |cursor: Option<String>| {
        let at: usize = cursor.map_or(0, |c| c.parse().unwrap());
        let page = turns.iter().skip(at).take(5).cloned().collect::<Vec<_>>();
        let next = (at + 5 < turns.len()).then(|| (at + 5).to_string());
        async move { Some((page, next)) }
    };
    answer(header("Piles"), args, fetch).await
}

fn texts(answer: &Value) -> Vec<String> {
    answer["items"].as_array().unwrap().iter().map(|i| i["text"].as_str().unwrap().to_string()).collect()
}

#[tokio::test]
async fn going_on_after_new_turns_came_starts_where_the_last_answer_stopped() {
    let numbered = |range: std::ops::Range<usize>| range.rev().map(|n| turn(&n.to_string(), "completed", json!([user(&format!("Ask {n}.")), said("r", &format!("Reply {n}."), None)]))).collect::<Vec<_>>();
    let older = numbered(0..12);
    let first = read_flat(&args(View::Messages, None, Some(7)), &older).await.unwrap();
    assert_eq!(texts(&first).last().map(String::as_str), Some("Reply 8."));
    // Six turns more, which move every older one on by more than a page.
    let grown: Vec<p::Turn> = numbered(12..18).into_iter().chain(older.clone()).collect();
    let before = first["before"].as_str().map(str::to_string);
    let next = read_flat(&args(View::Messages, before, Some(3)), &grown).await.unwrap();
    assert_eq!(texts(&next), ["Ask 8.", "Reply 7.", "Ask 7."]);
    // A cursor whose turn is gone says so.
    let gone = serde_json::to_string(&Cursor { page: None, turn: Some("99".into()), item: 0 }).unwrap();
    assert!(read_flat(&args(View::Messages, Some(gone), None), &grown).await.is_err());
}

#[tokio::test]
async fn ids_are_cut_and_an_item_that_cannot_fit_is_not_shown() {
    let from = crate::assignment::marker::Marker { from: ThreadId("i".repeat(30_000)), title: None, report: None };
    let marked = json!({"type": "userMessage", "id": "u", "content": [{"type": "text", "text": crate::assignment::marker::render(&from)}, {"type": "text", "text": "Go."}]});
    let read = read_flat(&args(View::Messages, None, None), &[turn("1", "completed", json!([marked]))]).await.unwrap();
    assert!(read["items"][0]["from"]["thread"].as_str().unwrap().ends_with("[cut: 29800 more characters]"), "{}", read["items"][0]);
    let one = turn("1", "completed", json!([said("r", &"x".repeat(5000), None)]));
    let items = TurnItems::new(&one, View::Messages);
    let value = items.entry(0, FULL).unwrap();
    assert_eq!(fit(&items, 0, value, 40), json!({"kind": "reply", "text": "[too large to show]"}));
}
