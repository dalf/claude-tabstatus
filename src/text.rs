//! The crossing between filesystem bytes and display text on the PAINT path,
//! and the two character classes the tab title cares about.
//!
//! Above this line a name is bytes, because that is what a path is on Linux and
//! what the `.git` walk hands back to the kernel. Below it a location is a
//! `String`, so the length cap counts characters and the JSON writer is never
//! handed a byte sequence it would have to invent a character for. [`repair`] is
//! the crossing, called wherever a value stops being something to OPEN and starts
//! being something to SHOW: `location::place`, `location::hostname`,
//! `git::branch_of`, and the environment overrides spliced in after the sanitizer.
//!
//! `manage` and `settings` report with `String::from_utf8_lossy` instead, on
//! purpose: those are diagnostic lines nothing measures or cuts, and the two
//! functions answer DIFFERENTLY on a truncated sequence (asserted below), so
//! swapping them would change bytes - `install --<E2><82>` prints one U+FFFD.

/// Every byte that starts no valid UTF-8 sequence replaced by U+FFFD, so that a
/// name the kernel accepts can go inside a JSON string.
///
/// One U+FFFD per offending BYTE, which is deliberately **not**
/// `String::from_utf8_lossy`: that emits one per maximal invalid subsequence, so
/// a truncated `F0 9F 98` comes back as one replacement where this gives three.
/// The difference is observable, because the length cap counts the characters
/// this produces and can cut between two of them, so the per-byte rule is part
/// of the behaviour rather than an implementation detail.
pub fn repair(bytes: &[u8]) -> String {
    if let Ok(s) = std::str::from_utf8(bytes) {
        return s.to_owned();
    }
    let mut out = String::with_capacity(bytes.len() + 8);
    let mut rest = bytes;
    while !rest.is_empty() {
        match std::str::from_utf8(rest) {
            Ok(s) => {
                out.push_str(s);
                break;
            }
            Err(e) => {
                let good = e.valid_up_to();
                // `valid_up_to` bytes of `rest` are valid UTF-8 by definition;
                // the byte at `good` starts no sequence, so it costs one U+FFFD
                // and the scan resumes at the very next byte.
                if let Ok(s) = std::str::from_utf8(&rest[..good]) {
                    out.push_str(s);
                }
                out.push('\u{fffd}');
                rest = &rest[good + 1..];
            }
        }
    }
    out
}

/// The control characters, as the tab title has to treat them: what a JSON
/// string literal cannot hold raw, plus the two Unicode line separators, which
/// would break the one-line-per-hook contract.
pub fn is_cntrl(c: char) -> bool {
    matches!(c, '\0'..='\u{1f}' | '\u{7f}'..='\u{9f}' | '\u{2028}' | '\u{2029}')
}

/// The blank characters trimmed off the end of a `HEAD` line, where git itself
/// ignores them. U+2007 (figure space) and U+180E are deliberately absent: they
/// are not blank.
pub fn is_blank(c: char) -> bool {
    matches!(
        c,
        '\t' | ' '
            | '\u{1680}'
            | '\u{2000}'..='\u{2006}'
            | '\u{2008}'..='\u{200a}'
            | '\u{205f}'
            | '\u{3000}'
    )
}

/// Anything outside printable ASCII. The sanitizer's bound can land mid-string,
/// and a character it cannot vouch for is not a character to leave at the cut.
pub fn is_nonprint(c: char) -> bool {
    !(' '..='~').contains(&c)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_utf8_passes_through() {
        assert_eq!(repair(b""), "");
        assert_eq!(repair(b"/home/a/code"), "/home/a/code");
        assert_eq!(repair("caf\u{e9}".as_bytes()), "caf\u{e9}");
        assert_eq!(repair("\u{1f535} ok".as_bytes()), "\u{1f535} ok");
    }

    #[test]
    fn one_replacement_per_offending_byte() {
        // A lone invalid byte: the only shape the golden corpus contains, and
        // the one shape where from_utf8_lossy happens to agree.
        assert_eq!(repair(b"\xff"), "\u{fffd}");
        assert_eq!(repair(b"a\xffb"), "a\u{fffd}b");
        // Truncated sequences: three bytes of a four-byte emoji are THREE
        // replacements here and one under from_utf8_lossy.
        assert_eq!(repair(b"\xf0\x9f\x98"), "\u{fffd}\u{fffd}\u{fffd}");
        assert_eq!(repair(b"\xe2\x82"), "\u{fffd}\u{fffd}");
        assert_eq!(repair(b"x\xe0\xa0y"), "x\u{fffd}\u{fffd}y");
        assert_eq!(String::from_utf8_lossy(b"\xe2\x82"), "\u{fffd}");
    }

    #[test]
    fn a_bad_byte_beside_a_good_multibyte_char() {
        assert_eq!(repair(b"\xc3\xa9\xff"), "\u{e9}\u{fffd}");
        assert_eq!(repair(b"\xff\xc3\xa9"), "\u{fffd}\u{e9}");
    }

    #[test]
    fn overlong_and_surrogate_encodings_are_invalid() {
        // C0 80 is an overlong NUL and ED A0 80 is a lone surrogate; both are
        // rejected byte by byte.
        assert_eq!(repair(b"\xc0\x80"), "\u{fffd}\u{fffd}");
        assert_eq!(repair(b"\xed\xa0\x80"), "\u{fffd}\u{fffd}\u{fffd}");
        // F5 is past U+10FFFF.
        assert_eq!(repair(b"\xf5\x80\x80\x80"), "\u{fffd}\u{fffd}\u{fffd}\u{fffd}");
    }

    #[test]
    fn a_repaired_string_keeps_one_character_per_input_unit() {
        // What the length cap counts: an invalid byte is one character, so the
        // count is the same taken over the bytes or over the repaired text.
        assert_eq!(repair(b"ab\xff\xffcd").chars().count(), 6);
    }

    #[test]
    fn cntrl_class_edges() {
        for c in ['\0', '\u{1f}', '\u{7f}', '\u{80}', '\u{9f}', '\u{2028}', '\u{2029}'] {
            assert!(is_cntrl(c), "{:?} should be cntrl", c);
        }
        for c in [' ', '~', '\u{a0}', '\u{20}', '\u{2027}', '\u{202a}', '\u{fffd}'] {
            assert!(!is_cntrl(c), "{:?} should not be cntrl", c);
        }
    }

    #[test]
    fn blank_class_edges() {
        for c in ['\t', ' ', '\u{1680}', '\u{2000}', '\u{2006}', '\u{2008}', '\u{200a}',
                  '\u{205f}', '\u{3000}'] {
            assert!(is_blank(c), "{:?} should be blank", c);
        }
        // The two deliberate holes, plus the neighbours of each range.
        for c in ['\u{2007}', '\u{180e}', '\u{167f}', '\u{1681}', '\u{1fff}', '\u{2007}',
                  '\u{200b}', '\u{205e}', '\u{2060}', '\u{2fff}', '\u{3001}', 'a', '\n'] {
            assert!(!is_blank(c), "{:?} should not be blank", c);
        }
    }

    #[test]
    fn nonprint_is_outside_printable_ascii() {
        assert!(!is_nonprint(' '));
        assert!(!is_nonprint('~'));
        assert!(!is_nonprint('a'));
        assert!(is_nonprint('\x1f'));
        assert!(is_nonprint('\u{7f}'));
        assert!(is_nonprint('\u{e9}'));
        assert!(is_nonprint('\u{fffd}'));
    }
}
