use serde_json::Value;

use super::*;
use crate::embedded::types::{ApprovalPolicy, SandboxMode, ThreadOptions, ThreadPolicy};

fn raw(text: &str) -> &RawValue {
    let boxed: Box<RawValue> = RawValue::from_string(text.to_owned()).unwrap();
    Box::leak(boxed)
}

fn nested(depth: usize) -> String {
    // `{"a":` then `[` up to `depth` open containers in all.
    let mut text = String::from("{\"a\":");
    text.push_str(&"[".repeat(depth - 1));
    text.push_str(&"]".repeat(depth - 1));
    text.push('}');
    text
}

#[test]
fn prescan_takes_json_and_refuses_the_rest() {
    for good in ["{}", " {\"a\":[1,-2.5e+3,true,false,null,\"x\\u00e9\\n\"]} ", "[]", "0", "\"s\"", "{\"a\":{\"b\":{}}}"] {
        assert!(prescan(good), "{good}");
    }
    for bad in
        ["", "{", "{}}", "{} {}", "{\"a\"}", "{\"a\":1,}", "[1,]", "01", "1.", "-", "1e", "\"\\x\"", "\"\u{1}\"", "\"\\u12\"", "tru", "nul", "{1:2}", "\"open"]
    {
        assert!(!prescan(bad), "{bad:?}");
    }
}

#[test]
fn prescan_nests_127_deep_not_128() {
    assert!(prescan(&nested(127)));
    assert!(!prescan(&nested(128)));
    assert!(frame(&format!("{{\"method\":\"x\",\"params\":{}}}", nested(126))).is_some());
    assert!(frame(&format!("{{\"method\":\"x\",\"params\":{}}}", nested(127))).is_none());
}

#[test]
fn prescan_bounds_the_length() {
    let pad = |n: usize| format!("{{\"method\":\"x\",\"p\":\"{}\"}}", "a".repeat(n));
    let base = pad(0).len();
    assert!(prescan(&pad(FRAME_TEXT - base)));
    assert!(!prescan(&pad(FRAME_TEXT - base + 1)));
}

#[test]
fn keys_are_matched_raw() {
    let [id, method] = pick("{\"i\\u0064\":1,\"method\":\"m\"}", ["id", "method"]).unwrap();
    assert!(id.is_none());
    assert_eq!(method.unwrap().get(), "\"m\"");
    assert!(pick("{\"id\":1,\"id\":2}", ["id"]).is_none(), "a key named twice");
    assert!(pick("[1]", ["id"]).is_none());
    assert!(frame("{\"m\\u0065thod\":\"x\"}").is_none(), "an escaped method key is no method");
}

#[test]
fn elements_are_counted() {
    let mut seen = Vec::new();
    assert_eq!(elements("[1,{\"a\":2},\"x\"]", &mut |e| seen.push(e.get().to_owned())), Some(3));
    assert_eq!(seen, ["1", "{\"a\":2}", "\"x\""]);
    assert_eq!(elements("{}", &mut |_| {}), None);
}

#[test]
fn the_engines_unescaper() {
    let mut buf = [0; 64];
    assert_eq!(unescape(raw(r#""a\"b\\c\/d\b\f\n\r\t""#), &mut buf), Some("a\"b\\c/d\u{8}\u{c}\n\r\t"));
    assert_eq!(unescape(raw(r#""\u00e9\u6587\ud83d\ude00""#), &mut buf), Some("é文😀"));
    assert_eq!(unescape(raw(r#""\ud83d""#), &mut buf), None, "a lone high surrogate");
    assert_eq!(unescape(raw(r#""\ude00x""#), &mut buf), None, "a lone low surrogate");
    assert_eq!(unescape(raw(r#""\ud83dx""#), &mut buf), None);
    assert_eq!(unescape(raw("1"), &mut buf), None, "not a string");
    let mut small = [0; 4];
    assert_eq!(unescape(raw(r#""abcd""#), &mut small), Some("abcd"));
    assert_eq!(unescape(raw(r#""abcde""#), &mut small), None, "stops at the buffer's end");
    assert_eq!(unescape(raw(r#""ab\u6587""#), &mut small), None);
}

#[test]
fn identities_are_unescaped_and_bounded() {
    let mut buf = [0; IDENTITY];
    assert_eq!(identity(raw(r#""call_\"文\\x""#), &mut buf), Some("call_\"文\\x"));
    assert_eq!(identity(raw("\"\""), &mut buf), None);
    assert_eq!(identity(raw(r#""a\nb""#), &mut buf), None, "a control character");
    assert_eq!(identity(raw(r#""a\u0085""#), &mut buf), None, "a C1 control character");
    let long = |n: usize| format!("\"{}\"", "a".repeat(n));
    assert!(identity(raw(&long(256)), &mut buf).is_some());
    assert!(identity(raw(&long(257)), &mut buf).is_none());
    assert!(identity(raw(&format!("\"{}\\u0061\"", "a".repeat(255))), &mut buf).is_some(), "256 once unescaped");
}

#[test]
fn constants_are_taken_only_without_escapes() {
    assert_eq!(constant(raw("\"inProgress\"")), Some("inProgress"));
    assert_eq!(constant(raw(r#""in\u0050rogress""#)), None);
    assert_eq!(constant(raw("1")), None);
    assert_eq!(constant(raw(&format!("\"{}\"", "a".repeat(65)))), None);
}

#[test]
fn integers_from_raw_text() {
    for (text, value) in [("0", Some(0)), ("-0", Some(0)), ("42", Some(42)), ("9223372036854775807", Some(i64::MAX)), ("-9223372036854775808", Some(i64::MIN))]
    {
        assert_eq!(integer(raw(text)), value, "{text}");
    }
    for text in ["9223372036854775808", "-9223372036854775809", "1.0", "1e2", "\"1\"", "null", "123456789012345678901234567890"] {
        assert_eq!(integer(raw(text)), None, "{text}");
    }
}

#[test]
fn frames_are_exactly_one_kind() {
    assert!(matches!(frame("{\"id\":7,\"method\":\"item/tool/call\",\"params\":{}}"), Some(Frame::Request { .. })));
    assert!(matches!(frame("{\"id\":\"r\\\"1\",\"method\":\"x\"}"), Some(Frame::Request { .. })));
    assert!(matches!(frame("{\"method\":\"x\",\"emittedAtMs\":1}"), Some(Frame::Notification { params: None, .. })));
    assert!(matches!(frame("{\"id\":3,\"result\":null}"), Some(Frame::Response { id: 3, reply: Ok(_) })));
    match frame("{\"id\":-4,\"error\":{\"code\":-32600,\"message\":\"no \\\"x\\\"\"}}") {
        Some(Frame::Response { id: -4, reply: Err(error) }) => assert_eq!((error.code, error.message.get()), (-32600, "\"no \\\"x\\\"\"")),
        _ => panic!("an error response"),
    }
    for bad in [
        "[]",
        "{}",
        "{\"id\":1}",
        "{\"id\":1,\"result\":1,\"error\":{\"code\":1,\"message\":\"m\"}}",
        "{\"id\":1,\"error\":{\"code\":1.5,\"message\":\"m\"}}",
        "{\"id\":1,\"error\":{\"code\":1,\"message\":2}}",
        "{\"id\":1,\"error\":{\"code\":1}}",
        "{\"id\":\"1\",\"result\":{}}",
        "{\"id\":null,\"result\":{}}",
        "{\"id\":1,\"result\":{},\"params\":{}}",
        "{\"method\":\"x\",\"result\":{}}",
        "{\"method\":\"x\",\"params\":[]}",
        "{\"method\":\"x\",\"params\":null}",
        "{\"method\":1}",
        "{\"method\":\"\"}",
        "{\"method\":\"a\\u0000\"}",
        "{\"id\":1.5,\"method\":\"x\"}",
        "{\"id\":\"\",\"method\":\"x\"}",
        "{\"method\":\"x\",\"method\":\"y\"}",
    ] {
        assert!(frame(bad).is_none(), "{bad}");
    }
}

#[test]
fn the_writer_escapes_and_refuses_past_its_buffer() {
    let mut w = Writer::new();
    assert_eq!(w.capacity(), FRAME_TEXT);
    w.begin().raw("[").string("a\"\\\n\r\t\u{8}\u{c}\u{1}\u{1f}é").raw(",").integer(i64::MIN).raw(",").integer(0).raw("]");
    let text = w.text().unwrap().to_owned();
    assert_eq!(text, "[\"a\\\"\\\\\\n\\r\\t\\b\\f\\u0001\\u001fé\",-9223372036854775808,0]");
    assert_eq!(serde_json::from_str::<Value>(&text).unwrap()[0], "a\"\\\n\r\t\u{8}\u{c}\u{1}\u{1f}é");
    w.begin().raw(&"a".repeat(FRAME_TEXT));
    assert_eq!(w.text().map(str::len), Some(FRAME_TEXT));
    w.raw("b");
    assert!(w.text().is_none());
    assert_eq!(w.capacity(), FRAME_TEXT, "never grows");
    w.begin().raw("{}");
    assert_eq!(w.text(), Some("{}"));
}

#[test]
fn thread_start_carries_the_policy_and_the_namespace() {
    let mut w = Writer::new();
    let offer = Offer { namespace: "swbpipe", description: "SWBPIPE tools.", tools: "[{\"name\":\"a\"}]", work_dir: "/w\"x" };
    let options = ThreadOptions {
        model: Some("m".into()),
        provider: None,
        base_instructions: Some("be \"brief\"".into()),
        policy: ThreadPolicy { sandbox: SandboxMode::WorkspaceWrite, approval: ApprovalPolicy::OnRequest },
    };
    thread_start(&mut w, 9, &offer, &options);
    let sent: Value = serde_json::from_str(w.text().unwrap()).unwrap();
    let expected = serde_json::json!({"id": 9, "method": "thread/start", "params": {
        "model": "m", "baseInstructions": "be \"brief\"", "cwd": "/w\"x", "ephemeral": true,
        "sandbox": "workspace-write", "approvalPolicy": "on-request", "environments": [],
        "dynamicTools": [{"type": "namespace", "name": "swbpipe", "description": "SWBPIPE tools.", "tools": [{"name": "a"}]}],
    }});
    assert_eq!(sent, expected);
}

#[test]
fn the_unescaper_is_self_contained() {
    let mut buf = [0; 16];
    for bad in [r"\x", r"\u+12a", r"\u12", r"\", r"\ud83d\u+e00", r"\u00g0"] {
        assert_eq!(unescape_text(bad, &mut buf), None, "{bad}");
    }
    assert_eq!(unescape_text(r"\u00e9", &mut buf), Some("é"));
}

#[test]
fn tools_at_their_depth_keep_thread_start_within_127() {
    let tools = |depth: usize| format!("[{{\"name\":\"a\",\"x\":{}{}}}]", "[".repeat(depth - 2), "]".repeat(depth - 2));
    let offer = |tools: &str| {
        let mut w = Writer::new();
        let offer = Offer { namespace: "n", description: "", tools, work_dir: "/w" };
        thread_start(&mut w, 1, &offer, &crate::embedded::tests::OPTIONS);
        frame(w.text().unwrap()).is_some()
    };
    assert!(prescan_within(&tools(TOOLS_DEPTH), TOOLS_DEPTH) && offer(&tools(TOOLS_DEPTH)));
    assert!(!prescan_within(&tools(TOOLS_DEPTH + 1), TOOLS_DEPTH));
    assert!(!offer(&tools(TOOLS_DEPTH + 1)), "deeper tools would make the frame Codex reads too deep");
}
