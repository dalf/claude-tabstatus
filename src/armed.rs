//! Restore obligations, not receipts for terminal application.
//!
//! [`Armed::resolve`] chooses the surface whose restore bytes to attempt:
//! 1. tmux's session-scoped `@cctab_armed` retains shared arming policy. A surface
//!    name is recorded before client writes, even while detached; `-` means no
//!    retained policy. A non-arming start cannot erase another pane's policy.
//! 2. The session record's `s <surface>` is a conservative restore obligation.
//!    It is written after routing, after the tmux client attempts, and before
//!    the combined direct arm/title write. Skips, failures and partial writes
//!    do not cancel it. It has no negative value; absence falls through.
//! 3. [`ArmSource::Assumed`] reuses the end hook's environment, the legacy
//!    assumption when neither store answers.
//!
//! None is a confirmed-write record. Even a completed write only confirms the
//! transport accepted bytes, not that a terminal applied them. Unknown surface
//! names read as absent, so a later version cannot make us choose arbitrary bytes.
//!
//! Only the surface is remembered. The delivery channel and destination come
//! from the current stack, process and tmux client registry. Restoration assumes
//! stable topology (with tmux detach/reattach handled by its retained policy),
//! and remains best effort: stores can fail, guards can skip, writes can fail,
//! and teardown retires the obligation without acknowledgement or retries.
//! See docs/backend-architecture.md, "The armed record", for the full limits.

use crate::support::Support;
use crate::surface::Surface;

/// WHICH RUNG answered - provenance, the way [`crate::surface::CapSource`] is
/// provenance for a capability row, and printed by doctor for the same reason: a
/// reader who is told "this tab will be restored" is owed the difference between a
/// store that remembers and an environment that was asked again.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ArmSource {
    /// Rung 1: retained shared tmux policy, including starts with no clients.
    Mux,
    /// Rung 2: a conservative restore obligation in the session record.
    Record,
    /// Rung 3: nothing remembered, so the arming condition was re-derived from
    /// THIS hook's environment. The old behaviour, and the old bug, wearing its
    /// own name.
    Assumed,
}

impl ArmSource {
    /// The sentence doctor prints. Rung 3's says what could go wrong, because a
    /// report that called an assumption a record would be the defect in a new
    /// place.
    pub fn why(self) -> &'static str {
        match self {
            ArmSource::Mux => "recorded by the multiplexer as retained policy, not confirmed delivery",
            ArmSource::Record => "restore obligation recorded in the session state, not confirmed delivery",
            ArmSource::Assumed => {
                "ASSUMED from this hook's environment - no store could answer, so a \
                 CCTAB_TERMINAL that changes mid-session loses the restore"
            }
        }
    }
}

/// Which surface to attempt restoring, and the source of that decision.
///
/// `surface` is `None` for "no retained arming policy", a real answer and not an
/// absence: rung 1 can state it. The absences all live in the `Support` values
/// handed to [`Armed::resolve`], which is how the ladder is written without a
/// second absence vocabulary.
#[derive(Clone, Copy)]
pub struct Armed {
    surface: Option<Surface>,
    source: ArmSource,
}

impl Armed {
    /// Rung 3: the leaf this hook's own environment resolved, labelled.
    ///
    /// At SessionStart this selects the intended surface. At SessionEnd it is
    /// only an assumption about an earlier hook; it proves no delivery at either edge.
    pub fn assumed(leaf: Surface) -> Armed {
        Armed { surface: Some(leaf), source: ArmSource::Assumed }
    }

    /// The ladder, and the only place it exists: rung 1, then rung 2, then the
    /// assumption.
    ///
    /// Both stores arrive ALREADY READ, as `Support`, because only their owners
    /// know what an absence there means - `Unsupported` for "not inside a
    /// multiplexer", `Failed` for a socket that is gone - and because a rung that
    /// could not answer must not be able to look like one that answered "nothing".
    pub fn resolve(
        mux: Support<Option<Surface>>,
        record: Support<Option<Surface>>,
        leaf: Surface,
    ) -> Armed {
        if let Support::Available(surface) = mux {
            return Armed { surface, source: ArmSource::Mux };
        }
        if let Support::Available(surface) = record {
            return Armed { surface, source: ArmSource::Record };
        }
        Armed::assumed(leaf)
    }

    /// The surface whose restore bytes are owed or assumed, if any. `None` is
    /// "no retained policy", and [`crate::mux::route`] turns it into no appearance
    /// channel at all.
    pub fn surface(self) -> Option<Surface> {
        self.surface
    }

    /// The rung, for a report.
    pub fn source(self) -> ArmSource {
        self.source
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOTHING: Support<Option<Surface>> = Support::Unsupported("no store");

    /// The ladder, rung by rung. The third one is not a fallback that ought not to
    /// happen: it is the configuration every corpus case runs in.
    #[test]
    fn the_first_store_that_answers_wins_and_the_last_rung_always_answers() {
        let one = Armed::resolve(
            Support::Available(Some(Surface::Konsole)),
            Support::Available(Some(Surface::Vte)),
            Surface::Unknown,
        );
        assert!(one.surface() == Some(Surface::Konsole));
        assert!(one.source() == ArmSource::Mux);
        let two = Armed::resolve(NOTHING, Support::Available(Some(Surface::Konsole)), Surface::Vte);
        assert!(two.surface() == Some(Surface::Konsole));
        assert!(two.source() == ArmSource::Record);
        let three = Armed::resolve(NOTHING, NOTHING, Surface::Konsole);
        assert!(three.surface() == Some(Surface::Konsole));
        assert!(three.source() == ArmSource::Assumed);
    }

    /// THE DEFECT, as a value. The end hook's environment says Vte - somebody
    /// changed `CCTAB_TERMINAL` after the session started - and the record still
    /// says Konsole, so the restore is Konsole's.
    #[test]
    fn a_leaf_that_changed_between_start_and_end_does_not_move_the_restore() {
        for store in [ArmSource::Mux, ArmSource::Record] {
            let konsole = Support::Available(Some(Surface::Konsole));
            let armed = match store {
                ArmSource::Mux => Armed::resolve(konsole, NOTHING, Surface::Vte),
                _ => Armed::resolve(NOTHING, konsole, Surface::Vte),
            };
            assert!(armed.surface() == Some(Surface::Konsole));
            assert!(armed.surface().is_some_and(|s| s.caps().arming.is_some()));
        }
        // And the other direction: a store with no retained policy is not talked
        // out of it by an environment that now names Konsole.
        let none = Armed::resolve(Support::Available(None), NOTHING, Surface::Konsole);
        assert!(none.surface().is_none());
        assert!(none.source() == ArmSource::Mux);
    }

    /// A store that could not be read is not a store that said "nothing". Every
    /// non-`Available` word has to fall THROUGH, or a tmux server that went away
    /// between the two hooks would silently delete the restore - which is the
    /// defect this module exists to remove, arriving by a new road.
    #[test]
    fn a_store_that_could_not_answer_degrades_to_the_next_rung() {
        let unreadable = [
            Support::Unsupported("not inside a multiplexer"),
            Support::Disabled("CCTAB_NO_TMUX"),
            Support::Unverifiable(None, "the server did not answer"),
            Support::Failed(std::io::Error::other("gone")),
        ];
        for m in unreadable {
            let armed = Armed::resolve(m, NOTHING, Surface::Konsole);
            assert!(armed.source() == ArmSource::Assumed);
            assert!(armed.surface() == Some(Surface::Konsole));
        }
    }

    /// Every rung names itself in words a reader can act on, and only the
    /// assumption names the missing evidence.
    #[test]
    fn every_rung_says_which_one_it_is() {
        for s in [ArmSource::Mux, ArmSource::Record, ArmSource::Assumed] {
            assert!(!s.why().is_empty());
        }
        assert!(ArmSource::Assumed.why().contains("ASSUMED"));
        assert!(!ArmSource::Mux.why().contains("ASSUMED"));
        assert!(!ArmSource::Record.why().contains("ASSUMED"));
    }
}
