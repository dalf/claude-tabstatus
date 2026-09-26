//! Inside tmux the tab title stops being a latch and becomes a FUNCTION OF TIME.
//!
//! tmux re-evaluates `set-titles-string` on a timer and re-emits the result to
//! the outer terminal as an OSC 0. `%s` - the epoch - is available inside that
//! format, because tmux runs strftime over it before expanding `#{}`. So if the
//! paint carries the moment it happened, the format can render one glyph while
//! the paint is fresh, the idle glyph once it is stale, and nothing at all once
//! it is old - with no process running, no hook firing and nothing to go wrong.
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
//!
//! The glyph is NOT in it. Three measurements forced that: `#{=1:}` counts
//! COLUMNS and returns the EMPTY string for a width-2 emoji, so a glyph cannot be
//! sliced back out of the title; the glyph's POSITION already varies with
//! `CCTAB_GLYPH_POS`; and leaving it in would paint the current pane's glyph
//! twice, once in the strip and once in the label. A state LETTER is one ASCII
//! column, extractable from either end, and the glyph it stands for is looked up
//! in a server option the same SessionStart wrote.
//!
//! THE HOT PATH EXECS NOTHING. Only SessionStart runs tmux, and SessionEnd only
//! in Konsole mode. Measured on this machine: one `tmux set-option` costs 2.84ms
//! against a 0.37ms fork floor, and the whole twelve-command SessionStart batch
//! costs 3.1ms end to end - thirteen commands and 4.9ms in Konsole mode, where it
//! also lists the clients and writes the arming. The cost is the fork and the
//! socket round trip, not the commands, which is why batching is free and why a
//! per-tool-call exec would be a tenfold regression on a binary that runs in
//! 370us.
//!
//! WHAT IS DELIBERATELY NOT BUILT: the tmux STATUS LINE. A glyph per window in
//! `window-status-format` uses this same carrier and [`cell`] drops into it
//! unchanged, but the target here is the TAB, and a status line is the user's
//! own real estate. Left as a seam.

use crate::config::{self, Config, GlyphPos, Terminal};
use crate::edge::{Glyph, Paint};
use crate::emit;
use std::ffi::{OsStr, OsString};
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

/// The tag that marks a pane title as ours. Version 1 of the wire, so a future
/// shape can change it and let an old format ignore the new panes rather than
/// misread them.
const TAG: &str = "ct1";

// The server options SessionStart writes and the generated format reads. They
// are options rather than text spliced into the format because an option's VALUE
// is substituted literally: measured, `#{host}` and `%H` reach the tab intact
// through `#{@opt}` inside `#{T:}`, where the same bytes written into the format
// itself would have been expanded. That makes a hostile CCTAB_GLYPH_* inert.
const OPT_GW: &str = "@cctab_gw";
const OPT_GA: &str = "@cctab_ga";
const OPT_GI: &str = "@cctab_gi";
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
/// This binary's own path, for the re-arm hook to run. An option because an
/// option's value is substituted LITERALLY, which is what spares the hook every
/// layer of quoting; see [`ARM_HOOK`].
const OPT_EXE: &str = "@cctab_exe";

/// Is this pane carrying one of our records? ASCII and anchored at the end.
///
/// The `/r` flag needs the COLON form: `#{m/r|...}` parses as a single argument
/// and silently matches everything, which makes every shell pane a claude.
const IS: &str = "#{m/r: ct1 [wai] [0-9]+$,#{pane_title}}";
/// Seconds since the paint. `%s` is strftime's, expanded over the whole format
/// before `#{}` is, so every cell in one render shares one `now`.
///
/// The operands of `#{e|op|:}` are COMMA separated - the colon form yields the
/// empty string - and the test below must be `#{e|>|:}` and never `#{>:}`, which
/// is a STRING compare: measured, `#{>:10,9}` is 0.
const AGE: &str = "#{e|-|:%s,#{s|^.* ||:#{pane_title}}}";
/// The state letter. One ASCII column, so `#{=1:}` - which counts COLUMNS, and
/// returns EMPTY for a width-2 emoji - is safe here and nowhere near a glyph.
const ST: &str = "#{=1:#{s|^.* ct1 ||:#{pane_title}}}";
/// The location, with the record taken back off.
///
/// No colon may appear anywhere inside an `s` modifier's arguments: measured,
/// one silently empties the WHOLE expansion, with any delimiter and with `##:`
/// escaping. That is why the wire tag is colon-free.
const LOC: &str = "#{s| ct1 [wai] [0-9]*$||:#{pane_title}}";

/// A tier whose deadline has passed and whose remedy is the idle glyph.
fn ladder(deadline: &str, live: &str) -> String {
    format!("#{{?#{{e|>|:{AGE},#{{{deadline}}}}},#{{{OPT_GI}}},#{{{live}}}}}")
}

/// One pane's contribution: its glyph while fresh, the idle glyph once stale,
/// and nothing at all once it is old or was never ours.
///
/// Idle has no freshness tier of its own, because white is already what the
/// other two decay INTO - there is nothing left for it to become.
fn cell() -> String {
    let w = ladder(OPT_TW, OPT_GW);
    let a = ladder(OPT_TA, OPT_GA);
    let by_state = format!("#{{?#{{==:{ST},w}},{w},#{{?#{{==:{ST},a}},{a},#{{{OPT_GI}}}}}}}");
    let alive = format!("#{{?#{{e|>|:{AGE},#{{{OPT_TG}}}}},,{by_state}}}");
    format!("#{{?{IS},{alive},}}")
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
        if !is_tmux_env(raw.as_bytes()) {
            return None;
        }
        Some(Tmux {
            pane: config::var_nonempty("TMUX_PANE"),
        })
    }

    /// `-t <our pane>`, so a command lands on our session and not on whichever
    /// one tmux would have picked.
    fn target(&self, c: &mut Command) {
        if let Some(p) = &self.pane {
            c.arg("-t").arg(p);
        }
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
    carrier_at(paint, place, now())
}

fn carrier_at(paint: Paint, place: &str, epoch: u64) -> String {
    let Some(g) = paint.glyph() else {
        return String::new();
    };
    let state = match g {
        Glyph::Working => 'w',
        Glyph::Waiting => 'a',
        Glyph::Idle => 'i',
    };
    format!("{place} {TAG} {state} {epoch}")
}

/// The epoch the record carries. `CCTAB_NOW` pins it, which is the only reason
/// a corpus case that goes through tmux can be frozen at all.
///
/// No width is imposed: every extraction below is anchored, not offset, so a
/// mocked `CCTAB_NOW=5` works exactly like a real ten-digit one.
fn now() -> u64 {
    epoch(config::var_nonempty("CCTAB_NOW").as_deref())
}

fn epoch(raw: Option<&OsStr>) -> u64 {
    if let Some(b) = raw.map(OsStr::as_bytes) {
        if b.len() <= 10 && digits(b) {
            return b.iter().fold(0u64, |a, c| a * 10 + u64::from(c - b'0'));
        }
    }
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// Seconds before a state decays. `0` disables the tier, which is expressed as a
/// deadline no age can reach rather than as a second shape of format: the
/// generated string then stays one constant per glyph position.
const NEVER: &str = "2147483647";
/// MEASURED, not reasoned. `working` is repainted by `UserPromptSubmit`,
/// `PostToolUse` and `PostToolUseFailure` only, so during one long tool call
/// nothing paints at all - `PreToolUse` is matched to `AskUserQuestion` and
/// `ExitPlanMode` alone. Over the 91 most recent real transcripts on this
/// machine, 7709 WITHIN-TURN gaps between two successive `working` paints:
/// median 9.9s, p95 77s, p99 173s, p99.9 646s, and 39 gaps (0.51%) over 300s -
/// the old default. 1200s covers 99.95% of them; the four above it are 3343s,
/// 5677s, 23700s and 27751s, which are sessions resumed the next day rather than
/// a tool call still running.
///
/// The asymmetry is the whole argument: a lingering blue says "still busy" about
/// a session that has finished, which the next paint corrects in seconds, while a
/// premature white says "finished, come back" about a build that is still
/// running - the exact class of lie this slice exists to remove, pointed the
/// other way. The 3600s disappear horizon still catches a genuinely stuck one.
const DEFAULT_TTL_WORKING: u32 = 1200;
const DEFAULT_TTL_WAITING: u32 = 900;
const DEFAULT_TTL_GONE: u32 = 3600;

/// One to six digits with no leading zero; `0` is "never"; anything else is the
/// default. The same grammar `Cap` uses, for the same reason - `str::parse`
/// would read `08` as 8 and `0000000` as 0.
fn ttl(key: &str, default: u32) -> String {
    ttl_of(config::var(key).as_deref(), default)
}

fn ttl_of(raw: Option<&OsStr>, default: u32) -> String {
    match raw.map(OsStr::as_bytes) {
        Some(b"0") => NEVER.to_owned(),
        Some(v)
            if (1..=6).contains(&v.len())
                && (b'1'..=b'9').contains(&v[0])
                && v.iter().all(u8::is_ascii_digit) =>
        {
            String::from_utf8_lossy(v).into_owned()
        }
        _ => default.to_string(),
    }
}

/// Configure the server, once, in ONE exec.
///
/// The save of the user's own `set-titles` pair is the FIRST command in the
/// batch and is guarded by "only if nothing is saved yet", so a second claude
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
pub fn session_start(cfg: &Config) {
    let Some(t) = &cfg.tmux else { return };
    let fmt = title_format(cfg.glyph_pos);
    let sts = set_titles_string(cfg.glyph_pos);
    let mut c = Command::new("tmux");
    set(&mut c, OPT_GW, cfg.glyph(Glyph::Working));
    set(&mut c, OPT_GA, cfg.glyph(Glyph::Waiting));
    set(&mut c, OPT_GI, cfg.glyph(Glyph::Idle));
    set(&mut c, OPT_TW, &ttl("CCTAB_TTL_WORKING", DEFAULT_TTL_WORKING));
    set(&mut c, OPT_TA, &ttl("CCTAB_TTL_WAITING", DEFAULT_TTL_WAITING));
    set(&mut c, OPT_TG, &ttl("CCTAB_TTL_GONE", DEFAULT_TTL_GONE));
    set(&mut c, OPT_TITLE, &fmt);
    set(&mut c, OPT_STRING, sts);
    c.arg(";").arg("if").arg("-F").arg(format!("#{{==:#{{{OPT_SAVED}}},}}")).arg(format!(
        "set -Fs {OPT_PREV_STRING} \"#{{set-titles-string}}\" ; \
         set -Fs {OPT_PREV_TITLES} \"#{{set-titles}}\" ; \
         set -s {OPT_SAVED} 1"
    ));
    c.arg(";").arg("set").arg("-g").arg("set-titles").arg("on");
    c.arg(";").arg("set").arg("-g").arg("set-titles-string").arg(sts);
    // LAST in the batch, deliberately: `client-attached[N]` is an array option,
    // which a tmux older than 3.0 has no syntax for, and measured, a command that
    // fails at the END of a `;`-chained batch leaves every command before it
    // applied. So an old tmux loses the re-arm and keeps the whole decay.
    match (cfg.terminal, exe_path()) {
        // A path with a single quote in it has no representation inside ARM_HOOK's
        // sh quoting, and a path that is not UTF-8 cannot go into a format at all.
        // Both drop the re-arm and keep everything else.
        (Terminal::Konsole, Some(exe)) => {
            set(&mut c, OPT_EXE, &exe);
            c.arg(";").arg("set-hook");
            t.target(&mut c);
            c.arg(HOOK).arg(ARM_HOOK);
        }
        // Not Konsole: take any hook of ours back off. The layout and the TTLs are
        // already "the last SessionStart on this server wins", and leaving the
        // arming in force while the strip moved back to the end Konsole elides is
        // the one combination that is worse than either. doctor's `arm: WARN` then
        // names /clear as the way back.
        _ => {
            c.arg(";").arg("set-hook").arg("-u");
            t.target(&mut c);
            c.arg(HOOK);
        }
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
/// [`emit::tty_write`] is the guard: /dev/pts or /dev/tty, a character device,
/// writable, and silent about any of that failing.
pub fn arm_tty(path: &OsStr) {
    emit::tty_write(Path::new(path), emit::KONSOLE_ARM);
}

/// Take the re-arm hook back off.
fn drop_hook(t: &Tmux) {
    let mut c = Command::new("tmux");
    c.arg("set-hook").arg("-u");
    t.target(&mut c);
    c.arg(HOOK);
    run(c);
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
pub fn arm_konsole(cfg: &Config) {
    if cfg.terminal != Terminal::Konsole {
        return;
    }
    to_clients(cfg, emit::KONSOLE_ARM);
}

/// Put the tab formats back, but only when no OTHER claude is left in this
/// session - the arming is per TAB, and inside tmux one tab holds every window.
///
/// Our own pane is excluded from the count rather than relied on to have been
/// cleared already: session end's empty title travels through the pty and tmux's
/// parser, and racing that would silently skip the restore.
pub fn session_end(cfg: &Config) {
    let Some(t) = &cfg.tmux else { return };
    if cfg.terminal != Terminal::Konsole {
        return;
    }
    let mut c = Command::new("tmux");
    c.arg("display-message").arg("-p");
    t.target(&mut c);
    c.arg(others_expr(t));
    if capture(c).is_ok_and(|s| s.contains('1')) {
        return;
    }
    to_clients(cfg, emit::KONSOLE_RESTORE);
    // The last claude in this session is going: nothing is left for a reattach to
    // re-arm, so the hook goes with it.
    drop_hook(t);
}

/// `1` once per pane of this session that carries a record and is not ours.
///
/// Our own pane is excluded rather than relied on to have been cleared already:
/// session end's empty title travels through the pty and tmux's parser, and
/// racing that would silently skip the restore.
fn others_expr(t: &Tmux) -> String {
    let mine = match &t.pane {
        Some(p) => inert(&String::from_utf8_lossy(p.as_bytes())),
        None => String::new(),
    };
    format!("#{{W:#{{P:#{{?#{{==:#{{pane_id}},{mine}}},,{IS}}}}}}}")
}

/// Write `bytes` to the pty of every client attached to OUR session.
fn to_clients(cfg: &Config, bytes: &[u8]) {
    let Some(t) = &cfg.tmux else { return };
    let mut c = Command::new("tmux");
    c.arg("list-clients");
    t.target(&mut c);
    c.arg("-F").arg("#{client_tty}");
    let Ok(out) = capture(c) else { return };
    for line in out.lines().filter(|l| !l.is_empty()) {
        emit::tty_write(Path::new(line), bytes);
    }
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

/// Run a tmux command and throw everything away.
///
/// stdout AND stderr go to /dev/null, and neither is belt and braces: stdout
/// would corrupt the hook protocol's one JSON line, and a `$TMUX` naming a
/// socket that is gone prints `error connecting to ...` on stderr and exits 1 in
/// about 4ms - which two golden-corpus cases, both expecting empty stderr, would
/// catch as a failure.
fn run(mut c: Command) {
    let _ = c
        .stdin(Stdio::null())
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
    let out = match c.stdin(Stdio::null()).stderr(Stdio::null()).output() {
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
    let mut out: Vec<String> = Vec::new();
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
    // commands after it - which would be the restore.
    drop_hook(&t);

    // Ours come off whatever happens: they are our own namespace, and an orphaned
    // @cctab_title is exactly what would make a later doctor lie.
    let mut c = Command::new("tmux");
    for o in OURS {
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
    let mut c = Command::new("tmux");
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
        c.arg("set").arg("-g").arg("set-titles-string").arg(prev_string);
        restored.push("set-titles-string".to_owned());
    }
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
            run(c);
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
const OURS: [&str; 12] = [
    OPT_GW,
    OPT_GA,
    OPT_GI,
    OPT_TW,
    OPT_TA,
    OPT_TG,
    OPT_TITLE,
    OPT_STRING,
    OPT_SAVED,
    OPT_PREV_STRING,
    OPT_PREV_TITLES,
    OPT_EXE,
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
    let mut c = Command::new("tmux");
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

/// Everything about the tmux situation, in two execs.
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
    let shown = String::from_utf8_lossy(raw.as_bytes()).into_owned();
    let Some(t) = Tmux::detect() else {
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
                p.as_bytes()
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
    if cfg.terminal == Terminal::Konsole {
        out.push(format!(
            "           arm: {}",
            match get(8) {
                h if h == ARM_HOOK =>
                    "OK   client-attached re-arms this tab's Konsole format on every \
                     reattach",
                "" => "WARN no client-attached hook, so a detach/reattach lands in an \
                       un-armed tab. Remedy: /clear, which re-runs SessionStart",
                _ => "WARN client-attached[1971] is not ours, so a reattach may land \
                      in an un-armed tab",
            }
        ));
    }
    let mut c = Command::new("tmux");
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
        if cfg.terminal == Terminal::Konsole {
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
    if v == NEVER {
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

    #[test]
    fn the_ttl_grammar_is_the_caps_grammar() {
        let t = |v: Option<&str>| ttl_of(v.map(OsStr::new), 300);
        assert_eq!(t(Some("0")), NEVER);
        assert_eq!(t(Some("1")), "1");
        assert_eq!(t(Some("999999")), "999999");
        // A leading zero, a seventh digit, a sign and a word are all the default.
        for bad in ["08", "0300", "1000000", "-1", "3.5", "nope", " 8", ""] {
            assert_eq!(t(Some(bad)), "300", "{bad:?}");
        }
        assert_eq!(t(None), "300");
    }

    /// The format is one constant per glyph position, so it can be pinned here
    /// rather than believed. What it RENDERS is pinned by tests/run.sh, against a
    /// private tmux server.
    #[test]
    fn the_generated_format_is_a_strip_and_a_label_either_way_round() {
        const LABEL: &str = "#{?#{m/r: ct1 [wai] [0-9]+$,#{pane_title}},\
                             #{s| ct1 [wai] [0-9]*$||:#{pane_title}},\
                             #{session_name}:#{window_index}:#{window_name}}";
        let prefix = title_format(GlyphPos::Prefix);
        let suffix = title_format(GlyphPos::Suffix);
        // One strip, one label, one separator, and the separator is the ONLY
        // place the two variants differ - which is what makes the two trims in
        // `set_titles_string` symmetric.
        assert!(prefix.starts_with("#{W:#{P:"));
        assert!(prefix.ends_with(&format!(" {LABEL}")));
        assert!(suffix.starts_with(LABEL));
        assert!(suffix[LABEL.len()..].starts_with(" #{W:#{P:"));
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
    fn the_three_states_each_decay_into_the_idle_glyph_and_then_into_nothing() {
        let c = cell();
        // working and waiting each have their own deadline, idle has none.
        assert_eq!(c.matches("@cctab_tw").count(), 1);
        assert_eq!(c.matches("@cctab_ta").count(), 1);
        assert_eq!(c.matches("@cctab_tg").count(), 1);
        // The disappear test comes FIRST, so an old pane costs one comparison.
        let gone = c.find("@cctab_tg").expect("a disappear tier");
        assert!(gone < c.find("@cctab_tw").expect("a working tier"));
        // The numeric comparison, never the string one.
        assert!(!c.contains("#{>:"));
        assert_eq!(c.matches("#{e|>|:").count(), 3);
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
        assert_eq!(OURS.len(), 12);
    }

    #[test]
    fn a_ttl_of_never_is_reported_as_never_and_not_as_the_sentinel() {
        assert_eq!(shown(NEVER), "never");
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

    #[test]
    fn the_epoch_can_be_pinned_for_a_test_and_bad_values_are_ignored() {
        let e = |v: Option<&str>| epoch(v.map(OsStr::new));
        assert_eq!(e(Some("1700000000")), 1_700_000_000);
        assert_eq!(e(Some("5")), 5);
        assert_eq!(e(Some("0")), 0);
        for bad in [None, Some(""), Some("12345678901"), Some("17000000x0"), Some("-1")] {
            assert!(e(bad) > 1_700_000_000, "{bad:?} must not be believed");
        }
    }
}
