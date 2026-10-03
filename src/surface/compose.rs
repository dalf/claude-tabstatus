//! Bytes, composed from a capability row into a buffer somebody else owns.
//!
//! Free functions with no receiver, so no lifetime reaches `Config` and so each
//! one is unit-testable on Linux for a grammar only a Mac or a Windows box could
//! ever deliver. What they do NOT do is decide anything: which channel the buffer
//! then travels on is the multiplexer's business, and whether a surface is armed
//! at all is the caller's - this file is the spelling of the bytes and nothing
//! else.
//!
//! [`crate::emit`] keeps stdout, the hook protocol's JSON escaping and the pty.
//! The split is that emit owns a DESTINATION and this owns a GRAMMAR.

use super::SurfaceCaps;
use crate::support::{Presence, Support, YES};

/// Restate what a const row said, as the answer a composer gives back.
///
/// Preserve uncertainty and any explicit gate layered over the row. A const
/// capability row cannot contain a failed attempt; that impossible case is
/// reported rather than panicking in a crate built with `panic = "abort"`.
fn restated<T>(row: &Support<T>) -> Presence {
    match row {
        Support::Available(_) => YES,
        Support::Unsupported(r) => Support::Unsupported(r),
        Support::Disabled(r) => Support::Disabled(r),
        Support::Unverifiable(value, r) => Support::Unverifiable(value.as_ref().map(|_| ()), r),
        Support::Failed(_) => Support::Unsupported("a const capability row cannot fail"),
    }
}

/// `ESC ] 0 ; <title> BEL`, the one title sequence with no `N` in any row of the
/// matrix.
///
/// Gated on `should_emit`, which says yes to `Unverifiable` as well as to
/// `Available`: Windows Terminal's `profiles.suppressApplicationTitle` may discard
/// this and nothing readable says whether it will, and refusing to write would
/// turn "we cannot tell" into a certain no for everyone who left the setting
/// alone.
pub fn push_title(out: &mut Vec<u8>, caps: &SurfaceCaps, title: &str) -> Presence {
    if caps.title.osc0.should_emit() {
        out.extend_from_slice(b"\x1b]0;");
        out.extend_from_slice(title.as_bytes());
        out.push(0x07);
    }
    restated(&caps.title.osc0)
}

/// The arming half of the pair, or the reason there is none.
pub fn push_arm(out: &mut Vec<u8>, caps: &SurfaceCaps) -> Presence {
    match &caps.arming {
        // The pair is read whole and one half used, here and at every other call
        // site, because that is all `Arming::pair` offers: naming an arm without
        // its restore is not expressible.
        Some(a) => {
            out.extend_from_slice(a.pair().0);
            YES
        }
        None => Support::Unsupported("no arm and restore pair is known for this surface"),
    }
}

/// The restore half, driven by the same const as the arm so the two cannot drift.
pub fn push_restore(out: &mut Vec<u8>, caps: &SurfaceCaps) -> Presence {
    match &caps.arming {
        Some(a) => {
            out.extend_from_slice(a.pair().1);
            YES
        }
        None => Support::Unsupported("no arm and restore pair is known for this surface"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::surface::Surface;

    /// The bytes that shipped, pinned. Konsole applies profile properties per tab,
    /// at runtime, in memory, and the restore is Konsole's COMPILED-IN defaults
    /// rather than whatever a customized profile held - because that is all a
    /// write-only channel can know. If either literal ever has to change, this is
    /// where the change is visible.
    #[test]
    fn the_konsole_pair_is_byte_for_byte_what_shipped() {
        let caps = Surface::Konsole.caps();
        let mut arm = Vec::new();
        let mut restore = Vec::new();
        assert!(push_arm(&mut arm, caps).is_available());
        assert!(push_restore(&mut restore, caps).is_available());
        assert_eq!(
            arm,
            b"\x1b]50;LocalTabTitleFormat=%w;RemoteTabTitleFormat=%w\x07".to_vec()
        );
        assert_eq!(
            restore,
            b"\x1b]50;LocalTabTitleFormat=%d : %n;RemoteTabTitleFormat=(%u) %H\x07".to_vec()
        );
    }

    /// A surface with no arming writes NOTHING, and says why. This is the property
    /// that keeps appearance bytes off a terminal that cannot be named.
    #[test]
    fn a_surface_with_no_arming_writes_nothing_and_says_why() {
        for s in Surface::ALL {
            if *s == Surface::Konsole {
                continue;
            }
            let mut out = Vec::new();
            let a = push_arm(&mut out, s.caps());
            let r = push_restore(&mut out, s.caps());
            assert!(out.is_empty(), "{}", s.caps().name);
            assert_eq!(a.label(), "n/a");
            assert!(r.reason().is_some_and(|w| !w.is_empty()));
        }
    }

    /// The title is the same four bytes, the payload, and a BEL - and an EMPTY
    /// title is how the tab is unpainted, which is why it is spelled as a title
    /// rather than as its own literal.
    #[test]
    fn a_title_is_osc_zero_and_an_empty_one_is_the_unpaint() {
        let caps = Surface::Unknown.caps();
        let mut out = Vec::new();
        assert!(push_title(&mut out, caps, "\u{26aa} ~/x").is_available());
        assert_eq!(out, "\x1b]0;\u{26aa} ~/x\x07".as_bytes().to_vec());
        let mut blank = Vec::new();
        assert!(push_title(&mut blank, caps, "").is_available());
        assert_eq!(blank, b"\x1b]0;\x07".to_vec());
    }

    /// Every surface accepts a title, including the ones whose row cannot promise
    /// the setting will let it through. A row that stopped the write would blank a
    /// tab that works today.
    #[test]
    fn every_surface_is_given_its_title() {
        for s in Surface::ALL {
            let mut out = Vec::new();
            let p = push_title(&mut out, s.caps(), "x");
            assert_eq!(out, b"\x1b]0;x\x07".to_vec(), "{}", s.caps().name);
            assert!(
                matches!(p.label(), "ok" | "?"),
                "{} says {}",
                s.caps().name,
                p.label()
            );
        }
    }

    #[test]
    fn an_uncertain_title_is_composed_unless_our_knob_disables_it() {
        for off in [None, Some("CCTAB_DRY_RUN")] {
            let mut caps = crate::surface::rows::WINDOWS_TERMINAL;
            caps.title.osc0 = caps.title.osc0.gate(off);
            let mut out = Vec::new();
            let result = push_title(&mut out, &caps, "x");
            if off.is_some() {
                assert!(out.is_empty());
                assert!(!result.should_emit());
                assert_eq!(result.to_string(), "off: CCTAB_DRY_RUN");
            } else {
                assert_eq!(out, b"\x1b]0;x\x07");
                assert!(result.should_emit());
                assert_eq!(result.to_string(), "?: profiles.suppressApplicationTitle silently discards it");
            }
        }
    }
}
