//! The multiplexer axis: a RETAINED-MODE renderer, and the supplier of the leaf's
//! channel.
//!
//! A leaf terminal is immediate mode - write bytes, they latch. tmux is not
//! another terminal: it re-renders the outer tab ON A TIMER from a format this
//! program installed earlier, which is the only reason the TTL decay in
//! [`tmux`] can exist at all, and no leaf can do it. It also owns a key-value
//! store, an attach event and a registry of the ptys its clients are attached to
//! - and that last one is the CHANNEL the leaf's own appearance bytes have to
//! travel on, because inside a pane the leaf never sees them. So the multiplexer
//! is neither a peer of the leaf nor a layer above it in the same sense: it is
//! `Option<Mux>` that both renders and DELIVERS.
//!
//! GNU screen is in the same axis and has none of those powers. It is detected
//! for exactly one purpose - `$STY` SUPPRESSES the leaf's environment evidence,
//! because `KONSOLE_*` and `ITERM_SESSION_ID` leak into every process a
//! multiplexer ever started - and drives nothing. Hence [`MuxCaps::renders_title`]
//! is `false` for it, and hence the routing test is the CAP and never
//! `mux.is_some()`: the corpus case `pty-session-start-konsole-in-screen` expects
//! a plain `ESC]0;<title>BEL` on the pty, and a presence test would hand it a
//! tmux carrier instead.
//!
//! [`route`] is the one function that decides who owns the title and whose channel
//! the leaf's appearance bytes ride. Before it existed the same decision was two
//! COMPLEMENTARY predicates in two files - `emit.rs` asked
//! `Konsole && tmux.is_none()` while `tmux::arm_konsole` asked
//! `surface != Konsole` and left the tmux half to a guard inside `to_clients` -
//! and either one lifted out alone arms the pane unconditionally, ON TOP of the
//! other's arm. A double arm is invisible to the corpus, whose pty cases and
//! whose tmux cases are disjoint sets. One total condition, in one place, is the
//! only shape that cannot do that.
//!
//! The third thing that decision has to answer is not about the stack at all.
//! Whether a channel can carry a MESSAGE is a property of the message: a title
//! travels everywhere, and raw appearance bytes travel only where there is a byte
//! route to write them to. Claude Code's `terminalSequence` allowlist refuses OSC
//! 50, and where the session's tab is a console rather than a pty
//! ([`sys::HAS_SESSION_TTY`] false) `sys::set_session_title` is the entire route
//! and it carries a TITLE. So on such a platform the arming has NO channel, and
//! that falls out of [`Channel::carries_raw`] making [`route`] return `None` -
//! never out of a `cfg` test in a backend.

pub mod tmux;

use crate::armed::Armed;
use crate::config;
use crate::edge::Paint;
use crate::support::{Presence, Support, YES};
use crate::surface::{self, Elide, Surface};
use crate::sys;

/// Which multiplexer - the key its capability row is looked up by, the way
/// [`Surface`] looks up its own.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum MuxKind {
    Tmux,
    Screen,
}

/// The multiplexer this process is running inside, when there is one.
///
/// `Screen` carries no handle because there is nothing to hold: screen has no
/// key-value store, no client registry a format can read and no title carrier
/// this design can use. It exists in the enum so that `$STY` has somewhere to be
/// ANSWERED rather than tested, and so that `renders_title: false` is a row
/// rather than an `if`.
pub enum Mux {
    Tmux(tmux::Tmux),
    Screen,
}

/// What a multiplexer can do FOR us, as one `const` per kind.
///
/// Only the two powers this commit's routing reads are here. The timer, the
/// key-value store and the attach event are equally real and equally load-bearing
/// - they are what `set-titles-string`, `@cctab_*` and `client-attached` already
/// use - but a row whose reader does not exist yet is a row no test can be wrong
/// about, so each arrives with the commit that reads it.
pub struct MuxCaps {
    /// Does this layer draw the OUTER tab itself? tmux: yes, from a format on its
    /// own timer, which is why our OSC 0 inside a pane becomes a RECORD instead
    /// of a title. screen: no.
    ///
    /// This is invariant I1's test - exactly one layer owns the title and it is
    /// the outermost RENDERER - and it is why `$STY` is a detection suppressor and
    /// nothing more.
    pub renders_title: bool,
    /// Can it name the pty of every client attached to our session? That registry
    /// IS `Channel::Clients`, and the reason the arming does not need tmux's
    /// `allow-passthrough`: passthrough would let any program in any pane write
    /// arbitrary bytes to the user's terminal, which is a grant a tab-title plugin
    /// has no business asking for.
    pub client_registry: Presence,
}

const TMUX: MuxCaps = MuxCaps {
    renders_title: true,
    client_registry: YES,
};

const SCREEN: MuxCaps = MuxCaps {
    renders_title: false,
    client_registry: Support::Unsupported("screen names no client's pty in a format"),
};

const fn caps_of(kind: MuxKind) -> &'static MuxCaps {
    match kind {
        MuxKind::Tmux => &TMUX,
        MuxKind::Screen => &SCREEN,
    }
}

/// What the environment says about a multiplexer, which is TWO facts and not one.
///
/// Conflating them changes what paints, measurably: `$TMUX=nonsense` and
/// `CCTAB_NO_TMUX=1` both leave us with NO multiplexer to drive, while both still
/// mean a multiplexer swallowed the leaf's environment evidence - a stale
/// `KONSOLE_VERSION` inherited through the server says nothing about the terminal
/// drawing the tab either way. Deriving the veto from `mux.is_some()` would turn
/// the nine `konsole-*-{tmux,sty,tmuxsty}` corpus cases and `tmux-kill-switch`
/// into Konsole sessions.
pub struct MuxEnv {
    /// The multiplexer this process can drive, or why it cannot. `Disabled`
    /// names our own kill switch, which is what lets doctor print it.
    pub mux: Support<Option<Mux>>,
    /// Was a multiplexer's environment present AT ALL - `$TMUX` or `$STY`, set and
    /// non-empty, whatever it holds and whatever we were told to do about it?
    /// This one bool IS the `!flag("TMUX") && !flag("STY")` conjunction that used
    /// to sit inside the Konsole detector, and it is the only thing the leaf axis
    /// is told about multiplexers.
    pub swallows_leaf_evidence: bool,
}

impl Mux {
    pub fn kind(&self) -> MuxKind {
        match self {
            Mux::Tmux(_) => MuxKind::Tmux,
            Mux::Screen => MuxKind::Screen,
        }
    }

    pub fn caps(&self) -> &'static MuxCaps {
        caps_of(self.kind())
    }

    /// Step 1 of [`resolve`]: the environment only, EXECUTING NOTHING.
    ///
    /// The kill switch is read here and the veto above it, in that order and not
    /// the other: `CCTAB_NO_TMUX` backs the tmux slice out of the way, and it was
    /// never a claim that the environment's `KONSOLE_*` became trustworthy again.
    pub fn probe() -> MuxEnv {
        let swallows_leaf_evidence = config::flag("TMUX") || config::flag("STY");
        if config::flag("CCTAB_NO_TMUX") {
            return MuxEnv {
                mux: Support::Disabled("CCTAB_NO_TMUX"),
                swallows_leaf_evidence,
            };
        }
        // tmux wins over `$STY` when both are set: the corpus case
        // `tmux-and-sty-together-is-tmux` pins it, and the reason is that only one
        // of the two can be the layer actually drawing the tab, and only one of
        // them can draw it at all.
        let mux = match tmux::Tmux::detect() {
            Some(t) => Some(Mux::Tmux(t)),
            None if config::flag("STY") => Some(Mux::Screen),
            None => None,
        };
        MuxEnv {
            mux: Support::Available(mux),
            swallows_leaf_evidence,
        }
    }
}

/// What the multiplexer already knows about the leaf, asked BEFORE the leaf is
/// resolved.
///
/// A GENERIC bound and never a trait object, because the hot path passes
/// [`NoOracle`] - a zero-sized type whose one answer is a constant - so the
/// question inlines to nothing and provably forks nothing. That turns the zero
/// `tests/run.sh` pins ("the hot edges exec no tmux at all") into a TYPE fact: a
/// hot edge cannot reach an exec without naming a different oracle.
///
/// An answer that is not `Available` means "I have no evidence" - `Unsupported`
/// for a multiplexer that cannot know, `Disabled` for an oracle we chose not to
/// ask - and never "there is no leaf". The multiplexer is the only source of leaf
/// identity in the one topology that matters - Konsole, ssh, tmux, where
/// `KONSOLE_*` did not survive the hop and only `tmux list-clients` can see the
/// outer terminal - which is
/// [#18](https://github.com/dalf/claude-tabstatus/issues/18), and when it merges
/// its probe becomes the body of an oracle that execs rather than a second call
/// site.
pub trait MuxOracle {
    fn leaf_hint(&mut self) -> Support<Surface>;
}

/// The hot path's oracle. A ZST; the answer is a constant.
pub struct NoOracle;

impl MuxOracle for NoOracle {
    fn leaf_hint(&mut self) -> Support<Surface> {
        Support::Disabled("the hot path does not ask the multiplexer")
    }
}

/// The resolved stack, computed ONCE per process.
///
/// It adds no allocation over the two fields it replaces: [`Surface`] is `Copy`
/// and `Option<Mux>` is the same `Option<OsString>` `Tmux::detect` already
/// allocated.
pub struct Stack {
    pub mux: Option<Mux>,
    pub leaf: Surface,
    /// Derived from `leaf` LAST, in [`resolve`], so a leaf verdict that arrives
    /// from the multiplexer cannot leave the layout computed against a different
    /// one. That staleness is the shape of #18's defect.
    pub elide: Elide,
}

impl Stack {
    /// Invariant I1's test. The CAP, never `mux.is_some()`.
    pub fn renders_title(&self) -> bool {
        self.mux.as_ref().is_some_and(|m| m.caps().renders_title)
    }

    fn has_clients(&self) -> bool {
        self.mux
            .as_ref()
            .is_some_and(|m| m.caps().client_registry.is_available())
    }

    /// The tmux handle, for a caller [`route`] has already sent to
    /// `Channel::Clients`. This is how the handle is REACHED, not a second test
    /// of presence: `Channel::Clients` cannot be produced without a tmux, and
    /// nothing below decides anything from the `None`.
    pub fn tmux(&self) -> Option<&tmux::Tmux> {
        match &self.mux {
            Some(Mux::Tmux(t)) => Some(t),
            _ => None,
        }
    }
}

/// THE resolution order, and the only place it exists:
///
///   1. probe the multiplexer from the environment - no exec
///   2. ask it for leaf evidence - MAY exec
///   3. resolve the leaf: override ▸ mux's word ▸ env probes ▸ `Unknown`
///   4. derive `elide` from the FINAL leaf, exactly once
///
/// The outer layer goes first because it may be the ONLY source of leaf identity,
/// and nothing derived from the leaf exists before step 4 runs - so there is no
/// window in which a layout was computed against a leaf we later revise.
pub fn resolve<O: MuxOracle>(ask: &mut O) -> Stack {
    let env = Mux::probe();
    let hint = ask.leaf_hint().ok();
    let leaf = surface::resolve_leaf(hint, env.swallows_leaf_evidence);
    Stack {
        mux: env.mux.ok().flatten(),
        leaf,
        elide: leaf.caps().elide,
    }
}

/// Where bytes go. Not a property of the stack but of the MESSAGE: one edge can
/// need two of these at once, which is the whole reason the arming and the title
/// stopped being one decision.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Channel {
    /// One line of hook-protocol JSON on stdout, and Claude Code emits the bytes.
    /// The allowlist channel: only notification and title OSCs travel here, so
    /// Konsole's OSC 50 never can.
    Protocol,
    /// The session's own tab, reached through `emit::write_session` - the pty
    /// resolved from `CLAUDE_PID` where there is one, and the console's title
    /// where there is not.
    Direct,
    /// The pty of every client attached to our multiplexer session - the OUTER
    /// terminal, reached through `list-clients -F '#{client_tty}'` plus
    /// `sys::write_tty`, and deliberately NOT through tmux's `allow-passthrough`.
    Clients,
}

impl Channel {
    /// Can this channel carry RAW bytes - anything that is not a tab title?
    ///
    /// The Konsole arming is OSC 50, and this is the question it has to ask that
    /// no amount of looking at the stack can answer. `Protocol` is Claude Code's
    /// `terminalSequence` allowlist, which passes title and notification OSCs and
    /// silently drops the rest. `Direct` is a byte route only where the session's
    /// tab IS a pty: where it is a console instead, `sys::set_session_title` is
    /// the whole of the route and what it carries is a TITLE, so there is no
    /// arming to be sent and no `cfg` test in any backend that says so.
    const fn carries_raw(self) -> bool {
        match self {
            Channel::Protocol => false,
            Channel::Direct => sys::HAS_SESSION_TTY,
            Channel::Clients => true,
        }
    }
}

/// The leaf's appearance bytes: WHOSE they are, and where they go.
///
/// The surface travels WITH the channel because the two are one decision and they
/// are made from different evidence: the channel is a fact about the stack HERE
/// AND NOW, while the surface is what was ARMED, which only a record can say (see
/// [`crate::armed`]). Splitting them let `session_end` take the channel from the
/// route and the bytes from `cfg.stack.leaf` - a restore composed half from a
/// record and half from the end hook's environment, which is the defect wearing a
/// disguise.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Appearance {
    pub channel: Channel,
    /// The surface whose [`crate::surface::Arming`] these bytes come from.
    pub surface: Surface,
}

/// Who writes what, for ONE paint.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Route {
    pub title: Channel,
    /// The leaf's appearance bytes - Konsole's tab-title formats today - or `None`
    /// when this paint has none to send, or nowhere to send them.
    pub appearance: Option<Appearance>,
}

impl Route {
    /// The surface whose appearance bytes ride `channel` on this paint, or `None`.
    ///
    /// Every caller that used to ask `route.appearance == Some(Channel::X)` and
    /// then reach for `cfg.stack.leaf.caps().arming` asks this instead, so the
    /// channel test and the choice of bytes cannot come from two different
    /// answers.
    pub fn arms(&self, channel: Channel) -> Option<Surface> {
        self.appearance
            .filter(|a| a.channel == channel)
            .map(|a| a.surface)
    }
}

/// THE ONE PLACE that decides who writes what.
///
/// Both halves are written as a `match` over the whole cross product rather than
/// as a ladder of early returns, because what this replaced was a CONJUNCTION
/// spread over two files that no single reader could see: `emit.rs` armed the pane
/// on `Konsole && tmux.is_none()` and `tmux::arm_konsole` armed the clients on
/// `surface == Konsole` with the tmux half hidden inside `to_clients`. A total
/// match is the shape in which "outside tmux, and again inside it" cannot be
/// written by accident.
///
/// The SURFACE is an argument and not `stack.leaf`, and that is this commit: what
/// is armed at SessionStart is the leaf the environment named, and what is
/// restored at SessionEnd is whatever [`crate::armed`] read back out of a store.
/// Re-deriving the second from the environment is the defect; taking both from one
/// argument is the repair. The surface is asked for `arming` and never for its
/// NAME, so a surface that grows an arming later needs no edit here.
pub fn route(stack: &Stack, paint: Paint, armed: Armed) -> Route {
    let title = match (paint, stack.renders_title()) {
        // session start and end write the tab themselves whatever is above them:
        // `terminalSequence` cannot carry either - SessionStart is too early for
        // the TUI writer to be mounted - and inside tmux the pty write is what
        // sets the carrier the outer format reads back.
        (Paint::SessionStart | Paint::SessionEnd, _) => Channel::Direct,
        (Paint::Line(_) | Paint::LineWithBackground(_), true) => Channel::Direct,
        (Paint::Line(_) | Paint::LineWithBackground(_), false) => Channel::Protocol,
    };
    // WHOSE bytes is the record's answer, never the leaf's: at SessionStart the
    // two are the same thing, and at SessionEnd they are the whole bug.
    let surface = armed.surface();
    let arms = surface.is_some_and(|s| s.caps().arming.is_some());
    let channel = match (paint, arms, stack.has_clients()) {
        // A painting edge arms nothing: the arming is what makes the tab able to
        // SHOW a title, and it is paired with the restore across the session.
        (Paint::Line(_) | Paint::LineWithBackground(_), _, _) => None,
        (_, false, _) => None,
        // Inside a pane our OSC 50 is swallowed - this pane is not the tab - so
        // the bytes go to the terminals the multiplexer names.
        (_, true, true) => Some(Channel::Clients),
        (_, true, false) => Some(Channel::Direct),
    }
    // Delivery is the MESSAGE's property, and this is where it is applied: a
    // channel that cannot carry raw bytes carries no arming, so the answer is
    // `None` and every backend below reads the same `None`.
    .filter(|c| c.carries_raw());
    let appearance = channel
        .zip(surface)
        .map(|(channel, surface)| Appearance { channel, surface });
    Route { title, appearance }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::edge::Glyph;

    /// Rung 3, which is what every caller below is asserting about unless it says
    /// otherwise: the leaf this hook's environment named, labelled an assumption.
    fn as_leaf(st: &Stack) -> Armed {
        Armed::assumed(st.leaf)
    }

    fn stack(mux: Option<Mux>, leaf: Surface) -> Stack {
        Stack {
            mux,
            leaf,
            elide: leaf.caps().elide,
        }
    }

    fn tmux_stack(leaf: Surface) -> Stack {
        stack(Some(Mux::Tmux(tmux::Tmux::for_test())), leaf)
    }

    const PAINTS: [Paint; 4] = [
        Paint::SessionStart,
        Paint::SessionEnd,
        Paint::Line(Glyph::Idle),
        Paint::LineWithBackground(Glyph::Working),
    ];

    /// I1, as a property of the rows rather than of the routing: screen is in the
    /// axis so that `$STY` can suppress leaf evidence, and for nothing else.
    #[test]
    fn only_tmux_renders_the_outer_title() {
        assert!(caps_of(MuxKind::Tmux).renders_title);
        assert!(!caps_of(MuxKind::Screen).renders_title);
        assert!(caps_of(MuxKind::Tmux).client_registry.is_available());
        assert!(!caps_of(MuxKind::Screen).client_registry.is_available());
        assert!(caps_of(MuxKind::Screen).client_registry.reason().is_some());
        assert!(Mux::Screen.kind() == MuxKind::Screen);
        assert!(Mux::Tmux(tmux::Tmux::for_test()).kind() == MuxKind::Tmux);
    }

    /// The conjunction the two old predicates made between them, as one table.
    /// Read it as: Konsole arms on every stack, and the multiplexer decides only
    /// WHERE - which is exactly what `emit.rs`'s `Konsole && tmux.is_none()` plus
    /// `arm_konsole`'s `surface == Konsole` plus `to_clients`'s hidden tmux gate
    /// added up to, and a double arm would show up here as an arm with no stack
    /// that lacks one.
    #[test]
    fn the_arming_channel_is_the_two_old_predicates_in_one_place() {
        // Outside a multiplexer the arming rides the session's own tab, which is a
        // byte route only where that tab is a pty. It is the SURFACE that is
        // asserted now, because who owns the bytes and where they go are one
        // answer.
        let own_tab = sys::HAS_SESSION_TTY.then_some(Surface::Konsole);
        for paint in [Paint::SessionStart, Paint::SessionEnd] {
            // No multiplexer: the session's own pty, as `emit.rs` did.
            let st = stack(None, Surface::Konsole);
            let r = route(&st, paint, as_leaf(&st));
            assert!(r.arms(Channel::Direct) == own_tab);
            // screen is not a renderer and names no client, so the leaf's own pty
            // is still the outer terminal. `mux.is_some()` would have sent these
            // bytes to a client registry that does not exist.
            let st = stack(Some(Mux::Screen), Surface::Konsole);
            let r = route(&st, paint, as_leaf(&st));
            assert!(r.arms(Channel::Direct) == own_tab);
            // tmux: the attached clients' ptys, as `arm_konsole` did.
            let st = tmux_stack(Surface::Konsole);
            let r = route(&st, paint, as_leaf(&st));
            assert!(r.arms(Channel::Clients) == Some(Surface::Konsole));
            // A leaf with no appearance bytes arms nowhere, on any stack.
            for s in [Surface::Unknown, Surface::WezTerm, Surface::WindowsTerminal] {
                for st in [stack(None, s), tmux_stack(s)] {
                    assert!(route(&st, paint, as_leaf(&st)).appearance.is_none());
                }
            }
        }
    }

    /// I3, as a property of the CHANNEL and not of any platform branch. The
    /// allowlist refuses OSC 50 on the protocol channel everywhere; the session's
    /// own tab carries it only where that tab is a pty, which is the same const
    /// `emit::write_session` routes on. A `#[cfg(windows)]` anywhere below would
    /// be the same fact stated twice, and the two copies would drift.
    #[test]
    fn a_channel_that_cannot_carry_raw_bytes_carries_no_arming() {
        assert!(!Channel::Protocol.carries_raw());
        assert!(Channel::Clients.carries_raw());
        assert_eq!(Channel::Direct.carries_raw(), sys::HAS_SESSION_TTY);
        // And the routing says so rather than the caller: with no byte route to
        // the session's own tab there is no arming to send, on the one stack that
        // would otherwise have asked for one.
        let st = stack(None, Surface::Konsole);
        let r = route(&st, Paint::SessionStart, as_leaf(&st));
        assert_eq!(r.appearance.is_some(), sys::HAS_SESSION_TTY);
    }

    /// A painting edge has no appearance bytes at all - the arming is paired with
    /// the restore across the whole session, so a per-tool-call edge that armed
    /// would be an arm with no matching un-arm, this project's named defect.
    #[test]
    fn a_painting_edge_arms_nothing_anywhere() {
        for paint in [Paint::Line(Glyph::Working), Paint::LineWithBackground(Glyph::Waiting)] {
            for st in [
                stack(None, Surface::Konsole),
                stack(Some(Mux::Screen), Surface::Konsole),
                tmux_stack(Surface::Konsole),
            ] {
                assert!(route(&st, paint, as_leaf(&st)).appearance.is_none());
            }
        }
    }

    /// The title is the mux's when the mux renders one, and the hook protocol's
    /// when nothing above the leaf does. `Clients` is never a title channel of
    /// OURS: inside tmux the outer title is tmux's own write, from the format
    /// SessionStart installed, and what we send to the pane is the carrier.
    #[test]
    fn the_title_channel_is_the_renderer_and_never_the_client_registry() {
        for leaf in [Surface::Unknown, Surface::Konsole] {
            for paint in PAINTS {
                for st in [
                    stack(None, leaf),
                    stack(Some(Mux::Screen), leaf),
                    tmux_stack(leaf),
                ] {
                    let want = match (paint, st.renders_title()) {
                        (Paint::SessionStart | Paint::SessionEnd, _) => Channel::Direct,
                        (_, true) => Channel::Direct,
                        (_, false) => Channel::Protocol,
                    };
                    assert!(route(&st, paint, as_leaf(&st)).title == want);
                    assert!(route(&st, paint, as_leaf(&st)).title != Channel::Clients);
                }
            }
        }
    }

    /// `Channel::Clients` can only ever be produced by a stack that HAS a client
    /// registry, which is what lets `tmux::session_end` reach for the handle
    /// instead of re-testing the multiplexer.
    #[test]
    fn nothing_is_routed_to_clients_without_a_client_registry() {
        for leaf in [Surface::Unknown, Surface::Konsole] {
            for paint in PAINTS {
                for st in [stack(None, leaf), stack(Some(Mux::Screen), leaf)] {
                    assert!(route(&st, paint, as_leaf(&st)).arms(Channel::Clients).is_none());
                }
                assert!(tmux_stack(leaf).tmux().is_some());
            }
        }
        assert!(stack(Some(Mux::Screen), Surface::Konsole).tmux().is_none());
        assert!(stack(None, Surface::Konsole).tmux().is_none());
    }

    /// Step 4 of `resolve`, as a property: the layout follows the leaf that was
    /// finally decided, never the one the environment first suggested.
    #[test]
    fn elide_is_derived_from_the_leaf_that_won() {
        for s in Surface::ALL {
            assert!(stack(None, *s).elide == s.caps().elide);
        }
    }

    /// The hot path's oracle answers without asking anything, and says whose
    /// decision that is.
    #[test]
    fn the_hot_paths_oracle_is_a_constant() {
        assert_eq!(std::mem::size_of::<NoOracle>(), 0);
        let answer = NoOracle.leaf_hint();
        assert_eq!(answer.label(), "off");
        assert!(!answer.is_available());
        assert!(answer.ok().is_none());
    }
}
