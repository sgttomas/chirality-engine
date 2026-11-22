//! The mark on a message one thread sends another: the first part of the
//! message, naming the sender (and, on a report, how the turn ended), so the
//! receiving thread's history says where it came from. Codex's listing
//! carries a thread's first message whole as its preview, so the mark on a
//! brief is where an assignment is read from.
//!
//! And a thread a message mentions: a part of its own after the text,
//! `<thread-ref id="<thread id>" title="<title>" project="<project>"/>`,
//! escaped as the mark is, read back from history into the message's threads.

use crate::api::*;

const OPEN: &str = "<thread-message from=\"";
const CLOSE: &str = "/>";

/// Who sent a message, and for a report, how the reported turn ended.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Marker {
    pub from: ThreadId,
    pub title: Option<String>,
    pub report: Option<TurnEnd>,
}

impl Marker {
    pub fn from(&self) -> MessageFrom {
        MessageFrom { thread: self.from.clone(), title: self.title.clone(), report: self.report }
    }
}

fn end_word(end: TurnEnd) -> &'static str {
    match end {
        TurnEnd::Completed => "completed",
        TurnEnd::Stopped => "stopped",
        TurnEnd::Failed => "failed",
    }
}

fn escape(text: &str) -> String {
    text.replace('&', "&amp;").replace('"', "&quot;").replace('<', "&lt;").replace('>', "&gt;").replace('\n', "&#10;").replace('\r', "&#13;")
}

fn unescape(text: &str) -> String {
    text.replace("&#13;", "\r").replace("&#10;", "\n").replace("&gt;", ">").replace("&lt;", "<").replace("&quot;", "\"").replace("&amp;", "&")
}

/// A mentioned thread as the agent reads it: its id, title and project.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Mention {
    pub thread: ThreadRef,
    pub project: Option<String>,
}

pub(crate) const REF_OPEN: &str = "<thread-ref id=\"";

/// `<thread-ref id="<id>" title="<title>" project="<project>"/>`.
pub(crate) fn render_ref(mention: &Mention) -> String {
    let mut text = format!("{REF_OPEN}{}\"", escape(&mention.thread.thread.0));
    for (name, value) in [("title", &mention.thread.title), ("project", &mention.project)] {
        if let Some(value) = value {
            text.push_str(&format!(" {name}=\"{}\"", escape(value)));
        }
    }
    text.push_str(CLOSE);
    text
}

/// A message part that is a thread reference and nothing else.
pub(crate) fn thread_ref(text: &str) -> Option<ThreadRef> {
    let inner = text.strip_prefix("<thread-ref")?.strip_suffix(CLOSE)?;
    let (mut id, mut title) = (None, None);
    for (name, value) in attributes(inner)? {
        match name {
            "id" => id = Some(ThreadId(value)),
            "title" => title = Some(value),
            "project" => {}
            _ => return None,
        }
    }
    Some(ThreadRef { thread: id.filter(|i| !i.0.is_empty())?, title })
}

/// `name="value"` pairs, unescaped; none if any is malformed or a value holds an unescaped close.
fn attributes(mut text: &str) -> Option<Vec<(&str, String)>> {
    let mut out = Vec::new();
    text = text.trim();
    while !text.is_empty() {
        let (name, rest) = text.split_once("=\"")?;
        let (value, rest) = rest.split_once('"')?;
        if value.contains(CLOSE) {
            return None;
        }
        out.push((name.trim(), unescape(value)));
        text = rest.trim_start();
    }
    Some(out)
}

/// `<thread-message from="<id>" title="<title>" report="completed"/>`.
pub(crate) fn render(marker: &Marker) -> String {
    let mut text = format!("{OPEN}{}\"", escape(&marker.from.0));
    if let Some(title) = &marker.title {
        text.push_str(&format!(" title=\"{}\"", escape(title)));
    }
    if let Some(end) = marker.report {
        text.push_str(&format!(" report=\"{}\"", end_word(end)));
    }
    text.push_str(CLOSE);
    text
}

/// The mark at the start of `text`, and how long it is.
fn leading(text: &str) -> Option<(Marker, usize)> {
    if !text.starts_with(OPEN) {
        return None;
    }
    // Every quote and angle bracket inside is escaped, so the first close ends it.
    let end = text.find(CLOSE)? + CLOSE.len();
    let (mut from, mut title, mut report) = (None, None, None);
    for (name, value) in attributes(&text["<thread-message".len()..end - CLOSE.len()])? {
        match name {
            "from" => from = Some(ThreadId(value)),
            "title" => title = Some(value),
            "report" => {
                report = Some(match value.as_str() {
                    "completed" => TurnEnd::Completed,
                    "stopped" => TurnEnd::Stopped,
                    "failed" => TurnEnd::Failed,
                    _ => return None,
                })
            }
            _ => return None,
        }
    }
    let from = from.filter(|f| !f.0.is_empty())?;
    Some((Marker { from, title, report }, end))
}

/// A message part that is a mark and nothing else.
pub(crate) fn part(text: &str) -> Option<Marker> {
    leading(text).filter(|(_, end)| *end == text.len()).map(|(m, _)| m)
}

/// The mark a thread's preview begins with (Codex joins the first message's parts, so the
/// brief's text follows it), and the rest of the preview.
pub(crate) fn from_preview(preview: &str) -> (Option<Marker>, &str) {
    match leading(preview) {
        Some((marker, end)) => (Some(marker), &preview[end..]),
        None => (None, preview),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_mark_reads_back_whole_whatever_the_title_holds() {
        let marker = Marker { from: ThreadId("01a2-b".into()), title: Some("Piles \"north\" <A&B>\nsecond".into()), report: Some(TurnEnd::Stopped) };
        let text = render(&marker);
        assert!(!text.contains('\n'));
        assert_eq!(part(&text), Some(marker.clone()));
        let preview = format!("{text}Check the piles./> and more");
        assert_eq!(from_preview(&preview), (Some(marker), "Check the piles./> and more"));
        let bare = Marker { from: ThreadId("x".into()), title: None, report: None };
        assert_eq!(render(&bare), "<thread-message from=\"x\"/>");
        assert_eq!(part(&render(&bare)), Some(bare));
    }

    #[test]
    fn text_that_only_looks_like_a_mark_is_none() {
        for text in ["<thread-message from=\"\"/>", "<thread-message from=\"a\" who=\"b\"/>", "<thread-message from=\"a\" report=\"maybe\"/>", "<thread-message from=\"a\"", "hello <thread-message from=\"a\"/>"] {
            assert_eq!(part(text), None, "{text}");
            assert_eq!(from_preview(text).0, None, "{text}");
        }
        assert_eq!(part("<thread-message from=\"a\"/> and text"), None, "a part is the mark alone");
    }

    #[test]
    fn a_reference_reads_back_whole_whatever_the_title_holds() {
        let mention = Mention { thread: ThreadRef { thread: ThreadId("01a2-b".into()), title: Some("Piles \"north\" <A&B>/>\nnext".into()) }, project: Some("Bridge & deck".into()) };
        let text = render_ref(&mention);
        assert!(!text.contains('\n'));
        assert_eq!(thread_ref(&text), Some(mention.thread.clone()));
        let bare = Mention { thread: ThreadRef { thread: ThreadId("x".into()), title: None }, project: None };
        assert_eq!(render_ref(&bare), "<thread-ref id=\"x\"/>");
        assert_eq!(thread_ref(&render_ref(&bare)), Some(bare.thread));
        for text in ["<thread-ref id=\"\"/>", "<thread-ref id=\"a\" who=\"b\"/>", "<thread-ref id=\"a\"", "see <thread-ref id=\"a\"/>", "<thread-ref id=\"a\"/> and text"] {
            assert_eq!(thread_ref(text), None, "{text}");
        }
    }
}
