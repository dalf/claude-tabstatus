//! The hook payload, read down to two bounded windows and searched as bytes.
//!
//! Genuinely byte-oriented, and the one part of this program that stays so: the
//! discriminators are exact JSON needles, quotes included, so an escaped copy
//! inside another string cannot match them, and nothing here is ever shown to
//! anybody. Decoding a 4 MB `tool_response` to look for `"agent_id"` would be
//! work spent to reach the same answer.

use std::io::{self, Read};

/// How much of the FRONT of the payload is ever looked at.
///
/// `agent_id` and `source` are serialized before anything unbounded, so a prefix
/// is the right window for both and the search cost is a CONSTANT: a 1 MB
/// `tool_response` costs the same as an empty one. That bound is the only reason
/// the `working` edge can afford to look at its payload at all, on an edge that
/// fires once per tool call against a 5s hook timeout.
///
/// `notification_type` is NOT one of them - see [`PAYLOAD_TAIL`]. README's
/// "States" section carries the capture measurements behind both windows.
pub const PAYLOAD_PREFIX: usize = 8192;

/// How much of the BACK of the payload is looked at, and why there has to be a
/// back at all.
///
/// A Notification serializes `notification_type` LAST, after an unbounded
/// `message` that an MCP elicitation lets the server supply. A front window
/// alone therefore loses the discriminator once the message is long enough, and
/// the notify edge falls through to painting nothing - on exactly the kinds for
/// which the Notification is the ONLY signal, so the tab stays blue while Claude
/// waits on a dialog. A window at each END fixes that and keeps the bound: the
/// search work stays constant and a first or last member is always within 8 KiB
/// of an end.
///
/// Only the notify edge consults the tail. `agent_id` deliberately does not: a
/// FALSE POSITIVE there silences every `working` repaint for the rest of the
/// session, and a PostToolUse payload ends in `tool_response`, which can be a
/// JSON OBJECT whose keys are not escaped. Prefix-only keeps that edge's failure
/// in the harmless direction.
pub const PAYLOAD_TAIL: usize = 8192;

/// The two windows of the payload's first line that are ever searched, and
/// nothing else.
#[derive(Default)]
pub struct Payload {
    head: Vec<u8>,
    tail: Vec<u8>,
}

impl Payload {
    /// What the edges that have no payload test see, and what an interactive
    /// run sees: no discriminator anywhere, so paint the edge as asked.
    pub fn empty() -> Payload {
        Payload::default()
    }

    /// Read the payload's first line down to its first [`PAYLOAD_PREFIX`] bytes and
    /// its last [`PAYLOAD_TAIL`] bytes, and drain the rest without searching it.
    ///
    /// The drain is not optional: stdin has to reach EOF, or Claude Code's writer
    /// sees EPIPE on a pipe it is still filling. What stays CONSTANT is the SEARCH
    /// work - however big the payload, the needles are looked for in at most 16 KiB.
    /// The linear part is the kernel's pipe copy, charged to the writer either way.
    pub fn read() -> Payload {
        Payload::from_reader(&mut io::stdin().lock())
    }

    /// Built from bytes rather than from stdin, so the unit tests in this crate
    /// can exercise the windowing without a process boundary.
    #[cfg(test)]
    pub fn from_bytes(bytes: &[u8]) -> Payload {
        Payload::from_reader(&mut &bytes[..])
    }

    fn from_reader(r: &mut impl Read) -> Payload {
        let mut head: Vec<u8> = Vec::with_capacity(1024);
        let mut tail: Vec<u8> = Vec::new();
        let mut len: usize = 0;
        let mut done = false;
        let mut buf = [0u8; 65536];
        while let Some(n) = read_chunk(r, &mut buf) {
            if done {
                continue;
            }
            let chunk = &buf[..n];
            let part = match chunk.iter().position(|&b| b == b'\n') {
                Some(i) => {
                    done = true;
                    &chunk[..i]
                }
                None => chunk,
            };
            len += part.len();
            if head.len() < PAYLOAD_PREFIX {
                let room = PAYLOAD_PREFIX - head.len();
                head.extend_from_slice(&part[..room.min(part.len())]);
            }
            // The tail SLIDES rather than being accumulated: a chunk at least
            // PAYLOAD_TAIL long replaces the window outright, so the usual
            // 64 KiB read costs one 8 KiB copy and the window never grows.
            if part.len() >= PAYLOAD_TAIL {
                tail.clear();
                tail.extend_from_slice(&part[part.len() - PAYLOAD_TAIL..]);
            } else if !part.is_empty() {
                tail.extend_from_slice(part);
                if tail.len() > PAYLOAD_TAIL {
                    tail.drain(..tail.len() - PAYLOAD_TAIL);
                }
            }
        }
        // A line that fits in `head` has no tail to search: keeping one would
        // only double the work. A line between the two bounds keeps an
        // OVERLAPPING tail on purpose, so a needle straddling byte 8192 is
        // still found whole in one window or the other.
        if len <= head.len() {
            tail.clear();
        }
        // NUL bytes are dropped, which is what the reference implementation's
        // `read` builtin did with them, and also the more conservative reading
        // of a payload that should not have contained them: a NUL cannot be used
        // to splice a discriminator together.
        for w in [&mut head, &mut tail] {
            if w.contains(&0) {
                w.retain(|&b| b != 0);
            }
        }
        Payload { head, tail }
    }

    /// A discriminator that is serialized near the front.
    pub fn head_has(&self, needle: &[u8]) -> bool {
        find(&self.head, needle).is_some()
    }

    /// A discriminator that may be the LAST member of the object.
    pub fn has(&self, needle: &[u8]) -> bool {
        find(&self.head, needle).is_some() || find(&self.tail, needle).is_some()
    }

    /// `"<field>"` present near the front as a member whose value is a
    /// NON-EMPTY string.
    ///
    /// "Non-empty" is the point rather than pedantry: `agent_id` is documented as
    /// "present only when the hook fires inside a subagent call", and the whole
    /// subagent filter turns on ABSENT vs PRESENT. A future Claude Code that sent
    /// `"agent_id":""` on the main thread would, with a plain substring test,
    /// silence every `working` repaint in the session - a tab stuck orange or blue
    /// forever. This costs one more byte comparison and removes that failure mode.
    pub fn head_has_field(&self, field: &[u8]) -> bool {
        has_string_field(&self.head, field)
    }
}

/// The `cat >/dev/null` case, for the edges that have no payload test: consume
/// stdin so the writer never sees EPIPE, and never look at the bytes.
pub fn drain_stdin() {
    let mut buf = [0u8; 65536];
    let mut s = io::stdin().lock();
    while read_chunk(&mut s, &mut buf).is_some() {}
}

fn read_chunk(r: &mut impl Read, buf: &mut [u8]) -> Option<usize> {
    loop {
        match r.read(buf) {
            Ok(0) => return None,
            Ok(n) => return Some(n),
            Err(ref e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(_) => return None,
        }
    }
}

/// The first offset at which `needle` occurs in `hay`, skipping on the first byte
/// rather than comparing every window. Same length of code, same answer, and it
/// cost 14ms less on a 1 MB line back before the read was bounded.
fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() {
        return Some(0);
    }
    if needle.len() > hay.len() {
        return None;
    }
    let first = needle[0];
    let last = hay.len() - needle.len();
    let mut i = 0;
    while i <= last {
        let j = i + hay[i..=last].iter().position(|&b| b == first)?;
        if &hay[j..j + needle.len()] == needle {
            return Some(j);
        }
        i = j + 1;
    }
    None
}

/// Spaces around the colon are skipped. An escaped occurrence inside another
/// string cannot match: in `"tool_response":"{\"agent_id\":\"x\"}"` the byte
/// after `agent_id` is a backslash, not a quote.
///
/// Bytes, like the rest of this module, but as slice steps rather than indices:
/// every bound is a `strip_prefix` or a slice pattern, so a needle at the very end
/// of the window is a slice too short to match rather than a read past it.
fn has_string_field(payload: &[u8], field: &[u8]) -> bool {
    let mut needle = Vec::with_capacity(field.len() + 2);
    needle.push(b'"');
    needle.extend_from_slice(field);
    needle.push(b'"');
    let mut from = 0;
    while let Some(k) = find(&payload[from..], &needle) {
        let after = skip_spaces(&payload[from + k + needle.len()..]);
        if let Some(value) = after.strip_prefix(b":".as_slice()) {
            // A quote that is not immediately closed: a non-empty string.
            if matches!(skip_spaces(value), [b'"', c, ..] if *c != b'"') {
                return true;
            }
        }
        from += k + 1;
    }
    false
}

fn skip_spaces(b: &[u8]) -> &[u8] {
    match b.iter().position(|&c| c != b' ') {
        Some(i) => &b[i..],
        None => &[],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn of(bytes: &[u8]) -> Payload {
        Payload::from_bytes(bytes)
    }

    #[test]
    fn a_short_line_is_all_head_and_no_tail() {
        let p = of(b"{\"notification_type\":\"idle_prompt\"}\n");
        assert!(p.has(b"\"notification_type\":\"idle_prompt\""));
        assert!(p.head_has(b"\"notification_type\":\"idle_prompt\""));
        assert!(p.tail.is_empty(), "a line that fits in head keeps no tail");
    }

    #[test]
    fn only_the_first_line_is_searched() {
        let p = of(b"{}\n{\"notification_type\":\"idle_prompt\"}\n");
        assert!(!p.has(b"idle_prompt"));
    }

    #[test]
    fn the_head_is_bounded_and_the_tail_slides() {
        // A first line longer than both windows: the needle in the middle is
        // unreachable, the ones at the ends are not.
        let mut line = Vec::new();
        line.extend_from_slice(b"FRONT");
        line.resize(PAYLOAD_PREFIX + 100, b'x');
        line.extend_from_slice(b"MIDDLE");
        line.resize(PAYLOAD_PREFIX + 4 * PAYLOAD_TAIL, b'y');
        line.extend_from_slice(b"BACK");
        line.push(b'\n');
        let p = of(&line);
        assert_eq!(p.head.len(), PAYLOAD_PREFIX);
        assert_eq!(p.tail.len(), PAYLOAD_TAIL);
        assert!(p.has(b"FRONT"));
        assert!(p.has(b"BACK"));
        assert!(!p.has(b"MIDDLE"), "nothing between the two windows is searched");
    }

    #[test]
    fn the_two_windows_overlap_for_a_line_between_the_bounds() {
        // A needle straddling byte 8192 survives because the tail is kept whole
        // whenever the line did not fit in the head.
        let mut line = vec![b'x'; PAYLOAD_PREFIX - 3];
        line.extend_from_slice(b"STRADDLE");
        line.resize(PAYLOAD_PREFIX + 500, b'y');
        line.push(b'\n');
        let p = of(&line);
        assert!(!p.head_has(b"STRADDLE"), "the head cuts the needle in half");
        assert!(p.has(b"STRADDLE"), "the overlapping tail still holds it whole");
    }

    #[test]
    fn nul_bytes_are_dropped() {
        let p = of(b"{\"a\":\"idle\0_prompt\"}\n");
        assert!(p.has(b"idle_prompt"));
    }

    #[test]
    fn chunked_input_reaches_the_same_answer() {
        // A reader that hands over one byte at a time exercises the accumulating
        // branch of the tail slide.
        struct Dribble<'a>(&'a [u8]);
        impl Read for Dribble<'_> {
            fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
                if self.0.is_empty() || buf.is_empty() {
                    return Ok(0);
                }
                buf[0] = self.0[0];
                self.0 = &self.0[1..];
                Ok(1)
            }
        }
        let mut line = vec![b'x'; PAYLOAD_TAIL + 10];
        line.extend_from_slice(b"END\n");
        let p = Payload::from_reader(&mut Dribble(&line));
        assert_eq!(p.tail.len(), PAYLOAD_TAIL);
        assert!(p.has(b"END"));
    }

    #[test]
    fn a_field_needs_a_non_empty_string_value() {
        assert!(has_string_field(br#"{"agent_id":"x"}"#, b"agent_id"));
        assert!(has_string_field(br#"{"agent_id": "x"}"#, b"agent_id"));
        assert!(!has_string_field(br#"{"agent_id":""}"#, b"agent_id"));
        assert!(!has_string_field(br#"{"agent_id":null}"#, b"agent_id"));
        assert!(!has_string_field(br#"{"agent_id":1}"#, b"agent_id"));
        assert!(!has_string_field(br#"{"agent_idx":"x"}"#, b"agent_id"));
        assert!(!has_string_field(br#"{"x":"agent_id"}"#, b"agent_id"));
    }

    #[test]
    fn an_escaped_copy_inside_another_string_does_not_match() {
        let p = br#"{"tool_response":"{\"agent_id\":\"x\"}"}"#;
        assert!(!has_string_field(p, b"agent_id"));
    }

    #[test]
    fn a_later_occurrence_still_matches() {
        // The scan must not stop at the first `"agent_id"` that fails the shape.
        let p = br#"{"a":"agent_id","agent_id":"x"}"#;
        assert!(has_string_field(p, b"agent_id"));
    }

    #[test]
    fn a_match_at_the_very_end_of_the_window_is_not_read_past() {
        // `"agent_id":"` with nothing after it: the value could still be empty,
        // so the bound must refuse rather than index out of range.
        assert!(!has_string_field(br#"{"agent_id":""#, b"agent_id"));
        assert!(!has_string_field(br#"{"agent_id":"#, b"agent_id"));
        assert!(!has_string_field(br#"{"agent_id""#, b"agent_id"));
    }
}
