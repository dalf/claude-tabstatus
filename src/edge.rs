//! The edge this run was invoked for, and what it resolves to.
//!
//! Which hook event maps to which edge is `hooks/hooks.json`'s business, not this
//! binary's. What is this module's business is that the mapping is a closed set,
//! decided once from argv and from a handful of substring tests on the payload,
//! so that nothing further down has to ask "was this the notify edge?" again.

use crate::payload::Payload;
use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;

/// The edge argument, as `hooks.json` spells it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Edge {
    SessionStart,
    Working,
    Waiting,
    Idle,
    Notify,
    SessionEnd,
    /// No argument, or a word this version does not know.
    Unknown,
}

/// The dot, which is the whole point.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Glyph {
    Working,
    Waiting,
    Idle,
}

/// What to paint, and how it has to be delivered - because neither mechanism
/// covers every edge.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Paint {
    /// A glyph beside the location, as one line of hook-protocol JSON.
    Line(Glyph),
    /// The idle glyph, written to the pty directly, after arming the tab.
    SessionStart,
    /// An EMPTY title, written to the pty directly, after restoring the tab.
    /// Session end carries no glyph at all, which is why it is not a `Line`: an
    /// empty title is what unpaints the tab, and a glyph override cannot change
    /// that.
    SessionEnd,
}

impl Paint {
    /// Which dot this paints, or `None` when the title is deliberately empty.
    pub fn glyph(self) -> Option<Glyph> {
        match self {
            Paint::Line(g) => Some(g),
            Paint::SessionStart => Some(Glyph::Idle),
            Paint::SessionEnd => None,
        }
    }
}

impl Edge {
    pub fn parse(arg: Option<&OsStr>) -> Edge {
        match arg.map(OsStr::as_bytes) {
            Some(b"session-start") => Edge::SessionStart,
            Some(b"working") => Edge::Working,
            Some(b"waiting") => Edge::Waiting,
            Some(b"idle") => Edge::Idle,
            Some(b"notify") => Edge::Notify,
            Some(b"session-end") => Edge::SessionEnd,
            // An edge a later `hooks.json` adds, or no argument at all. Both
            // paint the IDLE form rather than nothing: a tab that keeps painting
            // is the forward-compatible choice, and it is pinned by the tests.
            _ => Edge::Unknown,
        }
    }

    /// Whether this edge's state depends on the payload. The other edges only
    /// drain stdin, and never pay for a window of it.
    pub fn reads_payload(self) -> bool {
        matches!(self, Edge::Notify | Edge::SessionStart | Edge::Working)
    }

    /// The edge plus its payload, resolved to a single decision. `None` means
    /// emit nothing at all - which is not an empty title, that would blank the
    /// tab - and it is reached before the location walk is ever spent.
    pub fn resolve(self, payload: &Payload) -> Option<Paint> {
        match self {
            Edge::Working => {
                // PostToolUse is registered unmatched, so a SUBAGENT's tool calls
                // fire it in the main session and would repaint `working` over an
                // orange tab for as long as the subagent runs - "busy, do not
                // bother" while Claude is in fact blocked on you. `agent_id` is
                // documented as present only inside a subagent call, so its
                // PRESENCE is the discriminator. Only `working` is filtered; a
                // subagent's PermissionRequest still paints `waiting`, because you
                // are the one blocking on that dialog whoever asked for it.
                // README's "States" section has the cost this filter carries.
                if payload.head_has_field(b"agent_id") {
                    return None;
                }
                Some(Paint::Line(Glyph::Working))
            }
            Edge::Waiting => Some(Paint::Line(Glyph::Waiting)),
            Edge::Idle | Edge::Unknown => Some(Paint::Line(Glyph::Idle)),
            Edge::Notify => match Notification::detect(payload) {
                Notification::IdlePrompt => Some(Paint::Line(Glyph::Idle)),
                Notification::Waiting => Some(Paint::Line(Glyph::Waiting)),
                Notification::Other => None,
            },
            Edge::SessionStart => {
                // An auto-compaction re-fires SessionStart MID-TURN, which would
                // repaint the idle dot while Claude is still working AND arm the
                // tab a second time with no matching unarm. The needles carry the
                // compact spelling Claude Code actually writes plus the one-space
                // variant, so a pretty-printed payload matches nothing and falls
                // through to painting; hooks.json's
                // `"matcher": "startup|resume|clear|fork"` is the load-bearing
                // guard and this test is a belt to it.
                if payload.head_has(b"\"source\":\"compact\"")
                    || payload.head_has(b"\"source\": \"compact\"")
                {
                    return None;
                }
                Some(Paint::SessionStart)
            }
            Edge::SessionEnd => Some(Paint::SessionEnd),
        }
    }
}

/// What a Notification is telling us, which is a three-way and the third branch
/// is silence.
enum Notification {
    /// The quiet-turn nudge, fired ~60s after a turn ends. It is the one kind
    /// that MUST NOT paint waiting - it arrives after EVERY quiet turn end, so
    /// mapping it to waiting would turn every idle tab orange a minute later and
    /// collapse two of the three states into one. Matched FIRST for that reason,
    /// so it beats a co-present waiting kind.
    IdlePrompt,
    /// A BACKSTOP, not the fast path: `permission_prompt` is scheduled 6s after
    /// the dialog goes up, fires at most once per dialog, and is suppressed
    /// outright by `CLAUDE_CODE_DISABLE_PERMISSION_PROMPT_NOTIFY_HOOKS`.
    /// PermissionRequest, 23ms after PreToolUse, is the real-time signal.
    /// `permission_prompt` is kept anyway because it is the ONLY signal for the
    /// dialogs that are not tool calls, and the other four kinds are what
    /// PermissionRequest does not cover either.
    Waiting,
    /// Deliberately silent - `agent_completed`, the elicitation RESPONSE pair,
    /// `computer_use_exit`, `push_notification`, `auth_success`, and whatever
    /// kind a later Claude Code invents. None of them is a state change, and an
    /// unpainted tab keeps the state it already showed.
    Other,
}

const WAITING_KINDS: [&[u8]; 5] = [
    b"\"notification_type\":\"permission_prompt\"",
    b"\"notification_type\":\"worker_permission_prompt\"",
    b"\"notification_type\":\"agent_needs_input\"",
    b"\"notification_type\":\"elicitation_dialog\"",
    b"\"notification_type\":\"elicitation_url_dialog\"",
];

impl Notification {
    fn detect(payload: &Payload) -> Notification {
        // Both windows, because `notification_type` is serialized LAST, after an
        // unbounded `message`.
        if payload.has(b"\"notification_type\":\"idle_prompt\"") {
            Notification::IdlePrompt
        } else if WAITING_KINDS.iter().any(|k| payload.has(k)) {
            Notification::Waiting
        } else {
            Notification::Other
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn edge(word: &str) -> Edge {
        Edge::parse(Some(OsStr::new(word)))
    }

    #[test]
    fn every_edge_name_parses() {
        assert_eq!(edge("session-start"), Edge::SessionStart);
        assert_eq!(edge("working"), Edge::Working);
        assert_eq!(edge("waiting"), Edge::Waiting);
        assert_eq!(edge("idle"), Edge::Idle);
        assert_eq!(edge("notify"), Edge::Notify);
        assert_eq!(edge("session-end"), Edge::SessionEnd);
    }

    #[test]
    fn no_argv_and_an_unknown_word_are_both_unknown() {
        assert_eq!(Edge::parse(None), Edge::Unknown);
        assert_eq!(edge(""), Edge::Unknown);
        assert_eq!(edge("no-such-edge"), Edge::Unknown);
        assert_eq!(edge("Working"), Edge::Unknown);
        assert_eq!(edge("session_start"), Edge::Unknown);
    }

    #[test]
    fn an_unknown_edge_paints_the_idle_form_as_a_hook_line() {
        let p = Payload::empty();
        assert_eq!(Edge::Unknown.resolve(&p), Some(Paint::Line(Glyph::Idle)));
        assert_eq!(Edge::Idle.resolve(&p), Some(Paint::Line(Glyph::Idle)));
    }

    #[test]
    fn only_three_edges_pay_for_a_payload_window() {
        assert!(Edge::Notify.reads_payload());
        assert!(Edge::SessionStart.reads_payload());
        assert!(Edge::Working.reads_payload());
        assert!(!Edge::Waiting.reads_payload());
        assert!(!Edge::Idle.reads_payload());
        assert!(!Edge::SessionEnd.reads_payload());
        assert!(!Edge::Unknown.reads_payload());
    }

    #[test]
    fn session_end_paints_no_glyph_and_session_start_paints_idle() {
        let p = Payload::empty();
        assert_eq!(Edge::SessionEnd.resolve(&p), Some(Paint::SessionEnd));
        assert_eq!(Paint::SessionEnd.glyph(), None);
        assert_eq!(Edge::SessionStart.resolve(&p), Some(Paint::SessionStart));
        assert_eq!(Paint::SessionStart.glyph(), Some(Glyph::Idle));
    }

    #[test]
    fn a_compacting_session_start_paints_nothing() {
        for line in [
            &br#"{"source":"compact"}"#[..],
            &br#"{"source": "compact"}"#[..],
        ] {
            assert_eq!(Edge::SessionStart.resolve(&payload(line)), None);
        }
        // Two spaces is not the spelling Claude Code writes.
        let p = payload(br#"{"source":  "compact"}"#);
        assert_eq!(Edge::SessionStart.resolve(&p), Some(Paint::SessionStart));
    }

    #[test]
    fn a_subagent_working_edge_paints_nothing() {
        let p = payload(br#"{"hook_event_name":"PostToolUse","agent_id":"abc"}"#);
        assert_eq!(Edge::Working.resolve(&p), None);
        let p = payload(br#"{"hook_event_name":"PostToolUse"}"#);
        assert_eq!(Edge::Working.resolve(&p), Some(Paint::Line(Glyph::Working)));
        // An empty value must not silence the edge for the whole session.
        let p = payload(br#"{"agent_id":""}"#);
        assert_eq!(Edge::Working.resolve(&p), Some(Paint::Line(Glyph::Working)));
    }

    #[test]
    fn every_waiting_kind_paints_orange() {
        for kind in WAITING_KINDS {
            let mut line = b"{".to_vec();
            line.extend_from_slice(kind);
            line.extend_from_slice(b"}");
            assert_eq!(
                Edge::Notify.resolve(&payload(&line)),
                Some(Paint::Line(Glyph::Waiting)),
                "{}",
                String::from_utf8_lossy(kind)
            );
        }
    }

    #[test]
    fn idle_prompt_beats_a_co_present_waiting_kind() {
        let p = payload(
            br#"{"notification_type":"permission_prompt","x":"\"notification_type\":\"idle_prompt\""}"#,
        );
        // The escaped copy cannot match, so this one is orange.
        assert_eq!(Edge::Notify.resolve(&p), Some(Paint::Line(Glyph::Waiting)));
        let p = payload(
            br#"{"notification_type":"permission_prompt","also":1,"notification_type":"idle_prompt"}"#,
        );
        assert_eq!(Edge::Notify.resolve(&p), Some(Paint::Line(Glyph::Idle)));
    }

    #[test]
    fn an_unrecognised_kind_is_silent() {
        for line in [
            &br#"{"notification_type":"agent_completed"}"#[..],
            // One space after the colon is NOT tolerated on this edge.
            &br#"{"notification_type": "permission_prompt"}"#[..],
            &br#"{"notification_type":"xpermission_prompt"}"#[..],
            &b"{}"[..],
            &b""[..],
        ] {
            assert_eq!(
                Edge::Notify.resolve(&payload(line)),
                None,
                "{}",
                String::from_utf8_lossy(line)
            );
        }
    }

    #[test]
    fn a_kind_in_the_back_window_still_counts() {
        // `notification_type` is serialized after an unbounded `message`.
        let mut line = br#"{"message":""#.to_vec();
        line.resize(9000, b'm');
        line.extend_from_slice(br#"","notification_type":"agent_needs_input"}"#);
        line.push(b'\n');
        assert_eq!(
            Edge::Notify.resolve(&payload_with_newline(&line)),
            Some(Paint::Line(Glyph::Waiting))
        );
    }

    // Test helpers: a Payload built from bytes rather than from stdin.
    fn payload(line: &[u8]) -> Payload {
        let mut with = line.to_vec();
        with.push(b'\n');
        payload_with_newline(&with)
    }

    fn payload_with_newline(bytes: &[u8]) -> Payload {
        Payload::from_bytes(bytes)
    }
}
