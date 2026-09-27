//! A JSON reader and writer just big enough for `settings.json` and the state
//! record, so that `install` needs no jq and no crates.
//!
//! Two things make this different from a toy parser, and both exist for the
//! installer:
//!
//!   * every object it parses carries the BYTE SPANS of its braces and of each
//!     member's key and value. That is what lets the installer splice one
//!     member into the document text instead of reprinting it: every other byte
//!     of the user's `settings.json` - key order, indentation, blank lines,
//!     their `\/` escapes - survives literally. jq could not do that, and the
//!     reflow it caused was a documented wart of the shell installer.
//!   * it is STRICT. Trailing commas, comments, unquoted keys and a bare
//!     top-level scalar are all rejected, because the file is Claude Code's to
//!     parse and a document this tool cannot round-trip is a document it must
//!     refuse to touch.
//!
//! Strings come back as raw bytes with escapes folded, so a value can be
//! compared and re-emitted; bytes >= 0x80 are passed through unvalidated rather
//! than being rejected, since the goal is to preserve what is there.

use std::fmt::Write as _;

pub enum J {
    Null,
    Bool(bool),
    /// Kept as the raw source text: no float round-trip, no precision loss.
    Num(Vec<u8>),
    Str(Vec<u8>),
    Arr(Vec<J>),
    Obj(Obj),
}

pub struct Obj {
    pub members: Vec<Member>,
    /// Byte index of `{`.
    pub open: usize,
    /// Byte index of `}`.
    pub close: usize,
}

pub struct Member {
    pub key: Vec<u8>,
    pub val: J,
    /// Byte index of the opening quote of the key.
    pub start: usize,
    /// One past the last byte of the value - the member's text end, with no
    /// trailing comma or whitespace.
    pub end: usize,
    pub val_start: usize,
}

impl Obj {
    pub fn index_of(&self, key: &str) -> Option<usize> {
        self.members.iter().position(|m| m.key == key.as_bytes())
    }
    pub fn get(&self, key: &str) -> Option<&Member> {
        self.index_of(key).map(|i| &self.members[i])
    }
}

impl J {
    pub fn as_obj(&self) -> Option<&Obj> {
        match self {
            J::Obj(o) => Some(o),
            _ => None,
        }
    }
    pub fn as_str(&self) -> Option<&[u8]> {
        match self {
            J::Str(s) => Some(s),
            _ => None,
        }
    }
    pub fn as_bool(&self) -> Option<bool> {
        match self {
            J::Bool(b) => Some(*b),
            _ => None,
        }
    }
    /// The elements of an array, for the one array this crate reads back: the
    /// generated-file list in a standalone tree's marker.
    pub fn as_arr(&self) -> Option<&[J]> {
        match self {
            J::Arr(a) => Some(a),
            _ => None,
        }
    }
}

/// Structural equality, spans ignored. Object members must appear in the same
/// order: this is used as the installer's clobber guard, where a reordering is
/// exactly the kind of change that must be caught, not tolerated.
pub fn same(a: &J, b: &J) -> bool {
    match (a, b) {
        (J::Null, J::Null) => true,
        (J::Bool(x), J::Bool(y)) => x == y,
        (J::Num(x), J::Num(y)) => x == y,
        (J::Str(x), J::Str(y)) => x == y,
        (J::Arr(x), J::Arr(y)) => x.len() == y.len() && x.iter().zip(y).all(|(p, q)| same(p, q)),
        (J::Obj(x), J::Obj(y)) => {
            x.members.len() == y.members.len()
                && x.members
                    .iter()
                    .zip(&y.members)
                    .all(|(p, q)| p.key == q.key && same(&p.val, &q.val))
        }
        _ => false,
    }
}

// --- parsing ----------------------------------------------------------------

struct P<'a> {
    b: &'a [u8],
    i: usize,
    depth: u32,
}

/// Deep enough for any settings.json, shallow enough that a hostile file
/// cannot recurse this parser off the stack - which with `panic = "abort"`
/// would be a crash, not an error.
const MAX_DEPTH: u32 = 64;

pub fn parse(doc: &[u8]) -> Result<J, String> {
    let mut p = P { b: doc, i: 0, depth: 0 };
    p.ws();
    let v = p.value()?;
    p.ws();
    if p.i != p.b.len() {
        return Err(format!("trailing content at byte {}", p.i));
    }
    Ok(v)
}

impl P<'_> {
    fn ws(&mut self) {
        while self.i < self.b.len() && matches!(self.b[self.i], b' ' | b'\t' | b'\r' | b'\n') {
            self.i += 1;
        }
    }
    fn err<T>(&self, what: &str) -> Result<T, String> {
        Err(format!("{} at byte {}", what, self.i))
    }
    fn lit(&mut self, s: &[u8]) -> bool {
        if self.b[self.i..].starts_with(s) {
            self.i += s.len();
            true
        } else {
            false
        }
    }
    fn value(&mut self) -> Result<J, String> {
        if self.depth >= MAX_DEPTH {
            return self.err("nested too deeply");
        }
        if self.i >= self.b.len() {
            return self.err("unexpected end of document");
        }
        match self.b[self.i] {
            b'{' => self.object(),
            b'[' => self.array(),
            b'"' => Ok(J::Str(self.string()?)),
            b't' => {
                if self.lit(b"true") {
                    Ok(J::Bool(true))
                } else {
                    self.err("not a JSON value")
                }
            }
            b'f' => {
                if self.lit(b"false") {
                    Ok(J::Bool(false))
                } else {
                    self.err("not a JSON value")
                }
            }
            b'n' => {
                if self.lit(b"null") {
                    Ok(J::Null)
                } else {
                    self.err("not a JSON value")
                }
            }
            b'-' | b'0'..=b'9' => self.number(),
            _ => self.err("not a JSON value"),
        }
    }
    fn object(&mut self) -> Result<J, String> {
        let open = self.i;
        self.i += 1; // '{'
        self.depth += 1;
        let mut members: Vec<Member> = Vec::new();
        loop {
            self.ws();
            if self.i >= self.b.len() {
                return self.err("unterminated object");
            }
            if self.b[self.i] == b'}' {
                if !members.is_empty() {
                    // We only get here after a comma.
                    return self.err("trailing comma in object");
                }
                break;
            }
            if self.b[self.i] != b'"' {
                return self.err("object key is not a string");
            }
            let start = self.i;
            let key = self.string()?;
            self.ws();
            if self.i >= self.b.len() || self.b[self.i] != b':' {
                return self.err("expected ':' after object key");
            }
            self.i += 1;
            self.ws();
            let val_start = self.i;
            let val = self.value()?;
            members.push(Member { key, val, start, end: self.i, val_start });
            self.ws();
            if self.i < self.b.len() && self.b[self.i] == b',' {
                self.i += 1;
                continue;
            }
            break;
        }
        self.ws();
        if self.i >= self.b.len() || self.b[self.i] != b'}' {
            return self.err("expected '}'");
        }
        let close = self.i;
        self.i += 1;
        self.depth -= 1;
        Ok(J::Obj(Obj { members, open, close }))
    }
    fn array(&mut self) -> Result<J, String> {
        self.i += 1; // '['
        self.depth += 1;
        let mut items = Vec::new();
        loop {
            self.ws();
            if self.i >= self.b.len() {
                return self.err("unterminated array");
            }
            if self.b[self.i] == b']' {
                if !items.is_empty() {
                    return self.err("trailing comma in array");
                }
                break;
            }
            items.push(self.value()?);
            self.ws();
            if self.i < self.b.len() && self.b[self.i] == b',' {
                self.i += 1;
                continue;
            }
            break;
        }
        self.ws();
        if self.i >= self.b.len() || self.b[self.i] != b']' {
            return self.err("expected ']'");
        }
        self.i += 1;
        self.depth -= 1;
        Ok(J::Arr(items))
    }
    fn number(&mut self) -> Result<J, String> {
        let start = self.i;
        if self.i < self.b.len() && self.b[self.i] == b'-' {
            self.i += 1;
        }
        let ds = self.i;
        while self.i < self.b.len() && self.b[self.i].is_ascii_digit() {
            self.i += 1;
        }
        if self.i == ds {
            return self.err("number with no digits");
        }
        if self.b[ds] == b'0' && self.i - ds > 1 {
            return self.err("number with a leading zero");
        }
        if self.i < self.b.len() && self.b[self.i] == b'.' {
            self.i += 1;
            let fs = self.i;
            while self.i < self.b.len() && self.b[self.i].is_ascii_digit() {
                self.i += 1;
            }
            if self.i == fs {
                return self.err("number with no fraction digits");
            }
        }
        if self.i < self.b.len() && (self.b[self.i] | 0x20) == b'e' {
            self.i += 1;
            if self.i < self.b.len() && matches!(self.b[self.i], b'+' | b'-') {
                self.i += 1;
            }
            let es = self.i;
            while self.i < self.b.len() && self.b[self.i].is_ascii_digit() {
                self.i += 1;
            }
            if self.i == es {
                return self.err("number with no exponent digits");
            }
        }
        Ok(J::Num(self.b[start..self.i].to_vec()))
    }
    /// A JSON string, escapes folded into raw bytes. A `\uXXXX` escape is
    /// encoded as UTF-8, a surrogate pair is combined, and a lone surrogate
    /// becomes U+FFFD rather than an error: the aim is to read the file, not to
    /// audit it.
    fn string(&mut self) -> Result<Vec<u8>, String> {
        self.i += 1; // opening quote
        let mut out = Vec::new();
        loop {
            if self.i >= self.b.len() {
                return self.err("unterminated string");
            }
            let c = self.b[self.i];
            match c {
                b'"' => {
                    self.i += 1;
                    return Ok(out);
                }
                0x00..=0x1f => return self.err("raw control character in a string"),
                b'\\' => {
                    self.i += 1;
                    if self.i >= self.b.len() {
                        return self.err("unterminated escape");
                    }
                    let e = self.b[self.i];
                    self.i += 1;
                    match e {
                        b'"' => out.push(b'"'),
                        b'\\' => out.push(b'\\'),
                        b'/' => out.push(b'/'),
                        b'b' => out.push(0x08),
                        b'f' => out.push(0x0c),
                        b'n' => out.push(b'\n'),
                        b'r' => out.push(b'\r'),
                        b't' => out.push(b'\t'),
                        b'u' => {
                            let mut cp = self.hex4()? as u32;
                            if (0xd800..0xdc00).contains(&cp) {
                                // High surrogate: take the pair when it is there.
                                if self.b[self.i..].starts_with(b"\\u") {
                                    let save = self.i;
                                    self.i += 2;
                                    let lo = self.hex4()? as u32;
                                    if (0xdc00..0xe000).contains(&lo) {
                                        cp = 0x10000 + ((cp - 0xd800) << 10) + (lo - 0xdc00);
                                    } else {
                                        self.i = save;
                                        cp = 0xfffd;
                                    }
                                } else {
                                    cp = 0xfffd;
                                }
                            } else if (0xdc00..0xe000).contains(&cp) {
                                cp = 0xfffd;
                            }
                            push_utf8(&mut out, cp);
                        }
                        _ => return self.err("unknown escape"),
                    }
                }
                _ => {
                    out.push(c);
                    self.i += 1;
                }
            }
        }
    }
    fn hex4(&mut self) -> Result<u16, String> {
        if self.i + 4 > self.b.len() {
            return self.err("truncated \\u escape");
        }
        let mut v: u16 = 0;
        for k in 0..4 {
            let d = (self.b[self.i + k] as char).to_digit(16);
            match d {
                Some(d) => v = v * 16 + d as u16,
                None => return self.err("bad hex digit in \\u escape"),
            }
        }
        self.i += 4;
        Ok(v)
    }
}

pub fn push_utf8(out: &mut Vec<u8>, cp: u32) {
    match cp {
        0..=0x7f => out.push(cp as u8),
        0x80..=0x7ff => {
            out.push(0xc0 | (cp >> 6) as u8);
            out.push(0x80 | (cp & 0x3f) as u8);
        }
        0x800..=0xffff => {
            out.push(0xe0 | (cp >> 12) as u8);
            out.push(0x80 | ((cp >> 6) & 0x3f) as u8);
            out.push(0x80 | (cp & 0x3f) as u8);
        }
        _ => {
            out.push(0xf0 | (cp >> 18) as u8);
            out.push(0x80 | ((cp >> 12) & 0x3f) as u8);
            out.push(0x80 | ((cp >> 6) & 0x3f) as u8);
            out.push(0x80 | (cp & 0x3f) as u8);
        }
    }
}

// --- writing ----------------------------------------------------------------

/// A JSON string literal, including the quotes.
///
/// A byte sequence that is not valid UTF-8 is written as U+FFFD, one per
/// offending byte. JSON text has to be UTF-8, and the alternative - refusing to
/// write the state file because a path on this machine is not - would be worse
/// than recording a path that cannot be restored verbatim.
pub fn quote(s: &[u8]) -> String {
    let text = crate::text::repair(s);
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            c if (c as u32) < 0x20 || c == '\u{7f}' => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            // Everything else, including a repaired U+FFFD, is legal raw.
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn obj(doc: &str) -> Obj {
        match parse(doc.as_bytes()) {
            Ok(J::Obj(o)) => o,
            Ok(_) => panic!("expected an object, got another JSON value"),
            Err(e) => panic!("expected an object, got the refusal {:?}", e),
        }
    }

    fn str_of(doc: &str, key: &str) -> Vec<u8> {
        match &obj(doc).get(key).expect("the key").val {
            J::Str(s) => s.clone(),
            _ => panic!("not a string"),
        }
    }

    #[test]
    fn escapes_are_folded_and_surrogate_pairs_are_joined() {
        assert_eq!(str_of(r#"{"k":"a\nb"}"#, "k"), b"a\nb");
        assert_eq!(str_of(r#"{"k":"a\/b"}"#, "k"), b"a/b");
        assert_eq!(str_of(r#"{"k":"A"}"#, "k"), b"A");
        // U+1F535, as a surrogate pair.
        assert_eq!(str_of(r#"{"k":"🔵"}"#, "k"), "\u{1f535}".as_bytes());
        // A LONE surrogate is not a character, so it comes through as U+FFFD
        // rather than as an invalid encoding.
        assert_eq!(str_of(r#"{"k":"\ud83d"}"#, "k"), "\u{fffd}".as_bytes());
    }

    #[test]
    fn duplicate_keys_are_preserved_rather_than_collapsed() {
        // The installer refuses a document with duplicate keys; it can only do
        // that if the parser hands it both.
        let o = obj(r#"{"a":1,"a":2}"#);
        assert_eq!(o.members.len(), 2);
    }

    #[test]
    fn the_spans_point_at_the_document_text() {
        // This is what lets the installer splice one member into the document
        // instead of reprinting it.
        let doc = r#"{"a": 12 }"#;
        let o = obj(doc);
        assert_eq!(o.open, 0);
        assert_eq!(o.close, 9);
        let m = o.get("a").expect("the key");
        assert_eq!(&doc[m.start..m.end], r#""a": 12"#);
        assert_eq!(&doc[m.val_start..m.end], "12");
    }

    #[test]
    fn the_parser_is_strict() {
        for doc in [
            r#"{"a":1,}"#,
            r#"{a:1}"#,
            r#"{'a':1}"#,
            r#"{"a":01}"#,
            r#"{"a":1}// trailing"#,
            r#"{"a":/*c*/1}"#,
            r#"{"a":}"#,
            r#"{"a"}"#,
            r#"{"a":1"#,
            "{\"a\":\"raw\u{9}control\"}",
            "",
        ] {
            assert!(parse(doc.as_bytes()).is_err(), "should refuse {:?}", doc);
        }
    }

    #[test]
    fn nesting_past_max_depth_is_refused() {
        let n = MAX_DEPTH as usize;
        let ok = "[".repeat(n - 1) + &"]".repeat(n - 1);
        assert!(parse(ok.as_bytes()).is_ok());
        let deep = "[".repeat(n + 1) + &"]".repeat(n + 1);
        assert!(parse(deep.as_bytes()).is_err());
    }

    #[test]
    fn the_writer_escapes_what_a_string_literal_cannot_hold() {
        assert_eq!(quote(b"plain"), r#""plain""#);
        assert_eq!(quote(b"a\"b"), r#""a\"b""#);
        assert_eq!(quote(b"a\\b"), r#""a\\b""#);
        assert_eq!(quote(b"\n\r\t"), r#""\n\r\t""#);
        assert_eq!(quote(&[0x08, 0x0c]), r#""\b\f""#);
        assert_eq!(quote(&[0x01, 0x1f, 0x7f]), "\"\\u0001\\u001f\\u007f\"");
        // A forward slash needs no escape, and U+0085 is legal raw.
        assert_eq!(quote(b"a/b"), r#""a/b""#);
        assert_eq!(quote("\u{85}".as_bytes()), "\"\u{85}\"");
    }

    #[test]
    fn the_writer_repairs_a_path_that_is_not_valid_utf8() {
        // `install` quotes filesystem paths into the state record, and a path
        // the kernel accepts can be bytes JSON cannot carry. One U+FFFD per
        // offending byte, and never a refusal to write the record.
        assert_eq!(quote(b"/home/a\xffb"), "\"/home/a\u{fffd}b\"");
        assert_eq!(quote(b"\xe2\x82"), "\"\u{fffd}\u{fffd}\"");
        // And the result parses back as a string.
        let doc = format!("{{\"repo\":{}}}", quote(b"/tmp/\xffx"));
        assert_eq!(str_of(&doc, "repo"), "/tmp/\u{fffd}x".as_bytes());
    }

    #[test]
    fn a_round_trip_through_quote_and_parse_keeps_the_bytes() {
        for raw in [
            &b"plain"[..],
            &b"a\"b\\c"[..],
            &b"tab\there"[..],
            "caf\u{e9} \u{1f535}".as_bytes(),
        ] {
            let doc = format!("{{\"k\":{}}}", quote(raw));
            assert_eq!(str_of(&doc, "k"), raw, "{:?}", raw);
        }
    }
}
