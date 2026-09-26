//! The POSIX-sh semantics this port has to reproduce byte for byte.
//!
//! The POSIX sh implementation removed in this change is the specification. It is
//! preserved verbatim as `tests/oracle/tabstatus.sh`, and the corpus in
//! `tests/corpus/` pins the two together to the byte. Everything here therefore
//! models a shell operator rather than expressing what the operator was FOR:
//!
//!   * Paths, branch names, hostnames and env values are `[u8]`, never `String`.
//!     A directory name on Linux is bytes, and one that is not valid UTF-8 is
//!     the subject of an open limitation (a non-UTF-8 name yields invalid JSON).
//!     Reproducing that needs the bytes to survive intact.
//!   * every count is in CHARACTERS, always, decoded as UTF-8 and independent of
//!     the locale. The shell could not do that: `${#var}`, `${var%?}`, a
//!     `?`-glob prefix and `[[:cntrl:]]` counted characters only when the shell
//!     had multibyte support AND the locale was UTF-8, so the same accented path
//!     elided to three components under LC_ALL=C.UTF-8 and to one under
//!     LC_ALL=C, and bash fell back to BYTE matching for every pattern operator
//!     whenever the subject held one invalid sequence. That locale- and
//!     shell-dependent unit was README limitation 3, and the exemption it forced
//!     - a non-ASCII location was never cut at all - was limitation 4. Both are
//!     fixed here: one unit, one answer, every locale.
//!   * a display string is REPAIRED to valid UTF-8 (`utf8_repair`) at the one
//!     boundary where it stops being a filesystem name and becomes text. That is
//!     limitation 2: JSON text must be UTF-8, so an invalid byte in a directory
//!     name used to produce a hook line Claude Code could not parse, and no
//!     title at all. Paths used to REACH the filesystem are never repaired -
//!     they stay the bytes the kernel gave us.

use std::ffi::{OsStr, OsString};
use std::fs::File;
use std::io::{self, Read};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::Path;

// --- environment ------------------------------------------------------------

/// `${VAR-}` / `$VAR` as raw bytes. `None` only when the variable is UNSET; a
/// variable set to the empty string comes back as `Some([])`, which is the
/// distinction `${VAR-default}` turns on.
pub fn env_raw(key: &str) -> Option<Vec<u8>> {
    std::env::var_os(key).map(|v| v.into_vec())
}

/// `${VAR-default}`: the default applies only to an UNSET variable.
pub fn env_or(key: &str, default: &[u8]) -> Vec<u8> {
    match env_raw(key) {
        Some(v) => v,
        None => default.to_vec(),
    }
}

/// `${VAR-}`, i.e. the empty string for an unset variable.
pub fn env_str(key: &str) -> Vec<u8> {
    env_or(key, b"")
}

/// `[ -n "${VAR-}" ]`.
pub fn env_set(key: &str) -> bool {
    !env_str(key).is_empty()
}

// --- paths ------------------------------------------------------------------

pub fn as_path(b: &[u8]) -> &Path {
    Path::new(OsStr::from_bytes(b))
}

pub fn from_os(s: OsString) -> Vec<u8> {
    s.into_vec()
}

// --- characters -------------------------------------------------------------
//
// One rule, everywhere: a character is a UTF-8 sequence, and a byte that starts
// no valid sequence counts as one character belonging to no class. The locale is
// not consulted at all, and neither is any notion of "byte mode".
//
// What that replaces is worth recording, because it is the reason this file was
// the largest in the port. bash applies TWO different rules at once:
//
//   * `${#var}` is mbstrlen(): it walks with mbrlen and counts an invalid byte
//     as one character, however broken the rest of the string is.
//   * every PATTERN operator - `${v%?}`, `${v#$pat}`, `case $v in *[[:cntrl:]]*)`
//     - converts the whole SUBJECT to wide characters first, and falls back to
//     matching BYTES for the entire string when that conversion fails anywhere
//     in it (xdupmbstowcs returning -1 in strmatch() and remove_pattern()).
//
// Measured under bash-as-sh in C.UTF-8, the two disagree on the same string:
//
//     v=$'\xc3\xa9'        ${#v}=1   ${v%?}=""        (char)
//     v=$'\xc3\xa9\xff'    ${#v}=2   ${v%?}=$'\xc3\xa9'  (byte: one byte off)
//     U+0085 alone matches [[:cntrl:]]; U+0085 next to a bad byte does not.
//
// The port reproduced all of that to be provably byte-identical first. It is
// gone now: display strings are repaired to valid UTF-8 before any of these
// helpers see them, so there is no invalid subject left to disagree about.

/// The character starting at `s[0]`: its code point and its length in bytes.
/// `None` means the bytes there start no valid sequence, which counts as a
/// one-byte character belonging to no class.
fn next_char(s: &[u8]) -> (Option<u32>, usize) {
    if s.is_empty() {
        return (None, 0);
    }
    let b0 = s[0];
    if b0 < 0x80 {
        return (Some(b0 as u32), 1);
    }
    let (need, mut cp, lo, hi) = match b0 {
        0xC2..=0xDF => (1usize, (b0 & 0x1f) as u32, 0x80u32, 0x7ffu32),
        0xE0..=0xEF => (2, (b0 & 0x0f) as u32, 0x800, 0xffff),
        0xF0..=0xF4 => (3, (b0 & 0x07) as u32, 0x10000, 0x10ffff),
        _ => return (None, 1),
    };
    if s.len() <= need {
        return (None, 1);
    }
    for i in 1..=need {
        let b = s[i];
        if b & 0xC0 != 0x80 {
            return (None, 1);
        }
        cp = (cp << 6) | (b & 0x3f) as u32;
    }
    if cp < lo || cp > hi || (0xD800..=0xDFFF).contains(&cp) {
        return (None, 1);
    }
    (Some(cp), need + 1)
}

/// The length of a string in characters.
pub fn char_count(s: &[u8]) -> usize {
    let mut i = 0;
    let mut n = 0;
    while i < s.len() {
        let (_, l) = next_char(&s[i..]);
        i += if l == 0 { 1 } else { l };
        n += 1;
    }
    n
}

/// Byte offset of character `n`, or `s.len()` when there are fewer.
fn char_offset(s: &[u8], n: usize) -> usize {
    let mut i = 0;
    let mut seen = 0;
    while i < s.len() && seen < n {
        let (_, l) = next_char(&s[i..]);
        i += if l == 0 { 1 } else { l };
        seen += 1;
    }
    i
}

/// Length in bytes of the valid UTF-8 character starting at `s[0]`, or `None`
/// when the bytes there start no valid sequence.
pub fn utf8_len(s: &[u8]) -> Option<usize> {
    match next_char(s) {
        (Some(_), n) => Some(n),
        _ => None,
    }
}

/// Every byte that starts no valid UTF-8 sequence replaced by U+FFFD, so that a
/// name the kernel accepts can be put inside a JSON string. Called once, at the
/// boundary between "a path to open" and "text to show"; a valid string is
/// returned unchanged and unallocated work is the common case.
pub fn utf8_repair(s: Vec<u8>) -> Vec<u8> {
    let mut i = 0;
    let mut clean = true;
    while i < s.len() {
        let (cp, l) = next_char(&s[i..]);
        if cp.is_none() {
            clean = false;
            break;
        }
        i += l;
    }
    if clean {
        return s;
    }
    let mut out = Vec::with_capacity(s.len() + 8);
    let mut i = 0;
    while i < s.len() {
        let (cp, l) = next_char(&s[i..]);
        match cp {
            Some(_) => out.extend_from_slice(&s[i..i + l]),
            // U+FFFD, one per offending byte.
            None => out.extend_from_slice("\u{fffd}".as_bytes()),
        }
        i += if l == 0 { 1 } else { l };
    }
    out
}

/// Drop the first `n` characters; the whole string when it is shorter than `n`,
/// which is the shell glob `${v#????}` failing to match rather than matching
/// partially.
pub fn pat_strip_prefix<'a>(s: &'a [u8], n: usize) -> &'a [u8] {
    if char_count(s) < n {
        return s;
    }
    &s[char_offset(s, n)..]
}

/// The first `n` characters; empty when the string is shorter than `n`, for the
/// same reason.
pub fn pat_first_chars<'a>(s: &'a [u8], n: usize) -> &'a [u8] {
    if char_count(s) < n {
        return &[];
    }
    &s[..char_offset(s, n)]
}

/// Byte offset at which the last character starts, plus its code point.
fn last_char(s: &[u8]) -> (usize, Option<u32>) {
    let mut i = 0;
    let mut start = 0;
    let mut cp = None;
    while i < s.len() {
        let (c, l) = next_char(&s[i..]);
        let l = if l == 0 { 1 } else { l };
        start = i;
        cp = c;
        i += l;
    }
    (start, cp)
}

// --- character classes ------------------------------------------------------
//
// The classes the reference implementation matched with `[[:cntrl:]]` and
// `[[:blank:]]`, measured under bash-as-sh and identical in C.UTF-8,
// fr_FR.UTF-8 and en_US.utf8 - which is the definition kept, now unconditionally
// rather than only in a UTF-8 locale:
//
//   cntrl  U+0000-U+001F, U+007F-U+009F, U+2028, U+2029
//   blank  U+0009, U+0020, U+1680, U+2000-U+2006, U+2008-U+200A, U+205F, U+3000
//
// U+2007 and U+180E are NOT blank, which is why they are not in the range.

fn is_cntrl(cp: Option<u32>) -> bool {
    match cp {
        Some(c) => {
            c < 0x20 || c == 0x7f || (0x80..=0x9f).contains(&c) || c == 0x2028 || c == 0x2029
        }
        None => false,
    }
}

fn is_blank(cp: Option<u32>) -> bool {
    match cp {
        Some(0x09) | Some(0x20) => true,
        Some(c) => matches!(c, 0x1680 | 0x2000..=0x2006 | 0x2008..=0x200a | 0x205f | 0x3000),
        None => false,
    }
}

/// Whether the string holds a control character anywhere.
pub fn pat_has_cntrl(s: &[u8]) -> bool {
    let mut i = 0;
    while i < s.len() {
        let (cp, l) = next_char(&s[i..]);
        if is_cntrl(cp) {
            return true;
        }
        i += if l == 0 { 1 } else { l };
    }
    false
}

/// One conditional strip of a trailing control character.
pub fn pat_strip_trailing_cntrl<'a>(s: &'a [u8]) -> &'a [u8] {
    let (start, cp) = last_char(s);
    if !s.is_empty() && is_cntrl(cp) {
        &s[..start]
    } else {
        s
    }
}

/// One step of the trailing control-and-blank trim: the byte offset to truncate
/// to, or `None` to stop.
pub fn pat_trim_step(s: &[u8]) -> Option<usize> {
    if s.is_empty() {
        return None;
    }
    let (start, cp) = last_char(s);
    if is_cntrl(cp) || is_blank(cp) {
        Some(start)
    } else {
        None
    }
}

/// One step of stripping trailing non-ASCII: the byte offset to truncate to, or
/// `None`.
pub fn pat_strip_trailing_nonprint(s: &[u8]) -> Option<usize> {
    if s.is_empty() {
        return None;
    }
    let (start, _) = last_char(s);
    if has_nonprint(&s[start..]) {
        Some(start)
    } else {
        None
    }
}

/// One step of the sanitizer loop: the byte length of the first character of
/// `rest`, and whether it is one of the characters a JSON string literal cannot
/// hold - `"`, `\` or a control character.
pub fn pat_take_char(rest: &[u8]) -> (usize, bool) {
    let (cp, l) = next_char(rest);
    let l = if l == 0 { 1 } else { l };
    let drop = (l == 1 && (rest[0] == b'"' || rest[0] == b'\\')) || is_cntrl(cp);
    (l, drop)
}

/// `case $s in *[!\ -~]*)` - anything outside printable ASCII. Byte-exact in
/// every locale and for every subject, which is why the reference
/// implementation chose this test over `[[:print:]]`.
pub fn has_nonprint(s: &[u8]) -> bool {
    s.iter().any(|&b| b < 0x20 || b > 0x7e)
}

// --- byte-level glob operators ----------------------------------------------

/// `case $hay in *needle*)`, as a byte offset.
///
/// Skips on the first byte rather than comparing every window. That mattered
/// more before the payload read was bounded - the naive form cost 14ms on a 1 MB
/// line - and it is kept because it is the same length of code and the same
/// answer.
pub fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
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
        match hay[i..=last].iter().position(|&b| b == first) {
            Some(k) => {
                let j = i + k;
                if &hay[j..j + needle.len()] == needle {
                    return Some(j);
                }
                i = j + 1;
            }
            None => return None,
        }
    }
    None
}

pub fn contains(hay: &[u8], needle: &[u8]) -> bool {
    find(hay, needle).is_some()
}

/// `${v%/}` - one trailing slash.
pub fn rstrip_slash(s: &[u8]) -> &[u8] {
    match s.strip_suffix(b"/") {
        Some(t) => t,
        None => s,
    }
}

/// `${v%/*}` - everything before the LAST slash; unchanged when there is none.
pub fn dirname(s: &[u8]) -> &[u8] {
    match s.iter().rposition(|&b| b == b'/') {
        Some(i) => &s[..i],
        None => s,
    }
}

/// `${v##*/}` - everything after the LAST slash; the whole string when there
/// is none.
pub fn basename(s: &[u8]) -> &[u8] {
    match s.iter().rposition(|&b| b == b'/') {
        Some(i) => &s[i + 1..],
        None => s,
    }
}

/// `${v#*/}` - everything after the FIRST slash; unchanged when there is none.
pub fn after_first_slash(s: &[u8]) -> &[u8] {
    match s.iter().position(|&b| b == b'/') {
        Some(i) => &s[i + 1..],
        None => s,
    }
}

// --- one line of a file, the way the `read` builtin sees it ------------------

/// `IFS= read -r v 2>/dev/null < file`: the first line, no backslash
/// processing, NUL bytes dropped the way bash drops them, and the empty string
/// for a file that will not open. 1 MiB is far past any HEAD or gitfile and
/// keeps a pathological file from being slurped whole; the callers reject
/// anything over 4096 characters anyway.
pub fn read_first_line(path: &[u8]) -> Vec<u8> {
    let f = match File::open(as_path(path)) {
        Ok(f) => f,
        Err(_) => return Vec::new(),
    };
    let mut buf = Vec::new();
    if f.take(1 << 20).read_to_end(&mut buf).is_err() {
        return Vec::new();
    }
    let end = buf.iter().position(|&b| b == b'\n').unwrap_or(buf.len());
    buf.truncate(end);
    if buf.contains(&0) {
        buf.retain(|&b| b != 0);
    }
    buf
}

// --- stdin ------------------------------------------------------------------

fn read_chunk(s: &mut io::StdinLock<'_>, buf: &mut [u8]) -> Option<usize> {
    loop {
        match s.read(buf) {
            Ok(0) => return None,
            Ok(n) => return Some(n),
            Err(ref e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(_) => return None,
        }
    }
}

/// How much of the FRONT of the payload is ever looked at.
///
/// Two of the three discriminators are serialized before anything unbounded, and
/// that was checked against real captures rather than assumed: in 89 payloads
/// logged from live sessions, PostToolUse puts `agent_id` at byte 760 of 1360
/// (before `tool_input` and `tool_response`) and SessionStart puts `source` at
/// byte 713 of 769. A prefix is the right window for both, and it is a CONSTANT:
/// a 1 MB `tool_response` costs the same as an empty one.
///
/// `notification_type` is NOT one of them - see `PAYLOAD_TAIL`.
///
/// This is README limitation 5. The shell read the whole first line into a
/// variable and then ran `case` globs over it: measured on this machine, a 1 MB
/// Notification cost 165ms under dash and 20ms under bash-as-sh, against a 5s
/// hook timeout - on the edge table's hottest edge. Bounding the read is the only
/// reason the `working` edge can afford to look at its payload at all, which is
/// what the subagent fix needs. The same payload costs 1.1ms here, and a 4 MB one
/// 2.4ms - the linear parts being the kernel's pipe copy and one scan for the line
/// end, never a needle search.
pub const PAYLOAD_PREFIX: usize = 8192;

/// How much of the BACK of the payload is looked at, and why there has to be a
/// back at all.
///
/// A Notification serializes `notification_type` LAST, after `message` - which is
/// unbounded, and on an MCP elicitation is supplied by the server. Seven real
/// captures put the discriminator at byte 806-810 with a 28-32 character message,
/// so a prefix window alone silently loses it once the message passes ~7.35 KB,
/// and the notify edge then falls through to painting nothing. The kinds that
/// lose are exactly the ones for which this Notification is the ONLY signal -
/// `elicitation_dialog`, `elicitation_url_dialog`, `agent_needs_input` - so the
/// tab would stay blue while Claude waits on a dialog. Reproduced before the fix:
/// a 7.4 KB `message` on a real capture painted nothing where the shell painted
/// orange.
///
/// A window at each END fixes it without giving up the bound: the search work
/// stays constant, and a discriminator is found whenever it sits within 8 KiB of
/// either end, which a first or last member always does. Only the notify edge
/// consults the tail. `agent_id` deliberately does not: a FALSE POSITIVE there
/// silences every `working` repaint for the rest of the session, and the tail of a
/// PostToolUse payload is `tool_response`, which can be a JSON OBJECT whose keys
/// are not escaped. Prefix-only keeps that edge's failure in the harmless
/// direction, and the captures above say the field is always in the prefix.
pub const PAYLOAD_TAIL: usize = 8192;

/// The two windows of the payload's first line that are ever searched, and
/// nothing else. `tail` is empty whenever the whole line already fits in `head`,
/// which is every real payload.
#[derive(Default)]
pub struct Payload {
    pub head: Vec<u8>,
    pub tail: Vec<u8>,
}

impl Payload {
    /// A discriminator that is serialized near the front.
    pub fn head_has(&self, needle: &[u8]) -> bool {
        contains(&self.head, needle)
    }
    /// A discriminator that may be the LAST member of the object.
    pub fn has(&self, needle: &[u8]) -> bool {
        contains(&self.head, needle) || contains(&self.tail, needle)
    }
}

/// Read the payload's first line down to two bounded windows - its first
/// `PAYLOAD_PREFIX` bytes and its last `PAYLOAD_TAIL` bytes - and drain the rest
/// without searching it.
///
/// The drain is not optional and not free: stdin has to reach EOF, or Claude
/// Code's writer sees EPIPE on a pipe it is still filling. It costs one 64 KiB
/// read per chunk, no allocation and no copy of the payload past the two windows;
/// the linear parts are the kernel's pipe copy, charged to the writer either way,
/// and one byte scan for the line end. What stays CONSTANT is the SEARCH work:
/// however big the payload, the needles are looked for in at most 16 KiB.
///
/// The tail slides rather than being accumulated: a chunk at least `PAYLOAD_TAIL`
/// long replaces the window outright, so the usual 64 KiB read costs one 8 KiB
/// copy and the window never grows.
pub fn payload_prefix() -> Payload {
    let mut head: Vec<u8> = Vec::with_capacity(1024);
    let mut tail: Vec<u8> = Vec::new();
    let mut len: usize = 0;
    let mut done = false;
    let mut buf = [0u8; 65536];
    let mut s = io::stdin().lock();
    while let Some(n) = read_chunk(&mut s, &mut buf) {
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
    // A line that fits in `head` has no tail to search: keeping one would only
    // double the work and make the two windows overlap.
    if len <= head.len() {
        tail.clear();
    }
    // bash's `read` drops NUL bytes, and section 0b's comment records that as a
    // way a malformed payload could splice itself into a match. Kept, because
    // dropping them is also the more conservative reading of a payload that
    // should not have contained them.
    for w in [&mut head, &mut tail] {
        if w.contains(&0) {
            w.retain(|&b| b != 0);
        }
    }
    Payload { head, tail }
}

/// Whether `"<field>"` appears as a JSON member whose value is a NON-EMPTY
/// string. One space after the colon is tolerated, as elsewhere.
///
/// "Non-empty" is the point rather than pedantry: `agent_id` is documented as
/// "present only when the hook fires inside a subagent call", and the whole
/// subagent fix turns on ABSENT vs PRESENT. A future Claude Code that sent
/// `"agent_id":""` on the main thread would, with a plain substring test, silence
/// every `working` repaint in the session - a tab stuck orange or blue forever.
/// This costs one more byte comparison and removes that failure mode.
///
/// An escaped occurrence inside another string cannot match: in
/// `"tool_response":"{\"agent_id\":\"x\"}"` the byte after `agent_id` is a
/// backslash, not a quote.
pub fn has_string_field(payload: &[u8], field: &[u8]) -> bool {
    let mut needle = Vec::with_capacity(field.len() + 2);
    needle.push(b'"');
    needle.extend_from_slice(field);
    needle.push(b'"');
    let mut from = 0;
    while let Some(k) = find(&payload[from..], &needle) {
        let mut i = from + k + needle.len();
        while i < payload.len() && payload[i] == b' ' {
            i += 1;
        }
        if i < payload.len() && payload[i] == b':' {
            i += 1;
            while i < payload.len() && payload[i] == b' ' {
                i += 1;
            }
            if i + 1 < payload.len() && payload[i] == b'"' && payload[i + 1] != b'"' {
                return true;
            }
        }
        from += k + 1;
    }
    false
}

/// The `cat >/dev/null` branch, kept for the edges that have no payload test:
/// consume stdin so the writer never sees EPIPE, and never look at the bytes.
pub fn drain_stdin() {
    let mut buf = [0u8; 65536];
    let mut s = io::stdin().lock();
    while read_chunk(&mut s, &mut buf).is_some() {}
}
