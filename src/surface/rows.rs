//! The fourteen capability rows, one `const` each.
//!
//! Every claim here comes from `docs/research/terminal-capability-matrix.md`,
//! which cites the vendor source file and line for each one. Nothing in this file
//! is computed and nothing in it branches: a row is DATA, so a surface nobody can
//! run here is still readable, testable and printable from a Linux `cargo test`.
//!
//! Two rules govern what a row is allowed to say.
//!
//! `Unsupported` is for what the terminal CANNOT do with bytes - including a
//! capability it has behind a settings file, a remote-control socket or an
//! extension API, because none of those is a byte this program can write.
//! `Unverifiable` is for what it may or may not do with THEIR setting deciding
//! and nothing we can read telling us which: `profiles.suppressApplicationTitle`
//! silently discards OSC 0, VS Code's `pushTitle` defaults to absent, and almost
//! every terminal's BEL becomes urgency only if its own configuration says so.
//!
//! And `elide` is `Right` in every row but two. Konsole's `Left` is the one
//! measured exception, and `Unknown`'s is an admission; that is what keeps every
//! `CCTAB_TERMINAL` value other than `konsole` on the layout it has today.

use super::{
    Arming, AttentionCaps, CapSource, Elide, MinimumVersion, NotifySyntax, ProgressSyntax, Protocol,
    SurfaceCaps, TabColor, Terminator, TitleCaps,
};
use crate::support::{Presence, Support, YES};

/// The five rows where all four title sequences work. `OSC 0` and `OSC 2` have no
/// `N` in any row of the matrix - all fifteen terminals surveyed implement both -
/// so only the exceptions are spelled out, one row at a time. Deliberately used
/// whole and never with `..TITLE_ALL`: a functional update would move three fields
/// out of this const and DROP the fourth, and const evaluation cannot run a
/// destructor.
const TITLE_ALL: TitleCaps = TitleCaps {
    osc0: YES,
    osc1: YES,
    osc2: YES,
    stack_22t: YES,
};

/// The inbound half of attention, which no surface can offer: a one-shot hook has
/// nowhere to read a reply. fd 0 is /dev/null, `exec 3>/dev/tty` fails, and the
/// pty's input side is owned by Claude Code's TUI in raw mode - so DA1, XTVERSION
/// and DECRQSS are all unavailable and capability detection can never be dynamic
/// in this process model.
const NO_ACK: Presence = Support::Unsupported("a one-shot hook has no reader");

/// Nothing said anything, so nothing is claimed beyond the one sequence every
/// terminal in the survey implements.
///
/// This is what ssh looks like: `KONSOLE_*` and its equivalents are not forwarded
/// by any stock ssh config, and `CCTAB_TERMINAL` is the only way a leaf is
/// knowable in that topology. `arming: None` is the load-bearing part - appearance
/// bytes are never written to a terminal that cannot be named.
pub const UNKNOWN: SurfaceCaps = SurfaceCaps {
    name: "unknown",
    human: "an unidentified terminal",
    elide: Elide::Unknown,
    title: TitleCaps {
        osc0: YES,
        osc1: Support::Unsupported("nothing beyond OSC 0 is assumed of a terminal we cannot name"),
        osc2: YES,
        stack_22t: Support::Unsupported(
            "nothing beyond OSC 0 is assumed of a terminal we cannot name",
        ),
    },
    tab_color: Protocol::new(&Support::Unsupported("a colour cannot be restored on a terminal we cannot name")),
    attention: AttentionCaps {
        bell: Support::Unsupported("no bell behaviour is assumed of a terminal we cannot name"),
        notify: Protocol::new(&Support::Unsupported("the three notification grammars do not overlap, so an \
                                      unnamed surface has no safe one")),
        progress: Protocol::new(&Support::Unsupported("OSC 9 means two unrelated things, so an unnamed surface \
                                        has no safe one")),
        acknowledge: NO_ACK,
    },
    arming: None,
    source: CapSource::Inferred,
};

/// Konsole's per-tab title format, set to "the title the shell sent" and back to
/// Konsole's COMPILED-IN defaults. Named here rather than spelled at each use,
/// because inside tmux the same two byte strings go to a tmux CLIENT's pty
/// instead of to this pane - and an arming with no matching restore is this
/// project's named defect.
///
/// SEAM: TabColor=#RRGGBB rides in this same property list - and whoever adds it
/// must add TabColor=#000000 to the restore, or the colour outlives the session.
///
/// The `tab_color` row below is that seam's other half: `OSC 34` is the direct
/// route and needs no profile round trip.
///
/// The catalogue uses the oldest grammar for each effect. Its version floors
/// live beside the syntax below and are resolved only for capability reporting.
/// Nothing emits these three protocols yet.
pub const KONSOLE: SurfaceCaps = SurfaceCaps {
    name: "konsole",
    human: "Konsole",
    // QTabBar::setElideMode(Qt::ElideLeft) at a hardcoded call site, which no
    // config key reads.
    elide: Elide::Left,
    title: TitleCaps {
        osc0: YES,
        osc1: YES,
        osc2: YES,
        stack_22t: Support::Unsupported(
            "parsed and deliberately discarded, Vt102Emulation.cpp:2054",
        ),
    },
    tab_color: Protocol::since(
        &Support::Available(TabColor::Osc34(Terminator::Bel)),
        MinimumVersion::Konsole(241200),
    ),
    attention: AttentionCaps {
        bell: Support::Unverifiable("the profile's bell mode decides whether it is seen"),
        notify: Protocol::since(
            &Support::Available(NotifySyntax::Osc777(Terminator::Bel)),
            MinimumVersion::Konsole(230400),
        ),
        progress: Protocol::since(
            &Support::Available(ProgressSyntax::Osc94(Terminator::Bel)),
            MinimumVersion::Konsole(260400),
        ),
        acknowledge: NO_ACK,
    },
    arming: Some(Arming::new(
        b"\x1b]50;LocalTabTitleFormat=%w;RemoteTabTitleFormat=%w\x07",
        b"\x1b]50;LocalTabTitleFormat=%d : %n;RemoteTabTitleFormat=(%u) %H\x07",
    )),
    source: CapSource::Measured,
};

/// The LIBRARY, so the row promises only what every VTE product does.
///
/// `OSC 9;4` is the row that made [`Terminator`] a field: VTE returns early on a
/// BEL-terminated one on purpose (vteseq.cc:2160), where kitty and Konsole take
/// either. The same capability, a different encoding, and a boolean cannot say
/// so.
pub const VTE: SurfaceCaps = SurfaceCaps {
    name: "vte",
    human: "VTE (GNOME Terminal, Tilix, Terminator, Ptyxis, ...)",
    elide: Elide::Right,
    title: TitleCaps {
        osc0: YES,
        osc1: YES,
        osc2: YES,
        // Ps=1 is an explicit no-op; the window-title half works.
        stack_22t: YES,
    },
    tab_color: Protocol::new(&Support::Unsupported("VTE has no tab-colour escape; it is a settings file")),
    attention: AttentionCaps {
        bell: Support::Unverifiable("the VTE product's own bell setting decides whether it \
                                    is seen"),
        notify: Protocol::new(&Support::Unsupported("OSC 9 is routed to the progress parser, OSC 777 needs \
                                      enable_legacy_osc777 and is not a desktop notification, \
                                      and OSC 99 is not parsed")),
        progress: Protocol::new(&Support::Available(ProgressSyntax::Osc94(Terminator::St))),
        acknowledge: NO_ACK,
    },
    arming: None,
    source: CapSource::VendorSource,
};

pub const KITTY: SurfaceCaps = SurfaceCaps {
    name: "kitty",
    human: "kitty",
    elide: Elide::Right,
    title: TITLE_ALL,
    tab_color: Protocol::new(&Support::Unsupported("`kitty @ set-tab-color` is a remote-control socket round \
                                     trip behind allow_remote_control, not an escape")),
    attention: AttentionCaps {
        bell: Support::Unverifiable("window_alert_on_bell decides whether it is seen"),
        notify: Protocol::new(&Support::Available(NotifySyntax::Osc99(Terminator::St))),
        progress: Protocol::new(&Support::Available(ProgressSyntax::Osc94(Terminator::St))),
        acknowledge: NO_ACK,
    },
    arming: None,
    source: CapSource::VendorSource,
};

pub const ALACRITTY: SurfaceCaps = SurfaceCaps {
    name: "alacritty",
    human: "Alacritty",
    elide: Elide::Right,
    title: TitleCaps {
        osc0: YES,
        // The Rust `vte` crate, which is Alacritty's parser and has nothing to do
        // with GNOME's VTE above.
        osc1: Support::Unsupported(
            "the vte crate's osc_dispatch matches 0 and 2 only, so OSC 1 falls through to \
             unhandled",
        ),
        osc2: YES,
        stack_22t: YES,
    },
    tab_color: Protocol::new(&Support::Unsupported("Alacritty has no tabs")),
    attention: AttentionCaps {
        bell: Support::Unverifiable("Alacritty's own bell configuration decides whether it \
                                    is seen"),
        notify: Protocol::new(&Support::Unsupported("the vte crate's OSC table carries no notification grammar")),
        progress: Protocol::new(&Support::Unsupported("the vte crate's OSC table carries no progress grammar")),
        acknowledge: NO_ACK,
    },
    arming: None,
    source: CapSource::VendorSource,
};

/// `OSC 1` is WezTerm's tab title when it is non-empty, which makes it the one
/// row where the icon-title sequence is the interesting one.
pub const WEZTERM: SurfaceCaps = SurfaceCaps {
    name: "wezterm",
    human: "WezTerm",
    elide: Elide::Right,
    title: TitleCaps {
        osc0: YES,
        osc1: YES,
        osc2: YES,
        stack_22t: Support::Unsupported(
            "parsed, then matched to an empty arm placed above the unhandled logger, so it is \
             not even logged",
        ),
    },
    tab_color: Protocol::new(&Support::Unsupported("a colour needs a lua format-tab-title handler reading a \
                                     SetUserVar, which is configuration on the other side")),
    attention: AttentionCaps {
        bell: Support::Unverifiable("WezTerm's own bell configuration decides whether it is seen"),
        // OSC 9 is a toast here too, but this surface also parses OSC 9;4, and
        // 777 carries a title where 9 carries only a body.
        notify: Protocol::new(&Support::Available(NotifySyntax::Osc777(Terminator::St))),
        progress: Protocol::new(&Support::Available(ProgressSyntax::Osc94(Terminator::St))),
        acknowledge: NO_ACK,
    },
    arming: None,
    source: CapSource::VendorSource,
};

pub const FOOT: SurfaceCaps = SurfaceCaps {
    name: "foot",
    human: "foot",
    elide: Elide::Right,
    title: TITLE_ALL,
    tab_color: Protocol::new(&Support::Unsupported("foot has no tabs")),
    attention: AttentionCaps {
        bell: Support::Unverifiable("foot's own bell configuration decides whether it is seen"),
        notify: Protocol::new(&Support::Available(NotifySyntax::Osc99(Terminator::St))),
        progress: Protocol::new(&Support::Unsupported("OSC 9;4 is not in foot-ctlseqs")),
        acknowledge: NO_ACK,
    },
    arming: None,
    source: CapSource::VendorSource,
};

pub const GHOSTTY: SurfaceCaps = SurfaceCaps {
    name: "ghostty",
    human: "Ghostty",
    elide: Elide::Right,
    title: TITLE_ALL,
    tab_color: Protocol::new(&Support::Unsupported("Ghostty has no tab-colour escape")),
    attention: AttentionCaps {
        bell: Support::Unverifiable("Ghostty's own bell configuration decides whether it is seen"),
        notify: Protocol::new(&Support::Available(NotifySyntax::Osc99(Terminator::St))),
        progress: Protocol::new(&Support::Available(ProgressSyntax::Osc94(Terminator::St))),
        acknowledge: NO_ACK,
    },
    arming: None,
    source: CapSource::VendorSource,
};

/// The reference implementation, and the row that shows a capability can be a
/// MODE rather than a property: the title stack is on by default while the title
/// QUERY is off (`disallowedWindowOps` is `GetIconTitle,GetWinTitle`), and
/// `bellIsUrgent` is off by default but `CSI ? 1042 h` turns it on.
pub const XTERM: SurfaceCaps = SurfaceCaps {
    name: "xterm",
    human: "xterm",
    elide: Elide::Right,
    title: TITLE_ALL,
    tab_color: Protocol::new(&Support::Unsupported("xterm has no tabs")),
    attention: AttentionCaps {
        bell: Support::Unverifiable("bellIsUrgent, default false"),
        notify: Protocol::new(&Support::Unsupported("no notification grammar is in ctlseqs")),
        progress: Protocol::new(&Support::Unsupported("no progress grammar is in ctlseqs")),
        acknowledge: NO_ACK,
    },
    arming: None,
    source: CapSource::VendorSource,
};

pub const ITERM2: SurfaceCaps = SurfaceCaps {
    name: "iterm2",
    human: "iTerm2",
    elide: Elide::Right,
    title: TITLE_ALL,
    tab_color: Protocol::new(&Support::Available(TabColor::Osc1337(Terminator::St))),
    attention: AttentionCaps {
        bell: Support::Unverifiable("iTerm2's own bell configuration decides whether it is seen"),
        // The one surface whose only notification grammar is the overloaded OSC
        // 9; the `4;` prefix is what keeps it apart from the progress row above.
        notify: Protocol::new(&Support::Available(NotifySyntax::Osc9(Terminator::St))),
        progress: Protocol::new(&Support::Available(ProgressSyntax::Osc94(Terminator::St))),
        acknowledge: NO_ACK,
    },
    arming: None,
    source: CapSource::VendorSource,
};

/// The one row with no source to read and no published table: `nsterm`'s terminfo
/// on a Mac gives `tsl=\E]2;` `fsl=^G`, and everything else here has to be
/// measured on a Mac before it can be believed.
pub const APPLE_TERMINAL: SurfaceCaps = SurfaceCaps {
    name: "apple-terminal",
    human: "Terminal.app",
    elide: Elide::Right,
    title: TitleCaps {
        osc0: YES,
        osc1: Support::Unverifiable("no source and no documentation; it must be measured on a Mac"),
        osc2: YES,
        stack_22t: Support::Unverifiable(
            "no source and no documentation; it must be measured on a Mac",
        ),
    },
    tab_color: Protocol::new(&Support::Unsupported("no per-tab colour escape is documented")),
    attention: AttentionCaps {
        bell: Support::Unverifiable("the profile's bell setting decides whether it is seen"),
        notify: Protocol::new(&Support::Unsupported("no notification grammar is documented")),
        progress: Protocol::new(&Support::Unsupported("no progress grammar is documented")),
        acknowledge: NO_ACK,
    },
    arming: None,
    source: CapSource::Inferred,
};

/// The row that forced `Unverifiable` into the vocabulary.
///
/// `profiles.suppressApplicationTitle` silently discards OSC 0 and OSC 2, and
/// `compatibility.allowOSC777` defaults to FALSE - and neither is exposed through
/// an environment variable, a version string or a query this process could read.
/// Calling the title row `Available` lies to the users who changed the setting and
/// `Unsupported` lies to everyone else. It does not gate the write: a discarded
/// sequence costs nothing, so `should_emit` still says yes.
pub const WINDOWS_TERMINAL: SurfaceCaps = SurfaceCaps {
    name: "windows-terminal",
    human: "Windows Terminal",
    elide: Elide::Right,
    title: TitleCaps {
        osc0: Support::Unverifiable("profiles.suppressApplicationTitle silently discards it"),
        osc1: YES,
        osc2: Support::Unverifiable("profiles.suppressApplicationTitle silently discards it"),
        stack_22t: Support::Unsupported("WindowManipulationType has no 22 and no 23"),
    },
    tab_color: Protocol::new(&Support::Unsupported("tabColor is a profile setting; there is no colour verb")),
    attention: AttentionCaps {
        bell: Support::Unverifiable("bellStyle decides whether it is seen"),
        notify: Protocol::new(&Support::Unverifiable("compatibility.allowOSC777, default false")),
        progress: Protocol::new(&Support::Available(ProgressSyntax::Osc94(Terminator::Bel))),
        acknowledge: NO_ACK,
    },
    arming: None,
    source: CapSource::VendorSource,
};

/// The same state machine as Windows Terminal with none of the host around it, so
/// the difference is entirely in what the host does with a parsed sequence.
pub const CONHOST: SurfaceCaps = SurfaceCaps {
    name: "conhost",
    human: "the Windows console host",
    elide: Elide::Right,
    title: TitleCaps {
        osc0: YES,
        osc1: YES,
        osc2: YES,
        stack_22t: Support::Unsupported("WindowManipulationType has no 22 and no 23"),
    },
    tab_color: Protocol::new(&Support::Unsupported("conhost has no tabs")),
    attention: AttentionCaps {
        bell: Support::Unverifiable("the console host's own bell setting decides whether it \
                                    is seen"),
        notify: Protocol::new(&Support::Unsupported("conhost never sets the DesktopNotification optional feature")),
        progress: Protocol::new(&Support::Unverifiable("the taskbar ring belongs to the Windows Terminal host; \
                                         conhost's own handling is not documented")),
        acknowledge: NO_ACK,
    },
    arming: None,
    source: CapSource::VendorSource,
};

/// xterm.js with VS Code's options, which is the row that shows a named backend
/// is not the same as an answer: `pushTitle` and the OSC 99 handler are real code
/// behind settings that default to absent.
pub const VSCODE: SurfaceCaps = SurfaceCaps {
    name: "vscode",
    human: "VS Code's integrated terminal",
    elide: Elide::Right,
    title: TitleCaps {
        osc0: YES,
        osc1: YES,
        osc2: YES,
        stack_22t: Support::Unverifiable(
            "ITerminalOptions.windowOptions is {} by default and VS Code does not set pushTitle",
        ),
    },
    tab_color: Protocol::new(&Support::Unsupported("createTerminal({color}) is an extension API, not an escape")),
    attention: AttentionCaps {
        bell: Support::Unverifiable("terminal.integrated.enableBell decides whether it is seen"),
        notify: Protocol::new(&Support::Unverifiable("the OSC 99 handler is gated on VS Code's own \
                                       enable-notifications setting")),
        progress: Protocol::new(&Support::Unsupported("no progress grammar is in xterm.js's OSC table")),
        acknowledge: NO_ACK,
    },
    arming: None,
    source: CapSource::VendorSource,
};
