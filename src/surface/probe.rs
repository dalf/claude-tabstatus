//! Detection as DATA, and the one signal that overrides all of it.
//!
//! A probe is a row - a variable, the surface its presence names, and what that
//! presence survives - rather than a function, because the questions doctor has
//! to answer are "what did each candidate look for" and "what did it see", and a
//! detector written as code can be asked neither. It also means a Linux
//! `cargo test` can run the macOS and Windows candidate lists, which is the only
//! place any of them will ever be run.
//!
//! The probe table is presence-only, with `config::flag` semantics: set AND
//! non-empty, value irrelevant. That is what the detector it replaces did, and it
//! is the whole reason `TERM_PROGRAM` is absent from every list below - its value
//! is the evidence and its namespace is shared with no registry, with tmux itself
//! setting it to `tmux` in recent versions. A probe that tested only its presence
//! would name the wrong terminal, confidently.

use super::Surface;
use crate::config;
use std::ffi::OsStr;

/// One environment-shaped piece of evidence, as data.
pub struct Probe {
    /// Tested with `config::flag` semantics - set and non-empty - so `TMUX=''`
    /// suppresses nothing and `KONSOLE_VERSION=''` names nothing. The corpus
    /// cases `konsole-tmux-empty-does-not-suppress` and
    /// `konsole-kv-empty-is-not-konsole` pin both halves.
    pub var: &'static str,
    pub surface: Surface,
    /// Whether this evidence still means anything INSIDE a multiplexer.
    ///
    /// `$TMUX` / `$STY` take the multiplexer case out: `KONSOLE_*` leaks into any
    /// child launched from a Konsole shell, and into every pane of a tmux server
    /// that was first started under Konsole, so inside a multiplexer those
    /// variables say nothing about the terminal actually drawing the tab.
    ///
    /// FALSE for that reason for `KONSOLE_VERSION`, `KONSOLE_DBUS_SESSION`,
    /// `ITERM_SESSION_ID` and `LC_TERMINAL` alike - `update-environment` carries no
    /// terminal identity variable, so whatever the tmux SERVER was started with is
    /// what every pane of every session sees forever.
    ///
    /// This one flag IS the `!flag("TMUX") && !flag("STY")` conjunction that used
    /// to sit inside the Konsole detector, lifted out of it and attached to the
    /// evidence instead. That is what keeps the nine
    /// `konsole-*-{tmux,sty,tmuxsty}` corpus cases and both
    /// `pty-session-*-konsole-in-screen` cases byte-identical, and it is also
    /// what stops a stale `ITERM_SESSION_ID` reproducing the same defect on macOS
    /// before the macOS surface ever ships.
    pub survives_mux: bool,
    /// Whether this evidence crosses an ssh hop. TRUE only for `LC_TERMINAL`,
    /// which sshd forwards by default and which is the one signal engineered for
    /// the hop.
    ///
    /// Recorded, and deliberately NOT a veto. A variable that is PRESENT over ssh
    /// was put there by a `SendEnv` or a remote rc and is taken at its word -
    /// `tests/run.sh`'s "a KONSOLE_* that survived the hop is taken at its word"
    /// pins exactly that, and doctor's report rests on it. What this flag says is
    /// that ABSENCE over ssh is no evidence, which is a sentence doctor prints and
    /// not a branch the detector takes - so until doctor prints it, the only reader
    /// is the test below.
    #[allow(dead_code)]
    pub survives_ssh: bool,
}

/// The candidates for a Linux build.
///
/// Konsole and nothing else, which is what this build detected before the axis
/// existed. The other Linux signals in the matrix - `VTE_VERSION`, `KITTY_PID`,
/// `ALACRITTY_WINDOW_ID`, `WEZTERM_PANE`, `GHOSTTY_RESOURCES_DIR`, and
/// `WT_SESSION` for a WSL session facing Windows Terminal - each name a row that
/// exists below and are deliberately not probed yet: a probe is BEHAVIOUR, and
/// what this commit owes is a byte-identical program with a new axis under it.
/// The rows are reachable today through `CCTAB_TERMINAL`.
pub(crate) const LINUX_PROBES: &[Probe] = &[
    Probe {
        var: "KONSOLE_VERSION",
        surface: Surface::Konsole,
        survives_mux: false,
        survives_ssh: false,
    },
    Probe {
        var: "KONSOLE_DBUS_SESSION",
        surface: Surface::Konsole,
        survives_mux: false,
        survives_ssh: false,
    },
];

/// The candidates for a macOS build, where the same defect is already waiting:
/// `ITERM_SESSION_ID` leaks into every pane of a tmux server started from iTerm2,
/// exactly as `KONSOLE_*` does on Linux.
///
/// `LC_TERMINAL` is presence-only here like everything else, and its VALUE names
/// the terminal - iTerm2 sets it to `iTerm2` and WezTerm sets it too. Whoever
/// makes a macOS build detect rather than merely compile has to read that value
/// before this row can tell the two apart.
pub(crate) const MACOS_PROBES: &[Probe] = &[
    Probe {
        var: "ITERM_SESSION_ID",
        surface: Surface::ITerm2,
        survives_mux: false,
        survives_ssh: false,
    },
    Probe {
        var: "LC_TERMINAL",
        surface: Surface::ITerm2,
        survives_mux: false,
        survives_ssh: true,
    },
];

/// The candidates for a Windows build. `WT_SESSION` survives a multiplexer here
/// only because there is no multiplexer to survive: tmux does not run on native
/// Windows, and the flag is `true` because the variable is re-injected per tab
/// rather than inherited once by a server.
pub(crate) const WINDOWS_PROBES: &[Probe] = &[Probe {
    var: "WT_SESSION",
    surface: Surface::WindowsTerminal,
    survives_mux: true,
    survives_ssh: false,
}];

/// The candidate list for THIS build - the only platform choice in the surface
/// axis, and it selects DATA rather than code.
///
/// Written with `cfg!` in a const `if` rather than with three `#[cfg]` items on
/// purpose: `#[cfg]` would make the two lists this target does not select DEAD in
/// every build, needing an `#[allow(dead_code)]` apiece to silence a lint that
/// would be telling the truth. All three lists are meant to be reachable
/// everywhere - a Linux `cargo test` is the only place the macOS and Windows
/// detectors will ever run - and this spelling says so instead of apologising for
/// it. A fourth platform gets the Linux list, which is a wrong answer rather than
/// a compile error; the crate does not build on a fourth platform anyway, because
/// `sys` has no module for one.
const PROBES: &[Probe] = if cfg!(target_os = "macos") {
    MACOS_PROBES
} else if cfg!(target_os = "windows") {
    WINDOWS_PROBES
} else {
    LINUX_PROBES
};

/// The first probe whose variable is set and whose evidence has not been vetoed,
/// else [`Surface::Unknown`].
///
/// Generic over the reader rather than taking a `&dyn Fn`: this runs on every
/// painting edge, and monomorphised the real reader is `config::flag` inlined and
/// the test's fixture is a match on a literal - there is no indirect call on the
/// paint path.
fn detect_in<F: Fn(&str) -> bool>(probes: &[Probe], flag: F, in_mux: bool) -> Surface {
    for p in probes {
        if in_mux && !p.survives_mux {
            continue;
        }
        if flag(p.var) {
            return p.surface;
        }
    }
    Surface::Unknown
}

/// Which surface is drawing the tab, to the extent it can be known: step 3 of
/// [`crate::mux::resolve`].
///
/// `CCTAB_TERMINAL` first, then whatever the multiplexer said, then the
/// environment, then nothing. Both of the multiplexer's contributions ARRIVE as
/// arguments - the leaf it named, and whether it swallowed the environment
/// evidence - because no backend in this crate reads `$TMUX` or `$STY` for
/// itself, and because the outer layer may be the only source of leaf identity
/// there is and must therefore be asked before this runs rather than after.
pub fn resolve_leaf(hint: Option<Surface>, in_mux: bool) -> Surface {
    if let Some(s) = from_override(config::var_nonempty("CCTAB_TERMINAL").as_deref()) {
        return s;
    }
    if let Some(s) = hint {
        return s;
    }
    detect_in(PROBES, config::flag, in_mux)
}

/// What `CCTAB_TERMINAL` says, or `None` when it says nothing.
///
/// `CCTAB_TERMINAL` overrides all of it, and is the only honest signal in the
/// topology this exists for: ssh does not forward `KONSOLE_*`, so a session
/// reached over ssh from a Konsole tab - inside tmux or not - has nothing to
/// detect. `konsole` names it; any other value says explicitly that it is NOT
/// Konsole, which is how a false positive from a leaked `KONSOLE_*` is turned off.
///
/// It is the one knob in this crate that changes what paints OUTSIDE tmux as well
/// as in, which is why it is opt-in and why the name is matched case-INSENSITIVELY
/// (ASCII): a byte compare made `CCTAB_TERMINAL=Konsole` mean "explicitly not
/// Konsole" and silently turned the arming and the suffix layout OFF - the exact
/// opposite of what the user typed.
///
/// It is matched against EVERY variant's name, not only the ones this build
/// probes, because over ssh an override is the only way a leaf is knowable at all
/// - and a `windows-terminal` or an `iterm2` that reaches a Linux binary through
/// ssh or WSL is the whole reason the enum is total. A value that matches nothing
/// is still `Some`, so it still stops the ladder: that is what turns a leaked
/// `KONSOLE_*` off, and `konsolex` must remain a near miss rather than a match.
pub fn from_override(raw: Option<&OsStr>) -> Option<Surface> {
    // The raw bytes, never a `to_str` that could fail or a `to_lowercase` that
    // allocates: a `CCTAB_TERMINAL` which is not UTF-8 has to MISS every row
    // rather than be repaired into a near match, and the fold has to stay
    // ASCII-only so that no locale can decide whether `KONSOLE` names Konsole.
    let v = raw?.as_encoded_bytes();
    Some(
        Surface::ALL
            .iter()
            .copied()
            .find(|s| v.eq_ignore_ascii_case(s.caps().name.as_bytes()))
            .unwrap_or(Surface::Unknown),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::surface::Elide;

    /// The environment as a literal set of names, so a test never depends on the
    /// process it runs in.
    fn env<'a>(set: &'a [&'a str]) -> impl Fn(&str) -> bool + 'a {
        move |k| set.contains(&k)
    }

    #[test]
    fn the_terminal_override_names_konsole_in_any_case() {
        let t = |v: Option<&str>| from_override(v.map(OsStr::new));
        for yes in ["konsole", "Konsole", "KONSOLE", "kOnSoLe"] {
            assert!(matches!(t(Some(yes)), Some(Surface::Konsole)), "{yes}");
        }
        // Anything else is an explicit NOT Konsole, which is what turns a leaked
        // KONSOLE_* off; only absence leaves the detection to the environment.
        for no in ["wezterm", "konsol", "konsolex", "xterm", " konsole"] {
            assert!(matches!(t(Some(no)), Some(s) if s != Surface::Konsole), "{no}");
        }
        // And a value that names no row at all is still `Some`, so it still stops
        // the ladder: that is what `konsolex` beside a leaked KONSOLE_VERSION has
        // to keep doing.
        for miss in ["konsol", "konsolex", " konsole"] {
            assert!(matches!(t(Some(miss)), Some(Surface::Unknown)), "{miss}");
        }
        assert!(t(None).is_none());
    }

    /// Every row's name is reachable, because over ssh the override is the only
    /// thing that can name a leaf - including a leaf on another operating system.
    #[test]
    fn the_override_matches_every_variants_name_in_any_case() {
        for s in Surface::ALL {
            let name = s.caps().name;
            for spelling in [name.to_owned(), name.to_uppercase()] {
                let got = from_override(Some(OsStr::new(&spelling)));
                assert!(
                    matches!(got, Some(g) if g == *s),
                    "CCTAB_TERMINAL={spelling} did not name {name}"
                );
            }
        }
    }

    /// `wezterm` used to mean only "explicitly not Konsole"; it now names a row.
    /// Both answers put the glyph in the prefix, which is why the corpus case
    /// `tmux-terminal-override-is-not-konsole` cannot tell the difference.
    #[test]
    fn a_value_that_names_a_row_still_is_not_konsole() {
        let got = from_override(Some(OsStr::new("wezterm")));
        assert!(matches!(got, Some(Surface::WezTerm)));
        assert!(Surface::WezTerm.caps().elide == Elide::Right);
    }

    /// The conjunction this replaced, arm by arm: `!flag("TMUX") && !flag("STY")`
    /// applied to both Konsole variables at once. These nine shapes are the nine
    /// `konsole-*-{tmux,sty,tmuxsty}` corpus cases.
    #[test]
    fn a_multiplexer_vetoes_the_konsole_evidence_it_swallows() {
        let konsole = |set: &[&str], in_mux| detect_in(LINUX_PROBES, env(set), in_mux);
        for var in [
            vec!["KONSOLE_VERSION"],
            vec!["KONSOLE_DBUS_SESSION"],
            vec!["KONSOLE_VERSION", "KONSOLE_DBUS_SESSION"],
        ] {
            assert!(konsole(&var, false) == Surface::Konsole);
            // tmux, screen, and both at once are one answer: the evidence
            // describes whichever terminal started the server.
            assert!(konsole(&var, true) == Surface::Unknown);
        }
        assert!(konsole(&[], false) == Surface::Unknown);
        assert!(konsole(&[], true) == Surface::Unknown);
    }

    /// The same veto, on the platform where it has not shipped yet. This is the
    /// whole reason the candidate lists are data: the bug is fixed on macOS before
    /// a macOS build can detect anything at all.
    #[test]
    fn the_veto_is_per_signal_and_not_per_platform() {
        let m = |set: &[&str], in_mux| detect_in(MACOS_PROBES, env(set), in_mux);
        assert!(m(&["ITERM_SESSION_ID"], false) == Surface::ITerm2);
        assert!(m(&["ITERM_SESSION_ID"], true) == Surface::Unknown);
        assert!(m(&["LC_TERMINAL"], false) == Surface::ITerm2);
        assert!(m(&["LC_TERMINAL"], true) == Surface::Unknown);
        // WT_SESSION is re-injected per tab rather than inherited once by a
        // server, so it is the one probe a multiplexer does not silence.
        let w = |set: &[&str], in_mux| detect_in(WINDOWS_PROBES, env(set), in_mux);
        assert!(w(&["WT_SESSION"], true) == Surface::WindowsTerminal);
    }

    /// The order in the list is the precedence, and the first match wins - which
    /// is what the old `||` did.
    #[test]
    fn the_first_probe_that_matches_wins() {
        let s = detect_in(
            LINUX_PROBES,
            env(&["KONSOLE_DBUS_SESSION", "KONSOLE_VERSION"]),
            false,
        );
        assert!(s == Surface::Konsole);
    }

    /// `LC_TERMINAL` is the one signal engineered to cross an ssh hop, and the
    /// flag that records it is data for a report rather than a branch: a variable
    /// that is present was deliberately forwarded, and `tests/run.sh` pins that a
    /// `KONSOLE_*` which survived the hop is taken at its word.
    #[test]
    fn only_lc_terminal_claims_to_survive_ssh() {
        for p in LINUX_PROBES.iter().chain(WINDOWS_PROBES) {
            assert!(!p.survives_ssh, "{}", p.var);
        }
        for p in MACOS_PROBES {
            assert_eq!(p.survives_ssh, p.var == "LC_TERMINAL", "{}", p.var);
        }
    }
}
