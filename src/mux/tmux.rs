//! Inside tmux the tab title stops being a latch and becomes a FUNCTION OF TIME.
//!
//! tmux re-evaluates `set-titles-string` on a timer and re-emits the result to
//! the outer terminal as an OSC 0. `%s` - the epoch - is available inside that
//! format, because tmux runs strftime over it before expanding `#{}`. So if the
//! paint carries the moment it happened, the format can render one glyph while
//! the paint is fresh, the idle glyph once it is stale, and nothing at all once
//! it is old. Known background work is exempt: purple persists, and blue/orange
//! with background fall back to purple. No timer proves that work finished.
//!
//! That is worth far more than tmux convenience. This project's recurring defect
//! is "an edge that paints with no matching un-paint": an abandoned permission
//! dialog fires no hook, a local slash command paints `working` and never clears,
//! Ctrl+C while thinking fires nothing. Inside tmux all of them heal themselves,
//! because nothing has to fire for the tab to stop lying.
//!
//! THE CARRIER is the pane title, which the plugin ALREADY writes - the OSC 0 it
//! emits inside a pane is stored by tmux as `pane_title` and never reaches the
//! outer terminal on its own. Inside tmux that OSC 0 stops being a tab title and
//! becomes a record:
//!
//!     <location> ct1 <state> <epoch>          state = w | a | i
//!     <location> ct2 <state> <epoch>          state = p | W | A
//!
//! ct2 carries known background, including beneath main work or an input wait.
//!
//! The glyph is NOT in it. Three measurements forced that: `#{=1:}` counts
//! COLUMNS and returns the EMPTY string for a width-2 emoji, so a glyph cannot be
//! sliced back out of the title; the glyph's POSITION already varies with
//! `CCTAB_GLYPH_POS`; and leaving it in would paint the current pane's glyph
//! twice, once in the strip and once in the label. A state LETTER is one ASCII
//! column, extractable from either end, and the glyph it stands for is looked up
//! in a server option the same SessionStart wrote.
//!
//! THE HOT PATH EXECS NOTHING. Only SessionStart and SessionEnd run tmux.
//! Measured on this machine: one `tmux set-option` costs 2.84ms
//! against a 0.37ms fork floor. The original twelve-command title batch measured
//! 3.1ms end to end - thirteen commands and 4.9ms in Konsole mode, where it
//! also lists the clients and writes the arming. The cost is the fork and the
//! socket round trip, not the commands, which is why batching is free and why a
//! per-tool-call exec would be a tenfold regression on a binary that runs in
//! 370us. Window decoration adds cold SessionStart queries to preserve option
//! scopes; those queries do not run on tool events.
//!
//! The window list uses the same pane cells before each window's existing label.
//! Only windows that start Claude receive local format decorators. Window names,
//! automatic renaming, global window formats and the outer aggregate stay intact.

use crate::armed::Armed;
use crate::clock::{self, DEFAULT_TTL_GONE, DEFAULT_TTL_WAITING, DEFAULT_TTL_WORKING, NEVER};
use crate::config::{self, Config, GlyphPos};
use crate::edge::{Glyph, Paint};
use crate::mux::{Channel, Route};
use crate::support::Support;
use crate::surface::{self, Surface};
use crate::sys;
use std::ffi::{OsStr, OsString};
use std::io;
use std::path::Path;
use std::process::{Command, Stdio};

mod lifecycle;
pub use lifecycle::Lifecycle;

/// The tag that marks a pane title as ours. Version 1 of the wire, so a future
/// shape can change it and let an old format ignore the new panes rather than
/// misread them.
const TAG: &str = "ct1";
const BACKGROUND_TAG: &str = "ct2";

// The server options SessionStart writes and the generated format reads. They
// are options rather than text spliced into the format because an option's VALUE
// is substituted literally: measured, `#{host}` and `%H` reach the tab intact
// through `#{@opt}` inside `#{T:}`, where the same bytes written into the format
// itself would have been expanded. That makes a hostile CCTAB_GLYPH_* inert.
const OPT_GW: &str = "@cctab_gw";
const OPT_GA: &str = "@cctab_ga";
const OPT_GI: &str = "@cctab_gi";
const OPT_GP: &str = "@cctab_gp";
const OPT_TW: &str = "@cctab_tw";
const OPT_TA: &str = "@cctab_ta";
const OPT_TG: &str = "@cctab_tg";
/// The generated strip-and-label format. `set-titles-string` only points at it.
const OPT_TITLE: &str = "@cctab_title";
/// The `set-titles-string` we installed, so doctor can tell ours from a user's.
const OPT_STRING: &str = "@cctab_string";
/// `1` once the user's own set-titles pair has been copied aside.
const OPT_SAVED: &str = "@cctab_saved";
const OPT_PREV_STRING: &str = "@cctab_prev_string";
const OPT_PREV_TITLES: &str = "@cctab_prev_titles";
const OPT_WINDOW_STRIP: &str = "@cctab_window_strip";
const OPT_WINDOW_COLOR: &str = "@cctab_window_color";

/// Each local decorator has independent ownership and inheritance metadata, so
/// a user can replace one format without preventing restoration of the other.
struct WindowFormat {
    option: &'static str,
    saved: &'static str,
    previous: &'static str,
    local: &'static str,
}

const WINDOW_FORMATS: [WindowFormat; 2] = [
    WindowFormat {
        option: "window-status-format",
        saved: "@cctab_window_format_saved",
        previous: "@cctab_prev_window_format",
        local: "@cctab_prev_window_format_local",
    },
    WindowFormat {
        option: "window-status-current-format",
        saved: "@cctab_window_current_saved",
        previous: "@cctab_prev_window_current",
        local: "@cctab_prev_window_current_local",
    },
];
/// This binary's own path, for the re-arm hook to run. An option because an
/// option's value is substituted LITERALLY, which is what spares the hook every
/// layer of quoting; see [`ARM_HOOK`].
const OPT_EXE: &str = "@cctab_exe";

/// THE ARMED RECORD - rung 1 of [`crate::armed`]. The shared restore obligation
/// for this tmux session's outer tab, retained until the last Claude pane ends.
///
/// It is the only option in this file set at SESSION scope rather than server
/// scope, and that is deliberate: every other option feeds `set-titles-string`,
/// which is server-wide and genuinely is "the last SessionStart wins". An arming
/// is not. It goes to the ptys of the clients attached to ONE session, so two tmux
/// sessions on one server are two different outer tabs, and a server-wide record
/// would let either one's SessionEnd delete the other's. Measured on tmux 3.7c: a
/// session value shadows a server value, and unsetting it falls back to the server
/// one - which is why the unset below leaves the empty string that reads as "no
/// record" rather than as "no retained arming policy".
const OPT_ARMED: &str = "@cctab_armed";

/// What [`OPT_ARMED`] holds when no shared arming policy has been retained.
///
/// A sentinel rather than an empty value, because the two are different answers
/// and the empty one has to keep meaning "no record here". Without it, every tmux
/// server that predates this commit - and every session whose SessionStart hook
/// never ran - would read as "no retained arming policy" and silently lose its restore.
/// No surface is named `-`, so [`crate::surface::by_name`] answers `None` for it
/// the same way it answers `None` for a name from the future.
const ARMED_NONE: &str = "-";

/// Is this pane carrying one of our records? ASCII and anchored at the end.
///
/// The `/r` flag needs the COLON form: `#{m/r|...}` parses as a single argument
/// and silently matches everything, which makes every shell pane a claude.
const IS: &str = "#{||:#{m/r: ct1 [wai] [0-9]+$,#{pane_title}},#{m/r: ct2 [pWA] [0-9]+$,#{pane_title}}}";
const HAS_BACKGROUND: &str = "#{m/r: ct2 [pWA] [0-9]+$,#{pane_title}}";
/// Seconds since the paint. `%s` is strftime's, expanded over the whole format
/// before `#{}` is, so every cell in one render shares one `now`.
///
/// The operands of `#{e|op|:}` are COMMA separated - the colon form yields the
/// empty string - and the test below must be `#{e|>|:}` and never `#{>:}`, which
/// is a STRING compare: measured, `#{>:10,9}` is 0.
const AGE: &str = "#{e|-|:%s,#{s|^.* ||:#{pane_title}}}";
/// The state letter. One ASCII column, so `#{=1:}` - which counts COLUMNS, and
/// returns EMPTY for a width-2 emoji - is safe here and nowhere near a glyph.
const ST: &str = "#{=1:#{s|^.* ct[12] ||:#{pane_title}}}";
/// The location, with the record taken back off.
///
/// No colon may appear anywhere inside an `s` modifier's arguments: measured,
/// one silently empties the WHOLE expansion, with any delimiter and with `##:`
/// escaping. That is why the wire tag is colon-free.
const LOC: &str = "#{s| ct[12] [waipWA] [0-9]*$||:#{pane_title}}";

/// A tier whose deadline has passed, with its state-specific fallback.
fn ladder(deadline: &str, live: &str, fallback: &str) -> String {
    format!("#{{?#{{e|>|:{AGE},#{{{deadline}}}}},{fallback},{live}}}")
}

/// One pane's contribution. Without background, transient glyphs age to white
/// and eventually disappear. Known background stays purple, including beneath
/// aged working/waiting carriers. Unknown carrier versions contribute nothing.
fn cell() -> String {
    cell_values("#{@cctab_gw}", "#{@cctab_ga}", "#{@cctab_gi}", "#{@cctab_gp}")
}

/// Share carrier recognition and decay between glyphs and theme colors.
fn cell_values(working: &str, waiting: &str, idle: &str, background: &str) -> String {
    let mut by_state = idle.to_owned();
    for (state, value) in [
        ('p', background.to_owned()),
        ('A', ladder(OPT_TA, waiting, background)),
        ('W', ladder(OPT_TW, working, background)),
        ('a', ladder(OPT_TA, waiting, idle)),
        ('w', ladder(OPT_TW, working, idle)),
    ] {
        by_state = format!("#{{?#{{==:{ST},{state}}},{value},{by_state}}}");
    }
    // Known background work never expires to idle or disappears. Working and
    // waiting can still age, but fall back to that background state instead.
    let alive = format!("#{{?{HAS_BACKGROUND},{by_state},#{{?#{{e|>|:{AGE},#{{{OPT_TG}}}}},,{by_state}}}}}");
    format!("#{{?{IS},{alive},}}")
}

/// A theme can replace its per-pane strip with one colored cap. Select the
/// highest visible priority across ALL panes in this window, independently of
/// customized glyphs. Empty means no visible Claude state; the theme supplies
/// its neutral color. Like the strip, expand with T: so expiry uses tmux's clock.
fn window_color() -> String {
    let states = format!("#{{P:{}}}", cell_values("w", "a", "i", "p"));
    let mut color = String::new();
    for (state, hex) in [('i', "#e5e7eb"), ('p', "#c084fc"), ('w', "#60a5fa"), ('a', "#fb923c")] {
        color = format!("#{{?#{{m:*{state}*,{states}}},{hex},{color}}}");
    }
    color
}

/// A cell per claude PANE of the attached session, then where you are.
///
/// `#{W:#{P:}}` and not `#{W:}`: `#{pane_title}` inside a window loop reads the
/// window's ACTIVE pane, so two claudes split in one window would show one cell.
/// The pane loop also makes a pane-scoped option unusable and unused.
///
/// The session loop `#{S:}` is deliberately absent. One terminal tab shows one
/// attached session; looping every session would put another tab's claudes into
/// this tab's title.
fn title_format(pos: GlyphPos) -> String {
    let strip = format!("#{{W:#{{P:{}}}}}", cell());
    let label =
        format!("#{{?{IS},{LOC},#{{session_name}}:#{{window_index}}:#{{window_name}}}}");
    match pos {
        // Konsole elides the tab label from the LEFT, so there the strip goes
        // last. `both` is not doubled: a strip is not a marker, and six cells at
        // each end is the whole tab.
        GlyphPos::Suffix => format!("{label} {strip}"),
        GlyphPos::Prefix | GlyphPos::Both => format!("{strip} {label}"),
    }
}

/// T expands the strip's clock; E expands the original format without adding a
/// strftime pass to the user's label. No separator remains when no pane has a
/// live carrier. A split window has one cell per Claude pane, including inactive
/// panes, just as the outer aggregate does.
fn window_status_format(previous: &str) -> String {
    format!("#{{?#{{==:#{{T:{OPT_WINDOW_STRIP}}},}},,#{{T:{OPT_WINDOW_STRIP}}} }}#{{E:{previous}}}")
}

/// A user can explicitly place the strip inside a themed label. Removing the
/// strip option leaves that label intact, whereas removing a saved original
/// still referenced through E:previous would erase the label itself.
fn window_format_uses_saved_label(value: &str) -> bool {
    WINDOW_FORMATS.iter().any(|f| value.contains(f.previous))
}

/// What `set-titles-string` itself holds: a pointer at [`title_format`], plus the
/// trim that removes the separator when no claude is running anywhere.
///
/// The indirection buys three things, all measured: the trim (a claude-free
/// server renders `main:0:sh` and not ` main:0:sh`); a one-line answer to "is
/// this ours?"; and no double expansion - `#{T:}` expands the option's own text
/// but NOT the pane titles and option values substituted into it, so a location
/// or a glyph holding `#{host}` arrives literally.
fn set_titles_string(pos: GlyphPos) -> &'static str {
    match pos {
        GlyphPos::Suffix => "#{s| $||:#{T:@cctab_title}}",
        GlyphPos::Prefix | GlyphPos::Both => "#{s|^ ||:#{T:@cctab_title}}",
    }
}

/// A live tmux server this session is running inside.
pub struct Tmux {
    /// `$TMUX_PANE`. Targets every command at OUR pane's session, so a second
    /// session on the same server is never touched.
    pane: Option<OsString>,
    // Cold lifecycle commands inherit the locked file as stdin. If the hook is
    // killed while a tmux command is in flight, that child keeps the same lock
    // until it exits; a successor cannot overtake its pending mutation.
    lifecycle_lock: std::cell::RefCell<Option<std::fs::File>>,
}

impl Tmux {
    /// `None` outside tmux, and `None` for a `$TMUX` that is not one.
    ///
    /// The test is SYNTACTIC and deliberately does not stat the socket: the
    /// golden corpus carries a plausible fake socket path, and a corpus whose
    /// answer depends on which files exist on the replaying machine is not a
    /// corpus. The cost is that a stale exported `$TMUX` leaves a visible record
    /// in tmux's own `#T`, which doctor names.
    pub fn detect() -> Option<Tmux> {
        if config::flag("CCTAB_NO_TMUX") {
            return None;
        }
        let raw = config::var_nonempty("TMUX")?;
        if !is_tmux_env(raw.as_encoded_bytes()) {
            return None;
        }
        Some(Tmux {
            pane: config::var_nonempty("TMUX_PANE"),
            lifecycle_lock: Default::default(),
        })
    }

    /// `-t <our pane>`, so a command lands on our session and not on whichever
    /// one tmux would have picked.
    fn target(&self, c: &mut Command) {
        self.inherit_lock(c);
        if let Some(p) = &self.pane {
            c.arg("-t").arg(p);
        }
    }

    fn inherit_lock(&self, c: &mut Command) {
        if let Some(file) = self.lifecycle_lock.borrow().as_ref() {
            // A failed clone must not allow an uncoordinated mutation.
            match file.try_clone() {
                Ok(file) => { c.stdin(file); }
                Err(_) => { c.arg("--cctab-lock-unavailable"); }
            }
        }
    }

    /// A handle with no pane, for the routing tests in [`crate::mux`]. The pane
    /// is the one thing `route` never reads, and a test that had to name one would
    /// be asserting about a target rather than about a channel.
    #[cfg(test)]
    pub fn for_test() -> Tmux {
        Tmux { pane: None, lifecycle_lock: Default::default() }
    }
}

/// `$TMUX` is `<socket path>,<server pid>,<session id>`.
fn is_tmux_env(b: &[u8]) -> bool {
    let mut parts = b.split(|c| *c == b',');
    let (Some(sock), Some(pid), Some(sess), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return false;
    };
    sock.starts_with(b"/") && digits(pid) && digits(sess)
}

fn digits(b: &[u8]) -> bool {
    !b.is_empty() && b.iter().all(u8::is_ascii_digit)
}

/// The OSC 0 payload inside tmux: a record, not a tab title.
///
/// Session end has no glyph and therefore no record - its EMPTY title is what
/// clears `pane_title`, which is what removes the cell.
pub fn carrier(paint: Paint, place: &str) -> String {
    carrier_at(paint, place, clock::now())
}

fn carrier_at(paint: Paint, place: &str, epoch: u64) -> String {
    let Some(g) = paint.glyph() else {
        return String::new();
    };
    let state = match g {
        Glyph::Working => if paint.background() { 'W' } else { 'w' },
        Glyph::Waiting => if paint.background() { 'A' } else { 'a' },
        Glyph::Background => 'p',
        Glyph::Idle => 'i',
    };
    let tag = if paint.background() { BACKGROUND_TAG } else { TAG };
    format!("{place} {tag} {state} {epoch}")
}

/// One to six digits with no leading zero; `0` is "never"; anything else is the
/// default, as a STRING for the format the server evaluates. The grammar and the
/// clock behind it are [`crate::clock`]'s, because the state layer expires waits
/// on the same setting without needing a multiplexer to do it.
fn ttl(key: &str, default: u32) -> String {
    clock::ttl_secs(key, default).to_string()
}

/// Configure the server title in one batch, then decorate this pane's window.
///
/// The user's own `set-titles` pair is saved before that pair is replaced,
/// guarded by "only if nothing is saved yet", so a second claude
/// starting later cannot record OUR string as the user's. tmux serialises
/// commands on one event loop, so the batch is atomic with respect to every
/// other claude on the server; measured, fifty concurrent installers left the
/// user's string intact.
///
/// Nothing here touches `status` or `status-interval`, although they ARE the
/// decay clock. Measured: with `status off` and `status-interval 0` the title is
/// re-evaluated exactly once more, about five seconds after a client attaches,
/// and then never again - so a cell whitens and never disappears. Turning the
/// status line on anyway would run a user's `#(...)` in `status-right` on our
/// schedule, which is not ours to decide. doctor reports it instead.
pub fn session_start(cfg: &Config, route: Route) {
    let Some(t) = cfg.stack.tmux() else { return };
    let fmt = title_format(cfg.glyph_pos);
    let sts = set_titles_string(cfg.glyph_pos);
    let mut c = command();
    set(&mut c, OPT_GW, cfg.glyph(Glyph::Working));
    set(&mut c, OPT_GA, cfg.glyph(Glyph::Waiting));
    set(&mut c, OPT_GI, cfg.glyph(Glyph::Idle));
    set(&mut c, OPT_GP, cfg.glyph(Glyph::Background));
    set(&mut c, OPT_TW, &ttl("CCTAB_TTL_WORKING", DEFAULT_TTL_WORKING));
    set(&mut c, OPT_TA, &ttl("CCTAB_TTL_WAITING", DEFAULT_TTL_WAITING));
    set(&mut c, OPT_TG, &ttl("CCTAB_TTL_GONE", DEFAULT_TTL_GONE));
    set(&mut c, OPT_TITLE, &fmt);
    set(&mut c, OPT_WINDOW_STRIP, &format!("#{{P:{}}}", cell()));
    set(&mut c, OPT_WINDOW_COLOR, &window_color());
    set(&mut c, OPT_STRING, sts);
    c.arg(";").arg("if").arg("-F").arg(format!("#{{==:#{{{OPT_SAVED}}},}}")).arg(format!(
        "set -Fs {OPT_PREV_STRING} \"#{{set-titles-string}}\" ; \
         set -Fs {OPT_PREV_TITLES} \"#{{set-titles}}\" ; \
         set -s {OPT_SAVED} 1"
    ));
    c.arg(";").arg("set").arg("-g").arg("set-titles").arg("on");
    c.arg(";").arg("set").arg("-g").arg("set-titles-string").arg(sts);
    // The obligation belongs to the shared tab, not to the latest Claude start.
    // A non-arming start may initialise `-`, but cannot erase an earlier arm.
    // tmux's set -o checks and writes on the server, without a read/write race.
    let armed = route.arms(Channel::Clients);
    remember_armed(&mut c, t, armed);
    // LAST in the batch, deliberately: `client-attached[N]` is an array option,
    // which a tmux older than 3.0 has no syntax for, and measured, a command that
    // fails at the END of a `;`-chained batch leaves every command before it
    // applied. So an old tmux loses the re-arm and keeps the whole decay.
    // The hook and record retain the SAME policy, before any client write.
    // Detached starts legitimately install both without delivering any bytes.
    match (armed, exe_path()) {
        // A path with a single quote in it has no representation inside ARM_HOOK's
        // sh quoting, and a path that is not UTF-8 cannot go into a format at all.
        // Both drop the re-arm and keep everything else.
        (Some(_), Some(exe)) => {
            set(&mut c, OPT_EXE, &exe);
            c.arg(";").arg("set-hook");
            t.target(&mut c);
            c.arg(HOOK).arg(ARM_HOOK);
        }
        // Another Claude may still need the existing re-arm hook. Only the
        // final SessionEnd (or uninstall) releases this shared obligation.
        _ => {}
    }
    run(c);
    install_window_status(t);
}

/// Decorate only the pane's actual window. Without a known pane, tmux's default
/// target could be an unrelated client window, so leave the window list alone.
fn window_id(t: &Tmux) -> Option<String> {
    let pane = t.pane.as_ref()?.to_str()?;
    if !pane.starts_with('%') || !digits(pane[1..].as_bytes()) {
        return None;
    }
    let fields = ask(t, &["#{window_id}"]).ok()?;
    let id = fields.into_iter().next()?;
    valid_window_id(&id).then_some(id)
}

fn valid_window_id(id: &str) -> bool {
    id.starts_with('@') && digits(id[1..].as_bytes())
}

/// show without -A reports only explicitly local values. Keeping the option
/// name distinguishes an explicit empty string from an inherited value.
fn local_window_option(id: &str, option: &str) -> Result<bool, Fail> {
    let mut c = command();
    c.args(["show-options", "-wq", "-t", id, option]);
    capture(c).map(|s| !s.is_empty())
}

fn install_window_status(t: &Tmux) {
    let Some(id) = window_id(t) else { return };
    let mut c = command();
    t.inherit_lock(&mut c);
    for f in &WINDOW_FORMATS {
        let Ok(local) = local_window_option(&id, f.option) else { return };
        if c.get_args().next().is_some() {
            c.arg(";");
        }
        // This server-side guard makes overlapping SessionStart hooks save the
        // original once. Format values are expanded AFTER command parsing, so
        // quotes, newlines and semicolons in a user's format remain plain data.
        // Repeated starts leave user edits to an installed decorator untouched.
        c.args(["if-shell", "-F", "-t", &id]);
        let unsaved = format!("#{{&&:#{{==:#{{{}}},}},#{{==:#{{{}}},}}}}", f.saved, f.local);
        let independent = format!(
            "#{{&&:#{{==:#{{m:*@cctab_prev_window_*,#{{{}}}}},0}},#{{&&:#{{==:#{{m:*{OPT_WINDOW_STRIP}*,#{{{}}}}},0}},#{{==:#{{m:*{OPT_WINDOW_COLOR}*,#{{{}}}}},0}}}}}}",
            f.option, f.option, f.option,
        );
        // If an ownership marker was lost, never save our existing wrapper as
        // its own original: E:previous would then recurse into itself.
        c.arg(format!("#{{&&:{unsaved},{independent}}}"));
        c.arg(format!(
            "set -Fw -t {id} -- {} \"#{{{}}}\" ; \
             set -w -t {id} -- {} {} ; \
             set -w -t {id} -- {} 1 ; \
             set -w -t {id} -- {} \"{}\"",
            f.previous, f.option, f.local, if local { "1" } else { "0" },
            f.saved, f.option, window_status_format(f.previous),
        ));
    }
    run(c);
}

/// `; set -s -- <name> <value>`. The `--` is load-bearing: a glyph override of
/// `-x` would otherwise be read as a flag.
fn set(c: &mut Command, name: &str, value: &str) {
    if c.get_args().next().is_some() {
        c.arg(";");
    }
    c.arg("set").arg("-s").arg("--").arg(name).arg(value);
}

/// Record shared policy at SESSION scope before delivery, or initialise `-`.
fn remember_armed(c: &mut Command, t: &Tmux, armed: Option<Surface>) {
    if c.get_args().next().is_some() {
        c.arg(";");
    }
    c.arg("set");
    if armed.is_none() {
        c.arg("-o");
    }
    t.target(c);
    c.arg("--").arg(OPT_ARMED).arg(armed.map_or(ARMED_NONE, |s| s.caps().name));
}

/// The array index our `client-attached` hook occupies.
///
/// An INDEX and not a bare `set-hook -g client-attached`: measured on 3.7c, a
/// bare set REPLACES THE WHOLE ARRAY, and a user's own hooks live in it, while
/// `set-hook -u ...[1971]` removes exactly ours and leaves index 0 alone. The
/// number is far from the 0.. that a user's `-a` appends into.
///
/// It is set on OUR SESSION and not globally, measured both ways: a session hook
/// fires only for a client attaching to that session, so a Konsole tab whose tmux
/// session holds no claude is never armed - and an arming with no matching
/// un-arming is this project's named defect.
const HOOK: &str = "client-attached[1971]";

/// The hook's command, a CONSTANT: `run-shell` this binary's own re-arm verb, with
/// the path and the attaching client's pty both substituted by tmux.
///
/// WHY A HOOK AT ALL: SessionStart's arming goes to the ptys `list-clients` names,
/// so an arming made while DETACHED reaches nobody, and on reattach tmux replays
/// the TITLE but never the arming - measured, the reattached tab was governed by
/// Konsole's `RemoteTabTitleFormat=(%u) %H` again and the glyph was invisible.
/// "Close the laptop while Claude keeps working" is the topology this whole slice
/// exists for, and every reattach in it lands in a fresh, un-armed tab.
///
/// WHY THE BYTES GO THROUGH THIS BINARY rather than a `printf` in the hook: the
/// arming payload contains `%w` twice, so a tmux-side `printf` would have to
/// survive the SHELL's `%` handling as well, while [`arm_tty`] writes the same
/// constant the non-tmux path writes.
///
/// WHY THE PATH IS AN OPTION and not text inside this string: a hook's value is
/// PARSED when it is set, and measured, `$rd` inside tmux's double quotes is
/// expanded THERE - a path holding `$` lost a piece of itself before anything ran.
/// An option's value is substituted literally instead: measured, a path holding
/// `#`, `#{host}`, `%Y` and a space reached the shell byte for byte. So this string
/// carries no path, needs no escaping, and is one constant to compare against.
///
/// The sh single quotes are what make a space in the path safe, and they are also
/// why a path holding a single quote is refused in [`session_start`] rather than
/// mangled.
const ARM_HOOK: &str = "run-shell -b \"'#{@cctab_exe}' tmux-arm '#{client_tty}'\"";

/// This binary, for the hook to run, or `None` when it cannot be carried.
fn exe_path() -> Option<String> {
    let exe = std::env::current_exe().ok()?;
    let exe = exe.to_str()?;
    if exe.contains('\'') || exe.contains(|c: char| c.is_control()) {
        return None;
    }
    Some(exe.to_owned())
}

/// `tabstatus tmux-arm <tty>` - arm ONE terminal, named by tmux.
///
/// It takes the pty as an argument instead of asking tmux, so the hook costs one
/// fork and no socket round trip, and so the verb needs no `$TMUX` of its own.
/// [`sys::write_tty`] checks the pathname and the acquired terminal descriptor.
/// The caller keeps this hook silent even on an inspection or write failure.
pub fn arm_tty(path: &OsStr) -> io::Result<bool> {
    // The verb exists only for Konsole, so it reads Konsole's row directly rather
    // than resolving a surface it has no environment for: the hook fires in a tmux
    // server's own environment, where nothing names the leaf.
    if let Some(a) = &Surface::Konsole.caps().arming {
        return sys::write_tty(Path::new(path), a.pair().0);
    }
    Ok(false)
}

/// Take the re-arm hook back off, and the armed record with it.
///
/// ONE function, because the two have exactly one lifetime between them: the hook
/// exists to apply the retained policy on reattach, and the record exists to
/// select its eventual restore bytes. Dropping one without the other leaves a
/// hook without a restore obligation, or policy that reattachment cannot renew.
///
/// The record is UNSET rather than set to [`ARMED_NONE`]: the session option
/// falls back to the server's, which we never write, so the answer becomes the
/// empty string - "no record here" - and the next rung gets to speak. Saying
/// "no retained arming policy" would be a claim about a session that no longer exists.
fn disarm(t: &Tmux) {
    let mut c = command();
    c.arg("set-hook").arg("-u");
    t.target(&mut c);
    c.arg(HOOK);
    c.arg(";").arg("set").arg("-u");
    t.target(&mut c);
    c.arg("--").arg(OPT_ARMED);
    run(c);
}

/// RUNG 1: this tmux session's retained arming policy, read back out of the
/// multiplexer's own key-value store.
///
/// An `Available` answer STOPS the ladder, so every way of not knowing has to be
/// one of the other words - a tmux that is not on PATH, a socket that went away
/// between the two hooks, a server whose SessionStart predates this record, or a
/// value naming a surface this build has no row for. Any of those falls through to
/// the state record and then to the assumption; only the server's own `-` is
/// allowed to say "no retained arming policy", and only because SessionStart writes it.
///
/// ONE round trip, on SessionEnd and on doctor, both of which already exec tmux.
/// Nothing on the paint path reaches this.
pub fn armed(t: &Tmux) -> Support<Option<Surface>> {
    match ask(t, &[&format!("#{{{OPT_ARMED}}}")]) {
        Ok(f) => armed_value(f.first().map_or("", String::as_str)),
        Err(Fail::NoBinary) => Support::Unsupported("tmux(1) is not on PATH"),
        Err(Fail::NoAnswer) => {
            Support::Unsupported("the tmux server did not answer, so it remembers nothing")
        }
    }
}

/// What one [`OPT_ARMED`] value means. Split out of [`armed`] because doctor
/// already has the value - it reads the option in the same one round trip it makes
/// for everything else - and a report that re-derived the verdict from a second
/// query is a report that can disagree with the paint path.
fn armed_value(v: &str) -> Support<Option<Surface>> {
    if v.is_empty() {
        return Support::Unsupported("this tmux session holds no retained arming policy");
    }
    if v == ARMED_NONE {
        return Support::Available(None);
    }
    // ABSENT, never DIFFERENT: a name from a later version, or one a user set by
    // hand, must not choose escape bytes.
    match surface::by_name(v.as_bytes()) {
        Some(leaf) => Support::Available(Some(leaf)),
        None => Support::Unsupported("the record names a surface this build has no row for"),
    }
}

/// Arm the outer terminal's tab, from inside tmux.
///
/// The route is the CLIENT's pty, not `allow-passthrough`. Passthrough would
/// work - measured, `on` passes the active pane and `all` passes background
/// windows, with every inner ESC doubled - but turning it on lets any program in
/// any pane write arbitrary bytes to the user's terminal, which is a security
/// decision a tab-title plugin has no business taking. `list-clients` names each
/// attached client's pty, and writing there needs no grant, works from a
/// background window, and needs no DCS wrapper.
///
/// Opt-in via `CCTAB_TERMINAL=konsole`, because in the topology this is for -
/// Konsole, ssh, tmux - `KONSOLE_*` does not survive the ssh and there is
/// nothing to detect.
///
/// WHETHER to arm, and to whom, is [`crate::mux::route`]'s answer and not this
/// function's: what used to be here was `surface != Konsole` with the tmux half
/// of the condition hidden inside [`to_clients`], while `emit::session_start`
/// carried the complementary half. Two halves in two files is how a pane gets
/// armed twice.
pub fn arm_konsole(cfg: &Config, route: Route) -> io::Result<bool> {
    let Some(surface) = route.arms(Channel::Clients) else { return Ok(false) };
    let Some(t) = cfg.stack.tmux() else { return Ok(false) };
    if let Some(a) = &surface.caps().arming {
        return to_clients(t, a.pair().0);
    }
    Ok(false)
}

/// Put the tab formats back, but only when no OTHER claude is left in this
/// session - the arming is per TAB, and inside tmux one tab holds every window.
///
/// The lifecycle guard has already retired our explicit membership. It holds
/// the shared session lock through these writes and disarm; a successor cannot
/// register or publish until retirement finishes. Registered panes do not rely
/// on asynchronous title parsing for either ownership or retirement.
pub fn session_end(cfg: &Config, route: Route, lifecycle: &Lifecycle<'_>) -> io::Result<bool> {
    // WHOSE bytes came out of the record, not out of this hook's environment; see
    // [`crate::armed`]. All this function still decides is whether anyone else is
    // using them.
    let Some(surface) = route.arms(Channel::Clients) else { return Ok(false) };
    let Some(t) = cfg.stack.tmux() else { return Ok(false) };
    // Another claude is still painting into this tab, so the arming is still in
    // force for it. Leaving the RECORD alone here is the same decision as leaving
    // the tab armed: the session that does turn the lights out has to still be
    // able to read what to put back.
    if lifecycle.has_owners() {
        return Ok(false);
    }
    let delivered = match &surface.caps().arming {
        Some(a) => to_clients(t, a.pair().1),
        None => Ok(false),
    };
    // Retire the policy even if restoration failed or no client was attached:
    // the last Claude is leaving, so future attaches must not arm again. This
    // is best-effort cleanup, not an acknowledgement or a pending retry queue.
    disarm(t);
    delivered
}

/// `1` once per pane of this session that carries a record and is not ours.
///
/// Our own pane is excluded rather than relied on to have been cleared already:
/// session end's empty title travels through the pty and tmux's parser, and
/// racing that would silently skip the restore.
fn others_expr(t: &Tmux) -> String {
    let mine = match &t.pane {
        Some(p) => inert(&String::from_utf8_lossy(p.as_encoded_bytes())),
        None => String::new(),
    };
    format!("#{{W:#{{P:#{{?#{{==:#{{pane_id}},{mine}}},,{IS}}}}}}}")
}

/// Write `bytes` to the pty of every client attached to OUR session.
///
/// It takes the handle rather than the whole `Config` because the multiplexer's
/// presence is no longer its business: it used to carry the tmux half of the
/// arming condition in that `let Some` - the half `emit.rs` complemented, and the
/// half that made lifting the caller's body into a method an unconditional arm.
///
/// `Ok(true)` means at least one completed write, `Ok(false)` none (detached or
/// all destinations skipped). Return the first error after trying every client;
/// it can coexist with complete or partial writes to other clients. No outcome
/// proves terminal application or changes the retained arming policy.
fn to_clients(t: &Tmux, bytes: &[u8]) -> io::Result<bool> {
    let mut c = command();
    c.arg("list-clients");
    t.target(&mut c);
    c.arg("-F").arg("#{client_tty}");
    let out = capture(c).map_err(|_| io::Error::other("tmux client listing failed"))?;
    write_clients(&out, bytes, sys::write_tty)
}

fn write_clients(
    out: &str,
    bytes: &[u8],
    mut write: impl FnMut(&Path, &[u8]) -> io::Result<bool>,
) -> io::Result<bool> {
    let mut result = Ok(false);
    for line in out.lines().filter(|l| !l.is_empty()) {
        // Evaluate the next write even when an earlier client failed.
        result = match (result, write(Path::new(line), bytes)) {
            (Ok(any), Ok(wrote)) => Ok(any || wrote),
            (Err(e), _) | (_, Err(e)) => Err(e),
        };
    }
    result
}

/// Make a value safe to splice into a format's TEXT, which is a different
/// question from splicing it into an option's value: format text is run through
/// strftime and then through `#{}` expansion, so both `%` and `#` have to be
/// doubled.
///
/// A pane id is the one value that needs this, and it needs it for real:
/// measured, `x%0y` in a format renders as `x26`, because strftime reads `%0` as
/// a zero-pad flag on the `y` behind it. It happens to be harmless where the id
/// is followed by `}`, which glibc passes through - so this is a correctness fix
/// that the current expression does not yet need, and the next one would.
fn inert(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    for c in s.chars() {
        if c == '%' || c == '#' {
            out.push(c);
        }
        out.push(c);
    }
    out
}

/// Default to no hook input; a lifecycle target replaces stdin with its lock.
fn command() -> Command {
    let mut c = Command::new("tmux");
    c.stdin(Stdio::null());
    c
}

/// Run a tmux command and throw everything away.
///
/// stdout AND stderr go to /dev/null, and neither is belt and braces: stdout
/// would corrupt the hook protocol's one JSON line, and a `$TMUX` naming a
/// socket that is gone prints `error connecting to ...` on stderr and exits 1 in
/// about 4ms - which two golden-corpus cases, both expecting empty stderr, would
/// catch as a failure.
fn run(mut c: Command) {
    let _ = c
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

/// Why a tmux command produced no answer.
///
/// The two are worth telling apart in exactly one place: doctor gave "tmux(1) is
/// not on PATH" and "the socket named by $TMUX is gone" the same FAIL branch and
/// the same hedged sentence, although one is fixed by installing tmux and the
/// other by unsetting a stale exported `$TMUX`. Everywhere else both mean "do
/// nothing", which is why every other caller still ignores which it was.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Fail {
    /// `execvp` said ENOENT: there is no tmux binary to talk to.
    NoBinary,
    /// It ran and refused, or its output was not UTF-8.
    NoAnswer,
}

/// Run a tmux command and read its stdout.
fn capture(mut c: Command) -> Result<String, Fail> {
    let out = match c.stderr(Stdio::null()).output() {
        Ok(o) => o,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Err(Fail::NoBinary),
        Err(_) => return Err(Fail::NoAnswer),
    };
    if !out.status.success() {
        return Err(Fail::NoAnswer);
    }
    String::from_utf8(out.stdout).map_err(|_| Fail::NoAnswer)
}

// --- uninstall ---------------------------------------------------------------

/// Restore tracked windows across the whole server, including other sessions
/// and linked windows. Saved values remain inside tmux and travel through format
/// expansions, never diagnostic stdout or interpolated command strings.
fn restore_window_status() -> Result<Vec<String>, Fail> {
    let mut list = command();
    list.args(["list-windows", "-a", "-F", "#{window_id}"]);
    let ids: std::collections::BTreeSet<_> = capture(list)?
        .lines().filter(|id| valid_window_id(id)).map(str::to_owned).collect();
    let mut restored = 0;
    let mut kept = 0;
    for id in ids {
        let target = Tmux { pane: Some(OsString::from(&id)), lifecycle_lock: Default::default() };
        for f in &WINDOW_FORMATS {
            // The previous value goes last, preserving embedded/trailing
            // newlines. These reads inspect ownership only: tmux 3.4 escapes
            // dollar signs in diagnostic stdout, so they cannot restore bytes.
            let fields = ask(&target, &[
                &format!("#{{{}}}", f.saved),
                &format!("#{{{}}}", f.local),
                &format!("#{{{}}}", f.previous),
            ])?;
            let current = ask(&target, &[&format!("#{{{}}}", f.option)])?;
            if fields[0] != "1" {
                if window_format_uses_saved_label(&current[0]) {
                    return Err(Fail::NoAnswer);
                }
                continue;
            }
            let mut c = command();
            if current[0] == window_status_format(f.previous) {
                // An absent original is not an explicitly empty original. If
                // metadata is incomplete, retain the wrapper and shared options
                // rather than inventing a label or changing its inheritance.
                if !local_window_option(&id, f.saved)?
                    || !local_window_option(&id, f.previous)?
                    || !local_window_option(&id, f.local)?
                    || (fields[1] != "0" && fields[1] != "1")
                    || window_format_uses_saved_label(&fields[2]) {
                    return Err(Fail::NoAnswer);
                }
                c.arg("set");
                if fields[1] == "1" {
                    c.args(["-Fw", "-t", &id, "--", f.option,
                        &format!("#{{{}}}", f.previous)]);
                } else {
                    c.args(["-wu", "-t", &id, "--", f.option]);
                }
                restored += 1;
            } else {
                // An edited wrapper may still depend on our saved original;
                // removing that would damage the user's edited label. An
                // explicit strip-only placement is independent: after uninstall
                // its missing strip expands to nothing and its label remains.
                if window_format_uses_saved_label(&current[0]) {
                    return Err(Fail::NoAnswer);
                }
                // The user replaced or unset our wrapper after installation.
                // Preserve that choice while taking our saved metadata away.
                kept += 1;
            }
            for option in [f.saved, f.previous, f.local] {
                if c.get_args().next().is_some() {
                    c.arg(";");
                }
                c.args(["set", "-wu", "-t", &id, "--", option]);
            }
            capture(c)?;
        }
    }
    Ok(if restored != 0 || kept != 0 {
        vec![format!("tmux:     window list: reset {restored} owned format(s); kept {kept} user edit(s).")]
    } else {
        Vec::new()
    })
}

fn report_window_status(t: &Tmux) -> String {
    let Some(id) = window_id(t) else {
        return "           window list: WARN no known TMUX_PANE; no window format is selected for decoration".to_owned();
    };
    let target = Tmux { pane: Some(OsString::from(id)), lifecycle_lock: Default::default() };
    let mut installed = 0;
    let mut custom = 0;
    let mut changed = 0;
    for f in &WINDOW_FORMATS {
        let Ok(values) = ask(&target, &[
            &format!("#{{{}}}", f.saved),
            &format!("#{{{}}}", f.option),
        ]) else {
            return "           window list: WARN could not read this window's formats".to_owned();
        };
        if values[0] == "1" && values[1] == window_status_format(f.previous) {
            installed += 1;
        } else if (values[1].contains(OPT_WINDOW_STRIP) || values[1].contains(OPT_WINDOW_COLOR))
            && !window_format_uses_saved_label(&values[1]) {
            custom += 1;
        } else if values[0] == "1" {
            changed += 1;
        }
    }
    if installed + custom == 2 {
        if custom > 0 {
            "           window list: OK   current and background indicators active, with custom theme placement".to_owned()
        } else {
            "           window list: OK   current and background formats show this window's Claude panes".to_owned()
        }
    } else if custom > 0 || changed > 0 {
        format!("           window list: INFO {installed} decorator(s), {custom} custom placement(s); {changed} user override(s) preserved")
    } else {
        "           window list: WARN formats are not decorated; start a Claude session in this pane".to_owned()
    }
}

/// Put `set-titles` and `set-titles-string` back the way SessionStart found them,
/// and remove every option we wrote.
///
/// Only uninstall does this, never SessionEnd: the options are server-wide and
/// another claude window may still be painting into them.
///
/// Returns the lines to report. Restoring is possible only from inside tmux, and
/// saying so is better than claiming success.
pub fn uninstall() -> Vec<String> {
    let Some(t) = Tmux::detect() else {
        return vec![
            "tmux:     not inside tmux. If a tmux server still carries our \
             set-titles-string,"
                .to_owned(),
            "          run `tabstatus uninstall` from inside it, or restart it - the \
             options do not outlive the server."
                .to_owned(),
        ];
    };
    // OPT_PREV_STRING is a user's own format string and may hold newlines, so it
    // goes LAST.
    let Ok(saved) = ask(
        &t,
        &[
            &format!("#{{{OPT_SAVED}}}"),
            &format!("#{{{OPT_PREV_TITLES}}}"),
            &others_expr(&t),
            &format!("#{{{OPT_PREV_STRING}}}"),
        ],
    ) else {
        return vec!["tmux:     the server did not answer, so nothing was changed.".to_owned()];
    };
    let (flag, prev_titles, prev_string) = (&saved[0], &saved[1], &saved[3]);
    // Uninstalling stops every claude window on this server, not just this one,
    // and said nothing about it. Their records stay in their pane titles and then
    // show up raw in whatever title the restored string renders - the hazard
    // doctor already names.
    // Window wrappers depend on our server glyph options, so restore their
    // original labels before removing that shared data.
    let mut out = match restore_window_status() {
        Ok(lines) => lines,
        Err(_) => return vec!["tmux:     could not finish window-format restoration; shared title options were left intact.".to_owned()],
    };
    let others = saved[2].matches('1').count();
    if others > 0 {
        out.push(format!(
            "tmux:     {others} other claude pane(s) in this session are still \
             painting. Their tab cells"
        ));
        out.push(
            "          stop updating now, and their pane titles keep a raw record \
             until those sessions end."
                .to_owned(),
        );
    }
    // Ours, and not in the batch below: `client-attached[N]` is an array option an
    // old tmux cannot parse, and an error inside a `;`-chained batch stops the
    // commands after it - which would be the restore. It takes the armed record
    // with it - a SESSION option, which the server-scope sweep below cannot reach.
    disarm(&t);
    let mut members = command();
    members.args(["set-option", "-u"]);
    t.target(&mut members);
    members.args(["--", lifecycle::MEMBERS]);
    run(members);

    // Ours come off whatever happens: they are our own namespace, and an orphaned
    // @cctab_title is exactly what would make a later doctor lie. Keep the saved
    // title until the final batch so restoration never reimports diagnostic
    // stdout (tmux 3.4 escapes dollar signs there).
    let mut c = command();
    for o in OURS {
        if flag == "1" && o == OPT_PREV_STRING {
            continue;
        }
        if c.get_args().next().is_some() {
            c.arg(";");
        }
        c.arg("set").arg("-su").arg("--").arg(o);
    }

    // The user's pair is only touched if WE are the ones who changed it. Without
    // this test, an uninstall on a server no SessionStart had ever reached would
    // unset a set-titles-string the user had set themselves - the sharpest way to
    // get this wrong, because nothing would report it.
    if flag != "1" {
        run(c);
        out.push(
            "tmux:     our own options are removed. set-titles and \
             set-titles-string were not ours to change, so they are untouched."
                .to_owned(),
        );
        return out;
    }

    // Unsetting first is what lets the DEFAULT case be restored as unset rather
    // than pinned to a literal a later tmux may change its mind about. It is safe
    // here and only here, because `flag` proves we are the ones who set them.
    c.arg(";").arg("set").arg("-gu").arg("set-titles-string");
    c.arg(";").arg("set").arg("-gu").arg("set-titles");
    c.arg(";")
        .arg("display-message")
        .arg("-p")
        .arg("#{set-titles}\n#{set-titles-string}");
    let Ok(defaults) = capture(c) else {
        out.push(
            "tmux:     the server did not answer, so the restore is incomplete - \
             restart the tmux server to be sure."
                .to_owned(),
        );
        return out;
    };
    let (def_titles, def_string) = split_first_line(&defaults);

    // A SAVED STRING THAT POINTS AT OUR OWN OPTION must not be put back: the batch
    // above has already unset @cctab_title, so restoring it would leave the outer
    // tab PERMANENTLY BLANK while uninstall reported "restored", recoverable only
    // by a server restart. Two real routes in, both measured: the user pinned the
    // pair from `tabstatus tmux-format` into ~/.tmux.conf - which the README
    // invites - so it was already in force when the first SessionStart saved it;
    // or @cctab_saved was lost some way and a later SessionStart recorded OUR
    // string as theirs.
    //
    // The test is the whole namespace and not equality with @cctab_string, which
    // is the narrower question: a server whose last SessionStart installed the
    // OTHER glyph position holds a different string of ours, and it points at the
    // same option.
    let ours_saved = prev_string.contains("@cctab_");
    let mut c = command();
    let mut restored: Vec<String> = Vec::new();
    if *prev_titles != def_titles {
        let v = if prev_titles == "1" { "on" } else { "off" };
        c.arg("set").arg("-g").arg("set-titles").arg(v);
        restored.push(format!("set-titles {v}"));
    }
    if *prev_string != def_string && !ours_saved {
        if !restored.is_empty() {
            c.arg(";");
        }
        c.arg("set").arg("-Fg").arg("set-titles-string")
            .arg(format!("#{{{OPT_PREV_STRING}}}"));
        restored.push("set-titles-string".to_owned());
    }
    if c.get_args().next().is_some() {
        c.arg(";");
    }
    c.args(["set", "-su", "--", OPT_PREV_STRING]);
    run(c);
    match (restored.is_empty(), ours_saved) {
        (true, false) => out.push(
            "tmux:     restored - set-titles and set-titles-string had been at \
             tmux's own defaults, and are unset again."
                .to_owned(),
        ),
        // Saying "restored, they were at tmux's defaults" here would be false of
        // the string: it was one of ours, and the lines below say so.
        (true, true) => out.push(
            "tmux:     our own options are removed, and set-titles-string is back \
             at tmux's default."
                .to_owned(),
        ),
        (false, _) => {
            out.push(format!("tmux:     restored {}", restored.join(" and ")));
        }
    }
    if ours_saved {
        out.push(
            "          The saved set-titles-string was one of OURS - it points at \
             @cctab_title, which is"
                .to_owned(),
        );
        out.push(
            "          now unset - so it was NOT put back; set-titles-string is at \
             tmux's default."
                .to_owned(),
        );
    }
    out
}

/// Every option SessionStart writes, so that uninstall cannot forget one.
///
/// The sweep that reads this list unsets at SERVER scope. [`OPT_ARMED`] is written
/// at SESSION scope and is removed by [`disarm`], which uninstall calls first; it
/// is listed here anyway, because the list is also the answer to "what is in our
/// namespace", and because a `@cctab_armed` someone set at server scope by hand
/// would otherwise outlive an uninstall.
const OURS: [&str; 16] = [
    OPT_GW,
    OPT_GA,
    OPT_GI,
    OPT_GP,
    OPT_TW,
    OPT_TA,
    OPT_TG,
    OPT_TITLE,
    OPT_STRING,
    OPT_SAVED,
    OPT_PREV_STRING,
    OPT_PREV_TITLES,
    OPT_EXE,
    OPT_ARMED,
    OPT_WINDOW_STRIP,
    OPT_WINDOW_COLOR,
];

/// Read several options in ONE invocation, one per line.
///
/// `display-message -p` and not `show -sv`: `show` EXITS 1 on an option that was
/// never set (`invalid option: @x`), so a code path that reads its status cannot
/// tell "unset" from "the server is gone", while a format yields empty for the
/// first and fails only for the second.
///
/// The LAST expression may expand to something holding newlines of its own - a
/// user's format string can - so everything after the first `n - 1` lines belongs
/// to it. Put the risky one last.
fn ask(t: &Tmux, exprs: &[&str]) -> Result<Vec<String>, Fail> {
    let mut c = command();
    c.arg("display-message").arg("-p");
    t.target(&mut c);
    c.arg(exprs.join("\n"));
    let out = capture(c)?;
    let mut fields: Vec<String> = Vec::with_capacity(exprs.len());
    let mut rest = out.as_str();
    for _ in 0..exprs.len() - 1 {
        let (head, tail) = rest.split_once('\n').ok_or(Fail::NoAnswer)?;
        fields.push(head.to_owned());
        rest = tail;
    }
    fields.push(rest.strip_suffix('\n').unwrap_or(rest).to_owned());
    Ok(fields)
}

/// The first line, and everything after it with one trailing newline removed.
fn split_first_line(s: &str) -> (&str, &str) {
    let (head, tail) = s.split_once('\n').unwrap_or((s, ""));
    (head, tail.strip_suffix('\n').unwrap_or(tail))
}

// --- doctor ------------------------------------------------------------------

/// Inspect server title settings, clients, and this pane's window decorators.
pub fn report(cfg: &Config) -> Vec<String> {
    let mut out = Vec::new();
    if config::flag("CCTAB_NO_TMUX") {
        out.push("tmux:      off   CCTAB_NO_TMUX is set, so nothing tmux-specific runs".to_owned());
        return out;
    }
    let Some(raw) = config::var_nonempty("TMUX") else {
        out.push("tmux:      -    not inside tmux ($TMUX is not set)".to_owned());
        return out;
    };
    let shown = String::from_utf8_lossy(raw.as_encoded_bytes()).into_owned();
    // The handle from the stack doctor resolved, not a second `Tmux::detect()`: the
    // two cannot disagree today, and the one that is reached through `Config` is the
    // one the paint path used.
    let Some(t) = cfg.stack.tmux() else {
        out.push(format!(
            "tmux:      WARN $TMUX={shown} is not <socket>,<pid>,<session>, so it is \
             treated as NOT tmux"
        ));
        out.push(
            "           A stale exported $TMUX does this. Unset it, or the tab keeps \
             the plain title."
                .to_owned(),
        );
        return out;
    };
    // `#{==:A,B}` splits on the comma in the FORMAT TEXT, not in the expanded
    // values, so a user's set-titles-string holding commas compares correctly.
    let ours = format!("#{{?#{{==:#{{set-titles-string}},#{{{OPT_STRING}}}}},x,y}}");
    // A user's hook value can hold a newline of its own, so it goes LAST - ask()'s
    // rule. @cctab_string cannot: we wrote it, and it is one of two constants.
    let f = match ask(
        &t,
        &[
            "#{version}",
            "#{socket_path}",
            "#{status}",
            "#{status-interval}",
            "#{set-titles}",
            &ours,
            &format!("#{{{OPT_SAVED}}}"),
            &format!("#{{{OPT_STRING}}}"),
            &format!("#{{{OPT_ARMED}}}"),
            &format!("#{{{HOOK}}}"),
        ],
    ) {
        Ok(f) => f,
        Err(e) => {
            out.push(format!(
                "tmux:      FAIL $TMUX is set but {}",
                match e {
                    Fail::NoBinary => "tmux(1) is not on PATH, so nothing can be \
                                       configured or asked",
                    Fail::NoAnswer => "the socket it names is gone - a stale exported \
                                       $TMUX does this",
                }
            ));
            out.push(format!("           $TMUX={shown}"));
            out.push(
                "           The record in the pane title is then never consumed and \
                 shows up raw in tmux's own #T."
                    .to_owned(),
            );
            return out;
        }
    };
    let get = |i: usize| f.get(i).map_or("", String::as_str);
    out.push(format!(
        "tmux:      OK   tmux {} on {}, pane {}",
        get(0),
        get(1),
        t.pane
            .as_ref()
            .map_or("(no $TMUX_PANE)".to_owned(), |p| String::from_utf8_lossy(
                p.as_encoded_bytes()
            )
            .into_owned())
    ));
    let (status, interval) = (get(2), get(3));
    if status == "on" && interval != "0" {
        out.push(format!(
            "           decay: OK   status on, status-interval {interval}s - the tab \
             re-renders on that timer"
        ));
    } else {
        out.push(format!(
            "           decay: WARN status {status}, status-interval {interval} - the \
             timer that re-renders the tab is OFF"
        ));
        out.push(
            "                  A stale glyph then survives until the next paint. \
             Remedy: set -g status on ; set -g status-interval 5"
                .to_owned(),
        );
    }
    if get(4) != "1" {
        out.push(
            "           title: FAIL set-titles is off, so tmux sends the outer \
             terminal no title at all"
                .to_owned(),
        );
        // Blaming a session-local override on a server no SessionStart has ever
        // reached prescribes a no-op, and contradicts the line printed just below.
        out.push(if get(6) == "1" {
            "                  SessionStart set it globally, so a session-local \
             `set-titles off` is beating that. Remedy: set -u set-titles"
                .to_owned()
        } else {
            "                  No SessionStart has configured this server, so it is \
             still at tmux's own default off. Remedy: start a claude session here."
                .to_owned()
        });
    }
    out.push(format!(
        "           title: {}",
        match (get(5), get(6)) {
            ("x", _) => "OK   set-titles-string is the one SessionStart installed",
            (_, "1") => "WARN set-titles-string is not ours any more, though we saved \
                         the previous one - something else set it after us",
            _ => "WARN set-titles-string is not ours and nothing is saved - no \
                  SessionStart has run on this server yet",
        }
    ));
    if let Ok(formats) = ask(&t, &[&format!("#{{{OPT_TITLE}}}"), &format!("#{{{OPT_WINDOW_STRIP}}}")]) {
        if formats.iter().any(|f| !f.contains(HAS_BACKGROUND)) {
            out.push("           background: WARN installed formats cannot reliably display ct2 background work; update all plugin copies and start a new Claude session to refresh this server".to_owned());
        } else {
            out.push("           background: OK   ct1/ct2 consumers installed; known background never expires".to_owned());
        }
    }
    // The glyph LAYOUT is server-wide too, and the `ours` test above can never
    // catch a drift: the SessionStart that changed the layout rewrote
    // @cctab_string in the same batch, so the two always agree. Comparing what
    // THIS session would install against what is installed does catch it - and it
    // is the Konsole case that matters, where a prefix strip is elided away.
    let want_string = set_titles_string(cfg.glyph_pos);
    if get(5) == "x" && get(7) != want_string {
        out.push(format!(
            "           layout: WARN the server has the strip {}, but this session \
             would install it {}",
            end_of(get(7)),
            end_of(want_string)
        ));
        out.push(
            "                   CCTAB_TERMINAL and CCTAB_GLYPH_POS are server-wide \
             and the LAST SessionStart wins. Set them the same for every claude here."
                .to_owned(),
        );
    }
    // THE ARMED RECORD - rung 1, and which rung would answer if it were empty.
    // Printed for every session inside tmux and not only in Konsole mode, because
    // "this tab is armed and nothing here can say by whom" is the shape of the
    // defect, and a line that only spoke when the environment already agreed could
    // never show it.
    //
    // Labelled `restore:` and not `record:`, because doctor already prints a
    // column-zero `record:` for the state directory and two labels reading the
    // same in one report is a reader's problem, not a naming preference. It also
    // names the ACT a user is looking for when a tab is stuck.
    let record = Armed::resolve(
        armed_value(get(8)),
        Support::Unsupported("doctor has no session id, so it cannot read a state record"),
        cfg.stack.leaf,
    );
    out.push(format!(
        "           restore: {}",
        match record.surface().filter(|s| s.caps().arming.is_some()) {
            Some(s) => format!("{}, {}", s.caps().name, record.source().why()),
            None => format!("no arming policy, {}", record.source().why()),
        }
    ));
    if cfg.stack.leaf == Surface::Konsole {
        out.push(format!(
            "           arm: {}",
            match get(9) {
                h if h == ARM_HOOK =>
                    "OK   client-attached attempts Konsole arming on every \
                     reattach",
                "" => "WARN no client-attached hook, so a detach/reattach lands in an \
                       un-armed tab. Remedy: /clear, which re-runs SessionStart",
                _ => "WARN client-attached[1971] is not ours, so a reattach may land \
                      in an un-armed tab",
            }
        ));
    }
    let mut c = command();
    c.arg("list-clients");
    t.target(&mut c);
    c.arg("-F")
        .arg("#{client_tty} #{client_termname} #{?#{m:*title*,#{client_termfeatures}},HASTITLE,NOTITLE}");
    match capture(c).as_deref().map(str::trim) {
        Ok("") | Err(_) => out.push(
            "           client: none attached, so nothing is being painted right now"
                .to_owned(),
        ),
        Ok(s) => {
            for line in s.lines() {
                out.push(format!("           client: {line}"));
                if line.ends_with("NOTITLE") {
                    out.push(
                        "                   tmux is not granting that terminal the \
                         `title` feature, so it emits no OSC 0. Remedy:"
                            .to_owned(),
                    );
                    out.push(
                        "                   set -as terminal-features \",$TERM:title\" \
                         - for an older tmux; measured, 3.7c grants it to every client \
                         that can attach at all."
                            .to_owned(),
                    );
                }
            }
        }
    }
    out.push(format!(
        "           konsole: {}",
        if cfg.stack.leaf == Surface::Konsole {
            "on   CCTAB_TERMINAL=konsole - OSC 50 goes to each attached client's pty"
        } else {
            "off  set CCTAB_TERMINAL=konsole when the outer terminal is Konsole \
             (KONSOLE_* does not survive ssh)"
        }
    ));
    out.push(format!(
        "           ttl: working {}, waiting {}, gone {} (0 = never)",
        shown_ttl("CCTAB_TTL_WORKING", DEFAULT_TTL_WORKING),
        shown_ttl("CCTAB_TTL_WAITING", DEFAULT_TTL_WAITING),
        shown_ttl("CCTAB_TTL_GONE", DEFAULT_TTL_GONE)
    ));
    out.push(report_window_status(&t));
    out
}

/// Which end of the tab a `set-titles-string` of ours puts the strip on.
fn end_of(sts: &str) -> &'static str {
    if sts == set_titles_string(GlyphPos::Suffix) {
        "last"
    } else {
        "first"
    }
}

/// A TTL as doctor shows it. The ladder's own value for "never" is INT32_MAX, and
/// printing that at a user who typed `0` made them recognise 2147483647 to know
/// their setting had taken, on a line whose own legend says `0 = never`.
fn shown_ttl(key: &str, default: u32) -> String {
    shown(&ttl(key, default))
}

fn shown(v: &str) -> String {
    if v == NEVER.to_string() {
        "never".to_owned()
    } else {
        format!("{v}s")
    }
}

/// `tabstatus tmux-format` - print what SessionStart would install, so the test
/// suite can drive a private server with exactly the string the product uses,
/// and so a user can paste it into `~/.tmux.conf` if they would rather pin it.
pub fn format_lines(cfg: &Config) -> [String; 2] {
    [
        set_titles_string(cfg.glyph_pos).to_owned(),
        title_format(cfg.glyph_pos),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_writes_distinguish_skips_and_continue_after_failure() {
        assert!(!write_clients("", b"arm", |_, _| panic!("detached")).unwrap());
        assert!(!write_clients("skipped\n", b"arm", |_, _| Ok(false)).unwrap());
        assert!(write_clients("skipped\nwritten\n", b"arm", |p, _| {
            Ok(p == Path::new("written"))
        }).unwrap());
        let mut seen = Vec::new();
        let result = write_clients("failed\nwritten\nskipped\nfailed-again\n", b"arm", |p, bytes| {
            assert_eq!(bytes, b"arm");
            seen.push(p.to_path_buf());
            match p.to_str().unwrap() {
                "failed" => Err(io::Error::other("first failure, possibly partial")),
                "failed-again" => Err(io::Error::other("second failure")),
                "written" => Ok(true),
                _ => Ok(false),
            }
        });
        assert_eq!(seen.len(), 4);
        assert_eq!(result.unwrap_err().to_string(), "first failure, possibly partial");
    }

    #[test]
    fn a_tmux_env_is_a_socket_path_and_two_numbers() {
        assert!(is_tmux_env(b"/tmp/tmux-1000/default,1234,0"));
        assert!(is_tmux_env(b"/t,1,0"));
        // Not one.
        assert!(!is_tmux_env(b""));
        assert!(!is_tmux_env(b"nonsense"));
        assert!(!is_tmux_env(b"/tmp/s,1"));
        assert!(!is_tmux_env(b"/tmp/s,1,0,2"));
        assert!(!is_tmux_env(b"tmp/s,1,0"), "the socket path is absolute");
        assert!(!is_tmux_env(b"/tmp/s,x,0"));
        assert!(!is_tmux_env(b"/tmp/s,,0"));
        // A socket path holding a comma is not one we can parse, and saying so is
        // better than talking to the wrong server.
        assert!(!is_tmux_env(b"/tmp/a,b/s,1,0"));
    }

    #[test]
    fn the_record_names_the_state_and_session_end_has_none() {
        let c = |p| carrier_at(p, "~/x", 1_700_000_000);
        assert_eq!(c(Paint::Line(Glyph::Working)), "~/x ct1 w 1700000000");
        assert_eq!(c(Paint::Line(Glyph::Waiting)), "~/x ct1 a 1700000000");
        assert_eq!(c(Paint::Line(Glyph::Idle)), "~/x ct1 i 1700000000");
        assert_eq!(c(Paint::SessionStart), "~/x ct1 i 1700000000");
        assert_eq!(c(Paint::SessionEnd), "");
        // The record is what the FORMAT matches, so the two cannot drift apart.
        let place = "srv:~/a b,c}d#{host}e:f|g";
        assert!(carrier_at(Paint::Line(Glyph::Waiting), place, 5).starts_with(place));
    }

    /// The format is one constant per glyph position, so it can be pinned here
    /// rather than believed. What it RENDERS is pinned by tests/run.sh, against a
    /// private tmux server.
    #[test]
    fn the_generated_format_is_a_strip_and_a_label_either_way_round() {
        let label = format!("#{{?{IS},{LOC},#{{session_name}}:#{{window_index}}:#{{window_name}}}}");
        let label = label.as_str();
        let prefix = title_format(GlyphPos::Prefix);
        let suffix = title_format(GlyphPos::Suffix);
        // One strip, one label, one separator, and the separator is the ONLY
        // place the two variants differ - which is what makes the two trims in
        // `set_titles_string` symmetric.
        assert!(prefix.starts_with("#{W:#{P:"));
        assert!(prefix.ends_with(&format!(" {label}")));
        assert!(suffix.starts_with(label));
        assert!(suffix[label.len()..].starts_with(" #{W:#{P:"));
        assert_eq!(prefix.len(), suffix.len());
        assert_eq!(prefix.matches("#{W:#{P:").count(), 1);
        assert_eq!(suffix.matches("#{W:#{P:").count(), 1);
        // `both` is not doubled: a strip is not a marker, and six cells at each
        // end is the whole tab.
        assert_eq!(prefix, title_format(GlyphPos::Both));
        // No colon anywhere inside an `s` modifier's arguments: measured, one
        // silently empties the WHOLE expansion, with any delimiter.
        for f in [&prefix, &suffix] {
            for (i, _) in f.match_indices("#{s") {
                let end = f[i..].find(":#{").expect("an s modifier ends at :#{");
                assert!(
                    !f[i + 3..i + end].contains(':'),
                    "a colon inside an s modifier's arguments: {}",
                    &f[i..i + end]
                );
            }
        }
        // The trims are what drop the separator when no claude is running, so
        // each variant must get the one that matches its layout.
        assert_eq!(set_titles_string(GlyphPos::Prefix), "#{s|^ ||:#{T:@cctab_title}}");
        assert_eq!(set_titles_string(GlyphPos::Both), "#{s|^ ||:#{T:@cctab_title}}");
        assert_eq!(set_titles_string(GlyphPos::Suffix), "#{s| $||:#{T:@cctab_title}}");
    }

    #[test]
    fn window_decorators_keep_time_expansion_out_of_the_original_label() {
        for f in &WINDOW_FORMATS {
            let wrapper = window_status_format(f.previous);
            assert!(wrapper.ends_with(&format!("#{{E:{}}}", f.previous)));
            assert!(!wrapper.contains(&format!("#{{T:{}}}", f.previous)));
            assert!(wrapper.contains(&format!("#{{T:{OPT_WINDOW_STRIP}}}")));
            assert!(!wrapper.contains("window_name"));
            assert!(!wrapper.contains("rename"));
        }
    }

    #[test]
    fn window_targets_are_numeric_ids_not_interpolated_names() {
        assert!(valid_window_id("@0"));
        assert!(valid_window_id("@123"));
        for invalid in ["", "@", "%0", "@1;kill-server", "a window", "@1\n"] {
            assert!(!valid_window_id(invalid), "{invalid:?}");
        }
    }

    #[test]
    fn known_background_bypasses_disappearance_and_is_the_decay_fallback() {
        let c = cell();
        assert!(c.contains(HAS_BACKGROUND));
        assert!(c.contains("@cctab_gp"));
        assert!(c.contains("@cctab_tw"));
        assert!(c.contains("@cctab_ta"));
        assert!(c.contains("@cctab_tg"));
        assert!(!c.contains("#{>:"));
    }

    #[test]
    fn a_value_spliced_into_a_format_is_made_inert() {
        // Both, because format text is strftime'd and THEN expanded.
        assert_eq!(inert("%0"), "%%0");
        assert_eq!(inert("%12"), "%%12");
        assert_eq!(inert("#{host}"), "##{host}");
        assert_eq!(inert("a%b#c"), "a%%b##c");
        assert_eq!(inert("plain"), "plain");
        assert_eq!(inert(""), "");
    }

    /// The re-arm hook is the one place a path could have been spliced into a
    /// format, and the one place an array option could have been clobbered whole.
    #[test]
    fn the_re_arm_hook_carries_no_path_and_touches_one_index() {
        // The path travels as an OPTION because a hook's value is PARSED when it is
        // set: measured, `$rd` inside tmux's double quotes is expanded THERE, so a
        // path holding `$` would lose a piece of itself before anything ran.
        assert!(ARM_HOOK.contains("'#{@cctab_exe}'"), "{ARM_HOOK}");
        assert!(ARM_HOOK.contains("tmux-arm"));
        assert!(ARM_HOOK.contains("'#{client_tty}'"));
        // Nothing in it may be expanded at set time, and nothing needs escaping.
        assert!(!ARM_HOOK.contains('$'));
        assert!(!ARM_HOOK.contains('\\'));
        // ONE index of the array: measured, a bare `set-hook -g client-attached`
        // replaces the WHOLE array, and a user's own hooks live in it.
        assert!(HOOK.starts_with("client-attached["), "{HOOK}");
        assert!(HOOK.ends_with(']'));
        // Every option SessionStart writes has to be in OURS or uninstall leaks it,
        // and @cctab_exe is the one this slice added.
        assert!(OURS.contains(&OPT_EXE));
        assert!(OURS.contains(&OPT_WINDOW_STRIP));
        assert!(OURS.contains(&OPT_ARMED));
        assert_eq!(OURS.len(), 16);
    }

    /// The armed record's grammar, which is the whole of rung 1's reliability.
    ///
    /// Three answers and not two: "no retained arming policy" is a STATEMENT, and the
    /// empty string has to keep meaning "no record here", or every tmux server
    /// that predates this option - and every session whose SessionStart hook never
    /// ran - would read as a session that armed nothing and silently lose its
    /// restore.
    #[test]
    fn the_armed_record_reads_an_unknown_value_as_absent_and_never_as_a_surface() {
        let seen = |v: &str| matches!(armed_value(v), Support::Available(Some(s)) if s == Surface::Konsole);
        assert!(seen("konsole"));
        // The option is written from `caps().name`, so it arrives canonical; the
        // reader is case-insensitive anyway, because `CCTAB_TERMINAL` taught this
        // crate what a byte compare costs.
        assert!(seen("KONSOLE"));
        assert!(matches!(armed_value(ARMED_NONE), Support::Available(None)));
        // Everything else has to fall THROUGH to the next rung rather than choose
        // bytes: a name from a later version, a name from no version at all, a
        // hand-edited value, and the empty string a server that never wrote one
        // gives back.
        for miss in ["", "konsol", "konsolex", "kitty-next", "-x", "1", " konsole"] {
            assert!(!matches!(armed_value(miss), Support::Available(Some(_))), "{miss}");
        }
        // And a name this build DOES know but which arms nothing is still read
        // back faithfully - the routing, not the reader, is what declines to send
        // bytes for it.
        assert!(matches!(armed_value("wezterm"), Support::Available(Some(s)) if s == Surface::WezTerm));
        assert!(Surface::WezTerm.caps().arming.is_none());
        // No surface may ever be named `-`, or the sentinel would become a row.
        assert!(surface::by_name(ARMED_NONE.as_bytes()).is_none());
    }

    #[test]
    fn a_ttl_of_never_is_reported_as_never_and_not_as_the_sentinel() {
        assert_eq!(shown(&NEVER.to_string()), "never");
        assert_eq!(shown("1200"), "1200s");
        assert_eq!(shown("0"), "0s", "0 never reaches here - ttl() maps it to NEVER");
    }

    #[test]
    fn the_layout_warning_names_the_end_the_strip_is_on() {
        assert_eq!(end_of(set_titles_string(GlyphPos::Suffix)), "last");
        assert_eq!(end_of(set_titles_string(GlyphPos::Prefix)), "first");
        assert_eq!(end_of(set_titles_string(GlyphPos::Both)), "first");
        // A string that is not ours at all is reported as the default end rather
        // than guessed at; the branch is only reached when it IS ours.
        assert_eq!(end_of("MY OWN TITLE"), "first");
    }
}
