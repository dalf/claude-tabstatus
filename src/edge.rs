//! The edge this run was invoked for, and what it resolves to.
//!
//! Which hook event maps to which edge is `hooks/hooks.json`'s business, not this
//! binary's. What is this module's business is that the mapping is a closed set,
//! decided once from argv and selectively parsed top-level hook metadata,
//! so that nothing further down has to ask "was this the notify edge?" again.

use crate::payload::Payload;
use std::ffi::OsStr;

/// The edge argument, as `hooks.json` spells it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Edge {
    SessionStart,
    Working,
    Waiting,
    Idle,
    Notify,
    SessionEnd,
    /// A subagent finished. It paints NOTHING on its own - see [`Edge::resolve`] -
    /// and exists only for the state layer, which uses it to clear a wait that
    /// agent owned.
    SubagentStop,
    /// Observes an MCP request; never supplies an answer or permission decision.
    Elicitation,
    /// Retires only a request identified by persisted state.
    ElicitationResult,
    /// No argument, or a word this version does not know.
    Unknown,
}

/// The dot, which is the whole point.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Glyph {
    Working,
    Waiting,
    Background,
    Idle,
}

/// What to paint, and how it has to be delivered - because neither mechanism
/// covers every edge.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Paint {
    /// A glyph beside the location, as one line of hook-protocol JSON.
    Line(Glyph),
    /// A visible state with background activity still known. Tmux must retain
    /// that fact when aging a working/waiting paint instead of falling to idle.
    LineWithBackground(Glyph),
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
            Paint::Line(g) | Paint::LineWithBackground(g) => Some(g),
            Paint::SessionStart => Some(Glyph::Idle),
            Paint::SessionEnd => None,
        }
    }

    pub fn background(self) -> bool {
        matches!(self, Paint::LineWithBackground(_) | Paint::Line(Glyph::Background))
    }
}


/// Net change in logical attention, independent of title painting or delivery.
/// See `docs/state-contract.md#logical-attention-transitions`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Transition {
    Entered,
    /// Both waiting states, or both non-waiting states, compare equal.
    Remained,
    Left,
    /// No recognised prior record was observed; never assume prior idle.
    Unknown,
}

/// An evaluated decision. A silent paint can still carry a known transition.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Resolved {
    pub paint: Option<Paint>,
    /// Describes the logical decision, not persistence or delivery success.
    /// No delivery consumer uses this yet.
    pub transition: Transition,
}

impl Resolved {
    pub fn stateless(paint: Paint) -> Resolved {
        Resolved { paint: Some(paint), transition: Transition::Unknown }
    }

    /// Compare the stored state BEFORE expiry with the decision AFTER expiry
    /// and the event. The optional paint must not determine either state.
    pub fn stateful(before: Option<bool>, after: bool, paint: Option<Paint>) -> Resolved {
        let transition = match (before, after) {
            (Some(false), true) => Transition::Entered,
            (Some(true), false) => Transition::Left,
            (Some(_), _) => Transition::Remained,
            (None, _) => Transition::Unknown,
        };
        Resolved { paint, transition }
    }
}

/// Every assertion that predates the transition asserts on the PAINT, in both
/// this module's tests and `state.rs`'s, and they say so with one token rather
/// than with a `map` that would bury what they are pinning. The transition has
/// its own tests, which name it.
#[cfg(test)]
pub(crate) trait JustPaint {
    fn paint(self) -> Option<Paint>;
}

#[cfg(test)]
impl JustPaint for Option<Resolved> {
    fn paint(self) -> Option<Paint> {
        self.and_then(|r| r.paint)
    }
}
impl Edge {
    pub fn parse(arg: Option<&OsStr>) -> Edge {
        match arg.map(OsStr::as_encoded_bytes) {
            Some(b"session-start") => Edge::SessionStart,
            Some(b"working") => Edge::Working,
            Some(b"waiting") => Edge::Waiting,
            Some(b"idle") => Edge::Idle,
            Some(b"notify") => Edge::Notify,
            Some(b"session-end") => Edge::SessionEnd,
            Some(b"subagent-stop") => Edge::SubagentStop,
            Some(b"elicitation") => Edge::Elicitation,
            Some(b"elicitation-result") => Edge::ElicitationResult,
            // An edge a later `hooks.json` adds, or no argument at all. Both
            // paint the IDLE form rather than nothing: a tab that keeps painting
            // is the forward-compatible choice, and it is pinned by the tests.
            _ => Edge::Unknown,
        }
    }

    /// Whether this edge's state depends on the payload. The other edges only
    /// drain stdin without parsing it when no state directory is configured.
    pub fn reads_payload(self) -> bool {
        matches!(
            self,
            Edge::Notify | Edge::SessionStart | Edge::Working | Edge::SubagentStop
                | Edge::Elicitation | Edge::ElicitationResult
        )
    }

    /// The decision of a run with no record: the paint below, and
    /// [`Transition::Unknown`], which is what "stateless" means here and not a
    /// gap to be filled in later.
    pub fn resolve(self, payload: &Payload) -> Option<Resolved> {
        Some(Resolved::stateless(self.paint(payload)?))
    }

    /// The edge plus its payload, resolved to a single decision. `None` means
    /// emit nothing at all - which is not an empty title, that would blank the
    /// tab - and it is reached before the location walk is ever spent.
    fn paint(self, payload: &Payload) -> Option<Paint> {
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
                if payload.agent_id().is_some() {
                    return None;
                }
                Some(Paint::Line(Glyph::Working))
            }
            Edge::Waiting => Some(Paint::Line(Glyph::Waiting)),
            Edge::Elicitation => {
                (payload.hook_event_name() == Some("Elicitation")
                    && payload.elicitation_mode_supported())
                    .then_some(Paint::Line(Glyph::Waiting))
            }
            // Without a persisted request there is nothing a response can
            // safely retire. In particular, do not blindly paint working.
            Edge::ElicitationResult => None,
            Edge::Idle | Edge::Unknown => Some(Paint::Line(Glyph::Idle)),
            Edge::Notify => match Notification::detect(payload) {
                Notification::IdlePrompt => Some(Paint::Line(Glyph::Idle)),
                Notification::Waiting => Some(Paint::Line(Glyph::Waiting)),
                Notification::Other => None,
            },
            Edge::SessionStart => {
                // Compaction is a mid-turn SessionStart, not a new idle session.
                // The hook matcher excludes it too; parsed metadata is a backstop.
                if payload.source() == Some("compact") {
                    return None;
                }
                Some(Paint::SessionStart)
            }
            Edge::SessionEnd => Some(Paint::SessionEnd),
            // Stateless, a subagent finishing is not a state change at all: it
            // must not read as the session going idle, and it fires for the
            // internal compaction summarizer too. With no state directory this
            // edge is therefore a complete no-op, which is what makes registering
            // the hook safe on a machine that has nowhere to keep a record.
            Edge::SubagentStop => None,
        }
    }
}

/// What a Notification is telling us, which is a three-way and the third branch
/// is silence.
pub enum Notification {
    /// The quiet-turn nudge, fired ~60s after a turn ends. It is the one kind
    /// that MUST NOT paint waiting - it arrives after EVERY quiet turn end, so
    /// mapping it to waiting would turn every idle tab orange a minute later and
    /// collapse two of the three states into one.
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

const WAITING_KINDS: [&str; 5] = [
    "permission_prompt",
    "worker_permission_prompt",
    "agent_needs_input",
    "elicitation_dialog",
    "elicitation_url_dialog",
];

impl Notification {
    pub fn detect(payload: &Payload) -> Notification {
        match payload.notification_type() {
            Some("idle_prompt") => Notification::IdlePrompt,
            Some(kind) if WAITING_KINDS.contains(&kind) => Notification::Waiting,
            _ => Notification::Other,
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
        assert_eq!(edge("subagent-stop"), Edge::SubagentStop);
        assert_eq!(edge("elicitation"), Edge::Elicitation);
        assert_eq!(edge("elicitation-result"), Edge::ElicitationResult);
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
        assert_eq!(Edge::Unknown.resolve(&p).paint(), Some(Paint::Line(Glyph::Idle)));
        assert_eq!(Edge::Idle.resolve(&p).paint(), Some(Paint::Line(Glyph::Idle)));
    }

    #[test]
    fn only_the_edges_with_a_discriminator_parse_stateless_payloads() {
        assert!(Edge::Notify.reads_payload());
        assert!(Edge::SessionStart.reads_payload());
        assert!(Edge::Working.reads_payload());
        assert!(Edge::SubagentStop.reads_payload());
        assert!(Edge::Elicitation.reads_payload());
        assert!(Edge::ElicitationResult.reads_payload());
        assert!(!Edge::Waiting.reads_payload());
        assert!(!Edge::Idle.reads_payload());
        assert!(!Edge::SessionEnd.reads_payload());
        assert!(!Edge::Unknown.reads_payload());
    }

    #[test]
    fn session_end_paints_no_glyph_and_session_start_paints_idle() {
        let p = Payload::empty();
        assert_eq!(Edge::SessionEnd.resolve(&p).paint(), Some(Paint::SessionEnd));
        assert_eq!(Paint::SessionEnd.glyph(), None);
        assert_eq!(Edge::SessionStart.resolve(&p).paint(), Some(Paint::SessionStart));
        assert_eq!(Paint::SessionStart.glyph(), Some(Glyph::Idle));
    }

    #[test]
    fn a_compacting_session_start_paints_nothing() {
        for line in [
            &br#"{"source":"compact"}"#[..],
            &br#"{"source": "compact"}"#[..],
        ] {
            assert_eq!(Edge::SessionStart.resolve(&payload(line)).paint(), None);
        }
        // Every valid JSON whitespace spelling has the same meaning.
        let p = payload(br#"{"source":  "compact"}"#);
        assert_eq!(Edge::SessionStart.resolve(&p).paint(), None);
    }

    #[test]
    fn a_subagent_stop_paints_nothing_without_a_state_directory() {
        let p = payload(br#"{"agent_id":"abc","hook_event_name":"SubagentStop"}"#);
        assert_eq!(Edge::SubagentStop.resolve(&p).paint(), None);
        assert_eq!(Edge::SubagentStop.resolve(&Payload::empty()).paint(), None);
    }

    #[test]
    fn stateless_elicitation_observes_requests_but_cannot_resolve_them() {
        for mode in [None, Some("form"), Some("url")] {
            let p = payload(serde_json::json!({"hook_event_name":"Elicitation","mode":mode}).to_string().as_bytes());
            assert_eq!(Edge::Elicitation.resolve(&p).paint(), Some(Paint::Line(Glyph::Waiting)));
            assert_eq!(Edge::ElicitationResult.resolve(&p).paint(), None);
        }
        for p in [Payload::empty(), payload(br#"{"hook_event_name":"Elicitation","mode":"unknown"}"#), payload(br#"{"hook_event_name":"ElicitationResult","action":"accept"}"#)] {
            assert_eq!(Edge::Elicitation.resolve(&p).paint(), None);
            assert_eq!(Edge::ElicitationResult.resolve(&p).paint(), None);
        }
    }

    #[test]
    fn a_subagent_working_edge_paints_nothing() {
        let p = payload(br#"{"hook_event_name":"PostToolUse","agent_id":"abc"}"#);
        assert_eq!(Edge::Working.resolve(&p).paint(), None);
        let p = payload(br#"{"hook_event_name":"PostToolUse"}"#);
        assert_eq!(Edge::Working.resolve(&p).paint(), Some(Paint::Line(Glyph::Working)));
        // An empty value must not silence the edge for the whole session.
        let p = payload(br#"{"agent_id":""}"#);
        assert_eq!(Edge::Working.resolve(&p).paint(), Some(Paint::Line(Glyph::Working)));
    }

    #[test]
    fn every_waiting_kind_paints_orange() {
        for kind in WAITING_KINDS {
            let line = format!(r#"{{"notification_type": "{kind}"}}"#).into_bytes();
            assert_eq!(
                Edge::Notify.resolve(&payload(&line)).paint(),
                Some(Paint::Line(Glyph::Waiting)),
                "{}",
                kind
            );
        }
    }

    #[test]
    fn nested_text_cannot_override_a_kind_and_duplicate_kinds_are_rejected() {
        let p = payload(
            br#"{"notification_type":"permission_prompt","x":"\"notification_type\":\"idle_prompt\""}"#,
        );
        // The escaped copy cannot match, so this one is orange.
        assert_eq!(Edge::Notify.resolve(&p).paint(), Some(Paint::Line(Glyph::Waiting)));
        assert!(Payload::from_bytes(
            br#"{"notification_type":"permission_prompt","also":1,"notification_type":"idle_prompt"}"#,
        ).is_err());
    }

    #[test]
    fn an_unrecognised_kind_is_silent() {
        for line in [
            &br#"{"notification_type":"agent_completed"}"#[..],
            &br#"{"notification_type":"xpermission_prompt"}"#[..],
            &b"{}"[..],
            &b""[..],
        ] {
            assert_eq!(
                Edge::Notify.resolve(&payload(line)).paint(),
                None,
                "{}",
                String::from_utf8_lossy(line)
            );
        }
    }

    #[test]
    fn a_kind_after_a_large_message_still_counts() {
        // `notification_type` is serialized after an unbounded `message`.
        let mut line = br#"{"message":""#.to_vec();
        line.resize(9000, b'm');
        line.extend_from_slice(br#"","notification_type":"agent_needs_input"}"#);
        line.push(b'\n');
        assert_eq!(
            Edge::Notify.resolve(&payload_with_newline(&line)).paint(),
            Some(Paint::Line(Glyph::Waiting))
        );
    }

    /// The other half of #14's seam: a run with no record cannot compare, and
    /// `Unknown` is that answer rather than a guess at one. Every stateless edge
    /// that paints reports it, including session end without a record and an edge word this version
    /// does not know.
    #[test]
    fn the_stateless_path_reports_an_unknown_transition() {
        let p = Payload::empty();
        for edge in [
            Edge::Waiting,
            Edge::Working,
            Edge::Idle,
            Edge::SessionStart,
            Edge::SessionEnd,
            Edge::Unknown,
        ] {
            assert_eq!(
                edge.resolve(&p).map(|r| r.transition),
                Some(Transition::Unknown),
                "{edge:?}"
            );
        }
    }

    // Test helpers: a Payload built from bytes rather than from stdin.
    fn payload(line: &[u8]) -> Payload {
        Payload::from_bytes(line).unwrap()
    }

    fn payload_with_newline(bytes: &[u8]) -> Payload {
        Payload::from_bytes(bytes).unwrap()
    }
}
