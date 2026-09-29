//! The environment, read once and turned into types.
//!
//! Every closed set the paint path branches on is decided here and nowhere else,
//! so the code below matches on an enum instead of re-testing a string. The three
//! readers exist because "set" has three meanings here and conflating any two of
//! them is a behaviour change:
//!
//!   * [`var`] - `None` only when UNSET, so `CCTAB_ELLIPSIS=''` is an EMPTY
//!     ellipsis rather than the default one.
//!   * [`var_nonempty`] - set AND non-empty, for a flag with a payload:
//!     `GIT_DIR=''` is ignored, `CLAUDE_PID=''` is not a pty.
//!   * [`flag`] - value never matters; `TMUX=''` does not suppress Konsole
//!     detection.
//!
//! Read with `var_os`, never `env::var`: `CCTAB_HOST`, `CCTAB_ELLIPSIS` and the
//! glyph overrides can hold bytes that are not valid UTF-8, and those are REPAIRED
//! into a visible U+FFFD rather than thrown away for the default.

use crate::edge::Glyph;
use crate::text;
use crate::tmux::Tmux;
use std::ffi::{OsStr, OsString};
use std::os::unix::ffi::OsStrExt;
use std::path::PathBuf;

pub fn var(key: &str) -> Option<OsString> {
    std::env::var_os(key)
}

pub fn var_nonempty(key: &str) -> Option<OsString> {
    var(key).filter(|v| !v.is_empty())
}

pub fn flag(key: &str) -> bool {
    var_nonempty(key).is_some()
}

/// Whether this session arrived over an ssh hop.
///
/// ONE definition, used by both the host prefix and [`Terminal::detect`], because
/// the two have to agree: a report that withheld the Konsole verdict as
/// unknowable-over-ssh while the title beside it painted no host prefix would be
/// contradicting itself on the same page, and whichever of the two a later reader
/// believed would be a coin toss. `sshd` sets `SSH_CONNECTION` for every session
/// and `SSH_TTY` only when it allocated a pty, so either one alone is enough and
/// neither is redundant.
pub fn over_ssh() -> bool {
    flag("SSH_CONNECTION") || flag("SSH_TTY")
}

/// `${VAR-default}` as display text: the default applies only to an UNSET
/// variable, and anything set is repaired rather than rejected.
fn var_or(key: &str, default: &str) -> String {
    match var(key) {
        Some(v) => text::repair(v.as_bytes()),
        None => default.to_owned(),
    }
}

/// Which terminal is drawing the tab, to the extent it can be known.
///
/// `$TMUX` / `$STY` take the multiplexer case out: `KONSOLE_*` leaks into any
/// child launched from a Konsole shell, and into every pane of a tmux server that
/// was first started under Konsole, so inside a multiplexer those variables say
/// nothing about the terminal actually drawing the tab.
///
/// `SSH_*` takes out the other one, and for a reason worth stating rather than
/// assuming. `KONSOLE_*` does not normally cross a hop - OpenSSH accepts no
/// environment by default at either end - so over ssh the variables are usually
/// absent and `CCTAB_TERMINAL` is already the answer. When they DO arrive, from a
/// deliberate `SendEnv`/`AcceptEnv` pair or from a remote rc, nothing here can
/// tell those two apart: the first is a statement about this hop, the second a
/// statement about nothing. Trusting them anyway would make the rare case behave
/// unlike the common one, and it is the case where being wrong actually lands -
/// outside tmux the Konsole verdict writes OSC 50 STRAIGHT to the pty, and OSC 50
/// is xterm's SET FONT. Konsole's own header concedes the clash. So a terminal
/// identity that crossed a host boundary is not evidence here, which is the same
/// rule the in-tmux case already follows by proving ownership instead.
///
/// `CCTAB_TERMINAL` overrides all of it, and is the only honest signal in the
/// topology this exists for: over ssh there is nothing to detect - Konsole's DBus
/// is unreachable from the remote, and the one probe that would be proof,
/// `XTVERSION` on the pty, cannot be read by a hook without stealing bytes from
/// the TUI that owns that pty. `konsole` names it; any other value says
/// explicitly that it is NOT Konsole, which is how a false positive from a leaked
/// `KONSOLE_*` is turned off.
///
/// It is the one knob in this file that changes what paints OUTSIDE tmux as well
/// as in, which is why it is opt-in and why the name is matched case-INSENSITIVELY
/// (ASCII): a byte compare made `CCTAB_TERMINAL=Konsole` mean "explicitly not
/// Konsole" and silently turned the arming and the suffix layout OFF - the exact
/// opposite of what the user typed.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Terminal {
    Konsole,
    Unknown,
}

impl Terminal {
    /// What `CCTAB_TERMINAL` says, or `None` when it says nothing.
    fn from_override(raw: Option<&OsStr>) -> Option<Terminal> {
        let v = raw.map(OsStr::as_bytes)?;
        Some(if v.eq_ignore_ascii_case(b"konsole") {
            Terminal::Konsole
        } else {
            Terminal::Unknown
        })
    }

    pub fn detect() -> Terminal {
        if let Some(t) = Terminal::from_override(var_nonempty("CCTAB_TERMINAL").as_deref()) {
            return t;
        }
        if !flag("TMUX")
            && !flag("STY")
            && !over_ssh()
            && (flag("KONSOLE_VERSION") || flag("KONSOLE_DBUS_SESSION"))
        {
            Terminal::Konsole
        } else {
            Terminal::Unknown
        }
    }
}

/// Where the glyph goes relative to the location.
///
/// Konsole's tab bar elides from the LEFT - `QTabBar::setElideMode(Qt::ElideLeft)`
/// at a hardcoded call site, which no config key reads - so a LEADING glyph is the
/// first thing cut and under Konsole the glyph goes last. Windows Terminal
/// truncates from the RIGHT, so there it goes first, which is also the safe
/// default for any terminal we cannot identify - including every session reached
/// over ssh, because the local terminal's variables do not travel.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum GlyphPos {
    Prefix,
    Suffix,
    Both,
}

impl GlyphPos {
    fn parse(raw: Option<&OsStr>, terminal: Terminal) -> GlyphPos {
        match raw.map(OsStr::as_bytes) {
            Some(b"suffix") => GlyphPos::Suffix,
            Some(b"both") => GlyphPos::Both,
            // An unrecognised value falls through to prefix rather than failing.
            Some(_) => GlyphPos::Prefix,
            None => match terminal {
                Terminal::Konsole => GlyphPos::Suffix,
                Terminal::Unknown => GlyphPos::Prefix,
            },
        }
    }
}

/// A length limit in characters, or none at all.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Cap {
    Off,
    Max(usize),
}

impl Cap {
    /// `0` turns the cap off; one to three digits with no leading zero are the
    /// limit; anything else is the DEFAULT. Then a floor, because a cap shorter
    /// than the ellipsis it appends would be a title made entirely of marker.
    ///
    /// Deliberately not `str::parse`: that would read `08` as 8, `0032` as 32 and
    /// `1000` as 1000, where all three have to fall back to the default. The
    /// grammar is the behaviour, and the corpus pins every arm of it.
    fn parse(raw: Option<&OsStr>, default: usize, floor: usize) -> Cap {
        let n = match raw.map(OsStr::as_bytes) {
            Some(b"0") => return Cap::Off,
            Some(b)
                if (1..=3).contains(&b.len())
                    && (b'1'..=b'9').contains(&b[0])
                    && b.iter().all(u8::is_ascii_digit) =>
            {
                b.iter().fold(0usize, |a, c| a * 10 + (c - b'0') as usize)
            }
            _ => default,
        };
        Cap::Max(n.max(floor))
    }
}

/// The compiled-in defaults, named once so the test fixture below cannot drift
/// from what `from_env` actually installs.
const DEFAULT_ELLIPSIS: &str = "\u{2026}";
const DEFAULT_GLYPH_WORKING: &str = "\u{1f535}";
const DEFAULT_GLYPH_WAITING: &str = "\u{1f7e0}";
const DEFAULT_GLYPH_IDLE: &str = "\u{26aa}";
const DEFAULT_GLYPH_BACKGROUND: &str = "\u{1f7e3}";
/// Roughly what a Konsole tab shows before it elides, and a floor below which a
/// title would be made mostly of marker.
const DEFAULT_MAX_LOCATION: usize = 32;
const MIN_MAX_LOCATION: usize = 8;
/// The kernel allows a 64-byte host name; a tab does not.
const DEFAULT_MAX_HOST: usize = 16;
const MIN_MAX_HOST: usize = 4;

pub struct Config {
    /// Print the computed title and emit nothing at all. This is what makes the
    /// edge table testable with no Claude session.
    pub dry_run: bool,
    pub terminal: Terminal,
    pub glyph_pos: GlyphPos,
    pub ellipsis: String,
    glyph_working: String,
    glyph_waiting: String,
    glyph_idle: String,
    glyph_background: String,
    pub max_location: Cap,
    pub max_host: Cap,
    /// Whether this session came in over ssh. Only then is a host prefix
    /// painted, because the ABSENCE of one is how a local session is recognized.
    pub ssh: bool,
    /// `CCTAB_HOST` and `$HOSTNAME`. Kept as the raw inputs rather than a
    /// resolved name: resolving reads `/proc` and, where that is absent, forks
    /// `hostname`, and neither may happen on a session that will paint no prefix.
    pub host_override: Option<OsString>,
    pub hostname_env: Option<OsString>,
    /// `$HOME` with one trailing slash removed, which a `$HOME` that carries one
    /// needs or the home directory renders as its own full path.
    pub home: Option<PathBuf>,
    pub pwd: Option<OsString>,
    pub git_dir: Option<OsString>,
    pub claude_pid: Option<OsString>,
    /// The tmux server this session runs inside, when there is one. It decides
    /// ONE thing on the paint path - whether the OSC 0 carries a tab title or a
    /// record - and everything else it is used for is a cold path.
    pub tmux: Option<Tmux>,
}

impl Config {
    pub fn from_env() -> Config {
        let terminal = Terminal::detect();
        Config {
            dry_run: var("CCTAB_DRY_RUN").is_some_and(|v| v.as_bytes() == b"1"),
            terminal,
            glyph_pos: GlyphPos::parse(var_nonempty("CCTAB_GLYPH_POS").as_deref(), terminal),
            ellipsis: var_or("CCTAB_ELLIPSIS", DEFAULT_ELLIPSIS),
            glyph_working: var_or("CCTAB_GLYPH_WORKING", DEFAULT_GLYPH_WORKING),
            glyph_waiting: var_or("CCTAB_GLYPH_WAITING", DEFAULT_GLYPH_WAITING),
            glyph_idle: var_or("CCTAB_GLYPH_IDLE", DEFAULT_GLYPH_IDLE),
            glyph_background: var_or("CCTAB_GLYPH_BACKGROUND", DEFAULT_GLYPH_BACKGROUND),
            max_location: Cap::parse(
                var("CCTAB_MAX_LOCATION").as_deref(),
                DEFAULT_MAX_LOCATION,
                MIN_MAX_LOCATION,
            ),
            max_host: Cap::parse(
                var("CCTAB_MAX_HOST").as_deref(),
                DEFAULT_MAX_HOST,
                MIN_MAX_HOST,
            ),
            ssh: over_ssh(),
            host_override: var_nonempty("CCTAB_HOST"),
            hostname_env: var("HOSTNAME"),
            home: var_nonempty("HOME").as_deref().and_then(home_dir),
            pwd: var("PWD"),
            git_dir: var_nonempty("GIT_DIR"),
            claude_pid: var_nonempty("CLAUDE_PID"),
            tmux: Tmux::detect(),
        }
    }

    /// The compiled-in defaults and an empty environment, so the unit tests in
    /// this crate do not depend on the process they run in.
    #[cfg(test)]
    pub fn for_test() -> Config {
        Config {
            dry_run: false,
            terminal: Terminal::Unknown,
            glyph_pos: GlyphPos::Prefix,
            ellipsis: DEFAULT_ELLIPSIS.to_owned(),
            glyph_working: DEFAULT_GLYPH_WORKING.to_owned(),
            glyph_waiting: DEFAULT_GLYPH_WAITING.to_owned(),
            glyph_idle: DEFAULT_GLYPH_IDLE.to_owned(),
            glyph_background: DEFAULT_GLYPH_BACKGROUND.to_owned(),
            max_location: Cap::Max(DEFAULT_MAX_LOCATION),
            max_host: Cap::Max(DEFAULT_MAX_HOST),
            ssh: false,
            host_override: None,
            hostname_env: None,
            home: None,
            pwd: None,
            git_dir: None,
            claude_pid: None,
            tmux: None,
        }
    }

    #[cfg(test)]
    pub fn set_glyph(&mut self, which: Glyph, to: String) {
        match which {
            Glyph::Working => self.glyph_working = to,
            Glyph::Waiting => self.glyph_waiting = to,
            Glyph::Idle => self.glyph_idle = to,
            Glyph::Background => self.glyph_background = to,
        }
    }

    pub fn glyph(&self, which: Glyph) -> &str {
        match which {
            Glyph::Working => &self.glyph_working,
            Glyph::Waiting => &self.glyph_waiting,
            Glyph::Idle => &self.glyph_idle,
            Glyph::Background => &self.glyph_background,
        }
    }
}

/// `$HOME` with ONE trailing slash removed - not every one, and not a
/// normalisation: this only has to stop a `HOME` that carries a slash from
/// missing the prefix test in `location`. `HOME=/` therefore abbreviates nothing,
/// which is the same answer as an unset `HOME`: there is no prefix left to
/// replace with `~`.
fn home_dir(raw: &OsStr) -> Option<PathBuf> {
    let b = raw.as_bytes();
    let trimmed = b.strip_suffix(b"/").unwrap_or(b);
    if trimmed.is_empty() {
        return None;
    }
    Some(PathBuf::from(OsStr::from_bytes(trimmed).to_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cap(raw: Option<&str>, default: usize, floor: usize) -> Cap {
        Cap::parse(raw.map(OsStr::new), default, floor)
    }

    #[test]
    fn the_terminal_override_names_konsole_in_any_case() {
        let t = |v: Option<&str>| Terminal::from_override(v.map(OsStr::new));
        for yes in ["konsole", "Konsole", "KONSOLE", "kOnSoLe"] {
            assert!(matches!(t(Some(yes)), Some(Terminal::Konsole)), "{yes}");
        }
        // Anything else is an explicit NOT Konsole, which is what turns a leaked
        // KONSOLE_* off; only absence leaves the detection to the environment.
        for no in ["wezterm", "konsol", "konsolex", "xterm", " konsole"] {
            assert!(matches!(t(Some(no)), Some(Terminal::Unknown)), "{no}");
        }
        assert!(t(None).is_none());
    }

    #[test]
    fn zero_turns_the_cap_off() {
        assert_eq!(cap(Some("0"), 32, 8), Cap::Off);
        assert_eq!(cap(Some("0"), 16, 4), Cap::Off);
    }

    #[test]
    fn one_to_three_digits_without_a_leading_zero_are_the_limit() {
        assert_eq!(cap(Some("9"), 32, 8), Cap::Max(9));
        assert_eq!(cap(Some("40"), 32, 8), Cap::Max(40));
        assert_eq!(cap(Some("999"), 32, 8), Cap::Max(999));
    }

    #[test]
    fn a_leading_zero_or_a_fourth_digit_falls_back_to_the_default() {
        // Where `str::parse` would have said 8, 32, 32 and 1000.
        assert_eq!(cap(Some("08"), 32, 8), Cap::Max(32));
        assert_eq!(cap(Some("032"), 32, 8), Cap::Max(32));
        assert_eq!(cap(Some("0032"), 32, 8), Cap::Max(32));
        assert_eq!(cap(Some("1000"), 32, 8), Cap::Max(32));
        assert_eq!(cap(Some("020"), 16, 4), Cap::Max(16));
    }

    #[test]
    fn a_sign_a_dot_or_a_word_falls_back_to_the_default() {
        for raw in ["-1", "3.5", "nope", " 8", "8 ", "+8", "1e2"] {
            assert_eq!(cap(Some(raw), 32, 8), Cap::Max(32), "{:?}", raw);
        }
    }

    #[test]
    fn unset_and_set_empty_both_mean_the_default() {
        assert_eq!(cap(None, 32, 8), Cap::Max(32));
        assert_eq!(cap(Some(""), 32, 8), Cap::Max(32));
        assert_eq!(cap(None, 16, 4), Cap::Max(16));
        assert_eq!(cap(Some(""), 16, 4), Cap::Max(16));
    }

    #[test]
    fn the_floor_applies_to_a_value_that_parsed() {
        assert_eq!(cap(Some("2"), 32, 8), Cap::Max(8));
        assert_eq!(cap(Some("3"), 16, 4), Cap::Max(4));
        assert_eq!(cap(Some("8"), 32, 8), Cap::Max(8));
    }

    #[test]
    fn invalid_utf8_in_a_cap_is_not_a_number() {
        let raw = OsStr::from_bytes(b"3\xff");
        assert_eq!(Cap::parse(Some(raw), 32, 8), Cap::Max(32));
    }

    #[test]
    fn glyph_pos_parses_the_two_named_positions_and_nothing_else() {
        let p = |s: Option<&str>, t| GlyphPos::parse(s.map(OsStr::new), t);
        assert!(p(Some("suffix"), Terminal::Unknown) == GlyphPos::Suffix);
        assert!(p(Some("both"), Terminal::Unknown) == GlyphPos::Both);
        assert!(p(Some("prefix"), Terminal::Konsole) == GlyphPos::Prefix);
        assert!(p(Some("sideways"), Terminal::Konsole) == GlyphPos::Prefix);
    }

    #[test]
    fn an_unset_glyph_pos_lets_the_terminal_decide() {
        // And `CCTAB_GLYPH_POS=''` is unset, because `var_nonempty` feeds this.
        let p = |t| GlyphPos::parse(None, t);
        assert!(p(Terminal::Konsole) == GlyphPos::Suffix);
        assert!(p(Terminal::Unknown) == GlyphPos::Prefix);
    }

    #[test]
    fn home_loses_one_trailing_slash_and_root_loses_itself() {
        let h = |s: &str| home_dir(OsStr::new(s));
        assert_eq!(h("/home/a"), Some(PathBuf::from("/home/a")));
        assert_eq!(h("/home/a/"), Some(PathBuf::from("/home/a")));
        assert_eq!(h("/"), None);
        assert_eq!(h("relative"), Some(PathBuf::from("relative")));
    }
}
