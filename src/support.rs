//! One vocabulary for absence, because the crate has six and none of them can
//! say "you turned it off".
//!
//! A capability question here is answered by whatever shape the answering
//! function happened to have, and today that is six different shapes:
//!
//!   * `sys::session_tty(claude_pid: &OsStr) -> Option<File>` - no hook
//!     subprocess, an fd 1 that will not resolve, and an open that failed are one
//!     `None`.
//!   * `sys::set_session_title(claude_pid: &OsStr, title: &str) -> io::Result<bool>`
//!     - `Ok(false)` is the headless guard refusing AND "this build has no console
//!     route at all", two answers under one word, with `Err` beside them.
//!   * `state::Origin::alive(self) -> bool` - a pid that is gone and a pid this
//!     user may not open answer the same word, deliberately, and the caller cannot
//!     tell which it got.
//!   * `state::dir() -> Option<PathBuf>` - `None` is "no record lock on this
//!     platform" and "no `CCTAB_STATE_DIR` and no `XDG_RUNTIME_DIR`" at once;
//!     `sys::NO_STATE_DIR` exists only because that `None` cannot say which.
//!   * `manage::install(dir: Option<OsString>) -> Result<(), String>` - a prose
//!     sentence, the only one of the six that CAN say why, and the one nothing can
//!     match on.
//!   * and, for the Konsole arming, no return type at all: a pair of complementary
//!     predicates, `terminal == Konsole && tmux.is_none()` in `emit::session_start`
//!     and its complement in `tmux::arm_konsole`, which two separate files have to
//!     be read together to see is total.
//!
//! Every one of them collapses two or three different answers into one, and the
//! one they all lose is the answer a user can act on: a `None` that means "this
//! build cannot" reads exactly like a `None` that means "CCTAB_NO_TMUX is set" and
//! like a `None` that means "the open failed with EACCES".
//!
//! There are FIVE words here and each one exists because a measured fact forces
//! it. Four are obvious. The fifth is [`Support::Unverifiable`], and it is forced
//! by Windows Terminal: `compatibility.allowOSC777` defaults to **false**, and
//! `profiles.suppressApplicationTitle` silently discards OSC 0 and OSC 2 - and
//! neither one is exposed through an environment variable, a version string or a
//! query this process could read. Worse, there is no reader at all: a hook
//! subprocess is detached with fd 0 on /dev/null and `exec 3>/dev/tty` fails, so
//! DA1, XTVERSION and DECRQSS are all unavailable and capability detection can
//! never be dynamic in this process model. Calling such a row `Available` lies to
//! the users who left the default alone and calling it `Unsupported` lies to the
//! ones who changed it, so it gets its own word. It does NOT gate emission - a
//! discarded escape sequence costs nothing - it gates what `doctor` claims.
//!
//! The type PARAMETER is what keeps this one vocabulary instead of two. Without
//! it there would be a capability enum answering "can you?" beside a `Result`
//! answering "did it work?", which is today's defect in a new place; with it the
//! reporting boundary lifts `sys::session_tty` to a `Support<File>` and
//! `sys::set_session_title` to a `Support<bool>` without touching either
//! signature, and a `bell: Presence` row prints through the same formatter.
//!
//! [`Support::Failed`] holds a real `io::Error` rather than a flattened
//! `(ErrorKind, raw_os_error)` pair, because the error's own message is the whole
//! diagnostic value and nothing here can improve on it. The price is that
//! `Support` is neither `Copy` nor `Clone` nor `PartialEq`, so a const capability
//! table is reached as `&'static` - which is what the paint path wants anyway -
//! and a test compares [`Support::label`] and [`Support::reason`] instead of the
//! value.
//!
//! Liveness is deliberately NOT one of these words, and it does not move here. It
//! answers a question about a FOREIGN PROCESS - is that pid still itself - and not
//! about a capability of this build, and folding it in would make `Unsupported`
//! mean "that pid is not a thing". It stays the `Option<bool>` that
//! [`crate::sys::process_alive`] and [`crate::sys::same_process`] already answer
//! with: there `None` is "this user may not ask about that process", which is a
//! fact about somebody else's process and not an absence this build could report
//! or a knob anyone could turn, and `state::Origin::alive` reads it as "keep the
//! record" for exactly that reason.

use std::borrow::Cow;
use std::fmt;

/// Every capability query in this crate answers with this, and `doctor` prints
/// exactly these five words.
pub enum Support<T = ()> {
    /// The capability is here, and this is what it produced.
    Available(T),
    /// This build, this platform, this terminal cannot, ever. The string is the
    /// REASON in the present tense - "the reply lands on the TUI's stdin" - not a
    /// restatement of the question.
    Unsupported(&'static str),
    /// It could, and a knob of OURS turned it off. The string NAMES THAT KNOB, so
    /// that a user who reads it can grep the README for it: `"CCTAB_NO_TMUX"`,
    /// `"CCTAB_DRY_RUN"`. Never "disabled by configuration", which names nothing.
    // A const capability row may not say it - `surface::tests` asserts exactly
    // that - so the only thing that can construct one is [`Support::gate`], whose
    // consumer is doctor's report in a later commit of this series.
    #[allow(dead_code)]
    Disabled(&'static str),
    /// The terminal may or may not honour it and NOTHING WE CAN READ SAYS WHICH.
    /// The string names the FOREIGN setting the user must go and check:
    /// `"compatibility.allowOSC777"`, `"profiles.suppressApplicationTitle"`.
    ///
    /// The Windows Terminal rows of the surface axis are what force it, and it
    /// landed here before them because the vocabulary is the thing being fixed,
    /// and a fifth word added later is a fifth word every existing caller has to
    /// be re-read for.
    Unverifiable(&'static str),
    /// It was attempted and the OS said no.
    // Only an ATTEMPT produces one, so it arrives through [`Support::of_io`] at
    // the boundary that lifts `sys::session_tty` and `sys::set_session_title`
    // into this vocabulary - doctor's report, a later commit of this series.
    #[allow(dead_code)]
    Failed(std::io::Error),
}

/// A capability that yields nothing but its own presence.
pub type Presence = Support<()>;

/// `Available(())`, spelled once so a const table row is one word wide.
pub const YES: Presence = Support::Available(());

// `should_emit`, `label` and `reason` are live - the composer gates on the first
// and `Display` prints the other two. The six below are the REPORTING boundary's
// half of this file, and doctor is a later commit of this series; each carries the
// lint suppression on its own line so that promoting one is a one-line edit.
impl<T> Support<T> {
    #[allow(dead_code)]
    pub fn ok(self) -> Option<T> {
        match self {
            Support::Available(v) => Some(v),
            _ => None,
        }
    }

    #[allow(dead_code)]
    pub fn is_available(&self) -> bool {
        matches!(self, Support::Available(_))
    }

    /// Emit? `Available` and `Unverifiable` say yes; the other three say no.
    ///
    /// This is the one place the reporting/emitting distinction is decided, and
    /// it is why `Unverifiable` is safe to add: a sequence a terminal discards
    /// costs one write that was going to happen anyway, and refusing to send it
    /// would turn an unknown into a certain no.
    pub fn should_emit(&self) -> bool {
        matches!(self, Support::Available(_) | Support::Unverifiable(_))
    }

    #[allow(dead_code)]
    pub fn map<U>(self, f: impl FnOnce(T) -> U) -> Support<U> {
        match self {
            Support::Available(v) => Support::Available(f(v)),
            Support::Unsupported(r) => Support::Unsupported(r),
            Support::Disabled(r) => Support::Disabled(r),
            Support::Unverifiable(r) => Support::Unverifiable(r),
            Support::Failed(e) => Support::Failed(e),
        }
    }

    /// Change the payload type while KEEPING the reason, so that a layer boundary
    /// can never silently invent one. `Ok` is the value; `Err` is this same
    /// absence wearing the outer layer's type parameter.
    #[allow(dead_code)]
    pub fn carry<U>(self) -> Result<T, Support<U>> {
        match self {
            Support::Available(v) => Ok(v),
            // Spelled out rather than `other.map(|_| unreachable!())`: that
            // closure is never called, but it puts a panic in the crate, and
            // `panic = "abort"` is why there are none.
            Support::Unsupported(r) => Err(Support::Unsupported(r)),
            Support::Disabled(r) => Err(Support::Disabled(r)),
            Support::Unverifiable(r) => Err(Support::Unverifiable(r)),
            Support::Failed(e) => Err(Support::Failed(e)),
        }
    }

    /// Layer OUR knob over a fact the platform or a const row already stated, so
    /// that the row can say `Available` without knowing the knob exists. `None`
    /// means no knob applies. doctor's `hostname` row is what will read it: the
    /// platform answers, and `CCTAB_HOST` is what decides whether that answer is
    /// the one painted.
    #[allow(dead_code)]
    pub fn gate(self, off: Option<&'static str>) -> Support<T> {
        match (self, off) {
            (Support::Available(_), Some(knob)) => Support::Disabled(knob),
            (other, _) => other,
        }
    }

    #[allow(dead_code)]
    pub fn of_io(r: std::io::Result<T>) -> Support<T> {
        match r {
            Ok(v) => Support::Available(v),
            Err(e) => Support::Failed(e),
        }
    }

    /// `ok` / `n/a` / `off` / `?` / `fail`. The ONLY place these are spelled, so
    /// that a report cannot grow a sixth word without this line changing.
    pub fn label(&self) -> &'static str {
        match self {
            Support::Available(_) => "ok",
            Support::Unsupported(_) => "n/a",
            Support::Disabled(_) => "off",
            Support::Unverifiable(_) => "?",
            Support::Failed(_) => "fail",
        }
    }

    /// The right-hand column of a report: the reason, the knob, the foreign
    /// setting, or the error's own text.
    ///
    /// Borrowed for four of the five variants; `Failed` formats and therefore
    /// allocates, which is affordable because only a cold path ever holds one.
    pub fn reason(&self) -> Option<Cow<'_, str>> {
        match self {
            Support::Available(_) => None,
            Support::Unsupported(r) | Support::Disabled(r) | Support::Unverifiable(r) => {
                Some(Cow::Borrowed(*r))
            }
            Support::Failed(e) => Some(Cow::Owned(e.to_string())),
        }
    }
}

/// `ok`, or `<label>: <reason>`. The whole of the report formatter, here rather
/// than in `doctor`, so that one answer cannot print two ways.
impl<T> fmt::Display for Support<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.reason() {
            None => f.write_str(self.label()),
            Some(why) => write!(f, "{}: {}", self.label(), why),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Error, ErrorKind};

    #[test]
    fn the_five_words_are_five_distinct_labels() {
        let rows: [Presence; 5] = [
            YES,
            Support::Unsupported("a one-shot hook has no reader"),
            Support::Disabled("CCTAB_NO_TMUX"),
            Support::Unverifiable("compatibility.allowOSC777"),
            Support::Failed(Error::from(ErrorKind::PermissionDenied)),
        ];
        let labels: Vec<&str> = rows.iter().map(Support::label).collect();
        assert_eq!(labels, ["ok", "n/a", "off", "?", "fail"]);
    }

    #[test]
    fn a_report_line_names_what_the_user_has_to_go_and_change() {
        assert_eq!(YES.to_string(), "ok");
        assert_eq!(
            Support::<()>::Disabled("CCTAB_NO_TMUX").to_string(),
            "off: CCTAB_NO_TMUX"
        );
        assert_eq!(
            Support::<()>::Unverifiable("compatibility.allowOSC777").to_string(),
            "?: compatibility.allowOSC777"
        );
        assert_eq!(
            Support::<()>::Unsupported("the reply lands on the TUI's stdin").to_string(),
            "n/a: the reply lands on the TUI's stdin"
        );
    }

    #[test]
    fn a_failure_prints_the_oss_own_words_and_nothing_of_ours() {
        let line = Support::<()>::Failed(Error::from(ErrorKind::PermissionDenied)).to_string();
        assert!(line.starts_with("fail: "), "{line}");
        assert!(line.contains("permission denied"), "{line}");
    }

    /// The distinction the crate did not have: a knob of ours is not a platform
    /// limit, and the report has to be able to say which one it hit.
    #[test]
    fn our_knob_turns_a_fact_into_off_and_never_the_other_way_round() {
        assert_eq!(YES.gate(Some("CCTAB_NO_TMUX")).label(), "off");
        assert_eq!(YES.gate(None).label(), "ok");
        // A knob cannot promote something the platform cannot do.
        let n = Support::<()>::Unsupported("no /proc on this platform").gate(Some("CCTAB_NO_TMUX"));
        assert_eq!(n.reason().as_deref(), Some("no /proc on this platform"));
    }

    /// `Unverifiable` gates the CLAIM, not the write. A row that stopped emitting
    /// would turn "we cannot tell" into a certain "no".
    #[test]
    fn unverifiable_still_emits_and_disabled_does_not() {
        assert!(Support::<()>::Unverifiable("allowOSC777").should_emit());
        assert!(YES.should_emit());
        assert!(!Support::<()>::Disabled("CCTAB_NO_TMUX").should_emit());
        assert!(!Support::<()>::Unsupported("no console").should_emit());
        assert!(!Support::<()>::Failed(Error::from(ErrorKind::NotFound)).should_emit());
    }

    #[test]
    fn a_reason_survives_a_change_of_payload_type() {
        let r: Result<u32, Presence> = Support::<u32>::Unsupported("no /proc").carry();
        match r {
            Err(p) => assert_eq!(p.reason().as_deref(), Some("no /proc")),
            Ok(_) => panic!("Unsupported is not a value"),
        }
        assert_eq!(Support::Available(2u32).map(|v| v + 1).ok(), Some(3));
    }

    #[test]
    fn an_io_result_becomes_available_or_failed_and_never_a_reason_we_invented() {
        assert!(Support::of_io(Ok(7)).is_available());
        let f: Support<u8> = Support::of_io(Err(Error::from(ErrorKind::NotFound)));
        assert_eq!(f.label(), "fail");
    }
}
