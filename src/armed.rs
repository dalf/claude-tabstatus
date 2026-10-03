//! What a backend ARMED, recorded where the END hook can read it back.
//!
//! This is the LIFETIME half of this project's named recurring defect. The
//! spelling half is already fixed: [`crate::surface::Arming`] pairs the arm and
//! the restore in one const, so a restore whose bytes drifted from its arm cannot
//! be written. But pairing two byte strings says nothing about WHEN they are
//! chosen, and `session_end` chose them by re-deriving the arming condition FROM
//! THE END HOOK'S OWN ENVIRONMENT - `CCTAB_TERMINAL`, `$TMUX`, `KONSOLE_*` - while
//! `session_start` had derived it from the START hook's, minutes or hours earlier.
//! Change any of those in between and the two answers differ: a restore with no
//! arm, or, far worse, an arm with no restore, which leaves the tab governed by
//! `LocalTabTitleFormat=%w` for as long as it lives.
//!
//! So: what a backend arms must be RECORDED, and restore must be driven by that
//! record. Three rungs, in order of preference:
//!
//! | rung | store | available when |
//! |---|---|---|
//! | 1 | the multiplexer's own key-value store (`@cctab_armed`) | inside tmux |
//! | 2 | the session state record (`s <surface>`) | a state directory exists |
//! | 3 | [`ArmSource::Assumed`] - the old predicate, LABELLED as an assumption | neither |
//!
//! **Rung 3 is not a cop-out; it is the honest name for today's behaviour.** It is
//! also the only rung the 312-case corpus exercises, because `tests/run.sh` unsets
//! the state directory globally and no corpus case drives a live tmux server. That
//! is precisely why the corpus stays byte-identical across this commit: every
//! corpus case takes rung 3, and rung 3 is the predicate that was already there.
//!
//! THE STORES ARE ASYMMETRIC, and the asymmetry is deliberate. Rung 1 can say
//! "nothing armed" (`-`), initialised by a non-arming SessionStart only if no
//! shared record exists. A later non-arming start preserves an outstanding arm;
//! the last Claude pane restores it, even if that pane did not arm it. Rung 2
//! cannot record a negative: a record line written on
//! every session would change the bytes of every state record on disk, and the
//! record format's own rule is that an update never changes what it did not mean
//! to. So rung 2 answers only "this surface armed", and its silence falls through
//! to rung 3 - which fixes "an arm with no restore" everywhere a state directory
//! exists, and "a restore with no arm" only inside tmux.
//!
//! AN UNRECOGNISED VALUE READS AS ABSENT, never as a different surface. Both
//! stores are read with [`crate::surface::by_name`], which answers `None` for a
//! name no row in this build spells - so a value written by a later version, or a
//! `@cctab_armed` a user set by hand, drops to the next rung instead of choosing
//! bytes at random. That is the same rule the platform stamp follows.

use crate::support::Support;
use crate::surface::Surface;

/// WHICH RUNG answered - provenance, the way [`crate::surface::CapSource`] is
/// provenance for a capability row, and printed by doctor for the same reason: a
/// reader who is told "this tab will be restored" is owed the difference between a
/// store that remembers and an environment that was asked again.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ArmSource {
    /// Rung 1: the multiplexer's own key-value store, written by the same
    /// SessionStart that armed.
    Mux,
    /// Rung 2: the session's state record.
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
            ArmSource::Mux => "recorded by the multiplexer, so session end restores what was armed",
            ArmSource::Record => "recorded in the session's state record",
            ArmSource::Assumed => {
                "ASSUMED from this hook's environment - no store could answer, so a \
                 CCTAB_TERMINAL that changes mid-session loses the restore"
            }
        }
    }
}

/// What remains armed (shared across Claude panes in tmux), and which rung said so.
///
/// `surface` is `None` for "nothing was armed", which is a real answer and not an
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
    /// SessionStart uses this and is RIGHT to: at the moment of arming, the leaf
    /// the environment names IS the surface that arms. It is only the END hook, an
    /// unbounded time later, for which the same derivation is a guess.
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

    /// The surface whose appearance bytes are in force, if any. `None` is
    /// "nothing is armed", and [`crate::mux::route`] turns it into no appearance
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
        // And the other direction: a store that says nothing armed is not talked
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
    /// assumption admits what it costs.
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
