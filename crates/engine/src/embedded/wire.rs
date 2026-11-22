//! The wire: the allocation-free pre-scan, the hand-written visitors that read
//! every key and value as `&RawValue`, the engine's own unescaper, and the
//! writer. Nothing here unescapes through serde_json or derives
//! `Deserialize` on a frame.

use serde::de::{DeserializeSeed, Deserializer, MapAccess, SeqAccess, Visitor};
use serde_json::value::RawValue;

use super::budget::{DEPTH, FRAME_TEXT};

/// An identity's bound, and a reported fact's.
pub(crate) const IDENTITY: usize = 256;
/// `cwd`'s and an error message's bound.
pub(crate) const LONG_TEXT: usize = 1024;
/// A constant's bound (`status`, `item_type`).
pub(crate) const CONSTANT: usize = 64;

// ---------- the pre-scan ----------

/// Whether `text` is one JSON value of at most [`FRAME_TEXT`] bytes nesting at
/// most [`DEPTH`] deep: a strict RFC 8259 check that allocates nothing.
pub(crate) fn prescan(text: &str) -> bool {
    text.len() <= FRAME_TEXT && prescan_within(text, DEPTH)
}

/// The deepest `tools` may nest: `thread/start` writes it inside four
/// containers (the frame, `params`, `dynamicTools` and the namespace).
pub(crate) const TOOLS_DEPTH: usize = DEPTH - 4;

/// Whether `text` is one JSON value nesting at most `depth` deep.
pub(crate) fn prescan_within(text: &str, depth: usize) -> bool {
    Scan { b: text.as_bytes(), i: 0, max: depth }.document()
}

struct Scan<'a> {
    b: &'a [u8],
    i: usize,
    max: usize,
}

impl Scan<'_> {
    fn peek(&self) -> Option<u8> {
        self.b.get(self.i).copied()
    }

    fn ws(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\t' | b'\n' | b'\r')) {
            self.i += 1;
        }
    }

    fn eat(&mut self, byte: u8) -> bool {
        let hit = self.peek() == Some(byte);
        self.i += hit as usize;
        hit
    }

    fn document(&mut self) -> bool {
        // Bit `d` of `objects` is set when the container at depth `d` is an object.
        let (mut depth, mut objects) = (0usize, 0u128);
        self.ws();
        'value: loop {
            match self.peek() {
                Some(open @ (b'{' | b'[')) => {
                    self.i += 1;
                    depth += 1;
                    if depth > self.max {
                        return false;
                    }
                    let object = open == b'{';
                    objects = (objects & !(1 << depth)) | ((object as u128) << depth);
                    self.ws();
                    if !self.eat(if object { b'}' } else { b']' }) {
                        if object && !self.key() {
                            return false;
                        }
                        continue 'value;
                    }
                    depth -= 1;
                }
                Some(b'"') => {
                    if !self.string() {
                        return false;
                    }
                }
                Some(b't') => self.literal(b"true"),
                Some(b'f') => self.literal(b"false"),
                Some(b'n') => self.literal(b"null"),
                Some(b'-' | b'0'..=b'9') => {
                    if !self.number() {
                        return false;
                    }
                }
                _ => return false,
            }
            if self.i > self.b.len() {
                return false;
            }
            // After a value: close containers until one takes another value.
            loop {
                self.ws();
                if depth == 0 {
                    return self.i == self.b.len();
                }
                let object = objects & (1 << depth) != 0;
                if self.eat(b',') {
                    self.ws();
                    if object && !self.key() {
                        return false;
                    }
                    continue 'value;
                }
                if !self.eat(if object { b'}' } else { b']' }) {
                    return false;
                }
                depth -= 1;
            }
        }
    }

    /// A member's key and its colon, leaving the scan at its value.
    fn key(&mut self) -> bool {
        if self.peek() != Some(b'"') || !self.string() {
            return false;
        }
        self.ws();
        let colon = self.eat(b':');
        self.ws();
        colon
    }

    fn literal(&mut self, word: &[u8]) {
        if self.b[self.i..].starts_with(word) {
            self.i += word.len();
        } else {
            self.i = usize::MAX;
        }
    }

    fn string(&mut self) -> bool {
        self.i += 1;
        loop {
            match self.peek() {
                Some(b'"') => {
                    self.i += 1;
                    return true;
                }
                Some(b'\\') => {
                    self.i += 1;
                    match self.peek() {
                        Some(b'"' | b'\\' | b'/' | b'b' | b'f' | b'n' | b'r' | b't') => self.i += 1,
                        Some(b'u') => {
                            let hex = self.b.get(self.i + 1..self.i + 5);
                            if !hex.is_some_and(|h| h.iter().all(u8::is_ascii_hexdigit)) {
                                return false;
                            }
                            self.i += 5;
                        }
                        _ => return false,
                    }
                }
                Some(0x00..=0x1f) | None => return false,
                Some(_) => self.i += 1,
            }
        }
    }

    fn number(&mut self) -> bool {
        self.eat(b'-');
        match self.peek() {
            Some(b'0') => self.i += 1,
            Some(b'1'..=b'9') => self.digits(),
            _ => return false,
        }
        if self.eat(b'.') && !self.some_digits() {
            return false;
        }
        if matches!(self.peek(), Some(b'e' | b'E')) {
            self.i += 1;
            if matches!(self.peek(), Some(b'+' | b'-')) {
                self.i += 1;
            }
            return self.some_digits();
        }
        true
    }

    fn digits(&mut self) {
        while matches!(self.peek(), Some(b'0'..=b'9')) {
            self.i += 1;
        }
    }

    fn some_digits(&mut self) -> bool {
        let start = self.i;
        self.digits();
        self.i > start
    }
}

/// `text` without JSON's whitespace around it.
pub(crate) fn trim(text: &str) -> &str {
    text.trim_matches([' ', '\t', '\n', '\r'])
}

// ---------- the visitors ----------

/// The values of `keys` in the object `raw`, each key matched on its raw
/// text, so an escaped key matches nothing. `None` when `raw` is not an
/// object, or names one of `keys` twice.
pub(crate) fn pick<'a, const N: usize>(raw: &'a str, keys: [&str; N]) -> Option<[Option<&'a RawValue>; N]> {
    if !raw.starts_with('{') {
        return None;
    }
    let (mut out, mut twice) = ([None; N], false);
    let mut de = serde_json::Deserializer::from_str(raw);
    Pick { keys, out: &mut out, twice: &mut twice }.deserialize(&mut de).ok()?;
    de.end().ok()?;
    (!twice).then_some(out)
}

struct Pick<'k, 'o, 'a, const N: usize> {
    keys: [&'k str; N],
    out: &'o mut [Option<&'a RawValue>; N],
    twice: &'o mut bool,
}

impl<'a, const N: usize> DeserializeSeed<'a> for Pick<'_, '_, 'a, N> {
    type Value = ();

    fn deserialize<D: Deserializer<'a>>(self, de: D) -> Result<(), D::Error> {
        de.deserialize_map(self)
    }
}

impl<'a, const N: usize> Visitor<'a> for Pick<'_, '_, 'a, N> {
    type Value = ();

    fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.write_str("an object")
    }

    fn visit_map<A: MapAccess<'a>>(self, mut map: A) -> Result<(), A::Error> {
        while let Some(key) = map.next_key::<&'a RawValue>()? {
            let value = map.next_value::<&'a RawValue>()?;
            let key = key.get();
            let bare = &key[1..key.len() - 1];
            if let Some(at) = self.keys.iter().position(|k| *k == bare) {
                *self.twice |= self.out[at].replace(value).is_some();
            }
        }
        Ok(())
    }
}

/// Calls `each` with every element of the array `raw`, in order; `None` when
/// `raw` is not an array, otherwise the count.
pub(crate) fn elements<'a>(raw: &'a str, each: &mut dyn FnMut(&'a RawValue)) -> Option<usize> {
    if !raw.starts_with('[') {
        return None;
    }
    let mut count = 0;
    let mut de = serde_json::Deserializer::from_str(raw);
    Elements { each, count: &mut count }.deserialize(&mut de).ok()?;
    de.end().ok()?;
    Some(count)
}

struct Elements<'e, 'a> {
    each: &'e mut dyn FnMut(&'a RawValue),
    count: &'e mut usize,
}

impl<'a> DeserializeSeed<'a> for Elements<'_, 'a> {
    type Value = ();

    fn deserialize<D: Deserializer<'a>>(self, de: D) -> Result<(), D::Error> {
        de.deserialize_seq(self)
    }
}

impl<'a> Visitor<'a> for Elements<'_, 'a> {
    type Value = ();

    fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.write_str("an array")
    }

    fn visit_seq<A: SeqAccess<'a>>(self, mut seq: A) -> Result<(), A::Error> {
        while let Some(element) = seq.next_element::<&'a RawValue>()? {
            *self.count += 1;
            (self.each)(element);
        }
        Ok(())
    }
}

// ---------- values ----------

pub(crate) fn is_object(raw: &RawValue) -> bool {
    raw.get().starts_with('{')
}

pub(crate) fn is_null(raw: &RawValue) -> bool {
    raw.get() == "null"
}

pub(crate) fn boolean(raw: &RawValue) -> Option<bool> {
    match raw.get() {
        "true" => Some(true),
        "false" => Some(false),
        _ => None,
    }
}

/// An integer, parsed from its raw text: no fraction, no exponent, within i64.
pub(crate) fn integer(raw: &RawValue) -> Option<i64> {
    let text = raw.get();
    let (negative, digits) = match text.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, text),
    };
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    // Accumulated negative, so i64::MIN fits.
    let mut value: i64 = 0;
    for digit in digits.bytes() {
        value = value.checked_mul(10)?.checked_sub((digit - b'0') as i64)?;
    }
    if negative { Some(value) } else { value.checked_neg() }
}

/// One of Codex's own constants: a string without escapes, at most
/// [`CONSTANT`] bytes. An escaped one is not taken.
pub(crate) fn constant(raw: &RawValue) -> Option<&str> {
    bare(raw, CONSTANT)
}

/// A string without escapes, at most `max` bytes, as it is in the frame.
pub(crate) fn bare(raw: &RawValue, max: usize) -> Option<&str> {
    let inner = raw.get().strip_prefix('"')?.strip_suffix('"')?;
    (!inner.contains('\\') && inner.len() <= max).then_some(inner)
}

/// A string unescaped by the engine into `buf`: `\"`, `\\`, `\/`, the five
/// control escapes and `\u` (pairs for astral characters). `None` when `raw`
/// is not a string, holds a lone surrogate, or does not fit `buf`.
pub(crate) fn unescape<'b>(raw: &RawValue, buf: &'b mut [u8]) -> Option<&'b str> {
    unescape_text(raw.get().strip_prefix('"')?.strip_suffix('"')?, buf)
}

/// A string's text between its quotes, unescaped into `buf`; `None` for any
/// escape outside JSON's.
fn unescape_text<'b>(inner: &str, buf: &'b mut [u8]) -> Option<&'b str> {
    let inner = inner.as_bytes();
    let (mut i, mut n) = (0, 0);
    while i < inner.len() {
        let byte = inner[i];
        if byte != b'\\' {
            *buf.get_mut(n)? = byte;
            n += 1;
            i += 1;
            continue;
        }
        let simple = match *inner.get(i + 1)? {
            b'"' => Some(b'"'),
            b'\\' => Some(b'\\'),
            b'/' => Some(b'/'),
            b'b' => Some(0x08),
            b'f' => Some(0x0c),
            b'n' => Some(b'\n'),
            b'r' => Some(b'\r'),
            b't' => Some(b'\t'),
            b'u' => None,
            _ => return None,
        };
        if let Some(byte) = simple {
            *buf.get_mut(n)? = byte;
            n += 1;
            i += 2;
            continue;
        }
        let high = hex4(inner.get(i + 2..i + 6)?)?;
        i += 6;
        let code = match high {
            0xd800..=0xdbff => {
                let low = match inner.get(i..i + 2) {
                    Some(b"\\u") => hex4(inner.get(i + 2..i + 6)?)?,
                    _ => return None,
                };
                if !(0xdc00..=0xdfff).contains(&low) {
                    return None;
                }
                i += 6;
                0x10000 + ((high - 0xd800) << 10) + (low - 0xdc00)
            }
            0xdc00..=0xdfff => return None,
            code => code,
        };
        let ch = char::from_u32(code)?;
        let len = ch.len_utf8();
        ch.encode_utf8(buf.get_mut(n..n + len)?);
        n += len;
    }
    std::str::from_utf8(&buf[..n]).ok()
}

/// Four hex digits, and nothing else (no sign).
fn hex4(digits: &[u8]) -> Option<u32> {
    let digits: &[u8; 4] = digits.try_into().ok()?;
    digits.iter().try_fold(0, |code, digit| Some(code * 16 + (*digit as char).to_digit(16)?))
}

/// An identity: a non-empty string of at most 256 bytes once unescaped, with
/// no control characters (SWBPIPE's rule).
pub(crate) fn identity<'b>(raw: &RawValue, buf: &'b mut [u8; IDENTITY]) -> Option<&'b str> {
    unescape(raw, buf).filter(|s| is_identity(s))
}

pub(crate) fn is_identity(s: &str) -> bool {
    !s.is_empty() && s.len() <= IDENTITY && !s.chars().any(char::is_control)
}

// ---------- frames ----------

/// A valid frame, still borrowed from its text.
pub(crate) enum Frame<'a> {
    Request { id: &'a RawValue, method: &'a RawValue, params: Option<&'a RawValue> },
    Notification { method: &'a RawValue, params: Option<&'a RawValue> },
    Response { id: i64, reply: Result<&'a RawValue, CodexError<'a>> },
}

pub(crate) struct CodexError<'a> {
    pub code: i64,
    pub message: &'a RawValue,
}

/// The frame `text` is, or `None` when it is not exactly one of a request, a
/// notification or a response. Other members are ignored.
pub(crate) fn frame(text: &str) -> Option<Frame<'_>> {
    if !prescan(text) {
        return None;
    }
    let [id, method, params, result, error] = pick(trim(text), ["id", "method", "params", "result", "error"])?;
    if params.is_some_and(|p| !is_object(p)) {
        return None;
    }
    let buf = &mut [0; IDENTITY];
    if let Some(method) = method {
        if result.is_some() || error.is_some() || identity(method, buf).is_none() {
            return None;
        }
        return match id {
            None => Some(Frame::Notification { method, params }),
            Some(id) if integer(id).is_some() || identity(id, buf).is_some() => Some(Frame::Request { id, method, params }),
            Some(_) => None,
        };
    }
    if params.is_some() {
        return None;
    }
    let id = integer(id?)?;
    let reply = match (result, error) {
        (Some(result), None) => Ok(result),
        (None, Some(error)) => {
            let [code, message] = pick(error.get(), ["code", "message"])?;
            let message = message.filter(|m| m.get().starts_with('"'))?;
            Err(CodexError { code: integer(code?)?, message })
        }
        _ => return None,
    };
    Some(Frame::Response { id, reply })
}

// ---------- the writer ----------

/// The one writer: a buffer of [`FRAME_TEXT`] bytes, allocated once, that
/// refuses a frame past it without allocating.
pub(crate) struct Writer {
    buf: Vec<u8>,
    len: usize,
    over: bool,
    store: bool,
}

impl Writer {
    pub(crate) fn new() -> Writer {
        Writer { buf: Vec::with_capacity(FRAME_TEXT), len: 0, over: false, store: true }
    }

    /// A writer that only measures what it would write, allocating nothing.
    pub(crate) fn measure() -> Writer {
        Writer { buf: Vec::new(), len: 0, over: false, store: false }
    }

    #[cfg(test)]
    pub(crate) fn capacity(&self) -> usize {
        self.buf.capacity()
    }

    pub(crate) fn begin(&mut self) -> &mut Writer {
        self.buf.clear();
        (self.len, self.over) = (0, false);
        self
    }

    pub(crate) fn raw(&mut self, text: &str) -> &mut Writer {
        if !self.over && self.len + text.len() <= FRAME_TEXT {
            if self.store {
                self.buf.extend_from_slice(text.as_bytes());
            }
            self.len += text.len();
        } else {
            self.over = true;
        }
        self
    }

    /// `text` as a JSON string.
    pub(crate) fn string(&mut self, text: &str) -> &mut Writer {
        self.raw("\"");
        let mut plain = 0;
        for (at, byte) in text.bytes().enumerate() {
            let escape = match byte {
                b'"' => "\\\"",
                b'\\' => "\\\\",
                b'\n' => "\\n",
                b'\r' => "\\r",
                b'\t' => "\\t",
                0x08 => "\\b",
                0x0c => "\\f",
                0x00..=0x1f => "",
                _ => continue,
            };
            self.raw(&text[plain..at]);
            if escape.is_empty() {
                let hex = b"0123456789abcdef";
                let code = [b'\\', b'u', b'0', b'0', hex[(byte >> 4) as usize], hex[(byte & 15) as usize]];
                self.raw(std::str::from_utf8(&code).unwrap_or_default());
            } else {
                self.raw(escape);
            }
            plain = at + 1;
        }
        self.raw(&text[plain..]).raw("\"")
    }

    pub(crate) fn integer(&mut self, value: i64) -> &mut Writer {
        let mut digits = [0u8; 20];
        let mut at = digits.len();
        let mut rest = value.unsigned_abs();
        loop {
            at -= 1;
            digits[at] = b'0' + (rest % 10) as u8;
            rest /= 10;
            if rest == 0 {
                break;
            }
        }
        if value < 0 {
            self.raw("-");
        }
        self.raw(std::str::from_utf8(&digits[at..]).unwrap_or_default())
    }

    /// Whether what was written fits a frame.
    pub(crate) fn fits(&self) -> bool {
        !self.over
    }

    /// The frame built, or `None` when it would not fit.
    pub(crate) fn text(&self) -> Option<&str> {
        if self.over || !self.store { None } else { std::str::from_utf8(&self.buf).ok() }
    }
}

#[cfg(test)]
#[path = "wire_tests.rs"]
mod tests;
