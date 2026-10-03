//! The one clock, and the one TTL grammar, shared by two layers that expire
//! independently.
//!
//! These lived in the tmux module because tmux is what made a timestamp worth
//! writing down: inside tmux the tab title is a FUNCTION OF TIME, so the paint
//! carries the moment it happened and the server-side format subtracts it from
//! `%s`. But the state layer reads the same clock and the same `CCTAB_TTL_WAITING`
//! for its own wait expiry, and once the multiplexer became an axis of its own
//! `state.rs` asking `mux::tmux::now()` for the wall clock would have said the
//! state record needs a multiplexer. It does not. The setting and the grammar are
//! shared; the expiry is not - state expires on an eligible hook, the tmux carrier
//! against its last paint time - and this file is the shared half with neither
//! layer's opinion in it.

use crate::config;
use std::ffi::OsStr;
use std::time::{SystemTime, UNIX_EPOCH};

/// The epoch the record carries. `CCTAB_NOW` pins it, which is the only reason
/// a corpus case that goes through tmux can be frozen at all.
///
/// No width is imposed: every extraction below is anchored, not offset, so a
/// mocked `CCTAB_NOW=5` works exactly like a real ten-digit one.
pub fn now() -> u64 {
    epoch(config::var_nonempty("CCTAB_NOW").as_deref())
}

fn epoch(raw: Option<&OsStr>) -> u64 {
    if let Some(b) = raw.map(OsStr::as_encoded_bytes) {
        // Spelled here rather than through the tmux module's `digits`, which is
        // about the SHAPE of `$TMUX` and stayed with it: this is the same
        // one-to-ten-ASCII-digits test `ttl_of` below spells inline, and an empty
        // value has to fall through to the real clock rather than fold to 0.
        if !b.is_empty() && b.len() <= 10 && b.iter().all(u8::is_ascii_digit) {
            return b.iter().fold(0u64, |a, c| a * 10 + u64::from(c - b'0'));
        }
    }
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// Seconds before a state decays. `0` disables the tier, which is expressed as a
/// deadline no age can reach rather than as a second shape of format: the
/// generated string then stays one constant per glyph position.
pub const NEVER: u32 = 2_147_483_647;
/// MEASURED, not reasoned. `working` is repainted by `UserPromptSubmit`,
/// `PostToolUse` and `PostToolUseFailure` only, so during one long tool call
/// nothing paints at all - `PreToolUse` is matched to `AskUserQuestion` and
/// `ExitPlanMode` alone. Over the 91 most recent real transcripts on this
/// machine, 7709 WITHIN-TURN gaps between two successive `working` paints:
/// median 9.9s, p95 77s, p99 173s, p99.9 646s, and 39 gaps (0.51%) over 300s -
/// the old default. 1200s covers 99.95% of them; the four above it are 3343s,
/// 5677s, 23700s and 27751s, which are sessions resumed the next day rather than
/// a tool call still running.
///
/// The asymmetry is the whole argument: a lingering blue says "still busy" about
/// a session that has finished, which the next paint corrects in seconds, while a
/// premature white says "finished, come back" about a build that is still
/// running - the exact class of lie this slice exists to remove, pointed the
/// other way. The 3600s disappear horizon still catches a genuinely stuck one.
pub const DEFAULT_TTL_WORKING: u32 = 1200;
pub const DEFAULT_TTL_WAITING: u32 = 900;
pub const DEFAULT_TTL_GONE: u32 = 3600;

/// The same TTL as a NUMBER for state wait expiry. State and tmux share the
/// setting and grammar, but expire independently: state on an eligible hook,
/// the tmux carrier against its last paint time.
pub fn ttl_secs(key: &str, default: u32) -> u64 {
    u64::from(ttl_of(config::var(key).as_deref(), default))
}

fn ttl_of(raw: Option<&OsStr>, default: u32) -> u32 {
    match raw.map(OsStr::as_encoded_bytes) {
        // "never", expressed as a deadline no age can reach rather than as a
        // second shape of format.
        Some(b"0") => NEVER,
        Some(v)
            if (1..=6).contains(&v.len())
                && (b'1'..=b'9').contains(&v[0])
                && v.iter().all(u8::is_ascii_digit) =>
        {
            v.iter().fold(0u32, |a, c| a * 10 + u32::from(c - b'0'))
        }
        _ => default,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_ttl_grammar_is_the_caps_grammar() {
        let t = |v: Option<&str>| ttl_of(v.map(OsStr::new), 300);
        assert_eq!(t(Some("0")), NEVER);
        assert_eq!(t(Some("1")), 1);
        assert_eq!(t(Some("999999")), 999_999);
        // A leading zero, a seventh digit, a sign and a word are all the default.
        for bad in ["08", "0300", "1000000", "-1", "3.5", "nope", " 8", ""] {
            assert_eq!(t(Some(bad)), 300, "{bad:?}");
        }
        assert_eq!(t(None), 300);
    }

    #[test]
    fn the_epoch_can_be_pinned_for_a_test_and_bad_values_are_ignored() {
        let e = |v: Option<&str>| epoch(v.map(OsStr::new));
        assert_eq!(e(Some("1700000000")), 1_700_000_000);
        assert_eq!(e(Some("5")), 5);
        assert_eq!(e(Some("0")), 0);
        for bad in [None, Some(""), Some("12345678901"), Some("17000000x0"), Some("-1")] {
            assert!(e(bad) > 1_700_000_000, "{bad:?} must not be believed");
        }
    }
}
