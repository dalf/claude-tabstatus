//! The leaf terminal: chosen at RUNTIME, dispatched STATICALLY.
//!
//! One Linux binary faces Konsole, VTE, kitty, WezTerm, Alacritty, foot, Ghostty
//! and - through WSL, which injects `WT_SESSION` via `WSLENV` - Windows Terminal,
//! so `cfg(target_os)` cannot pick the leaf the way it picks the platform in
//! [`crate::sys`]. The pick is made from the environment at runtime; the DISPATCH
//! is a `match` over a closed `Copy` enum returning `&'static` const data, so
//! nothing indirect ever appears on the paint path - no vtable, no registry, no
//! `Box<dyn>`, no allocation, no fallible lookup. This is what `config::Terminal`
//! already was, widened from two variants to fourteen.
//!
//! EVERY variant compiles in EVERY build, and only the detection CANDIDATE LIST
//! is cfg-selected - and that list is DATA, not code ([`probe`]). Two facts force
//! it. `CCTAB_TERMINAL` exists because ssh destroys the environment evidence, so
//! a Linux binary has to be able to be TOLD it is talking to iTerm2 or to Windows
//! Terminal. And a cfg-gated variant would put its capability row and its
//! grammars out of reach of the Linux `cargo test` that is the only place tests
//! ever run.
//!
//! What a row may CLAIM is bounded. A const row can only ever say `Available`,
//! `Unsupported` or `Unverifiable`: `Disabled` names a knob of OURS and is
//! layered over a row at the query by [`Support::gate`], and `Failed` is
//! something only an attempt can produce. The table walk in the tests below
//! asserts exactly that, and that every answer which is not `Available` carries
//! a non-empty reason a reader can act on.
//!
//! Six of these surfaces have never had a byte delivered to them, so every row
//! carries its own [`CapSource`]. A test can prove a row is well-FORMED; it
//! cannot prove it is TRUE, and a table that cannot say which rows were read off
//! a running terminal invites a reader to trust all fourteen equally. The
//! evidence behind each one is `docs/research/terminal-capability-matrix.md`.

pub mod compose;
mod probe;
mod rows;

pub use probe::{by_name, evidence, resolve_leaf};

use crate::support::{Presence, Support};

/// Which terminal is drawing the tab, to the extent it can be known.
///
/// `Copy` and `Eq` so that it travels in `Config` the way a `bool` would, and
/// deliberately NOT `Debug`: a fourteen-arm formatter compiled into a 370us
/// binary to serve test assertions is code for nothing, and `caps().name` reads
/// better in a failure message anyway.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Surface {
    /// Nothing said anything. This is what ssh looks like, and it is why this
    /// variant has no [`Arming`]: appearance bytes are not written to a terminal
    /// that cannot be named.
    Unknown,
    Konsole,
    /// The LIBRARY, not a product. `VTE_VERSION` is shared by GNOME Terminal,
    /// Tilix, Terminator, Ptyxis, Guake, Black Box and Xfce Terminal, which
    /// differ on tab colour and on notification behaviour, so the row promises
    /// only what all of them do.
    Vte,
    Kitty,
    Alacritty,
    WezTerm,
    Foot,
    Ghostty,
    Xterm,
    ITerm2,
    AppleTerminal,
    WindowsTerminal,
    ConHost,
    VsCode,
}

impl Surface {
    /// THE lookup: a `match` over a closed enum handing back `&'static` data.
    ///
    /// Adding a variant without a row is a compile error here, which is the whole
    /// reason the rows are reached this way rather than through a table indexed
    /// by a discriminant.
    pub fn caps(self) -> &'static SurfaceCaps {
        match self {
            Surface::Unknown => &rows::UNKNOWN,
            Surface::Konsole => &rows::KONSOLE,
            Surface::Vte => &rows::VTE,
            Surface::Kitty => &rows::KITTY,
            Surface::Alacritty => &rows::ALACRITTY,
            Surface::WezTerm => &rows::WEZTERM,
            Surface::Foot => &rows::FOOT,
            Surface::Ghostty => &rows::GHOSTTY,
            Surface::Xterm => &rows::XTERM,
            Surface::ITerm2 => &rows::ITERM2,
            Surface::AppleTerminal => &rows::APPLE_TERMINAL,
            Surface::WindowsTerminal => &rows::WINDOWS_TERMINAL,
            Surface::ConHost => &rows::CONHOST,
            Surface::VsCode => &rows::VSCODE,
        }
    }

    /// Every variant, as DATA: what `CCTAB_TERMINAL` is matched against, what the
    /// table walk below iterates, and what doctor will enumerate - including the
    /// surfaces this machine could never run, which is where the tests are.
    ///
    /// `caps()` makes a MISSING ROW a compile error; nothing makes a missing
    /// entry here one, so the length assertion in the tests is the guard.
    pub const ALL: &'static [Surface] = &[
        Surface::Unknown,
        Surface::Konsole,
        Surface::Vte,
        Surface::Kitty,
        Surface::Alacritty,
        Surface::WezTerm,
        Surface::Foot,
        Surface::Ghostty,
        Surface::Xterm,
        Surface::ITerm2,
        Surface::AppleTerminal,
        Surface::WindowsTerminal,
        Surface::ConHost,
        Surface::VsCode,
    ];
}

/// Which end of a too-long tab label the terminal throws away.
///
/// DATA handed to the layout decision, never the decision itself:
/// `config::GlyphPos::parse` still owns it and `CCTAB_GLYPH_POS` still wins.
/// Before this existed, `parse` tested `Terminal::Konsole` directly, which made
/// "Konsole" and "elides from the left" the same fact - and they are not, which
/// is why every one of the twelve rows added here is `Right` and only Konsole is
/// `Left`.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Elide {
    Left,
    Right,
    /// We do not know, which is NOT the same fact as `Right` even though the two
    /// produce the same layout: prefix is the safe default for a terminal we
    /// cannot identify, and doctor has to be able to say "unknown" rather than
    /// claim a truncation behaviour nobody observed.
    Unknown,
}

/// Where a row's claim came from.
///
/// Six of the fourteen surfaces below - Windows Terminal, conhost, iTerm2,
/// Terminal.app, Ghostty and VS Code - have never had a byte delivered to them
/// by this program. The rows exist because `CCTAB_TERMINAL` can name them over
/// ssh; this field is the row admitting how far it should be trusted.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum CapSource {
    /// Read off a running terminal by this project.
    Measured,
    /// Read out of the vendor's own source or reference documentation.
    VendorSource,
    /// Reasoned from a neighbouring fact. Not read, not run.
    Inferred,
}

impl CapSource {
    /// How far a reader should trust the row, in one phrase, on the surface line of
    /// doctor's capability table. Six of the fourteen rows have never had a byte
    /// delivered to them, and a table that prints `ok` without saying whether that
    /// was MEASURED invites a reader to trust all fourteen equally.
    pub fn why(self) -> &'static str {
        match self {
            CapSource::Measured => "measured on a running terminal",
            CapSource::VendorSource => "read from vendor source, never run",
            CapSource::Inferred => "inferred, never read and never run",
        }
    }
}

/// An OSC's terminator, because it is part of the grammar and not a detail.
///
/// VTE takes `OSC 9;4` only with ST and drops a BEL-terminated one ON PURPOSE
/// (`if (seq.is_st_bel()) return;`, vteseq.cc:2160), while kitty and Konsole take
/// either. A boolean "supports progress" cannot express that, so the terminator
/// travels with the grammar.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Terminator {
    Bel,
    St,
}

impl Terminator {
    /// NAMED, never written: a report that echoed a real BEL would beep the
    /// terminal it is being read in, and one that echoed a real ESC would be
    /// parsed by it.
    pub fn name(self) -> &'static str {
        match self {
            Terminator::Bel => "BEL",
            Terminator::St => "ST",
        }
    }
}

/// Can this surface be given a title, and how.
///
/// Only `osc0` has a consumer on the paint path - `compose::push_title` - and the
/// other three are report rows: doctor prints the whole table.
pub struct TitleCaps {
    /// `OSC 0` - icon name AND window title. The one sequence with no `N` in any
    /// row of the matrix, which is why even an unidentified surface is still sent
    /// a title: withholding it would blank a tab that works today.
    pub osc0: Presence,
    /// `OSC 1` - the icon name alone.
    pub osc1: Presence,
    /// `OSC 2` - the window title alone.
    pub osc2: Presence,
    /// `CSI 22 t` / `CSI 23 t`, the title stack - the only capture/restore
    /// primitive a terminal offers, and not a portable one. Konsole and WezTerm
    /// PARSE it and do nothing, which from the writer's side is indistinguishable
    /// from support; the row can say so only because it was written from vendor
    /// source rather than from a probe.
    pub stack_22t: Presence,
}

/// Painting a tab a colour, by ESCAPE SEQUENCE. A settings file, an extension API
/// or a remote-control socket is `Unsupported` here, because none of them is a
/// byte this program can write.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum TabColor {
    /// `ESC ] 34 ; <colour> BEL` - Konsole's `SessionColor`, which also sets
    /// `tabColorSetByUser`.
    Osc34(Terminator),
    /// `ESC ] 1337 ; SetColors=tab=RRGGBB ST` - iTerm2's, with `=default` to
    /// clear it again.
    Osc1337(Terminator),
}

impl TabColor {
    /// Which OSC, and how it is terminated - the two things doctor prints and the
    /// two things a reader compares across surfaces. The terminator travels with
    /// the grammar for the reason [`Terminator`] gives, so a report cannot show one
    /// without the other.
    pub fn grammar(self) -> (&'static str, Terminator) {
        match self {
            TabColor::Osc34(t) => ("OSC 34", t),
            TabColor::Osc1337(t) => ("OSC 1337 SetColors", t),
        }
    }
}

/// Named by EFFECT, never by OSC number. `OSC 9` is TWO unrelated protocols -
/// iTerm2's text notification and ConEmu's taskbar progress - and Konsole routes
/// it to the progress handler and produces no notification at all, so a field
/// called `osc9` could not mean anything.
///
/// Nothing in this build emits attention: #14 is the commit that does. doctor
/// prints them. The grammars land here first because a row added later is a row
/// every existing caller has to be re-read for, and because naming `Terminator`
/// per grammar is the fact that stops a BEL-terminated `OSC 9;4` being written to
/// VTE, which drops it on purpose.
pub struct AttentionCaps {
    /// Whether a BEL produces an attention signal a user will actually notice.
    /// Almost everywhere this is `Unverifiable` and the reason names the foreign
    /// setting that decides it: xterm's `bellIsUrgent` defaults to false, and
    /// every other terminal hides the same choice under its own name.
    pub bell: Presence,
    pub notify: Support<NotifySyntax>,
    pub progress: Support<ProgressSyntax>,
    /// The INBOUND half: being told the tab was looked at. `Unsupported` on every
    /// surface, permanently, under this process model - fd 0 is /dev/null,
    /// /dev/tty is unusable and nothing in this crate is long-lived. It is in the
    /// table so that it resurfaces as a decision rather than as "we need a
    /// daemon".
    pub acknowledge: Presence,
}

/// The three competing desktop-notification grammars, which do not overlap:
/// kitty speaks all three, iTerm2 only `OSC 9`, Konsole `OSC 777` and `OSC 99`
/// but NOT `OSC 9`, VS Code only `OSC 99`, and VTE none of them.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum NotifySyntax {
    /// `ESC ] 9 ; <text>` - body only, no title. iTerm2's.
    Osc9(Terminator),
    /// `ESC ] 777 ; notify ; <title> ; <body>` - urxvt's.
    Osc777(Terminator),
    /// `ESC ] 99 ; <k=v:k=v> ; <payload>` - kitty's, and the only one with a
    /// focus gate (`o=unfocused`) that needs no reader.
    Osc99(Terminator),
}

impl NotifySyntax {
    /// See [`TabColor::grammar`]. The three do not overlap, so which one a surface
    /// speaks is the whole answer, and doctor prints it rather than a bool.
    pub fn grammar(self) -> (&'static str, Terminator) {
        match self {
            NotifySyntax::Osc9(t) => ("OSC 9", t),
            NotifySyntax::Osc777(t) => ("OSC 777 notify", t),
            NotifySyntax::Osc99(t) => ("OSC 99", t),
        }
    }
}

/// `ESC ] 9 ; 4 ; <state> ; <percent>` - ConEmu's taskbar progress grammar, the
/// one attention channel that is on Claude Code's `terminalSequence` allowlist,
/// portable to all three operating systems and free of any pty write.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ProgressSyntax {
    Osc94(Terminator),
}

impl ProgressSyntax {
    /// See [`TabColor::grammar`]. The terminator is the load-bearing half here:
    /// VTE returns early on a BEL-terminated `OSC 9;4` on purpose.
    pub fn grammar(self) -> (&'static str, Terminator) {
        match self {
            ProgressSyntax::Osc94(t) => ("OSC 9;4", t),
        }
    }
}

/// The appearance bytes a surface needs before a title will show at all, PAIRED
/// BY CONSTRUCTION: [`Arming::new`] is the only way to name an arm and it takes
/// the restore in the same breath, and [`Arming::pair`] is the only way to read
/// either one back out.
///
/// An arm with no matching restore - or a restore whose spelling drifted from
/// the arm's - is this project's named recurring defect. What prevented it before
/// was two `pub const`s sitting next to each other in `emit.rs` under a comment
/// asking the next editor to keep them together.
pub struct Arming {
    arm: &'static [u8],
    restore: &'static [u8],
}

impl Arming {
    pub const fn new(arm: &'static [u8], restore: &'static [u8]) -> Arming {
        Arming { arm, restore }
    }

    /// Arm and restore, together, always. A caller that wants one of them is
    /// handed both, so a row whose restore was never written cannot compile.
    pub fn pair(&self) -> (&'static [u8], &'static [u8]) {
        (self.arm, self.restore)
    }
}

/// Everything one surface can do, as one `const`. No method, no receiver, no
/// dispatch: a new surface is a row, not a code path, and doctor can print the
/// whole table for a terminal this machine cannot run.
pub struct SurfaceCaps {
    /// Stable lowercase ASCII. What `CCTAB_TERMINAL` is matched against, and what
    /// doctor prints. Never reused once shipped.
    pub name: &'static str,
    /// The vendor's own spelling, for a report a human reads. doctor is its only
    /// consumer; it lives in the row because the row is also where the answer about
    /// a terminal this machine cannot run lives.
    pub human: &'static str,
    pub elide: Elide,
    pub title: TitleCaps,
    /// Read by doctor's capability table, and by #11 when it lands. It is here
    /// because tab colour rides in Konsole's arming property list, and a colour
    /// armed without a restore outlives the session.
    pub tab_color: Support<TabColor>,
    /// Read by doctor's capability table, and by #14 when it lands.
    pub attention: AttentionCaps,
    /// `Some` iff this surface needs arming AND the paired restore is known.
    pub arming: Option<Arming>,
    /// Printed by doctor, on the surface line, through [`CapSource::why`].
    /// Asserted below, because six of these rows describe a terminal nobody has
    /// ever delivered a byte to and a row that cannot say so invites a reader to
    /// trust all fourteen equally.
    pub source: CapSource,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::borrow::Cow;

    /// Every capability answer of one row, as the two things a report shows: the
    /// label and the reason. `Support` is neither `Copy` nor `PartialEq` - it
    /// holds an `io::Error` - so a table walk compares what doctor would print
    /// rather than the values.
    type Answer = (&'static str, &'static str, Option<Cow<'static, str>>);

    fn answers(c: &'static SurfaceCaps) -> Vec<Answer> {
        vec![
            ("osc0", c.title.osc0.label(), c.title.osc0.reason()),
            ("osc1", c.title.osc1.label(), c.title.osc1.reason()),
            ("osc2", c.title.osc2.label(), c.title.osc2.reason()),
            ("stack_22t", c.title.stack_22t.label(), c.title.stack_22t.reason()),
            ("tab_color", c.tab_color.label(), c.tab_color.reason()),
            ("bell", c.attention.bell.label(), c.attention.bell.reason()),
            ("notify", c.attention.notify.label(), c.attention.notify.reason()),
            ("progress", c.attention.progress.label(), c.attention.progress.reason()),
            (
                "acknowledge",
                c.attention.acknowledge.label(),
                c.attention.acknowledge.reason(),
            ),
        ]
    }

    /// The claim a const row is allowed to make. `off` is OUR knob and is layered
    /// at the query by `Support::gate`; `fail` is something only an attempt
    /// produces. A row that said either would be lying about where the answer
    /// came from, and doctor would print an absence nobody can act on.
    #[test]
    fn a_const_row_only_ever_says_ok_na_or_unverifiable() {
        for s in Surface::ALL {
            let caps = s.caps();
            for (field, label, reason) in answers(caps) {
                assert!(
                    matches!(label, "ok" | "n/a" | "?"),
                    "{}.{field} says {label}, which only a query or an attempt may say",
                    caps.name
                );
                if label != "ok" {
                    let why = reason.unwrap_or(Cow::Borrowed(""));
                    assert!(
                        !why.is_empty(),
                        "{}.{field} is {label} with no reason a reader could act on",
                        caps.name
                    );
                }
            }
        }
    }

    /// Konsole's tab bar elides from the LEFT at a hardcoded `Qt::ElideLeft` call
    /// site; everything else in the table truncates from the right, or is
    /// unidentified. Both of the other two answers put the glyph in the prefix,
    /// so this is what keeps every `CCTAB_TERMINAL` value except `konsole` on
    /// today's layout - the corpus case `tmux-terminal-override-is-not-konsole`
    /// stays green by construction rather than by luck.
    #[test]
    fn only_konsole_elides_from_the_left() {
        for s in Surface::ALL {
            let want = match s {
                Surface::Konsole => Elide::Left,
                Surface::Unknown => Elide::Unknown,
                _ => Elide::Right,
            };
            assert!(s.caps().elide == want, "{}", s.caps().name);
        }
    }

    /// The name is an identifier a user types into `CCTAB_TERMINAL`, so a
    /// duplicate or an upper-case byte would make one of two surfaces
    /// unreachable, and the case-insensitive match silently ambiguous.
    #[test]
    fn every_name_is_unique_lowercase_ascii() {
        let mut seen: Vec<&str> = Vec::new();
        for s in Surface::ALL {
            let n = s.caps().name;
            assert!(!n.is_empty());
            assert!(!s.caps().human.is_empty(), "{n} has no name for a human");
            assert!(
                n.bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-'),
                "{n} is not lowercase ASCII"
            );
            assert!(!seen.contains(&n), "{n} names two surfaces");
            seen.push(n);
        }
    }

    /// `caps()` cannot compile with a variant missing, but `ALL` can, and
    /// everything that walks the table - doctor, the override, these tests -
    /// reads `ALL`.
    #[test]
    fn all_lists_every_variant() {
        assert_eq!(Surface::ALL.len(), 14);
    }

    /// Konsole is the only surface with appearance bytes, and the pair is
    /// obtained together so that a restore cannot be forgotten. The bytes
    /// themselves are pinned in `compose`'s tests against what shipped.
    #[test]
    fn only_konsole_arms_and_it_restores() {
        for s in Surface::ALL {
            let armed = s.caps().arming.is_some();
            assert_eq!(armed, *s == Surface::Konsole, "{}", s.caps().name);
            if let Some(a) = &s.caps().arming {
                let (arm, restore) = a.pair();
                assert!(!arm.is_empty() && !restore.is_empty());
            }
        }
    }

    /// The six surfaces nobody has delivered a byte to, named. This is not a
    /// property of the code; it is the row admitting its own provenance, and it
    /// is asserted so that promoting a row to `Measured` is a deliberate edit
    /// here rather than a slip in a table of fourteen.
    #[test]
    fn the_rows_nobody_has_ever_written_to_say_so() {
        for s in Surface::ALL {
            let c = s.caps();
            let measured = c.source == CapSource::Measured;
            assert_eq!(measured, *s == Surface::Konsole, "{}", c.name);
        }
    }
}
