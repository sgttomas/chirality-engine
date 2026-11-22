//! The mark on a message one thread sends another: the first part of the
//! message, naming the sender (and, on a report, how the turn ended), so the
//! receiving thread's history says where it came from. Codex's listing
//! carries a thread's first message whole as its preview, so the mark on a
//! brief is where an assignment is read from.

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
    let mut attributes = text["<thread-message".len()..end - CLOSE.len()].trim();
    let (mut from, mut title, mut report) = (None, None, None);
    while !attributes.is_empty() {
        let (name, rest) = attributes.split_once("=\"")?;
        let (value, rest) = rest.split_once('"')?;
        let value = unescape(value);
        match name.trim() {
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
        attributes = rest.trim_start();
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
}
