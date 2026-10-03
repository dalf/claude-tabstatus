//! Detection as DATA, and the one signal that overrides all of it.
//!
//! Each row names a variable, a typed matching rule, a surface and the contexts
//! in which the hint is eligible. Detection and doctor share the same walk.
//! All platform tables are tested on every host, alongside native platform tests.
//! Dedicated signals retain non-empty presence matching. Shared namespaces such
//! as `LC_TERMINAL` and `TERM_PROGRAM` require exact vendor values: a hint names
//! a family, never a version or proof that the terminal applied any bytes.

use super::Surface;
use crate::config;
use std::ffi::OsStr;

/// Evidence for reporting versioned protocols, deliberately separate from the
/// family detector on the painting path.
#[derive(Clone, Copy)]
pub enum VersionEvidence {
    Catalogue,
    Missing,
    Invalid,
    Unreliable,
    Konsole(u32),
}

/// Cold-path query only. An override establishes the family, not a version.
/// A claimed mux vetoes inherited versions even if disabled or malformed: neither
/// its server environment nor a family hint proves the versions of its clients.
pub fn version_evidence(surface: Surface, in_mux: bool) -> VersionEvidence {
    if surface != Surface::Konsole {
        return VersionEvidence::Missing;
    }
    konsole_version(std::env::var_os("KONSOLE_VERSION").as_deref(), in_mux)
}

fn konsole_version(raw: Option<&OsStr>, in_mux: bool) -> VersionEvidence {
    if in_mux {
        return VersionEvidence::Unreliable;
    }
    let Some(bytes) = raw.map(OsStr::as_encoded_bytes).filter(|b| !b.is_empty()) else {
        return VersionEvidence::Missing;
    };
    // Konsole exports major * 10000 + minor * 100 + patch. The supported
    // calendar-release spelling is exactly YYMMZZ; reject signs, whitespace,
    // suffixes, non-UTF-8 and out-of-range months rather than guessing a tier.
    if bytes.len() != 6 || !bytes.iter().all(u8::is_ascii_digit) {
        return VersionEvidence::Invalid;
    }
    let version = bytes.iter().fold(0u32, |v, b| v * 10 + u32::from(b - b'0'));
    if version / 10000 == 0 || !(1..=12).contains(&(version / 100 % 100)) {
        return VersionEvidence::Invalid;
    }
    VersionEvidence::Konsole(version)
}

/// The two matching policies supported by an environment probe.
#[derive(Clone, Copy)]
pub enum MatchRule {
    NonEmpty,
    /// Exact, case-sensitive vendor spelling, independent of surface names.
    Exact(&'static str),
}

impl MatchRule {
    fn matches(self, value: &OsStr) -> bool {
        let bytes = value.as_encoded_bytes();
        match self {
            Self::NonEmpty => !bytes.is_empty(),
            // Slice equality checks length first, bounding the comparison by the
            // vendor literal. No decoding, repair, case fold or trimming.
            Self::Exact(want) => bytes == want.as_bytes(),
        }
    }
}

/// The matched rule, without carrying an arbitrary environment value into a report.
pub struct Evidence {
    pub var: &'static str,
    pub value: Option<&'static str>,
}

impl std::fmt::Display for Evidence {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "${}", self.var)?;
        if let Some(value) = self.value {
            write!(f, "={value}")?;
        }
        Ok(())
    }
}

/// One environment-shaped piece of evidence, as data.
pub struct Probe {
    pub var: &'static str,
    pub rule: MatchRule,
    pub surface: Surface,
    /// Whether this evidence still means anything INSIDE a multiplexer.
    ///
    /// `$TMUX` / `$STY` take the multiplexer case out: `KONSOLE_*` leaks into any
    /// child launched from a Konsole shell, and into every pane of a tmux server
    /// that was first started under Konsole, so inside a multiplexer those
    /// variables say nothing about the terminal actually drawing the tab.
    ///
    /// FALSE for that reason for `KONSOLE_VERSION`, `KONSOLE_DBUS_SESSION`,
    /// `ITERM_SESSION_ID`, `LC_TERMINAL` and `TERM_PROGRAM` alike - inherited
    /// values can describe the terminal that started the server, not its clients.
    ///
    /// This one flag IS the `!flag("TMUX") && !flag("STY")` conjunction that used
    /// to sit inside the Konsole detector, lifted out of it and attached to the
    /// evidence instead. That is what keeps the nine
    /// `konsole-*-{tmux,sty,tmuxsty}` corpus cases and both
    /// `pty-session-*-konsole-in-screen` cases byte-identical, and it is also
    /// what stops a stale `ITERM_SESSION_ID` reproducing the same defect on macOS.
    pub survives_mux: bool,
    /// A candidate for configured SSH locale-variable forwarding. This is
    /// metadata, not a veto or a forwarding guarantee: both client `SendEnv` and
    /// server `AcceptEnv` configuration matter. Any variable may be set remotely;
    /// absence over SSH establishes nothing about the local terminal.
    #[allow(dead_code)]
    pub locale_forwarding: bool,
}

impl Probe {
    fn evidence(&self) -> Evidence {
        Evidence {
            var: self.var,
            value: match self.rule {
                MatchRule::NonEmpty => None,
                MatchRule::Exact(value) => Some(value),
            },
        }
    }
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
        rule: MatchRule::NonEmpty,
        surface: Surface::Konsole,
        survives_mux: false,
        locale_forwarding: false,
    },
    Probe {
        var: "KONSOLE_DBUS_SESSION",
        rule: MatchRule::NonEmpty,
        surface: Surface::Konsole,
        survives_mux: false,
        locale_forwarding: false,
    },
];

/// macOS precedence: dedicated iTerm2 session ID, exact LC_TERMINAL, then exact
/// TERM_PROGRAM. Invalid shared values do not stop the walk. Vendor spellings
/// and primary sources: terminal-capability-matrix.md, "Automatic probe policy".
pub(crate) const MACOS_PROBES: &[Probe] = &[
    Probe {
        var: "ITERM_SESSION_ID",
        rule: MatchRule::NonEmpty,
        surface: Surface::ITerm2,
        survives_mux: false,
        locale_forwarding: false,
    },
    Probe {
        var: "LC_TERMINAL",
        rule: MatchRule::Exact("iTerm2"),
        surface: Surface::ITerm2,
        survives_mux: false,
        locale_forwarding: true,
    },
    Probe {
        var: "TERM_PROGRAM",
        rule: MatchRule::Exact("iTerm.app"),
        surface: Surface::ITerm2,
        survives_mux: false,
        locale_forwarding: false,
    },
    Probe {
        var: "TERM_PROGRAM",
        rule: MatchRule::Exact("Apple_Terminal"),
        surface: Surface::AppleTerminal,
        survives_mux: false,
        locale_forwarding: false,
    },
];

/// The candidates for a Windows build. `WT_SESSION` survives a multiplexer here
/// only because there is no multiplexer to survive: tmux does not run on native
/// Windows, and the flag is `true` because the variable is re-injected per tab
/// rather than inherited once by a server.
pub(crate) const WINDOWS_PROBES: &[Probe] = &[Probe {
    var: "WT_SESSION",
    rule: MatchRule::NonEmpty,
    surface: Surface::WindowsTerminal,
    survives_mux: true,
    locale_forwarding: false,
}];

/// The candidate list for THIS build - the only platform choice in the surface
/// axis, and it selects DATA rather than code.
///
/// Written with `cfg!` in a const `if` rather than with three `#[cfg]` items on
/// purpose: `#[cfg]` would make the two lists this target does not select DEAD in
/// every build, needing an `#[allow(dead_code)]` apiece to silence a lint that
/// would be telling the truth. All three lists are meant to be reachable
/// everywhere, for cross-platform table tests as well as native execution.
/// A fourth platform gets the Linux list, which is a wrong answer rather than
/// a compile error; the crate does not build on a fourth platform anyway, because
/// `sys` has no module for one.
const PROBES: &[Probe] = if cfg!(target_os = "macos") {
    MACOS_PROBES
} else if cfg!(target_os = "windows") {
    WINDOWS_PROBES
} else {
    LINUX_PROBES
};

/// The first probe whose rule matches and whose evidence has not been vetoed,
/// else [`Surface::Unknown`].
///
/// Generic over the reader rather than taking a `&dyn Fn`: this runs on every
/// painting edge; both the real environment reader and borrowed test fixtures
/// are statically dispatched, with no indirect call on the paint path.
fn detect_in<V: AsRef<OsStr>, F: Fn(&str) -> Option<V>>(
    probes: &[Probe],
    read: F,
    in_mux: bool,
) -> Surface {
    matching(probes, read, in_mux).map_or(Surface::Unknown, |p| p.surface)
}

/// The candidate that answered, or `None`. Spelled once because doctor prints the
/// VARIABLE and the paint path takes the SURFACE, and two loops with the same veto
/// in them is a report that can name evidence a detector ignored.
fn matching<V: AsRef<OsStr>, F: Fn(&str) -> Option<V>>(
    probes: &[Probe],
    read: F,
    in_mux: bool,
) -> Option<&Probe> {
    probes.iter().find(|p| {
        (p.survives_mux || !in_mux) && read(p.var).is_some_and(|v| p.rule.matches(v.as_ref()))
    })
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
    resolve_in(
        PROBES,
        config::var,
        config::var_nonempty("CCTAB_TERMINAL").as_deref(),
        hint,
        in_mux,
    )
}

fn resolve_in<V: AsRef<OsStr>, F: Fn(&str) -> Option<V>>(
    probes: &[Probe],
    read: F,
    override_: Option<&OsStr>,
    hint: Option<Surface>,
    in_mux: bool,
) -> Surface {
    if let Some(s) = from_override(override_) {
        return s;
    }
    if let Some(s) = hint {
        return s;
    }
    detect_in(probes, read, in_mux)
}

/// Which rule of THIS build's candidate list matched the leaf -
/// the third rung of [`resolve_leaf`], asked again for a REPORT.
///
/// "What did each candidate look for, and what did it see" is the pair of questions
/// the probe table was made DATA to be able to answer, and a detector written as
/// code could be asked neither. `None` means no eligible rule matched, including
/// inside a multiplexer that swallowed inherited evidence. SSH is not a veto.
///
/// It re-reads only probe values for the report, without carrying their values on
/// the paint path. Doctor separately honours the retained mux hint and override
/// before reporting this lower-priority evidence.
pub fn evidence(in_mux: bool) -> Option<Evidence> {
    matching(PROBES, config::var, in_mux).map(Probe::evidence)
}

/// What `CCTAB_TERMINAL` says, or `None` when it says nothing.
///
/// `CCTAB_TERMINAL` overrides all of it. When ssh does not forward `KONSOLE_*`,
/// a session reached over ssh from a Konsole tab has nothing to
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
/// probes, because over ssh an override may be the only available family hint
/// - and a `windows-terminal` or an `iterm2` that reaches a Linux binary through
/// ssh or WSL is the whole reason the enum is total. A value that matches nothing
/// is still `Some`, so it still stops the ladder: that is what turns a leaked
/// `KONSOLE_*` off, and `konsolex` must remain a near miss rather than a match.
pub fn from_override(raw: Option<&OsStr>) -> Option<Surface> {
    // The raw bytes, never a `to_str` that could fail or a `to_lowercase` that
    // allocates: a `CCTAB_TERMINAL` which is not UTF-8 has to MISS every row
    // rather than be repaired into a near match, and the fold has to stay
    // ASCII-only so that no locale can decide whether `KONSOLE` names Konsole.
    let bytes = raw?.as_encoded_bytes();
    if bytes.is_empty() {
        return None;
    }
    Some(by_name(bytes).unwrap_or(Surface::Unknown))
}

/// The surface this NAME spells, or `None` when no row answers to it.
///
/// The STRICT half of [`from_override`], which turns a miss into
/// [`Surface::Unknown`] because a `CCTAB_TERMINAL` that named nothing still has to
/// stop the detection ladder. A STORE has the opposite obligation: a value written
/// by a future version - or by a user's stray `set -s @cctab_armed` - must read as
/// ABSENT and let the next rung answer, never as a different surface. That is the
/// same rule the platform stamp follows, and it is what keeps `@cctab_armed` and
/// the record's `s` line safe to extend.
pub fn by_name(name: &[u8]) -> Option<Surface> {
    Surface::ALL
        .iter()
        .copied()
        .find(|s| name.eq_ignore_ascii_case(s.caps().name.as_bytes()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::surface::Elide;

    #[test]
    fn version_evidence_requires_a_complete_calendar_version() {
        for raw in [None, Some("")] {
            assert!(matches!(
                konsole_version(raw.map(OsStr::new), false),
                VersionEvidence::Missing
            ));
        }
        for raw in [
            "23.04",
            "23040",
            "0230400",
            "+230400",
            " 230400",
            "230400\n",
            "230400beta",
            "230000",
            "231300",
            "000100",
            "999999",
            "é30400",
        ] {
            assert!(
                matches!(
                    konsole_version(Some(OsStr::new(raw)), false),
                    VersionEvidence::Invalid
                ),
                "{raw:?}"
            );
        }
        for raw in ["220400", "230400", "241200", "260400", "260801"] {
            assert!(
                matches!(konsole_version(Some(OsStr::new(raw)), false), VersionEvidence::Konsole(v) if v == raw.parse::<u32>().unwrap())
            );
        }
    }

    #[test]
    fn mux_versions_are_unreliable_even_when_well_formed() {
        for raw in [None, Some("260400"), Some("220400"), Some("invalid")] {
            assert!(matches!(
                konsole_version(raw.map(OsStr::new), true),
                VersionEvidence::Unreliable
            ));
        }
    }

    #[cfg(unix)]
    #[test]
    fn non_utf8_version_is_invalid() {
        use std::os::unix::ffi::OsStrExt;
        assert!(matches!(
            konsole_version(Some(OsStr::from_bytes(b"2604\xff0")), false),
            VersionEvidence::Invalid
        ));
    }

    /// Borrowed values: tests never depend on or mutate the process environment.
    fn env<'a>(set: &'a [(&str, &str)]) -> impl Fn(&str) -> Option<&'a OsStr> + 'a {
        move |k| {
            set.iter()
                .find(|(key, _)| *key == k)
                .map(|(_, v)| OsStr::new(v))
        }
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
            assert!(
                matches!(t(Some(no)), Some(s) if s != Surface::Konsole),
                "{no}"
            );
        }
        // And a value that names no row at all is still `Some`, so it still stops
        // the ladder: that is what `konsolex` beside a leaked KONSOLE_VERSION has
        // to keep doing.
        for miss in ["konsol", "konsolex", " konsole"] {
            assert!(matches!(t(Some(miss)), Some(Surface::Unknown)), "{miss}");
        }
        assert!(t(None).is_none());
        assert!(t(Some("")).is_none());
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
        let konsole = |set: &[(&str, &str)], in_mux| detect_in(LINUX_PROBES, env(set), in_mux);
        for var in [
            vec![("KONSOLE_VERSION", "invalid-version")],
            vec![("KONSOLE_DBUS_SESSION", "0")],
            vec![("KONSOLE_VERSION", "1"), ("KONSOLE_DBUS_SESSION", " ")],
        ] {
            assert!(konsole(&var, false) == Surface::Konsole);
            // tmux, screen, and both at once are one answer: the evidence
            // describes whichever terminal started the server.
            assert!(konsole(&var, true) == Surface::Unknown);
        }
        assert!(konsole(&[], false) == Surface::Unknown);
        assert!(konsole(&[], true) == Surface::Unknown);
    }

    #[test]
    fn the_veto_is_per_signal_and_not_per_platform() {
        for (var, value, surface) in [
            ("ITERM_SESSION_ID", "0", Surface::ITerm2),
            ("LC_TERMINAL", "iTerm2", Surface::ITerm2),
            ("TERM_PROGRAM", "iTerm.app", Surface::ITerm2),
            ("TERM_PROGRAM", "Apple_Terminal", Surface::AppleTerminal),
        ] {
            let set = [(var, value)];
            assert!(detect_in(MACOS_PROBES, env(&set), false) == surface);
            assert!(detect_in(MACOS_PROBES, env(&set), true) == Surface::Unknown);
        }
        // WT_SESSION is re-injected per tab rather than inherited once by a
        // server, so it is the one probe a multiplexer does not silence.
        assert!(
            detect_in(WINDOWS_PROBES, env(&[("WT_SESSION", "0")]), true)
                == Surface::WindowsTerminal
        );
    }

    #[test]
    fn the_evidence_a_report_names_is_the_probe_the_detector_took() {
        for probes in [LINUX_PROBES, MACOS_PROBES, WINDOWS_PROBES] {
            for p in probes {
                let value = match p.rule {
                    MatchRule::NonEmpty => "arbitrary",
                    MatchRule::Exact(value) => value,
                };
                for set in [
                    vec![],
                    vec![(p.var, "")],
                    vec![(p.var, "unrelated")],
                    vec![(p.var, value)],
                ] {
                    for in_mux in [false, true] {
                        let matched = matching(probes, env(&set), in_mux);
                        let leaf = detect_in(probes, env(&set), in_mux);
                        assert!(leaf == matched.map_or(Surface::Unknown, |p| p.surface));
                        if let Some(matched) = matched {
                            let evidence = matched.evidence();
                            assert_eq!(evidence.var, matched.var);
                            let expected = match matched.rule {
                                MatchRule::NonEmpty => format!("${}", matched.var),
                                MatchRule::Exact(value) => format!("${}={value}", matched.var),
                            };
                            assert_eq!(evidence.to_string(), expected);
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn the_first_probe_that_matches_wins() {
        let matched = matching(
            LINUX_PROBES,
            env(&[
                ("KONSOLE_DBUS_SESSION", "session"),
                ("KONSOLE_VERSION", "garbage"),
            ]),
            false,
        )
        .unwrap();
        assert!(matched.surface == Surface::Konsole);
        assert_eq!(matched.evidence().to_string(), "$KONSOLE_VERSION");
    }

    #[test]
    fn macos_shared_values_match_only_exact_vendor_spellings() {
        for (var, value, surface) in [
            ("LC_TERMINAL", "iTerm2", Surface::ITerm2),
            ("TERM_PROGRAM", "iTerm.app", Surface::ITerm2),
            ("TERM_PROGRAM", "Apple_Terminal", Surface::AppleTerminal),
        ] {
            assert!(detect_in(MACOS_PROBES, env(&[(var, value)]), false) == surface);
            let near = [
                value.to_ascii_lowercase(),
                value.to_ascii_uppercase(),
                format!(" {value}"),
                format!("{value} "),
                format!("{value}\n"),
                format!("{value}\t"),
                format!("x{value}"),
                format!("{value}x"),
                format!("{value}\0"),
                value[..value.len() - 1].to_owned(),
            ];
            for miss in near {
                let set = [(var, miss.as_str())];
                assert!(
                    detect_in(MACOS_PROBES, env(&set), false) == Surface::Unknown,
                    "{var}={miss:?}"
                );
                assert!(matching(MACOS_PROBES, env(&set), false).is_none());
            }
        }
        for var in ["LC_TERMINAL", "TERM_PROGRAM"] {
            for miss in [
                "",
                "some-other-terminal",
                "tmux",
                "iterm2",
                "apple-terminal",
                "WezTerm",
                "wezterm",
                "vscode",
                "ghostty",
                "iTerm",
                "\u{0130}Term2",
            ] {
                assert!(
                    detect_in(MACOS_PROBES, env(&[(var, miss)]), false) == Surface::Unknown,
                    "{var}={miss:?}"
                );
            }
        }
        // Values belong to vendor-specific namespaces, not to our surface names.
        assert!(
            detect_in(MACOS_PROBES, env(&[("LC_TERMINAL", "iTerm.app")]), false)
                == Surface::Unknown
        );
        assert!(
            detect_in(MACOS_PROBES, env(&[("TERM_PROGRAM", "iTerm2")]), false) == Surface::Unknown
        );
    }

    #[test]
    fn invalid_earlier_values_allow_later_evidence_and_conflicts_have_precedence() {
        for (set, expected, evidence) in [
            (vec![], Surface::Unknown, None),
            (
                vec![("ITERM_SESSION_ID", ""), ("LC_TERMINAL", "iTerm2")],
                Surface::ITerm2,
                Some("$LC_TERMINAL=iTerm2"),
            ),
            (
                vec![
                    ("LC_TERMINAL", "unrelated"),
                    ("TERM_PROGRAM", "Apple_Terminal"),
                ],
                Surface::AppleTerminal,
                Some("$TERM_PROGRAM=Apple_Terminal"),
            ),
            (
                vec![("LC_TERMINAL", ""), ("TERM_PROGRAM", "iTerm.app")],
                Surface::ITerm2,
                Some("$TERM_PROGRAM=iTerm.app"),
            ),
            (
                vec![
                    ("LC_TERMINAL", "iTerm2"),
                    ("TERM_PROGRAM", "Apple_Terminal"),
                ],
                Surface::ITerm2,
                Some("$LC_TERMINAL=iTerm2"),
            ),
            (
                vec![
                    ("ITERM_SESSION_ID", "session"),
                    ("LC_TERMINAL", "unrelated"),
                    ("TERM_PROGRAM", "Apple_Terminal"),
                ],
                Surface::ITerm2,
                Some("$ITERM_SESSION_ID"),
            ),
            (
                vec![
                    ("ITERM_SESSION_ID", "session"),
                    ("LC_TERMINAL", "iTerm2"),
                    ("TERM_PROGRAM", "Apple_Terminal"),
                ],
                Surface::ITerm2,
                Some("$ITERM_SESSION_ID"),
            ),
        ] {
            assert!(detect_in(MACOS_PROBES, env(&set), false) == expected);
            assert_eq!(
                matching(MACOS_PROBES, env(&set), false)
                    .map(|p| p.evidence().to_string())
                    .as_deref(),
                evidence
            );
        }
    }

    #[test]
    fn resolution_ladder_preserves_override_hint_and_environment_order() {
        for probes in [LINUX_PROBES, MACOS_PROBES, WINDOWS_PROBES] {
            let set = [
                ("KONSOLE_VERSION", "invalid"),
                ("ITERM_SESSION_ID", "session"),
                ("LC_TERMINAL", "iTerm2"),
                ("TERM_PROGRAM", "Apple_Terminal"),
                ("WT_SESSION", "session"),
            ];
            for in_mux in [false, true] {
                for hint in [
                    None,
                    Some(Surface::WezTerm),
                    Some(Surface::ITerm2),
                    Some(Surface::Unknown),
                ] {
                    for (value, expected) in [
                        ("ITeRm2", Surface::ITerm2),
                        ("unsupported", Surface::Unknown),
                    ] {
                        // Neither a recognised nor an unknown override reads probes.
                        assert!(
                            resolve_in(
                                probes,
                                |_| -> Option<&OsStr> { panic!("probe read") },
                                Some(OsStr::new(value)),
                                hint,
                                in_mux
                            ) == expected
                        );
                    }
                    for raw in [None, Some(OsStr::new(""))] {
                        assert!(
                            resolve_in(probes, env(&set), raw, hint, in_mux)
                                == hint.unwrap_or_else(|| detect_in(probes, env(&set), in_mux))
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn dedicated_presence_signals_keep_their_nonempty_semantics() {
        for (probes, var, expected) in [
            (LINUX_PROBES, "KONSOLE_VERSION", Surface::Konsole),
            (LINUX_PROBES, "KONSOLE_DBUS_SESSION", Surface::Konsole),
            (MACOS_PROBES, "ITERM_SESSION_ID", Surface::ITerm2),
            (WINDOWS_PROBES, "WT_SESSION", Surface::WindowsTerminal),
        ] {
            for value in ["0", " ", "arbitrary", "\u{fffd}"] {
                assert!(detect_in(probes, env(&[(var, value)]), false) == expected);
            }
            assert!(detect_in(probes, env(&[(var, "")]), false) == Surface::Unknown);
            assert!(detect_in(probes, env(&[]), false) == Surface::Unknown);
        }
        for probes in [LINUX_PROBES, WINDOWS_PROBES] {
            assert!(
                detect_in(
                    probes,
                    env(&[
                        ("LC_TERMINAL", "iTerm2"),
                        ("TERM_PROGRAM", "Apple_Terminal")
                    ]),
                    false
                ) == Surface::Unknown
            );
        }
    }

    fn invalid_encoded_values(raw: &OsStr) {
        assert!(matches!(
            konsole_version(Some(raw), false),
            VersionEvidence::Invalid
        ));
        for var in ["LC_TERMINAL", "TERM_PROGRAM"] {
            let read = |key: &str| (key == var).then_some(raw);
            assert!(detect_in(MACOS_PROBES, read, false) == Surface::Unknown);
            assert!(matching(MACOS_PROBES, read, false).is_none());
        }
        let read = |key: &str| match key {
            "LC_TERMINAL" => Some(raw),
            "TERM_PROGRAM" => Some(OsStr::new("Apple_Terminal")),
            _ => None,
        };
        assert!(detect_in(MACOS_PROBES, read, false) == Surface::AppleTerminal);
        assert_eq!(
            matching(MACOS_PROBES, read, false)
                .unwrap()
                .evidence()
                .to_string(),
            "$TERM_PROGRAM=Apple_Terminal"
        );
        assert!(from_override(Some(raw)) == Some(Surface::Unknown));
    }

    #[cfg(unix)]
    #[test]
    fn invalid_unix_bytes_are_never_repaired_into_a_vendor_value() {
        use std::os::unix::ffi::OsStrExt;
        for bytes in [
            b"iTerm2\xff".as_slice(),
            b"iTerm.app\xff",
            b"Apple_Terminal\xff",
            b"\xff",
        ] {
            invalid_encoded_values(OsStr::from_bytes(bytes));
        }
    }

    #[cfg(windows)]
    #[test]
    fn invalid_windows_encoding_is_never_repaired_into_a_vendor_value() {
        use std::os::windows::ffi::OsStringExt;
        for value in ["iTerm2", "iTerm.app", "Apple_Terminal", ""] {
            let wide: Vec<u16> = value.encode_utf16().chain([0xd800]).collect();
            invalid_encoded_values(&std::ffi::OsString::from_wide(&wide));
        }
    }

    #[test]
    fn only_lc_terminal_is_a_locale_forwarding_candidate() {
        for p in LINUX_PROBES.iter().chain(WINDOWS_PROBES) {
            assert!(!p.locale_forwarding, "{}", p.var);
        }
        for p in MACOS_PROBES {
            assert_eq!(p.locale_forwarding, p.var == "LC_TERMINAL", "{}", p.var);
        }
    }
}
