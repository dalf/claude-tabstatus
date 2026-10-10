//! WAIT OWNERSHIP: who is waiting, and therefore whose completion clears it.
//!
//! This is the one module that breaks the stateless invariant, and it breaks it
//! for a reason no stateless test could reach. From a real capture
//! (`slice3/log/s4`, timings relative to SessionStart):
//!
//! ```text
//!   66.283  PreToolUse        tool_name=Agent      (main: launching a Task)
//!   66.300  SubagentStart     agent_id=aec99e1f
//!   68.946  Stop              background_tasks=[subagent:running:aec99e]
//!   69.960  PermissionRequest agent_id=aec99e1f    -> ORANGE
//!   75.983  Notification      permission_prompt    (the 6s backstop, NO agent_id)
//!   98.459  SubagentStop      agent_id=a8e90c10    (a GHOST: agent_type="")
//!  107.496  PostToolUse       agent_id=aec99e1f    (you approved; the tool ran)
//!  110.230  SubagentStop      agent_id=aec99e1f
//! ```
//!
//! Three facts in there decide the whole design:
//!
//!   * The main thread's `Stop` at 68.946 lands BEFORE the subagent's dialog at
//!     69.960. So a subagent `PermissionRequest` cannot be no-oped - that would
//!     paint idle while something genuinely wants your approval.
//!   * The event that resolves the dialog is the SUBAGENT's `PostToolUse` at
//!     107.496, which the stateless `working` filter discards, because it cannot
//!     tell "a background subagent's tool call, do not repaint over the dialog"
//!     from "the tool the dialog was about, the dialog is gone". The difference
//!     is not in the payload; it is in what came before.
//!   * `SubagentStop` fires at 98.459 for `a8e90c10` - an agent id with no
//!     matching `SubagentStart`, `agent_type` empty, nine seconds before the user
//!     answered. (The same ghost fires in `s9` right before a `SessionStart`
//!     `source=compact`, so the compaction summarizer is one of them.) A
//!     `SubagentStop` therefore clears a wait ONLY when its `agent_id` matches
//!     the recorded owner.
//!
//! So a wait is an OVERLAY on a BASE. The base is what the tab shows when
//! nothing is waiting; a wait covers it; clearing the last wait restores the
//! base. That is the shape the next two slices need as well - see the SEAM
//! section at the bottom - and it is why the record carries the base at all
//! rather than repainting `working` and calling it close enough.
//!
//! WHAT RETIRES A WAIT, which is the whole of the risk. An overlay nothing can
//! lift is worse than no overlay at all: while any wait is held neither
//! [`Session::progress`] nor [`Session::free`] paints, so a wait that outlives its
//! dialog freezes the tab ORANGE - and outside tmux there is no decay to catch it.
//! Every wait therefore has more than one retirement condition, combining owner
//! evidence with recovery heuristics for hooks that never arrive:
//!
//!   * the OWNER's own completion - the agent's `PostToolUse`, or its
//!     `SubagentStop` when you declined and no tool ever ran.
//!   * `UserPromptSubmit`. You cannot type at the prompt while a modal dialog is
//!     up, so a new user prompt proves the screen is clear whoever owned it. This
//!     is the only retirement condition for the cases the captures show fire NO
//!     hook at all: Esc at a dialog (`s2`), a declined one (`s7`) and Ctrl+C
//!     mid-tool (`s5`) are all silent until the next prompt.
//!   * `background_tasks` empty at `Stop`. The array holds one entry per live
//!     subagent - the capture's 68.946 `Stop` lists the very agent that raises the
//!     dialog one second later. An EMPTY one is retained as a recovery heuristic
//!     for stale agent and unknown waits, not correlation with an MCP response.
//!   * its own expiry, `CCTAB_TTL_WAITING`, checked on eligible hooks. Tmux title
//!     decay shares the setting but independently ages its last paint. PER WAIT,
//!     not per record: one epoch shared by the whole list let
//!     every later dialog push a stale wait's clock forward, and a session raising
//!     dialogs more often than the TTL then never expired it at all.
//!
//! WHAT THIS COSTS. One read per painting edge, and one write per TRANSITION.
//! `write_if_changed` is what makes that true, and it is the half of the claim
//! that is cheap to CHECK rather than to believe: 50 steady-state main-thread
//! `working` edges leave the record's mtime untouched, and a subagent tool call
//! that owns no wait returns without writing at all - or creating a file at all.
//!
//! The numbers belong in `scripts/bench-state.sh`, not in this comment, and the
//! reason is a lesson: this header once carried remembered paired deltas
//! (-8/+30/+1/+19us against a 420-460us baseline) which did not reproduce, because
//! the harness forked three times per exec and put an 1100us floor under a 500us
//! measurement. The script interleaves the arms, reports the spread, and compares
//! the layer against ITSELF switched off. Two independent 21-round passes there:
//! Before structural payload parsing: +15us on the hot `working` read, +15 to
//! +22us on a transition, +16us on `idle`, against a ~460us
//! floor - and NEGATIVE for a subagent no-op, which returns before the location
//! walk and is therefore genuinely cheaper than a painting edge.
//!
//! WHEN IT IS ABSENT the binary behaves EXACTLY as it did stateless: no
//! `XDG_RUNTIME_DIR` (and no `CCTAB_STATE_DIR`), failure to create the directory, or a
//! payload with no `session_id` all leave [`Session::open`] returning `None`, and
//! `main` then runs the stateless `Edge::resolve` path. The golden corpus tests
//! that path - its environment sets neither
//! variable - and it is also why the corpus is the wrong home for this module's
//! behaviour: the corpus is frozen against the SHELL implementation, which has no
//! record to consult. `tests/run.sh` owns it instead.

// PRIOR ART, and the model below is partly taken from it: Yannis-Adn/terminal-addons
// (MIT), `plugins/wt-tab-status/scripts/tab-status.sh`. What is taken is the shape:
// one state file per `session_id` holding a state plus the OWNER of a wait
// (`update`, lines 182-204), the rule that while waiting only the waiting agent can
// end the wait (`target_state`, lines 71-74), and an `any` owner for the events
// that do not say who raised it (lines 194-197, this module's `Owner::Unknown`).
// Its `Stop` branch reading `background_tasks` for live agents (`has_running_agents`,
// lines 42-45) is what [`Session::free`]'s third retirement condition and the purple
// seam at the bottom are both built on.
//
// Four things it does that this does not, each for a measured reason:
//   * its `PermissionRequest` branch NO-OPS when `agent_id` is present (lines 84-90),
//     on the reasoning that a background subagent cannot show a prompt. The capture
//     above says otherwise - a subagent's dialog arrives after the main thread's
//     Stop and waits for a human - so that branch would paint idle over a live
//     dialog. It routes the case through the 6s `permission_prompt` notification
//     instead, which is suppressible and fires at most once.
//   * it registers no `SubagentStop`, so a DECLINED subagent dialog has nothing to
//     clear it: no hook fires for a denial.
//   * its owner is a single slot, and `update` only rewrites the file when the
//     STATE changes (line 192), so a second dialog cannot take ownership - the
//     first owner keeps it and the second one's answer clears nothing.
//   * its state file is removed on `SessionEnd` only, so a SIGKILL leaves it
//     forever, and all sessions serialize on one `flock` in the hot path. There IS
//     a lock here - see [`Session::lock`], because a read-modify-write without one
//     loses updates and a lost CLEAR is a tab stuck orange - but it is taken on the
//     session's OWN record, so five concurrent sessions never contend.

use crate::edge::{Edge, Glyph, Notification, Paint};
use crate::payload::{Payload, MAX_ELICITATION_ID_BYTES};
use crate::tmux;
use std::ffi::OsStr;
use std::fs;
use std::io::{Read, Write};
use std::os::unix::fs::{DirBuilderExt, MetadataExt};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

/// Version 5 carries known background separately from the main base. Versions
/// 1–4 remain readable. A future shape changes the tag so ordinary updates leave
/// newer records intact instead of dropping facts they cannot understand.
const TAG: &str = "cts5";

/// The prefix every version's tag shares, which is what lets this one tell "a
/// NEWER version's record, leave it alone" from "not one of ours at all".
const TAG_FAMILY: &str = "cts";

/// At most this many distinct waits are remembered, plus one overflow aggregate.
/// The same bound applies independently to completion tombstones.
const MAX_WAITS: usize = 8;

/// The FALLBACK reaper's horizon, for a file that does not say which process it
/// belongs to - a record from a version that wrote no origin, or a `.tmp` a
/// crashed write left behind. A day, because mtime cannot prove a session dead: an
/// idle session touches nothing, so a short horizon here WOULD delete a live
/// session's record. The origin rule below is the one that can prove it.
const REAP_AFTER: Duration = Duration::from_secs(86_400);

/// The most records one `SessionStart` will look at. The bound is what keeps the
/// reaper's cost knowable: each entry is one read plus one `/proc` read, ~3us, so a
/// full scan is under a millisecond on the coldest edge there is. Five concurrent
/// sessions is the real number; anything near this bound is pathological, and the
/// next `SessionStart` takes the next batch.
const REAP_SCAN: usize = 256;

/// The most a record may be and still be read. Eight waits and eight completion
/// tombstones with maximum-size identities fit under this cap. It keeps a damaged file from
/// being read into memory whole - see [`read_bytes`], which checks the SIZE before
/// it opens anything, because checking it afterwards had already cost 2.1 GB of
/// RSS by then.
const MAX_RECORD: usize = 8192;

/// The longest a session id may be, and it may only be `[A-Za-z0-9_-]`. Both are
/// the file NAME's business: the id arrives from the payload, so this is what
/// stops `session_id` = `../../../x` from choosing the path.
const MAX_ID: usize = 64;

/// How many times [`Session::lock`] re-takes the lock after finding the record
/// renamed out from under it. Each retry means another hook completed a write;
/// bursts resolving several elicitations can exceed four even below MAX_WAITS.
/// Allow ample retries for a full wait set and overlapping result tombstones,
/// while retaining a bound under sustained contention. Exhaustion fails silent
/// rather than applying an unlocked transition.
const LOCK_TRIES: usize = 64;

/// The process a record belongs to: `$CLAUDE_PID`, plus that pid's start time. The
/// PAIR is the point. A pid alone is forgeable by recycling; a start time is
/// immutable for the life of a process, so `(pid, start)` names one process and
/// not a slot, and that is what lets the reaper decide liveness with a single
/// `/proc` read and no daemon.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct Origin {
    pid: u32,
    start: u64,
}

impl Origin {
    /// The session this hook is running inside, or `None` when `$CLAUDE_PID` is
    /// absent or names nothing - in which case the record carries no origin and
    /// the reaper falls back to mtime for it.
    fn mine() -> Option<Origin> {
        let raw = crate::config::var_nonempty("CLAUDE_PID")?;
        Origin::mine_from(u32::try_from(digits(raw.to_str()?)?).ok()?)
    }

    /// The same for a pid already in hand, which is how the tests reach their OWN
    /// `/proc` entry - the only way to assert the live case against a process that
    /// is certainly running.
    fn mine_from(pid: u32) -> Option<Origin> {
        Some(Origin { pid, start: start_time(pid)? })
    }

    fn parse(rest: &str) -> Option<Origin> {
        let (pid, start) = rest.split_once(' ')?;
        Some(Origin {
            pid: u32::try_from(digits(pid)?).ok()?,
            start: digits(start)?,
        })
    }

    /// Whether the process that wrote this record is STILL the process under that
    /// pid. False means gone: either `/proc/<pid>` has disappeared, or something
    /// else has since been given the number.
    ///
    /// This is the reaper's whole decision, and it PROVABLY cannot reap a live
    /// session. A hook process is a child of `$CLAUDE_PID`, so while any of a
    /// session's hooks are running that process exists; a start time never
    /// changes; therefore `alive()` is true for every live session, and the
    /// unlink predicate is false. The error it CAN make is the harmless one -
    /// keeping a dead session's record, if a new process were handed the same pid
    /// inside the same 10ms start tick, which needs 4194304 intervening spawns
    /// (`pid_max`) at `CLK_TCK` 100, both measured on this machine.
    fn alive(self) -> bool {
        start_time(self.pid) == Some(self.start)
    }
}

/// Field 22 of `/proc/<pid>/stat`: the process start time, in clock ticks since
/// boot.
///
/// Parsed after the LAST `") "`, never by splitting the whole line on spaces.
/// Field 2 is the executable name in parentheses and may itself contain both -
/// measured on this machine, `/proc/1259713/stat` holds `(npm exec chrome...)`, so
/// the naive split reads the wrong field for exactly the processes a `claude`
/// session spawns. The tail begins at field 3, so field 22 is its 20th word.
fn start_time(pid: u32) -> Option<u64> {
    let raw = fs::read_to_string(format!("/proc/{}/stat", itoa(u64::from(pid)))).ok()?;
    digits(raw.rsplit_once(") ")?.1.split(' ').nth(19)?)
}

/// Who owns a wait.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Owner {
    /// The main conversation loop: no `agent_id` in the payload.
    Main,
    /// A subagent, by its `agent_id`.
    Agent(String),
    /// Somebody - the `Notification` backstop carries no `agent_id` at all, as the
    /// capture above shows at 75.983, and neither do `agent_needs_input` or
    /// `worker_permission_prompt`. Older records also use this for elicitation.
    ///
    /// What can retire it, and why none of them is "any background subagent's tool
    /// call", which would be the original defect with an extra step:
    ///   * a `Stop` whose `background_tasks` is EMPTY, or main-thread tool
    ///     progress: established stale-wait recovery heuristics, not proof that
    ///     a particular MCP elicitation was answered.
    ///   * a human `UserPromptSubmit`: the user is back at the prompt.
    ///   * its own expiry.
    /// A subagent's completion cannot identify the owner, even for a lone wait.
    Unknown,
    /// A permission request supplied an unusable agent identity. This is direct
    /// evidence of a dialog, independent of notification backstops and named
    /// requests. Keep one aggregate until human prompt, quiet Stop, expiry or reset.
    AnonymousPermission,
    /// An MCP elicitation notification with no known owner. Kept separate from
    /// permission backstops so an unrelated permission dialog cannot replace it.
    UnknownElicitation,
    /// A direct request whose identity was unavailable. Only a human prompt,
    /// expiry or session reset can retire it; an unidentified result cannot.
    AnonymousElicitation,
    /// Extra requests beyond the bounded named set. Never evict another wait;
    /// retain an aggregate until a human prompt, expiry or session reset.
    Overflow,
    /// A direct request identified by server and elicitation id, hex encoded in
    /// one bounded wire token. No form contents, messages or URLs are retained.
    Elicitation(String),
}

impl Owner {
    /// Who this payload is attributable to.
    ///
    /// `agent_id` or the main loop. Callers must separately guard unusable
    /// supplied IDs: they cannot prove main-thread progress, and permission
    /// requests with them must retain an unknown owner.
    fn of(p: &Payload) -> Owner {
        match p.agent_id().map(str::as_bytes).and_then(id_str) {
            Some(id) => Owner::Agent(id),
            None => Owner::Main,
        }
    }

    fn wire(&self) -> &str {
        match self {
            Owner::Main => "-",
            Owner::Unknown => "?",
            Owner::AnonymousPermission => "?p",
            Owner::UnknownElicitation => "?!",
            Owner::AnonymousElicitation => "!?",
            Owner::Overflow => "!+",
            Owner::Elicitation(key) => key,
            Owner::Agent(id) => id,
        }
    }

    fn parse(word: &str) -> Option<Owner> {
        match word {
            "-" => Some(Owner::Main),
            "?" => Some(Owner::Unknown),
            "?p" => Some(Owner::AnonymousPermission),
            "?!" => Some(Owner::UnknownElicitation),
            "!?" => Some(Owner::AnonymousElicitation),
            "!+" => Some(Owner::Overflow),
            _ if valid_elicitation_key(word) => Some(Owner::Elicitation(word.to_owned())),
            _ => id_str(word.as_bytes()).map(Owner::Agent),
        }
    }

    fn direct_elicitation(&self) -> bool {
        matches!(self, Owner::AnonymousElicitation | Owner::Elicitation(_) | Owner::Overflow)
    }

    fn permission(&self) -> bool {
        matches!(self, Owner::Main | Owner::Agent(_) | Owner::Unknown)
    }
}

/// Identity is never truncated: partial strings could falsely correlate two
/// requests. Hex keeps arbitrary valid UTF-8 names out of the record grammar.
fn elicitation_key(p: &Payload) -> Option<Owner> {
    fn component(raw: &str) -> Option<String> {
        if raw.is_empty() || raw.len() > MAX_ELICITATION_ID_BYTES || raw.chars().any(char::is_control) {
            return None;
        }
        let mut encoded = String::with_capacity(raw.len() * 2);
        for b in raw.bytes() {
            const HEX: &[u8] = b"0123456789abcdef";
            encoded.push(char::from(HEX[usize::from(b >> 4)]));
            encoded.push(char::from(HEX[usize::from(b & 15)]));
        }
        Some(encoded)
    }
    Some(Owner::Elicitation(format!("!{}.{}", component(p.mcp_server_name()?)?, component(p.elicitation_id()?)?)))
}

fn valid_elicitation_key(word: &str) -> bool {
    let Some((server, id)) = word.strip_prefix('!').and_then(|s| s.split_once('.')) else {
        return false;
    };
    [server, id].iter().all(|s| {
        !s.is_empty() && s.len() <= MAX_ELICITATION_ID_BYTES * 2 && s.len() % 2 == 0
            && s.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    })
}

/// One outstanding dialog: who raised it, and when.
///
/// The epoch is PER WAIT, and that is load-bearing rather than tidy. With one
/// epoch for the whole list, `raise` refreshed it unconditionally, so every later
/// dialog - from ANY owner - pushed a stale wait's expiry out with it: a session
/// raising dialogs more often than `CCTAB_TTL_WAITING` never expired the stale
/// one, and because nothing paints while a wait is held, the tab stayed orange for
/// the rest of the session. Measured on the shared-epoch shape with the TTL set to
/// 3s: one stale subagent wait plus four ordinary main turns 2s apart left 16
/// consecutive edges painting nothing, 8s into a 3s horizon.
#[derive(Clone, PartialEq, Eq, Debug)]
struct Wait {
    who: Owner,
    raised: u64,
}

impl Wait {
    /// `<owner>:<epoch>`. Split from the RIGHT, so nothing about the owner's
    /// grammar has to be re-asserted here. A word this cannot read whole is
    /// dropped, which is how a record in another shape of this line degrades to
    /// "nothing waiting" rather than to a wait with an invented clock.
    fn parse(word: &str) -> Option<Wait> {
        let (who, raised) = word.rsplit_once(':')?;
        Some(Wait { who: Owner::parse(who)?, raised: digits(raised)? })
    }

    fn render(&self) -> String {
        format!("{}:{}", self.who.wire(), itoa(self.raised))
    }
}

/// An id that is safe to put in a record and in a file name: non-empty, at most
/// [`MAX_ID`] bytes, and `[A-Za-z0-9_-]` only.
fn id_str(raw: &[u8]) -> Option<String> {
    if raw.is_empty() || raw.len() > MAX_ID {
        return None;
    }
    if !raw
        .iter()
        .all(|b| b.is_ascii_alphanumeric() || *b == b'_' || *b == b'-')
    {
        return None;
    }
    // ASCII by the test above, so this cannot fail.
    String::from_utf8(raw.to_vec()).ok()
}

/// The record, which is the whole of the state.
///
/// Wire format, one field per line, unknown keys SKIPPED so that a newer version
/// writing `n` (see the SEAM) does not confuse this one:
///
/// ```text
///   cts5
///   b i                                        base = w | a | i
///   g 1790380620                            last positive main Stop snapshot
///   p 3709427 84460384                         the session's (pid, start time)
///   w aec99e1f4bda1972b:1790380630 -:1790380631
/// ```
///
/// Each `w` word is one wait: its owner, a colon, and the epoch at which THAT wait
/// was raised. An absent `w` line means nothing is waiting. `-` is the main loop
/// and `?` is an unknown permission notification; `?p` is an anonymous permission
/// request. `?!` is an unidentified MCP notification,
/// `!?` an anonymous direct request, and `!+` an overflow aggregate. A direct
/// identity is `!<hex-server>.<hex-id>`. The optional `e` line stores completed
/// direct identities in the same owner:epoch shape, independently bounded to
/// eight. None of these markers can be an agent id. The cts5 tag prevents guarded
/// older readers from silently dropping background activity. The optional g
/// epoch records known in-flight work independently of the main base and never
/// expires. An absent `p` line means the reaper falls back to mtime for this file.
#[derive(Clone, PartialEq, Eq, Debug)]
struct Record {
    base: Glyph,
    /// Last main Stop reporting in-flight work. A bounded aggregate of an
    /// authoritative registry snapshot, not a guessed child count. Never expires.
    background: Option<u64>,
    /// Whose session this is. Written by the first edge that writes anything at
    /// all and then carried unchanged, because every later write starts from the
    /// record it read.
    origin: Option<Origin>,
    /// Oldest first, each with its own epoch.
    waits: Vec<Wait>,
    /// Recently completed exact identities, oldest first. Suppresses replayed
    /// starts and preserves result-before-start ordering within the wait TTL.
    completed: Vec<Wait>,
}

impl Record {
    /// What a session with no record yet is assumed to be: idle, nothing waiting.
    /// The same value a malformed record collapses to.
    fn fresh() -> Record {
        Record {
            base: Glyph::Idle,
            background: None,
            origin: None,
            waits: Vec::new(),
            completed: Vec::new(),
        }
    }

    /// What `SessionStart` writes: the same clean slate, but claimed by this
    /// process, so that the next session's reaper can tell this file from a dead
    /// session's.
    fn claimed() -> Record {
        Record { origin: Origin::mine(), ..Record::fresh() }
    }

    /// The record this text holds, or `None` when the first line is not our tag -
    /// which the caller has to be able to tell apart, because "not one of ours"
    /// and "an idle record" are the same paint but very different reports.
    ///
    /// No expiry here, deliberately: expiry needs a clock, and keeping it out
    /// means [`Session::load`] can hand a transition BOTH the record on disk and
    /// the record after expiry, so the expiry itself counts as a change and gets
    /// persisted. It used not to, and the dead `w` line then survived for the life
    /// of the session - long enough for a raised `CCTAB_TTL_WAITING` to resurrect
    /// a wait that had already been declared dead.
    fn parse(text: &str) -> Option<Record> {
        let mut lines = text.lines();
        let tag = lines.next();
        if !matches!(tag, Some(TAG | "cts1" | "cts2" | "cts3" | "cts4")) {
            return None;
        }
        let mut r = Record::fresh();
        for line in lines {
            let (key, rest) = match line.split_once(' ') {
                Some(kv) => kv,
                // A bare key carries nothing; `b` with no letter is not a base.
                None => continue,
            };
            match key {
                "b" => {
                    r.base = match rest {
                        "w" => Glyph::Working,
                        "a" => Glyph::Waiting,
                        "i" => Glyph::Idle,
                        // A base letter a later version invents - `p` for the
                        // purple state - is not one this version can paint, so it
                        // reads as idle rather than as a parse failure.
                        _ => Glyph::Idle,
                    }
                }
                // A `p` this version cannot parse leaves the origin absent, which
                // demotes this file to the mtime rule rather than inventing a
                // liveness answer for it.
                "p" => r.origin = Origin::parse(rest),
                // Earlier versions reserved g for a different, unimplemented
                // shape. Only the current version assigns it this meaning.
                "g" if tag == Some(TAG) => r.background = digits(rest),
                "w" => r.waits = rest.split(' ').filter_map(Wait::parse).take(MAX_WAITS + 1).collect(),
                "e" => r.completed = rest.split(' ').filter_map(Wait::parse)
                    .filter(|w| matches!(w.who, Owner::Elicitation(_)))
                    .take(MAX_WAITS).collect(),
                // Unknown fields remain forward-extensible.
                _ => {}
            }
        }
        Some(r)
    }

    /// Drop the waits whose own clock has run out, and say whether any went.
    ///
    /// A wait nothing ever cleared must not hold the tab orange forever: a
    /// subagent killed mid-dialog fires no `SubagentStop`. The horizon uses the
    /// same `CCTAB_TTL_WAITING` setting as tmux title decay, including `0` = never.
    /// Record expiry needs an eligible hook; tmux independently ages the most
    /// recently painted carrier, so these clocks need not expire together.
    fn expire(&mut self, now: u64, ttl: u64) -> bool {
        let before = self.waits.len();
        let completed_before = self.completed.len();
        self.waits.retain(|w| now.saturating_sub(w.raised) <= ttl);
        self.completed.retain(|w| now.saturating_sub(w.raised) <= ttl);
        self.waits.len() != before || self.completed.len() != completed_before
    }

    fn render(&self) -> String {
        let mut out = String::with_capacity(96);
        out.push_str(TAG);
        out.push_str("\nb ");
        out.push(match self.base {
            Glyph::Working => 'w',
            Glyph::Waiting => 'a',
            Glyph::Idle | Glyph::Background => 'i',
        });
        out.push('\n');
        if let Some(epoch) = self.background {
            out.push_str(&format!("g {epoch}\n"));
        }
        if let Some(o) = self.origin {
            out.push_str("p ");
            out.push_str(&itoa(u64::from(o.pid)));
            out.push(' ');
            out.push_str(&itoa(o.start));
            out.push('\n');
        }
        if !self.waits.is_empty() {
            out.push('w');
            for w in &self.waits {
                out.push(' ');
                out.push_str(&w.render());
            }
            out.push('\n');
        }
        if !self.completed.is_empty() {
            out.push('e');
            for w in &self.completed {
                out.push(' ');
                out.push_str(&w.render());
            }
            out.push('\n');
        }
        out
    }

    /// Add a wait, newest last. Re-raising a wait the same owner already holds
    /// only refreshes THAT wait's epoch, which is what keeps its TTL measured from
    /// the last time its own dialog was asserted.
    fn raise(&mut self, o: Owner, now: u64) {
        // An attributable dialog SUPERSEDES a lone permission `?`. The backstop describes a
        // dialog it could not name, so when a named one turns up while `?` is all
        // that is outstanding, they are one dialog reported twice - and the named
        // one has an owner that can retire it. The usual order is the other way
        // round, and `Session::wait`'s guard covers that one: the backstop fires 6s
        // AFTER the PermissionRequest it backs up.
        // An MCP notification is separate evidence and never participates in
        // permission backstop deduplication, in either arrival order.
        if matches!(o, Owner::Main | Owner::Agent(_)) && self.lone_unknown() {
            self.clear(&Owner::Unknown);
        }
        if let Some(w) = self.waits.iter_mut().find(|w| w.who == o) {
            w.raised = now;
            return;
        }
        if self.waits.iter().filter(|w| w.who != Owner::Overflow).count() >= MAX_WAITS {
            // Saturation must not delete an unrelated live dialog. The single
            // extra slot preserves waiting conservatively; activity cannot keep
            // pushing its expiry forward.
            if !self.waits.iter().any(|w| w.who == Owner::Overflow) {
                self.waits.push(Wait { who: Owner::Overflow, raised: now });
            }
            return;
        }
        self.waits.push(Wait { who: o, raised: now });
    }

    /// Whether the only permission backstop candidate is unattributable.
    /// Anonymous direct permission and MCP waits do not participate in this
    /// notification deduplication and remain independent.
    fn lone_unknown(&self) -> bool {
        let mut permissions = self.waits.iter().filter(|w| w.who.permission());
        matches!(permissions.next(), Some(w) if w.who == Owner::Unknown)
            && permissions.next().is_none()
    }

    /// What this record holds, for `doctor`. Words rather than the wire format,
    /// because the wire format is what you get by reading the file and the whole
    /// point of the report is to say what it MEANS.
    fn describe(&self, now: u64) -> String {
        let mut s = format!(
            "base {}",
            match self.base {
                Glyph::Working => "working",
                Glyph::Waiting => "waiting",
                Glyph::Idle | Glyph::Background => "idle",
            }
        );
        if let Some(epoch) = self.background {
            s.push_str(&format!(", background reported {}s ago (last known snapshot; not expired)", now.saturating_sub(epoch)));
        }
        match self.origin {
            Some(o) if o.alive() => s.push_str(&format!(", session pid {} live", o.pid)),
            Some(o) => s.push_str(&format!(", session pid {} GONE", o.pid)),
            None => s.push_str(", no session pid recorded"),
        }
        if self.waits.is_empty() {
            s.push_str(", nothing waiting");
        } else {
            let who: Vec<String> = self
                .waits
                .iter()
                .map(|w| format!("{} raised {}s ago", w.who.wire(), now.saturating_sub(w.raised)))
                .collect();
            s.push_str(&format!(
                ", waiting on {} ({})",
                self.waits.len(),
                who.join(", ")
            ));
        }
        s
    }

    fn paint(&self, glyph: Glyph) -> Paint {
        if self.background.is_some() {
            Paint::LineWithBackground(if glyph == Glyph::Idle { Glyph::Background } else { glyph })
        } else {
            Paint::Line(glyph)
        }
    }

    /// Drop a wait, and say whether one was actually held.
    fn clear(&mut self, o: &Owner) -> bool {
        let before = self.waits.len();
        self.waits.retain(|w| &w.who != o);
        self.waits.len() != before
    }

    /// Remember the first completion time; duplicate results do not extend the
    /// tombstone's lifetime. Bounded history deliberately forgets oldest entries.
    fn complete(&mut self, o: Owner, now: u64) {
        if self.completed.iter().any(|w| w.who == o) {
            return;
        }
        if self.completed.len() >= MAX_WAITS {
            self.completed.remove(0);
        }
        self.completed.push(Wait { who: o, raised: now });
    }

}

fn digits(w: &str) -> Option<u64> {
    let b = w.as_bytes();
    if b.is_empty() || b.len() > 20 || !b.iter().all(u8::is_ascii_digit) {
        return None;
    }
    b.iter()
        .try_fold(0u64, |a, c| a.checked_mul(10)?.checked_add(u64::from(c - b'0')))
}

/// `u64` as decimal without `format!`'s machinery. Small, and the one place this
/// module formats a number.
fn itoa(mut n: u64) -> String {
    if n == 0 {
        return "0".to_owned();
    }
    let mut buf = [0u8; 20];
    let mut i = buf.len();
    while n > 0 && i > 0 {
        i -= 1;
        buf[i] = b'0' + (n % 10) as u8;
        n /= 10;
    }
    String::from_utf8_lossy(&buf[i..]).into_owned()
}

/// `background_tasks` is present and EMPTY: this payload says nothing outside the
/// main loop is running.
///
/// ABSENT IS NOT EMPTY. A `Notification` carries no such member at all, and a
/// Claude Code that renamed it would carry none either, so "not found" has to mean
/// "I do not know" and leave every wait standing. That is the conservative
/// direction, and it is the capture's 68.946 `Stop`, whose array holds the agent
/// that raises a dialog one second later.
fn nothing_running(p: &Payload) -> bool {
    p.background_tasks_empty() == Some(true)
}

/// This `working` edge is a `UserPromptSubmit` carrying a prompt the USER typed.
///
/// Worth a discriminator because the two `working` events mean very different
/// things about the SCREEN. A tool completing says nothing about a dialog somebody
/// else raised; a prompt you typed proves there is no dialog at all, because you
/// cannot type at the prompt while a modal is up. That makes it the only
/// retirement condition for the shapes the captures show fire no hook whatsoever -
/// Esc at a dialog, a declined one, Ctrl+C mid-tool.
///
/// THE SECOND TEST IS THE LOAD-BEARING ONE, and without it this whole rule would
/// be wrong. Not every `UserPromptSubmit` comes from a human: the capture's
/// 110.251 event carries
/// `"prompt":"<task-notification>\n<task-id>aec99e1f4bda1972b</task-id>..."`, which
/// the PRODUCT injects when an async agent finishes. With two agents running, the
/// first one's completion would then retire the second one's live dialog - and the
/// injected event is unnecessary anyway, because that agent's own `SubagentStop`
/// fires 20ms earlier and clears its wait properly.
///
/// So the prompt must be present and must not begin with `<`. That is deliberately
/// broader than the one spelling measured. It is a recovery heuristic, not
/// authentication of human input; a human prompt beginning with `<` also loses
/// this recovery path. The check uses the first character without trimming.
///
fn at_the_prompt(p: &Payload) -> bool {
    p.hook_event_name() == Some("UserPromptSubmit")
        && matches!(p.prompt_first(), Some(c) if c != '<')
}

/// Where records live, or `None` when there is nowhere to put them.
///
/// `XDG_RUNTIME_DIR` and nothing else: it is per-user, mode 0700, on a tmpfs, and
/// emptied when the last login session ends - which is the reaper for anything
/// [`REAP_AFTER`] does not catch. There is deliberately no `$HOME` fallback: it
/// would put a state file inside the golden corpus's fixture HOME and make every
/// case that carries a `session_id` order-dependent, and the corpus environment
/// sets `HOME` and nothing else this function reads.
///
/// `CCTAB_STATE_DIR` overrides it, and the directory it names must be a DEDICATED
/// one, because [`Session::reap`] deletes files in it. It deletes only what it can
/// prove is ours - see [`reapable`], which is what stops it emptying a directory
/// that holds other things - but pointing this at a shared directory is still the
/// wrong thing to do, and README's row for the variable now says so.
///
/// That `XDG_RUNTIME_DIR` reaches a HOOK subprocess is measured, not assumed: an
/// empty state directory in production first looked like the variable being
/// stripped, and a temporary probe build recorded what a real `PostToolUse` hook
/// actually sees -
/// `xdg=Some("/run/user/1000") pid=Some("3709427") dir=Some("/run/user/1000/claude-tabstatus")`.
/// It was there; the empty directory was this module answering correctly. The
/// probe's other field is why: `agent=Some("a6efa33f05e9fc6c5")`, because the
/// session under observation was running a subagent, whose tool calls own no wait
/// and so write nothing. A `/proc/$CLAUDE_PID/environ` fallback was written for
/// the failure that turned out not to exist, and removed - it would also have made
/// the golden corpus's pty cases, which set `CLAUDE_PID` to a live helper, write
/// records into the real `/run/user/<uid>`.
pub fn dir() -> Option<PathBuf> {
    if let Some(d) = crate::config::var_nonempty("CCTAB_STATE_DIR") {
        return Some(PathBuf::from(d));
    }
    let base = crate::config::var_nonempty("XDG_RUNTIME_DIR")?;
    let mut p = PathBuf::from(base);
    p.push("claude-tabstatus");
    Some(p)
}

/// One session's record, and the file it lives in.
pub struct Session {
    dir: PathBuf,
    path: PathBuf,
    now: u64,
    ttl: u64,
}

impl Session {
    /// The record for the session this payload belongs to, or `None` - which
    /// means "behave exactly as the stateless version did".
    pub fn open(dir: Option<PathBuf>, p: &Payload) -> Option<Session> {
        let dir = dir?;
        let id = p.session_id().map(str::as_bytes).and_then(id_str)?;
        // Mode 0700 on creation rather than a check afterwards: inside
        // XDG_RUNTIME_DIR, itself 0700 and owned by us, there is nobody to race.
        if !dir.is_dir()
            && fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(&dir)
                .is_err()
        {
            return None;
        }
        let mut path = dir.clone();
        path.push(&id);
        Some(Session {
            dir,
            path,
            now: tmux::now(),
            ttl: tmux::ttl_secs("CCTAB_TTL_WAITING", tmux::DEFAULT_TTL_WAITING),
        })
    }

    /// The record on disk, and the record after expiry. BOTH, because an expiry is
    /// itself a transition: handing `write_if_changed` the already-expired record
    /// as `was` made the two compare equal, so the dead `w` line survived for the
    /// life of the session and a later `CCTAB_TTL_WAITING` could resurrect it.
    fn load(&self) -> (Record, Record) {
        let on_disk = match stored_at(&self.path) {
            Stored::Ours(r) => r,
            _ => Record::fresh(),
        };
        let mut live = on_disk.clone();
        live.expire(self.now, self.ttl);
        (on_disk, live)
    }

    /// An exclusive advisory lock on this session's own record, held for a whole
    /// read-modify-write.
    ///
    /// WHY. Every transition is read, mutate, write, and two hooks of ONE session
    /// do overlap - the capture has `SubagentStart` and `PostToolUse` 1ms apart.
    /// Unlocked, one of the two updates is lost, and when the lost one is a CLEAR
    /// the record keeps a phantom wait, after which `progress`, `free` and the 3s
    /// idle nudge ALL decline to paint: the tab is orange until the TTL, which
    /// outside tmux is the fifteen-minute lie this module exists to remove.
    /// Measured on the unlocked version, 300 rounds of a subagent's un-painting
    /// `PostToolUse` launched simultaneously with main's `Stop`: 43 lost the clear.
    ///
    /// PER RECORD. `std::fs::File::lock` is std-only (stable since 1.89; this tree
    /// builds on 1.98), and the file it is taken on is this session's own, so the
    /// five concurrent sessions never contend - the property the prior-art note
    /// above criticises `terminal-addons`'s single global `flock` for lacking.
    ///
    /// The INODE CHECK is the subtlety. `write_if_changed` renames a temp file over
    /// the record, so the path's inode changes under a waiter: it would wake
    /// holding an exclusive lock on an unlinked inode while a third hook held the
    /// new one. So after taking the lock we check that we hold the inode the path
    /// names NOW, and retry when we do not.
    ///
    /// BLOCKING: the normal critical section is one small read, one small write
    /// and a rename. Closing the fd or exiting releases the lock, including an
    /// abort or SIGKILL. There is no lock-acquisition timeout here; a suspended
    /// holder or stalled filesystem can delay another hook indefinitely.
    ///
    /// Creation-capable edges atomically create an empty file before taking the
    /// lock. This closes the first-update race too: two direct requests must not
    /// lose one wait, nor may result-before-start lose its completion tombstone.
    /// Nonpainting agent edges still create no file when there is no state.
    fn lock(&self, create: bool) -> Option<fs::File> {
        for _ in 0..LOCK_TRIES {
            // Do not follow links or open special files (a FIFO may block), and
            // never overwrite a record owned by a later format version.
            match fs::symlink_metadata(&self.path) {
                Ok(m) if !m.is_file() || m.len() > MAX_RECORD as u64 => return None,
                Ok(_) if matches!(stored_at(&self.path), Stored::Future) => return None,
                Ok(_) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound && create => {}
                Err(_) => return None,
            }
            let f = fs::OpenOptions::new()
                .read(true)
                .write(true)
                .create(create)
                .truncate(false)
                .open(&self.path)
                .ok()?;
            f.lock().ok()?;
            let held = f.metadata().ok()?;
            match fs::symlink_metadata(&self.path) {
                Ok(m) if m.is_file() && m.len() <= MAX_RECORD as u64
                    && (m.dev(), m.ino()) == (held.dev(), held.ino()) => {
                    if matches!(stored_at(&self.path), Stored::Future) {
                        return None;
                    }
                    return Some(f);
                }
                // Renamed out from under us. Drop this lock and take the new
                // inode's.
                _ => drop(f),
            }
        }
        None
    }

    /// Write only when the bytes would change. That is what keeps the hot edge's
    /// cost a READ: a run of main-thread `PostToolUse` calls with the base already
    /// `w` and nothing waiting writes nothing.
    ///
    /// Every write it DOES make stamps the origin when the record has none, which
    /// is what makes the reaper's liveness proof cover every record this version
    /// writes rather than only the ones a `SessionStart` claimed. The `/proc` read
    /// that costs happens once per session, on its first write, and only when a
    /// write was going to happen anyway - so no edge starts writing because of it.
    fn write_if_changed(&self, was: &Record, now: &mut Record) {
        if was == now {
            return;
        }
        if now.origin.is_none() {
            now.origin = Origin::mine();
        }
        // Temp-then-rename, so a reader in a concurrently running hook of the same
        // session - or the reaper, or `doctor`, neither of which takes the lock -
        // sees the old record or the new one and never a torn one.
        let mut tmp = self.path.clone();
        let mut name = match self.path.file_name().map(|n| n.to_owned()) {
            Some(n) => n,
            None => return,
        };
        name.push(".");
        name.push(itoa(u64::from(std::process::id())));
        name.push(".tmp");
        tmp.set_file_name(name);
        let text = now.render();
        let wrote = fs::File::create(&tmp).and_then(|mut f| f.write_all(text.as_bytes()));
        if wrote.is_ok() && fs::rename(&tmp, &self.path).is_ok() {
            return;
        }
        let _ = fs::remove_file(&tmp);
    }

    fn remove(&self) {
        let _ = fs::remove_file(&self.path);
    }

    /// Delete the records of sessions that are gone. Runs on `SessionStart` only -
    /// once per session, against a directory holding one small file per live
    /// session - so the cost never reaches a painting edge. This is the whole
    /// reaper: there is no daemon, and a SIGKILLed session, which fires no
    /// `SessionEnd`, is cleaned up by whichever session starts next.
    ///
    /// Our OWN file is skipped rather than tested, because this runs immediately
    /// after claiming it and the answer is known.
    fn reap(&self) {
        let Ok(entries) = fs::read_dir(&self.dir) else {
            return;
        };
        // BOUNDED, because this binary must never block a hook and every entry
        // costs a read plus a `/proc` read. Five concurrent sessions is the real
        // number and the directory is one nobody else should write to, so the
        // bound is only ever reached by something pathological - and then the next
        // SessionStart takes the next batch.
        for e in entries.flatten().take(REAP_SCAN) {
            let path = e.path();
            if path != self.path && reapable(&path, Named::of(&e.file_name())).is_some() {
                let _ = fs::remove_file(&path);
            }
        }
    }

    /// The paint for this edge, having consulted and updated the record.
    ///
    /// The edges the state layer has nothing to say about defer to
    /// [`Edge::resolve`], so there is exactly one place each of those decisions
    /// lives.
    pub fn resolve(&self, edge: Edge, p: &Payload) -> Option<Paint> {
        match edge {
            // A session starting over must not inherit the previous run's waits:
            // `--resume` and `--clear` both land here, and neither one leaves a
            // dialog on screen. A mid-turn compaction re-fire is filtered by
            // `Edge::resolve` BEFORE this, and so resets nothing.
            Edge::SessionStart => {
                let paint = edge.resolve(p)?;
                let mut claimed = Record::claimed();
                let Some(lock) = self.lock(claimed.origin.is_some()) else {
                    self.reap();
                    return Some(paint);
                };
                let (was, _) = self.load();
                self.write_if_changed(&was, &mut claimed);
                // Released before the scan: the reaper never touches our own file,
                // so holding it across a 256-entry walk would only delay the
                // session's own next hook.
                drop(lock);
                self.reap();
                Some(paint)
            }
            Edge::SessionEnd => {
                let paint = edge.resolve(p)?;
                self.remove();
                Some(paint)
            }
            Edge::Working => self.progress(Owner::of(p), p),
            Edge::Waiting => {
                let owner = Owner::of(p);
                // A supplied identity that cannot name an agent is not evidence
                // of a main-thread request. Keep an unattributed permission wait
                // rather than letting an ordinary main Stop retire it. `-` is
                // reserved for Main in the record, despite matching the ID grammar.
                let unusable = p.agent_id().is_some()
                    && (owner == Owner::Main || p.agent_id() == Some("-"));
                self.wait(if unusable { Owner::AnonymousPermission } else { owner })
            }
            Edge::Idle => self.free(p),
            Edge::SubagentStop => self.agent_gone(Owner::of(p)),
            Edge::Elicitation => self.elicitation_start(p, false),
            Edge::ElicitationResult => self.elicitation_result(p),
            // A notification is a three-way, and its two painting answers are the
            // same two transitions the Stop and PermissionRequest edges make.
            Edge::Notify => match Notification::detect(p) {
                Notification::IdlePrompt => self.free(p),
                // The backstop carries no `agent_id`, so its owner is unknowable.
                Notification::Waiting => match p.notification_type() {
                    Some("elicitation_dialog" | "elicitation_url_dialog") => self.elicitation_start(p, true),
                    _ => self.wait(Owner::Unknown),
                },
                Notification::Other => None,
            },
            Edge::Unknown => edge.resolve(p),
        }
    }

    /// `UserPromptSubmit`, `PostToolUse`, `PostToolUseFailure`: something ran.
    fn progress(&self, o: Owner, p: &Payload) -> Option<Paint> {
        // A supplied but unusable agent id is not evidence of main-thread
        // progress and must not inherit its cross-owner recovery permissions.
        if (o == Owner::Main && p.agent_id().is_some()) || p.agent_id() == Some("-") {
            return None;
        }
        let _lock = self.lock(o == Owner::Main)?;
        let (was, mut now) = self.load();
        match &o {
            Owner::Agent(_) => {
                // THE FIX. A subagent's tool call paints nothing - that filter is
                // what stopped a background subagent from repainting blue over
                // your open dialog - UNLESS this subagent is the one you were
                // waiting on, in which case its completion is the un-paint.
                // Unknown notification owners cannot be attributed to this
                // agent merely because no other wait is recorded.
                if !now.clear(&o) {
                    // Nothing of ours to retire. An expiry may still have to be
                    // persisted, but ONLY that: an edge with nothing to say must
                    // not be the one that starts writing records, which is what
                    // keeps a background subagent's tool call free of any write.
                    self.write_if_changed(&was, &mut now);
                    return None;
                }
                // It says nothing about the main loop, so the base is untouched.
            }
            // `Owner::of` never yields an unknown owner - it reads `agent_id` and falls
            // back to `Main` - so this arm is only ever reached as the main loop,
            // and clearing `Unknown` in it is the deliberate cross-owner
            // retirement, not a second owner's business. `Unknown` is named here
            // rather than swept into a `_` so a future owner cannot land in it
            // silently.
            Owner::Main | Owner::Unknown | Owner::UnknownElicitation => {
                // Main-thread tool activity keeps the established recovery for
                // main and unknown waits. It is not a correlated MCP response.
                // Injected task notifications are background activity, not
                // evidence that the main or an unknown dialog was answered.
                let human_prompt = at_the_prompt(p);
                if p.hook_event_name() != Some("UserPromptSubmit") || human_prompt {
                    now.clear(&Owner::Main);
                    now.clear(&Owner::Unknown);
                    now.clear(&Owner::UnknownElicitation);
                }
                // A new user PROMPT retires EVERY wait, whoever owns it: you
                // cannot type at the prompt while a modal dialog is up. This is
                // what bounds a subagent wait nothing else can retire - Esc at a
                // subagent's dialog fires no hook at all - to a single turn rather
                // than to CCTAB_TTL_WAITING.
                if !now.waits.is_empty() && human_prompt {
                    let completed: Vec<_> = now.waits.iter().filter_map(|w| {
                        matches!(w.who, Owner::Elicitation(_)).then(|| w.who.clone())
                    }).collect();
                    for owner in completed {
                        now.complete(owner, self.now);
                    }
                    now.waits.clear();
                }
                now.base = Glyph::Working;
            }
            // These owners cannot prove attributable progress.
            Owner::AnonymousPermission | Owner::AnonymousElicitation
            | Owner::Elicitation(_) | Owner::Overflow => return None,
        }
        self.write_if_changed(&was, &mut now);
        // A wait somebody else still holds keeps the tab: this is the second half
        // of the fix, and the half that matters most. Stateless, a main-thread
        // PostToolUse repainted blue over a subagent's open dialog.
        now.waits.is_empty().then_some(now.paint(now.base))
    }

    /// `PermissionRequest`, `PreToolUse` on the two tools that always block, and
    /// the `Notification` backstop: a dialog is up.
    fn wait(&self, o: Owner) -> Option<Paint> {
        let Some(_lock) = self.lock(true) else {
            return Some(self.unpersisted_wait());
        };
        let (was, mut now) = self.load();
        // The backstop fires ~6s after the dialog for a PermissionRequest already
        // reported - capture: PermissionRequest(aec99e) at 69.960, then
        // Notification permission_prompt at 75.983 for the SAME dialog. Adding an
        // unknown owner there would mean two waits for one dialog, only one of
        // which has an owner that can retire it. So a permission unknown is only
        // recorded when no permission wait is outstanding, which is the case
        // the backstop exists for: the dialogs that are not tool calls. The
        // reverse arrival order is handled by `Record::raise`.
        // Elicitations are independent of permission dialogs. A generic
        // backstop is redundant only when a permission wait already exists.
        let duplicate_anonymous = o == Owner::AnonymousPermission
            && now.waits.iter().any(|w| w.who == o);
        // Without an identity, another malformed request cannot be correlated
        // with this aggregate. Keep the first epoch so repeats cannot extend it.
        if !duplicate_anonymous
            && !(o == Owner::Unknown && now.waits.iter().any(|w| w.who.permission())) {
            now.raise(o, self.now);
        }
        self.write_if_changed(&was, &mut now);
        // Always orange, whatever the base: a dialog on screen is the truth, and
        // repainting refreshes the tmux carrier's epoch, which restarts the decay.
        Some(now.paint(Glyph::Waiting))
    }

    /// A failed write lock still permits a conservative orange paint. Preserve
    /// any readable background fact so that its tmux carrier cannot age to white.
    /// This reads one atomic snapshot and never mutates it without the lock.
    fn unpersisted_wait(&self) -> Paint {
        match stored_at(&self.path) {
            Stored::Ours(record) => record.paint(Glyph::Waiting),
            _ => Paint::Line(Glyph::Waiting),
        }
    }

    /// `Stop`, `StopFailure`, and the idle nudge: the main loop is free.
    fn free(&self, p: &Payload) -> Option<Paint> {
        // Live 2.1.274 captures establish main Stop as a complete parent-session
        // registry snapshot, including the automatic turn after a task finishes.
        // SubagentStop may still list its own finishing task; never infer that
        // one child finishing ends a workflow. Other hooks supply no such proof.
        let snapshot = (p.hook_event_name() == Some("Stop"))
            .then(|| p.background_tasks_empty())
            .flatten();
        let Some(_lock) = self.lock(snapshot == Some(false)) else {
            // A genuinely fresh quiet session needs no record. An inaccessible
            // or contended existing record must never be mutated without a lock.
            return matches!(fs::symlink_metadata(&self.path), Err(e) if e.kind() == std::io::ErrorKind::NotFound)
                .then_some(Paint::Line(if snapshot == Some(false) { Glyph::Background } else { Glyph::Idle }));
        };
        let (was, mut now) = self.load();
        now.base = Glyph::Idle;
        match snapshot {
            Some(false) => now.background = Some(self.now),
            Some(true) => now.background = None,
            None => {}
        }
        // The main loop could not have stopped while a MAIN-thread dialog blocked
        // it, so a main wait outstanding here is a stale one - a rejected
        // ExitPlanMode, whose PostToolUse never comes.
        now.clear(&Owner::Main);
        // An EMPTY `background_tasks` retains recovery for permission and
        // notification-only waits. Direct requests and the overflow aggregate
        // survive: this heuristic cannot correlate an MCP response. Without it, a subagent
        // whose dialog you dismissed with Esc - which fires NO hook - held the tab
        // orange for the whole TTL, and an `agent_needs_input` `?` held it orange
        // through every idle period until your next prompt, where the STATELESS
        // binary painted white.
        //
        // Non-empty or absent, and every wait stands. That is the capture's 68.946
        // `Stop`, whose array holds the very agent that raises a dialog at 69.960:
        // painting idle there is the defect this module exists to avoid.
        if nothing_running(p) {
            now.waits.retain(|w| w.who.direct_elicitation());
        }
        self.write_if_changed(&was, &mut now);
        // Never paint idle over an outstanding dialog. A changed background
        // fact may need an orange refresh so tmux retains the correct fallback.
        if now.waits.is_empty() {
            Some(now.paint(Glyph::Idle))
        } else if was.background.is_some() != now.background.is_some() {
            // Keep orange while updating tmux's fallback when this snapshot
            // first discovers background work, or proves it has ended.
            Some(now.paint(Glyph::Waiting))
        } else {
            None
        }
    }

    /// `SubagentStop`: this agent will never resolve anything again.
    ///
    /// The ONLY signal for a subagent whose dialog you DECLINED - no hook fires for
    /// a denial, so the tool never runs and no `PostToolUse` arrives - and for one
    /// that gave up. It clears only the wait it OWNS:
    /// the capture has a `SubagentStop` for `a8e90c10` nine seconds before the user
    /// answered `aec99e1f`'s dialog, so a `SubagentStop` that owns nothing is inert,
    /// including when the outstanding dialog has no identifiable owner.
    fn agent_gone(&self, o: Owner) -> Option<Paint> {
        let Owner::Agent(_) = o else {
            return None;
        };
        let _lock = self.lock(false)?;
        let (was, mut now) = self.load();
        if !now.clear(&o) {
            self.write_if_changed(&was, &mut now);
            return None;
        }
        self.write_if_changed(&was, &mut now);
        now.waits.is_empty().then_some(now.paint(now.base))
    }

    /// Direct starts and identified notification backstops share one exact key.
    /// An unidentified notification remains independent: without identity there
    /// is no evidence that it duplicates any active or completed direct request.
    fn elicitation_start(&self, p: &Payload, notification: bool) -> Option<Paint> {
        if !notification && (p.hook_event_name() != Some("Elicitation") || !p.elicitation_mode_supported()) {
            return None;
        }
        let owner = elicitation_key(p).unwrap_or(if notification {
            Owner::UnknownElicitation
        } else {
            Owner::AnonymousElicitation
        });
        let Some(_lock) = self.lock(true) else {
            // Painting a request is harmless without persistence; resolving one
            // without persistence is not, so results below instead stay silent.
            return Some(self.unpersisted_wait());
        };
        let (was, mut now) = self.load();
        if now.completed.iter().any(|w| w.who == owner) {
            self.write_if_changed(&was, &mut now);
            return None;
        }
        // Replayed direct starts do not extend a wait's TTL, including the
        // anonymous aggregate. Legacy notification-only waits retain refreshes.
        if owner == Owner::UnknownElicitation || !now.waits.iter().any(|w| w.who == owner) {
            now.raise(owner, self.now);
        }
        self.write_if_changed(&was, &mut now);
        Some(now.paint(Glyph::Waiting))
    }

    fn elicitation_result(&self, p: &Payload) -> Option<Paint> {
        if p.hook_event_name() != Some("ElicitationResult")
            || !p.elicitation_mode_supported()
            || !matches!(p.action(), Some("accept" | "decline" | "cancel")) {
            return None;
        }
        let owner = elicitation_key(p)?;
        // A failed lock must never lead to an unlocked removal or tombstone.
        let _lock = self.lock(true)?;
        let (was, mut now) = self.load();
        let cleared = now.clear(&owner);
        now.complete(owner, self.now);
        self.write_if_changed(&was, &mut now);
        (cleared && now.waits.is_empty()).then_some(now.paint(now.base))
    }
}

/// What a file in the state directory turned out to be.
enum Stored {
    /// No file, or one we may not read, or one whose size or type says it cannot
    /// be a record. Every paint path treats this as [`Record::fresh`], which is
    /// why a missing, unreadable or oversized file degrades to the stateless
    /// answer in silence rather than by a separate path.
    Absent,
    /// A first line that is a LATER version's tag. The whole point of having a
    /// tag: this version must ignore it rather than misread it, AND must not
    /// delete it, because the version that wrote it may be running right now.
    Future,
    /// A file that is not a record in any version's shape.
    Alien,
    Ours(Record),
}

/// At most [`MAX_RECORD`] bytes of a REGULAR file, or `None`.
///
/// The size is checked against the stat, before anything is opened, which is what
/// `MAX_RECORD`'s comment promised and `fs::read` did not deliver: measured, a 2 GB
/// file cost 2.1 GB of RSS and 0.64s inside a hook that has 5 seconds. The TYPE
/// test comes first for the same reason - `File::open` on a FIFO with no writer
/// blocks forever, and one of those in the state directory was enough to get the
/// hook killed. The read is bounded again on the handle, because the file may have
/// grown between the stat and the open.
fn read_bytes(path: &Path) -> Option<Vec<u8>> {
    let m = fs::metadata(path).ok()?;
    if !m.is_file() || m.len() > MAX_RECORD as u64 {
        return None;
    }
    let mut buf = Vec::with_capacity(MAX_RECORD.min(m.len() as usize + 1));
    fs::File::open(path)
        .ok()?
        .take(MAX_RECORD as u64 + 1)
        .read_to_end(&mut buf)
        .ok()?;
    (buf.len() <= MAX_RECORD).then_some(buf)
}

/// What this path holds. One function, so the paint path, the reaper and `doctor`
/// cannot disagree about whether a file is a record.
fn stored_at(path: &Path) -> Stored {
    let Some(b) = read_bytes(path) else {
        return Stored::Absent;
    };
    let text = String::from_utf8_lossy(&b);
    match Record::parse(&text) {
        Some(r) => Stored::Ours(r),
        // A tag of this family that is not ours is a newer version's record. The
        // length bound is so that a first line which merely BEGINS with `cts`
        // cannot claim the benefit of the doubt.
        None if matches!(text.lines().next(), Some(l) if l.starts_with(TAG_FAMILY) && l.len() <= 8) => {
            Stored::Future
        }
        None => Stored::Alien,
    }
}

/// What a file in the state directory is, by NAME alone. Checked BEFORE anything
/// is read, because `CCTAB_STATE_DIR` is a documented user knob: a directory that
/// holds other things is the wrong thing to point it at, but it must not be
/// silently emptied either.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Named {
    /// A session id, which is what a record is filed under.
    Record,
    /// `<id>.<pid>.tmp`, which a write in flight leaves and a crashed write leaves
    /// behind. `.` is outside the id grammar, so this can never collide with a
    /// record's name.
    Tmp,
    /// Anything else. Never reaped, whatever its age.
    Foreign,
}

impl Named {
    fn of(name: &OsStr) -> Named {
        let Some(n) = name.to_str() else {
            return Named::Foreign;
        };
        match n.split_once('.') {
            None if id_str(n.as_bytes()).is_some() => Named::Record,
            Some((id, rest))
                if id_str(id.as_bytes()).is_some()
                    && matches!(rest.rsplit_once('.'), Some((pid, "tmp")) if digits(pid).is_some()) =>
            {
                Named::Tmp
            }
            _ => Named::Foreign,
        }
    }
}

/// WHY this file may be deleted, or `None` when it may not. One function, so the
/// reaper and `doctor` cannot disagree about which files are stale - doctor prints
/// exactly the verdicts the next `SessionStart` will act on.
///
/// The reaper deletes only what it can PROVE is ours, and there are two proofs:
///
///   * a record that names its [`Origin`] is reaped iff that process is gone. This
///     is the rule that can prove a live session SAFE - see [`Origin::alive`] -
///     and it is why an idle session is not on a clock at all.
///   * a record with no origin, a newer version's record, or a `.tmp`, falls back
///     to mtime and [`REAP_AFTER`]. mtime cannot prove a session dead, so its
///     horizon is a day. Recognized background records with missing origin are
///     exempt: uncertainty cannot establish completion.
///
/// Everything else is left alone FOREVER: a file whose name is not a session id, a
/// file that is not a record in any version's shape, a file we could not read, and
/// anything that is not a regular file or a symlink. The name grammar cannot tell
/// a session id from `id_rsa`, so "old and unparseable" is not a proof of
/// ownership - acting on it once deleted a 30-day-old private key out of a
/// `CCTAB_STATE_DIR` that had other things in it. The price of the conservative
/// answer is bounded litter in a tmpfs that logout empties, and `doctor` names
/// every file it will not take.
fn reapable(path: &Path, name: Named) -> Option<String> {
    // A directory cannot be removed by `fs::remove_file`, so a stale verdict on
    // one is a promise the reaper cannot keep - and `doctor` then printed it
    // forever. `symlink_metadata`, because a symlink is reapable as ITSELF: the
    // link is unlinked and whatever it points at is untouched.
    let lm = fs::symlink_metadata(path).ok()?;
    if !(lm.is_file() || lm.is_symlink()) {
        return None;
    }
    match (name, stored_at(path)) {
        (Named::Record, Stored::Ours(r)) => match r.origin {
            Some(o) => (!o.alive()).then(|| format!("pid {} is not that process any more", o.pid)),
            // Missing process metadata cannot prove known background work has
            // ended. Keep this record until a snapshot or explicit cleanup.
            None if r.background.is_some() => None,
            None => stale_by_mtime(&lm, "no origin recorded"),
        },
        (Named::Record, Stored::Future) | (Named::Tmp, _) => {
            stale_by_mtime(&lm, "no origin recorded")
        }
        _ => None,
    }
}

/// The mtime half of [`reapable`], on the metadata already in hand.
///
/// A FUTURE mtime counts as fully aged rather than as an error. It is not one this
/// version wrote - a clock stepped backwards, an unpacked archive, a file copied
/// from a machine ahead of this one - and `duration_since` returns `Err` for it,
/// which used to make such a file immortal.
fn stale_by_mtime(m: &fs::Metadata, why: &str) -> Option<String> {
    let age = m.modified().ok().map(|t| {
        SystemTime::now()
            .duration_since(t)
            .unwrap_or(REAP_AFTER + Duration::from_secs(1))
    })?;
    (age > REAP_AFTER).then(|| format!("{why}, and untouched for {}h", age.as_secs() / 3600))
}

/// Remove the records this layer wrote, for `uninstall`. Returns where they were,
/// how many went, and whether the directory itself could go with them.
///
/// ONLY our own names, the same [`Named`] test the reaper uses, and then the
/// directory if that emptied it. `CCTAB_STATE_DIR` may be a directory the user
/// pointed here with other things in it, and an uninstaller that takes those is
/// worse than one that leaves a few files in a tmpfs the OS empties at logout. The
/// default directory is one this program created and nothing else writes to, so
/// there it goes whole.
///
/// Unlike the reaper this does NOT consult liveness: a still-running session's
/// record is removed too. That is safe by construction - a session with no record
/// paints exactly what it painted before this plugin existed - and by the time
/// uninstall runs the plugin is being unlinked, so no hook will write another.
pub fn purge() -> Option<(PathBuf, usize, bool)> {
    let d = dir()?;
    if !d.is_dir() {
        return None;
    }
    let mut gone = 0;
    if let Ok(entries) = fs::read_dir(&d) {
        for e in entries.flatten() {
            if Named::of(&e.file_name()) != Named::Foreign && fs::remove_file(e.path()).is_ok() {
                gone += 1;
            }
        }
    }
    Some((d.clone(), gone, fs::remove_dir(&d).is_ok()))
}

/// What `tabstatus doctor` reports about this layer: where records live, whether
/// the layer is on at all and writable, and for each record what it holds and
/// whether the next `SessionStart` will reap it.
///
/// READ-ONLY, with one exception it has to make. `doctor` is the command you run
/// when something is already wrong, so it must not be the command that deletes the
/// evidence; it names the files the reaper will take instead. The verdicts come
/// from [`reapable`], the same function the reaper calls, so the report cannot
/// drift from the behaviour. The exception is [`writable`], which creates and
/// immediately unlinks a file of its own - see there for why a report that cannot
/// answer that one question is worse than useless.
pub struct Survey {
    /// Where records live, or why there is nowhere to put them - in which case
    /// every edge falls back to the stateless answer.
    pub dir: Result<PathBuf, &'static str>,
    /// `Some(consequence)` when the directory exists and cannot be written.
    pub writable: Option<&'static str>,
    /// One line per file, in whatever order the directory yields them.
    pub records: Vec<(String, String)>,
    /// How many of those the next `SessionStart` would reap.
    pub stale: usize,
}

/// Whether a record could actually be WRITTEN here.
///
/// The one failure mode that resurrects the whole slice-3 defect in silence: a
/// state directory that is readable but not writable records no wait, so a
/// subagent's `PostToolUse` finds none to clear and paints nothing, and the tab
/// stays orange until the Task returns. Every other line `doctor` prints looks
/// healthy in that state, including "nothing recorded, which is also what a
/// session that has raised no dialog leaves behind".
///
/// A create-then-unlink of a file named for this process destroys no evidence, so
/// it does not breach the read-only contract above - it is the one write `doctor`
/// makes, and it removes what it made. The name begins with a dot, so
/// [`Named::of`] reads it as `Foreign` and the reaper would not touch it even if
/// the unlink failed.
fn writable(d: &Path) -> bool {
    let probe = d.join(format!(".cctab-probe.{}", itoa(u64::from(std::process::id()))));
    let ok = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&probe)
        .is_ok();
    let _ = fs::remove_file(&probe);
    ok
}

pub fn survey() -> Survey {
    let Some(d) = dir() else {
        return Survey {
            dir: Err("no CCTAB_STATE_DIR and no XDG_RUNTIME_DIR"),
            writable: None,
            records: Vec::new(),
            stale: 0,
        };
    };
    let now = tmux::now();
    let ttl = tmux::ttl_secs("CCTAB_TTL_WAITING", tmux::DEFAULT_TTL_WAITING);
    let mut records = Vec::new();
    let mut stale = 0;
    let listed = fs::read_dir(&d);
    let unreadable = listed.is_err() && d.exists();
    if let Ok(entries) = listed {
        for e in entries.flatten() {
            let path = e.path();
            let name = path
                .file_name()
                .map_or_else(String::new, |n| crate::text::repair(n.as_encoded_bytes()));
            let kind = Named::of(&e.file_name());
            // Five broken shapes used to print the same healthy-looking line as an
            // idle record, which made the report the wrong way to find out that
            // something was wrong.
            let mut what = match stored_at(&path) {
                Stored::Ours(mut r) => {
                    r.expire(now, ttl);
                    r.describe(now)
                }
                Stored::Future => "a NEWER version's record - ignored, and never reaped".to_owned(),
                Stored::Alien => {
                    "not a record in any version's shape - treated as absent".to_owned()
                }
                Stored::Absent => {
                    "unreadable, or too big to be a record - treated as absent".to_owned()
                }
            };
            if kind == Named::Foreign {
                what.push_str("; not a name this writes, so the reaper leaves it alone");
            }
            if let Some(why) = reapable(&path, kind) {
                stale += 1;
                what.push_str(&format!("; STALE ({why}), the next session-start reaps it"));
            }
            records.push((name, what));
        }
    }
    let cannot_write = d.is_dir() && !unreadable && !writable(&d);
    Survey {
        // A directory that does not exist yet is not a fault: it is created by the
        // first edge that has something to record.
        dir: if unreadable {
            Err("the state directory exists but cannot be read")
        } else {
            Ok(d)
        },
        writable: cannot_write.then_some(
            "not writable - no wait is ever recorded, so a subagent's dialog stays \
             orange until the Task returns",
        ),
        records,
        stale,
    }
}

// ---------------------------------------------------------------------------
// SEAM: what the next two slices need from this, and what they must add.
//
// THE SHAPE OF THE SEAM. `Record` is the whole of the state and `Session` is the
// whole of the I/O: `stored_at` parses, `render` serialises, `write_if_changed`
// decides whether a write happens at all, `lock` makes the read-modify-write
// atomic, and every transition is a method on `Session` that loads once, mutates a
// `Record`, and writes once. A new fact is therefore one field on `Record`, one
// arm in `Record::parse`, one block in `Record::render`, one line in
// `Record::describe` so `doctor` can show it, and a case in
// `every_field_survives_a_render_and_a_parse`. Nothing else moves, and in
// particular no caller learns a new shape: `resolve` still returns `Option<Paint>`.
//
// Field ORDER on the wire is load-bearing in exactly one way: `parse` splits each
// line on the FIRST space, so a free-form value must be last on its line, and the
// only free-form field planned - `n` - must therefore also be last in the file.
// `b`, `p` and `w` are ASCII words and may sit in any order.
//
// BACKGROUND. Main Stop snapshots own the aggregate `g` epoch. No per-child
// identity set is needed: a nonempty complete registry proves activity and an
// empty one retires it. Captured automatic completion turns supply that Stop.
// SubagentStop can still list its own finishing task, or its enclosing workflow;
// it clears wait ownership only. Missing metadata and duration never erase g.
// docs/indicator-semantics.md records evidence, reconciliation and migration.
//
// THE CACHED SESSION TITLE.
//   `aiTitle` lives in the transcript records at `transcript_path`, which every
//   payload carries, and it changes ONCE per session - so re-reading it per paint
//   would be waste. The shape:
//     * a reserved `n <epoch> <text>` line, LAST in the record and running to end
//       of line, because the title is free-form UTF-8 and everything above it is
//       ASCII words. `Record::parse` splits on the FIRST space only, so the text
//       arrives whole.
//     * `render()` writes it last for the same reason.
//     * a bounded tail read of `transcript_path` refreshes it when `n` is absent
//       or its epoch is older than the session's; the paint uses whatever is in
//       the record, so a miss costs a plain location and never a stall.
//     * it is the first field worth writing on an edge that would otherwise not
//       write, so it is also the first one that has to reckon with
//       `write_if_changed`'s contract - and with `MAX_RECORD`, which is 8192, so
//       the title has to be capped well inside it.
//   The cap and the elision then belong to `render::compose`, not here.
//
// THE TMUX DECAY CLOCK, recorded because it was asked and the answer is "as
// intended". Clearing a wait repaints the base with a FRESH epoch, so inside tmux
// the working/idle decay horizon slides forward by the whole duration of the
// dialog. That is defensible - the subagent's tool really did just complete, so
// the state really is that new - and it is confined to `agent_id` paths. If a
// later slice wants those horizons to measure the age of the STATE instead, the
// base needs its own epoch in the record, beside the per-wait ones, and
// `tmux::carrier` has to be handed that epoch rather than `now`.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;
    use std::sync::Mutex;

    /// `dir()` reads the environment, and Rust runs tests in threads of ONE
    /// process, so two tests setting `CCTAB_STATE_DIR` at once would see each
    /// other's value. Every test that touches the environment holds this.
    static ENV: Mutex<()> = Mutex::new(());

    struct Fixture {
        _guard: std::sync::MutexGuard<'static, ()>,
        dir: PathBuf,
        /// Saved so that a test which removes it cannot leak that into another:
        /// `cargo test` runs these in threads of one process.
        xdg: Option<OsString>,
        /// Saved for the same reason, and pinned rather than inherited: the suite
        /// runs INSIDE a Claude Code session, so a real `CLAUDE_PID` is in the
        /// environment and a write would otherwise record whichever session
        /// happened to run the tests. Pinned to our own pid, which is certainly
        /// alive, so the origin the reaper reads back is a true live one.
        claude_pid: Option<OsString>,
    }

    impl Fixture {
        /// A private state directory under the build's own target dir, so no test
        /// writes where a real session would.
        fn new(name: &str) -> Fixture {
            let guard = ENV.lock().unwrap_or_else(|e| e.into_inner());
            let mut dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
            dir.push("target");
            dir.push("state-tests");
            dir.push(name);
            let _ = fs::remove_dir_all(&dir);
            std::env::set_var("CCTAB_STATE_DIR", &dir);
            std::env::set_var("CCTAB_NOW", "1000000");
            std::env::remove_var("CCTAB_TTL_WAITING");
            let saved = std::env::var_os("CLAUDE_PID");
            std::env::set_var("CLAUDE_PID", itoa(u64::from(std::process::id())));
            Fixture {
                _guard: guard,
                dir,
                xdg: std::env::var_os("XDG_RUNTIME_DIR"),
                claude_pid: saved,
            }
        }

        /// The origin line every write stamps under this fixture.
        fn origin_line(&self) -> String {
            let o = Origin::mine().expect("our own pid is pinned into CLAUDE_PID");
            format!("p {} {}\n", o.pid, o.start)
        }

        fn session(&self, p: &Payload) -> Session {
            Session::open(dir(), p).expect("a state dir and a session id were provided")
        }

        fn record(&self) -> String {
            fs::read_to_string(self.dir.join("s1")).unwrap_or_default()
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            std::env::remove_var("CCTAB_STATE_DIR");
            std::env::remove_var("CCTAB_NOW");
            std::env::remove_var("CCTAB_TTL_WAITING");
            std::env::remove_var("CLAUDE_PID");
            if let Some(v) = self.xdg.take() {
                std::env::set_var("XDG_RUNTIME_DIR", v);
            }
            if let Some(v) = self.claude_pid.take() {
                std::env::set_var("CLAUDE_PID", v);
            }
            let _ = fs::remove_dir_all(&self.dir);
        }
    }

    fn payload(line: &str) -> Payload {
        let mut b = line.as_bytes().to_vec();
        b.push(b'\n');
        Payload::from_bytes(&b).unwrap()
    }

    /// A `Stop` whose `background_tasks` holds a live subagent, which is the
    /// capture's 68.946 shape and the conservative one: every wait stands.
    fn main_ev() -> Payload {
        payload(
            r#"{"session_id":"s1","hook_event_name":"Stop","background_tasks":[{"id":"aec99e1f4bda1972b","type":"subagent","status":"running"}]}"#,
        )
    }

    /// A `Stop` that retires permission and notification waits, not direct MCP waits.
    fn quiet_stop() -> Payload {
        payload(r#"{"session_id":"s1","hook_event_name":"Stop","background_tasks":[]}"#)
    }

    fn main_tool() -> Payload {
        payload(r#"{"session_id":"s1","hook_event_name":"PostToolUse"}"#)
    }

    fn user_prompt() -> Payload {
        payload(r#"{"session_id":"s1","hook_event_name":"UserPromptSubmit","prompt":"carry on"}"#)
    }

    /// What the PRODUCT injects when an async agent finishes - capture s4 at
    /// 110.251. It is a `UserPromptSubmit`, and it is not a human at the prompt.
    fn task_notification() -> Payload {
        payload(
            r#"{"session_id":"s1","hook_event_name":"UserPromptSubmit","prompt":"<task-notification>\n<task-id>aec99e1f4bda1972b</task-id>\n</task-notification>"}"#,
        )
    }

    fn agent_ev(id: &str) -> Payload {
        payload(&format!(
            r#"{{"session_id":"s1","agent_id":"{id}","hook_event_name":"PostToolUse"}}"#
        ))
    }

    fn notify(kind: &str) -> Payload {
        payload(&format!(
            r#"{{"session_id":"s1","hook_event_name":"Notification","notification_type":"{kind}"}}"#
        ))
    }

    fn agent_stop(id: &str) -> Payload {
        payload(&format!(
            r#"{{"session_id":"s1","agent_id":"{id}","hook_event_name":"SubagentStop"}}"#
        ))
    }

    fn elicitation(server: &str, id: &str, action: Option<&str>) -> Payload {
        let event = if action.is_some() { "ElicitationResult" } else { "Elicitation" };
        let response = action.map(|a| format!(",\"action\":\"{a}\"")).unwrap_or_default();
        payload(&format!(r#"{{"session_id":"s1","hook_event_name":"{event}","mcp_server_name":"{server}","elicitation_id":"{id}","mode":"url"{response}}}"#))
    }

    #[test]
    fn direct_results_clear_only_their_server_and_request_for_every_action() {
        for action in ["accept", "decline", "cancel"] {
            let f = Fixture::new("direct-actions");
            let main = main_tool();
            f.session(&main).resolve(Edge::Working, &main);
            for (server, id) in [("one", "A"), ("one", "B"), ("two", "A")] {
                let request = elicitation(server, id, None);
                assert_eq!(f.session(&request).resolve(Edge::Elicitation, &request), Some(Paint::Line(Glyph::Waiting)));
            }
            let first = elicitation("one", "A", Some(action));
            assert_eq!(f.session(&first).resolve(Edge::ElicitationResult, &first), None);
            assert_eq!(Record::parse(&f.record()).unwrap().waits.len(), 2);
            let second = elicitation("one", "B", Some(action));
            assert_eq!(f.session(&second).resolve(Edge::ElicitationResult, &second), None);
            let third = elicitation("two", "A", Some(action));
            assert_eq!(f.session(&third).resolve(Edge::ElicitationResult, &third), Some(Paint::Line(Glyph::Working)));
            assert_eq!(Record::parse(&f.record()).unwrap().completed.len(), 3);
        }
    }

    #[test]
    fn direct_waits_survive_main_activity_and_unidentified_results() {
        let f = Fixture::new("direct-recovery");
        let known = elicitation("server", "A", None);
        let anonymous = payload(r#"{"session_id":"s1","hook_event_name":"Elicitation","mcp_server_name":"server","mode":"form"}"#);
        for request in [&known, &anonymous] {
            f.session(request).resolve(Edge::Elicitation, request);
        }
        for (edge, event) in [
            (Edge::Working, main_tool()),
            (Edge::Working, task_notification()),
            (Edge::Working, agent_ev("aaa")),
            (Edge::Idle, quiet_stop()),
            (Edge::ElicitationResult, payload(r#"{"session_id":"s1","hook_event_name":"ElicitationResult","mcp_server_name":"server","action":"accept"}"#)),
        ] {
            assert_eq!(f.session(&event).resolve(edge, &event), None);
            assert_eq!(Record::parse(&f.record()).unwrap().waits.len(), 2);
        }
        let result = elicitation("server", "A", Some("cancel"));
        assert_eq!(f.session(&result).resolve(Edge::ElicitationResult, &result), None);
        assert_eq!(Record::parse(&f.record()).unwrap().waits[0].who, Owner::AnonymousElicitation);
        let prompt = user_prompt();
        assert_eq!(f.session(&prompt).resolve(Edge::Working, &prompt), Some(Paint::Line(Glyph::Working)));
        assert!(Record::parse(&f.record()).unwrap().waits.is_empty());
    }

    #[test]
    fn completion_tombstones_handle_out_of_order_replays_without_refreshing() {
        let f = Fixture::new("direct-replay");
        let result = elicitation("server", "A", Some("accept"));
        assert_eq!(f.session(&result).resolve(Edge::ElicitationResult, &result), None);
        let completed = f.record();
        std::env::set_var("CCTAB_NOW", "1000002");
        let request = elicitation("server", "A", None);
        assert_eq!(f.session(&request).resolve(Edge::Elicitation, &request), None);
        assert_eq!(f.session(&result).resolve(Edge::ElicitationResult, &result), None);
        assert_eq!(f.record(), completed);
        std::env::set_var("CCTAB_NOW", "1000901");
        assert_eq!(f.session(&request).resolve(Edge::Elicitation, &request), Some(Paint::Line(Glyph::Waiting)));
        let raised = f.record();
        std::env::set_var("CCTAB_NOW", "1000902");
        f.session(&request).resolve(Edge::Elicitation, &request);
        assert_eq!(f.record(), raised);
    }

    #[test]
    fn only_identified_notification_duplicates_coalesce() {
        let f = Fixture::new("direct-notifications");
        let identified = payload(r#"{"session_id":"s1","hook_event_name":"Notification","notification_type":"elicitation_url_dialog","mcp_server_name":"server","elicitation_id":"A"}"#);
        f.session(&identified).resolve(Edge::Notify, &identified);
        let request = elicitation("server", "A", None);
        f.session(&request).resolve(Edge::Elicitation, &request);
        assert_eq!(Record::parse(&f.record()).unwrap().waits.len(), 1);
        let result = elicitation("server", "A", Some("decline"));
        assert_eq!(f.session(&result).resolve(Edge::ElicitationResult, &result), Some(Paint::Line(Glyph::Idle)));
        assert_eq!(f.session(&identified).resolve(Edge::Notify, &identified), None);
        let unidentified = notify("elicitation_url_dialog");
        assert_eq!(f.session(&unidentified).resolve(Edge::Notify, &unidentified), Some(Paint::Line(Glyph::Waiting)));
        let record = Record::parse(&f.record()).unwrap();
        assert_eq!(record.waits.len(), 1);
        assert_eq!(record.waits[0].who, Owner::UnknownElicitation);
    }

    #[test]
    fn direct_wire_bounds_round_trip_without_losing_live_waits() {
        let mut record = Record::fresh();
        for i in 0..MAX_WAITS * 2 {
            let server = format!("{i:064}");
            let id = "i".repeat(64);
            let event = elicitation(&server, &id, None);
            let key = elicitation_key(&event).unwrap();
            record.raise(key.clone(), 1);
            record.complete(key, 2);
        }
        assert_eq!(record.waits.len(), MAX_WAITS + 1);
        assert_eq!(record.waits.last().unwrap().who, Owner::Overflow);
        assert_eq!(record.completed.len(), MAX_WAITS);
        let rendered = record.render();
        assert!(rendered.len() < MAX_RECORD);
        assert_eq!(Record::parse(&rendered), Some(record.clone()));
        record.raise(Owner::Agent("overflow-again".to_owned()), 10);
        assert_eq!(record.waits.last().unwrap().raised, 1);
        assert_eq!(Owner::parse("!aa."), None);
        assert_eq!(Owner::parse("!a.bb"), None);
        assert_eq!(Owner::parse(&format!("!{}.bb", "aa".repeat(65))), None);
    }

    #[test]
    fn the_captured_subagent_sequence_ends_orange_and_then_restores_the_base() {
        let f = Fixture::new("capture");
        let main = main_ev();
        let agent = agent_ev("aec99e1f4bda1972b");
        // 68.946 Stop, with the subagent already launched and LISTED in
        // background_tasks, which is why this one retires nothing.
        assert_eq!(
            f.session(&main).resolve(Edge::Idle, &main),
            Some(Paint::LineWithBackground(Glyph::Background))
        );
        // 69.960 the subagent's PermissionRequest.
        assert_eq!(
            f.session(&agent).resolve(Edge::Waiting, &agent),
            Some(Paint::LineWithBackground(Glyph::Waiting))
        );
        // 75.983 the 6s backstop for the SAME dialog: still orange, and it must
        // not add an owner nothing can clear.
        let backstop = notify("permission_prompt");
        assert_eq!(
            f.session(&backstop).resolve(Edge::Notify, &backstop),
            Some(Paint::LineWithBackground(Glyph::Waiting))
        );
        let origin = f.origin_line();
        let held = format!("cts5\nb i\ng 1000000\n{origin}w aec99e1f4bda1972b:1000000\n");
        assert_eq!(f.record(), held);
        // 98.459 the GHOST SubagentStop, for an agent that owns nothing.
        let ghost = agent_stop("a8e90c10430da8891");
        assert_eq!(f.session(&ghost).resolve(Edge::SubagentStop, &ghost), None);
        assert_eq!(f.record(), held);
        // 107.496 you approved: the subagent's own PostToolUse restores the base.
        assert_eq!(
            f.session(&agent).resolve(Edge::Working, &agent),
            Some(Paint::LineWithBackground(Glyph::Background))
        );
        assert_eq!(f.record(), format!("cts5\nb i\ng 1000000\n{origin}"));
    }

    #[test]
    fn a_background_subagents_tool_call_still_paints_nothing() {
        let f = Fixture::new("bg");
        let agent = agent_ev("aaa");
        // Nothing is waiting, so this is the ORIGINAL filter, unchanged - and it
        // writes nothing at all, which is what keeps the hot edge a read.
        assert_eq!(f.session(&agent).resolve(Edge::Working, &agent), None);
        assert_eq!(f.record(), "");
        assert!(!f.dir.join("s1").exists());
    }

    #[test]
    fn a_main_thread_tool_call_does_not_repaint_over_an_agents_dialog() {
        let f = Fixture::new("overlap");
        let agent = agent_ev("aaa");
        let main = main_tool();
        f.session(&agent).resolve(Edge::Waiting, &agent);
        // The base is still updated - main IS working - but the tab keeps the
        // dialog. Stateless, this painted blue over an open dialog.
        assert_eq!(f.session(&main).resolve(Edge::Working, &main), None);
        assert_eq!(
            f.record(),
            format!("cts5\nb w\n{}w aaa:1000000\n", f.origin_line())
        );
        // ...and when the agent resolves it, the base that comes back is the
        // WORKING one this turn established, not a guess.
        assert_eq!(
            f.session(&agent).resolve(Edge::Working, &agent),
            Some(Paint::Line(Glyph::Working))
        );
    }

    #[test]
    fn two_overlapping_dialogs_both_have_to_be_answered() {
        let f = Fixture::new("two");
        let agent = agent_ev("aaa");
        let main = main_ev();
        f.session(&agent).resolve(Edge::Waiting, &agent);
        f.session(&main).resolve(Edge::Waiting, &main);
        assert_eq!(
            f.record(),
            format!("cts5\nb i\n{}w aaa:1000000 -:1000000\n", f.origin_line())
        );
        // Answering the main one leaves the agent's dialog on screen, so the tab
        // stays orange. A single owner slot would have got this wrong whichever
        // one it kept.
        let mainwork = main_tool();
        assert_eq!(f.session(&mainwork).resolve(Edge::Working, &mainwork), None);
        assert_eq!(
            f.session(&agent).resolve(Edge::Working, &agent),
            Some(Paint::Line(Glyph::Working))
        );
    }

    #[test]
    fn stop_does_not_paint_idle_over_an_outstanding_agent_dialog() {
        let f = Fixture::new("stopguard");
        let agent = agent_ev("aaa");
        let main = main_ev();
        f.session(&agent).resolve(Edge::Waiting, &agent);
        assert_eq!(f.session(&main).resolve(Edge::Idle, &main), Some(Paint::LineWithBackground(Glyph::Waiting)));
        // The idle nudge is the same transition and is guarded the same way. It
        // matters because this user's `messageIdleNotifThresholdMs` is 3000. Its
        // payload carries no `background_tasks` at all, so it cannot retire
        // anything either.
        let nudge = notify("idle_prompt");
        assert_eq!(f.session(&nudge).resolve(Edge::Notify, &nudge), None);
    }

    #[test]
    fn stop_does_clear_a_stale_main_wait() {
        let f = Fixture::new("stopmain");
        let main = main_ev();
        // A rejected ExitPlanMode: PreToolUse painted waiting, and no PostToolUse
        // ever comes. The main loop could not have stopped while a main-thread
        // dialog blocked it, so this needs no help from `background_tasks` - and
        // this Stop's array is deliberately NON-empty to prove that.
        f.session(&main).resolve(Edge::Waiting, &main);
        assert_eq!(
            f.session(&main).resolve(Edge::Idle, &main),
            Some(Paint::LineWithBackground(Glyph::Background))
        );
        assert_eq!(f.record(), format!("cts5\nb i\ng 1000000\n{}", f.origin_line()));
    }

    /// The fix for the stale AGENT wait: a `Stop` that says nothing is running
    /// proves the agent is gone, whatever hook it failed to fire on the way out.
    /// Esc at a subagent's dialog fires no hook at all, so without this the tab
    /// stayed orange for the whole 900s TTL - and outside tmux nothing else decays.
    #[test]
    fn an_empty_background_tasks_at_stop_retires_an_abandoned_agent_wait() {
        let f = Fixture::new("bgempty");
        let agent = agent_ev("aaa");
        f.session(&agent).resolve(Edge::Waiting, &agent);
        let quiet = quiet_stop();
        assert_eq!(
            f.session(&quiet).resolve(Edge::Idle, &quiet),
            Some(Paint::Line(Glyph::Idle))
        );
        assert_eq!(f.record(), format!("cts5\nb i\n{}", f.origin_line()));
    }

    /// And the other main-thread retirement: you cannot type at the prompt while a
    /// modal dialog is up, so a `UserPromptSubmit` proves the screen is clear. This
    /// bounds a stale agent wait to ONE turn even when no `Stop` ever arrives - a
    /// Ctrl+C mid-tool fires nothing at all, measured on capture s5.
    #[test]
    fn a_user_prompt_retires_every_wait_and_a_tool_call_does_not() {
        let f = Fixture::new("prompt");
        let agent = agent_ev("aaa");
        f.session(&agent).resolve(Edge::Waiting, &agent);
        // A main-thread TOOL call says nothing about somebody else's dialog.
        let tool = main_tool();
        assert_eq!(f.session(&tool).resolve(Edge::Working, &tool), None);
        // Nor does the `UserPromptSubmit` the PRODUCT injects when an async agent
        // finishes: with two agents running, the first one's completion would
        // otherwise retire the second one's live dialog. Capture s4, 110.251.
        let injected = task_notification();
        assert_eq!(f.session(&injected).resolve(Edge::Working, &injected), None);
        assert_eq!(
            f.record(),
            format!("cts5\nb w\n{}w aaa:1000000\n", f.origin_line())
        );
        // The user typing does.
        let prompt = user_prompt();
        assert_eq!(
            f.session(&prompt).resolve(Edge::Working, &prompt),
            Some(Paint::Line(Glyph::Working))
        );
        assert_eq!(f.record(), format!("cts5\nb w\n{}", f.origin_line()));
    }

    /// The defect a SHARED epoch caused: unrelated later dialogs refreshed a stale
    /// wait's clock, so it never expired and nothing painted again all session.
    #[test]
    fn an_unrelated_dialog_does_not_refresh_a_stale_waits_clock() {
        let f = Fixture::new("epochs");
        let agent = agent_ev("aaa");
        f.session(&agent).resolve(Edge::Waiting, &agent);
        // Four UNRELATED dialogs, 2s apart, inside the first agent's horizon. Each
        // one used to push agent aaa's expiry out with it, because the epoch was
        // one field for the whole list.
        let other = agent_ev("bbb");
        for t in ["1000002", "1000004", "1000006", "1000008"] {
            std::env::set_var("CCTAB_NOW", t);
            f.session(&other).resolve(Edge::Waiting, &other);
        }
        assert_eq!(
            f.record(),
            format!("cts5\nb i\n{}w aaa:1000000 bbb:1000008\n", f.origin_line())
        );
        // aaa is 9s old against a 3s horizon and goes; bbb is 1s old and stays, so
        // the tab is still correctly orange and nothing paints.
        std::env::set_var("CCTAB_NOW", "1000009");
        std::env::set_var("CCTAB_TTL_WAITING", "3");
        let main = main_ev();
        assert_eq!(f.session(&main).resolve(Edge::Idle, &main), Some(Paint::LineWithBackground(Glyph::Waiting)));
        // And the expiry was PERSISTED rather than recomputed for ever, so raising
        // the TTL later cannot resurrect a wait already declared dead.
        assert_eq!(
            f.record(),
            format!("cts5\nb i\ng 1000009\n{}w bbb:1000008\n", f.origin_line())
        );
    }

    #[test]
    fn an_unusable_permission_owner_is_anonymous_not_main() {
        for id in ["-".to_owned(), "?".to_owned(), "a/b".to_owned(), "a".repeat(65)] {
            let f = Fixture::new("invalid_permission_owner");
            let request = payload(&format!(
                r#"{{"session_id":"s1","hook_event_name":"PermissionRequest","agent_id":"{id}"}}"#
            ));
            assert_eq!(f.session(&request).resolve(Edge::Waiting, &request),
                Some(Paint::Line(Glyph::Waiting)));
            assert_eq!(f.record(), format!("cts5\nb i\n{}w ?p:1000000\n", f.origin_line()));
            let stop = payload(r#"{"session_id":"s1","hook_event_name":"Stop"}"#);
            assert_eq!(f.session(&stop).resolve(Edge::Idle, &stop), None);
            let unrelated = agent_stop("other");
            assert_eq!(f.session(&unrelated).resolve(Edge::SubagentStop, &unrelated), None);
            let human = user_prompt();
            assert_eq!(f.session(&human).resolve(Edge::Working, &human),
                Some(Paint::Line(Glyph::Working)));
            assert!(!f.record().contains("\nw "));
        }
    }

    #[test]
    fn an_unknown_owner_needs_more_than_unrelated_agent_progress() {
        let f = Fixture::new("unknown");
        let backstop = notify("agent_needs_input");
        assert_eq!(
            f.session(&backstop).resolve(Edge::Notify, &backstop),
            Some(Paint::Line(Glyph::Waiting))
        );
        assert_eq!(
            f.record(),
            format!("cts5\nb i\n{}w ?:1000000\n", f.origin_line())
        );
        // A `Stop` with a live subagent still says nothing about it.
        let busy = main_ev();
        assert_eq!(f.session(&busy).resolve(Edge::Idle, &busy), Some(Paint::LineWithBackground(Glyph::Waiting)));
        // No evidence connects this agent to the notification's unknown owner.
        let agent = agent_ev("aaa");
        let held = f.record();
        assert_eq!(f.session(&agent).resolve(Edge::Working, &agent), None);
        assert_eq!(f.record(), held);
        let quiet = quiet_stop();
        assert_eq!(
            f.session(&quiet).resolve(Edge::Idle, &quiet),
            Some(Paint::Line(Glyph::Idle))
        );
        assert_eq!(f.record(), format!("cts5\nb i\n{}", f.origin_line()));
    }

    /// The `?` used to survive `Stop` AND the 3s nudge, so once the user had dealt
    /// with the dialog the tab sat orange through the whole idle period - where the
    /// STATELESS binary painted white. All five waiting kinds behaved that way,
    /// not the two the README named.
    #[test]
    fn a_quiet_stop_retires_an_unknown_owner_for_every_waiting_kind() {
        for kind in [
            "permission_prompt",
            "worker_permission_prompt",
            "agent_needs_input",
            "elicitation_dialog",
            "elicitation_url_dialog",
        ] {
            let f = Fixture::new("unknownkinds");
            let backstop = notify(kind);
            assert_eq!(
                f.session(&backstop).resolve(Edge::Notify, &backstop),
                Some(Paint::Line(Glyph::Waiting)),
                "{kind}"
            );
            let quiet = quiet_stop();
            assert_eq!(
                f.session(&quiet).resolve(Edge::Idle, &quiet),
                Some(Paint::Line(Glyph::Idle)),
                "{kind}"
            );
        }
    }

    /// The backstop arriving FIRST, which `wait()`'s guard cannot catch. The
    /// attributable dialog supersedes it, so one dialog is one wait and the agent
    /// that owns it can retire it.
    #[test]
    fn an_attributable_dialog_supersedes_a_lone_unknown() {
        let f = Fixture::new("supersede");
        let backstop = notify("worker_permission_prompt");
        f.session(&backstop).resolve(Edge::Notify, &backstop);
        let agent = agent_ev("aaa");
        f.session(&agent).resolve(Edge::Waiting, &agent);
        assert_eq!(
            f.record(),
            format!("cts5\nb i\n{}w aaa:1000000\n", f.origin_line())
        );
        assert_eq!(
            f.session(&agent).resolve(Edge::Working, &agent),
            Some(Paint::Line(Glyph::Idle))
        );
    }

    #[test]
    fn a_declined_dialog_is_cleared_by_the_agents_subagent_stop() {
        let f = Fixture::new("declined");
        let agent = agent_ev("aaa");
        f.session(&agent).resolve(Edge::Waiting, &agent);
        // No hook fires for a denial, so the tool never runs and no PostToolUse
        // arrives. SubagentStop is the only signal left.
        let stop = agent_stop("aaa");
        assert_eq!(
            f.session(&stop).resolve(Edge::SubagentStop, &stop),
            Some(Paint::Line(Glyph::Idle))
        );
        assert_eq!(f.record(), format!("cts5\nb i\n{}", f.origin_line()));
    }

    /// A lone unknown wait provides no evidence linking it to a stopping agent.
    #[test]
    fn a_subagent_stop_preserves_a_lone_unknown() {
        let f = Fixture::new("unknownstop");
        let backstop = notify("worker_permission_prompt");
        f.session(&backstop).resolve(Edge::Notify, &backstop);
        let stop = agent_stop("aaa");
        let held = f.record();
        assert_eq!(f.session(&stop).resolve(Edge::SubagentStop, &stop), None);
        assert_eq!(f.record(), held);
    }

    #[test]
    fn elicitation_notifications_survive_unrelated_completions_after_reload() {
        for kind in ["elicitation_dialog", "elicitation_url_dialog"] {
            let f = Fixture::new("elicitation-unrelated");
            let prompt = user_prompt();
            f.session(&prompt).resolve(Edge::Working, &prompt);
            let notification = notify(kind);
            f.session(&notification).resolve(Edge::Notify, &notification);
            let held = format!("cts5\nb w\n{}w ?!:1000000\n", f.origin_line());
            assert_eq!(f.record(), held);
            for (edge, event) in [
                (Edge::Working, agent_ev("unrelated")),
                (Edge::SubagentStop, agent_stop("unrelated")),
                (Edge::Working, task_notification()),
                (Edge::Working, payload(r#"{"session_id":"s1","agent_id":"invalid/id","hook_event_name":"PostToolUse"}"#)),
                (Edge::Working, payload(r#"{"session_id":"s1","agent_id":"-","hook_event_name":"PostToolUse"}"#)),
                (Edge::Working, payload(r#"{"session_id":"s1","agent_id":"unrelated","hook_event_name":"PostToolUseFailure"}"#)),
            ] {
                assert_eq!(f.session(&event).resolve(edge, &event), None, "{kind}");
                assert_eq!(f.record(), held, "{kind}");
            }
            assert_eq!(f.session(&prompt).resolve(Edge::Working, &prompt), Some(Paint::Line(Glyph::Working)));
            assert!(!f.record().contains("\nw "));
        }
    }

    #[test]
    fn elicitation_and_owned_permission_waits_coexist_in_both_orders() {
        for elicitation_first in [false, true] {
            let f = Fixture::new("elicitation-overlap");
            let notification = notify("elicitation_dialog");
            let permission = agent_ev("aaa");
            let events = if elicitation_first {
                [(Edge::Notify, &notification), (Edge::Waiting, &permission)]
            } else {
                [(Edge::Waiting, &permission), (Edge::Notify, &notification)]
            };
            for (edge, event) in events {
                f.session(event).resolve(edge, event);
            }
            assert_eq!(Record::parse(&f.record()).unwrap().waits.len(), 2);
            assert_eq!(f.session(&permission).resolve(Edge::Working, &permission), None);
            assert_eq!(f.record(), format!("cts5\nb i\n{}w ?!:1000000\n", f.origin_line()));
        }
    }

    #[test]
    fn elicitation_and_permission_notifications_remain_independent() {
        for elicitation_first in [false, true] {
            let f = Fixture::new("elicitation-backstops");
            let elicitation = notify("elicitation_url_dialog");
            let permission = notify("worker_permission_prompt");
            let events = if elicitation_first { [&elicitation, &permission] } else { [&permission, &elicitation] };
            for event in events {
                f.session(event).resolve(Edge::Notify, event);
            }
            assert_eq!(Record::parse(&f.record()).unwrap().waits.len(), 2);
            let owner = agent_ev("aaa");
            f.session(&owner).resolve(Edge::Waiting, &owner);
            assert_eq!(f.record(), format!("cts5\nb i\n{}w ?!:1000000 aaa:1000000\n", f.origin_line()));
            assert_eq!(f.session(&owner).resolve(Edge::Working, &owner), None);
            assert_eq!(f.record(), format!("cts5\nb i\n{}w ?!:1000000\n", f.origin_line()));
        }
    }

    #[test]
    fn elicitation_waits_keep_main_progress_and_expiry_recovery() {
        for expire in [false, true] {
            let f = Fixture::new("elicitation-recovery");
            let notification = notify("elicitation_dialog");
            f.session(&notification).resolve(Edge::Notify, &notification);
            if expire {
                std::env::set_var("CCTAB_NOW", "1000901");
                let unrelated = agent_ev("aaa");
                assert_eq!(f.session(&unrelated).resolve(Edge::Working, &unrelated), None);
            } else {
                let main = main_tool();
                assert_eq!(f.session(&main).resolve(Edge::Working, &main), Some(Paint::Line(Glyph::Working)));
            }
            assert!(!f.record().contains("\nw "));
        }
    }

    #[test]
    fn legacy_unknown_waits_are_read_and_protected_from_agents() {
        let f = Fixture::new("legacy-unknown");
        fs::create_dir_all(&f.dir).unwrap();
        let legacy = "cts1\nb w\nw ?:1000000\n";
        fs::write(f.dir.join("s1"), legacy).unwrap();
        for (edge, event) in [(Edge::Working, agent_ev("aaa")), (Edge::SubagentStop, agent_stop("aaa"))] {
            assert_eq!(f.session(&event).resolve(edge, &event), None);
            assert_eq!(f.record(), legacy);
        }
        let prompt = user_prompt();
        f.session(&prompt).resolve(Edge::Working, &prompt);
        assert!(f.record().starts_with("cts5\n"));
        assert!(!f.record().contains("\nw "));
    }

    #[test]
    fn a_wait_nothing_ever_cleared_expires() {
        let f = Fixture::new("ttl");
        let agent = agent_ev("aaa");
        f.session(&agent).resolve(Edge::Waiting, &agent);
        // 901s later, past the 900s CCTAB_TTL_WAITING default: a subagent killed
        // mid-dialog fires no SubagentStop, and the tab must not stay orange. This
        // Stop still says a subagent is running, so ONLY the expiry can do it.
        std::env::set_var("CCTAB_NOW", "1000901");
        let main = main_ev();
        assert_eq!(
            f.session(&main).resolve(Edge::Idle, &main),
            Some(Paint::LineWithBackground(Glyph::Background))
        );
    }

    #[test]
    fn session_start_resets_and_session_end_removes() {
        let f = Fixture::new("lifecycle");
        let agent = agent_ev("aaa");
        f.session(&agent).resolve(Edge::Waiting, &agent);
        let start =
            payload(r#"{"session_id":"s1","hook_event_name":"SessionStart","source":"resume"}"#);
        assert_eq!(
            f.session(&start).resolve(Edge::SessionStart, &start),
            Some(Paint::SessionStart)
        );
        // Reset, AND claimed: the origin is what lets another session's reaper
        // tell this file from a dead session's.
        assert_eq!(f.record(), format!("cts5\nb i\n{}", f.origin_line()));
        let end = payload(r#"{"session_id":"s1","hook_event_name":"SessionEnd"}"#);
        assert_eq!(
            f.session(&end).resolve(Edge::SessionEnd, &end),
            Some(Paint::SessionEnd)
        );
        assert_eq!(f.record(), "");
    }

    /// The origin is carried by every later write without any of them re-reading
    /// `/proc`, and its absence is not a failure - just the mtime fallback.
    #[test]
    fn the_origin_is_written_once_and_then_carried() {
        let f = Fixture::new("origin");
        let start = payload(r#"{"session_id":"s1","hook_event_name":"SessionStart"}"#);
        f.session(&start).resolve(Edge::SessionStart, &start);
        let origin = f.origin_line();
        // A painting edge that changes the base rewrites the record, and the origin
        // survives that rewrite - otherwise the reaper would lose the session after
        // its first tool call.
        let main = main_tool();
        f.session(&main).resolve(Edge::Working, &main);
        assert_eq!(f.record(), format!("cts5\nb w\n{origin}"));

        // With no `CLAUDE_PID` there is no origin to claim, so a fresh session's
        // `SessionStart` has nothing to say that the ABSENCE of a file does not
        // already say - and `write_if_changed` therefore creates no file at all.
        // A record that never exists cannot be reaped wrongly either.
        std::env::remove_var("CLAUDE_PID");
        let s2 = payload(r#"{"session_id":"s2","hook_event_name":"SessionStart"}"#);
        Session::open(dir(), &s2)
            .expect("a state dir")
            .resolve(Edge::SessionStart, &s2);
        assert!(!f.dir.join("s2").exists());
    }

    /// A record written by a version that stamped no origin gains one on its next
    /// write, so the reaper's liveness proof covers it too rather than leaving it
    /// on the 24h mtime clock for the life of the session. One such record existed
    /// on this machine, for a session that was running.
    #[test]
    fn a_write_stamps_an_origin_the_record_is_missing() {
        let f = Fixture::new("restamp");
        fs::create_dir_all(&f.dir).expect("a writable state dir");
        fs::write(f.dir.join("s1"), "cts5\nb i\n").expect("a writable state dir");
        let main = main_tool();
        f.session(&main).resolve(Edge::Working, &main);
        assert_eq!(f.record(), format!("cts5\nb w\n{}", f.origin_line()));
    }

    #[test]
    fn a_mid_turn_compaction_session_start_resets_nothing() {
        let f = Fixture::new("compact");
        let agent = agent_ev("aaa");
        f.session(&agent).resolve(Edge::Waiting, &agent);
        let before = f.record();
        assert_eq!(
            before,
            format!("cts5\nb i\n{}w aaa:1000000\n", f.origin_line())
        );
        let start =
            payload(r#"{"session_id":"s1","hook_event_name":"SessionStart","source":"compact"}"#);
        assert_eq!(f.session(&start).resolve(Edge::SessionStart, &start), None);
        assert_eq!(f.record(), before);
    }

    #[test]
    fn no_session_id_and_no_state_dir_are_both_inert() {
        let _f = Fixture::new("inert");
        // A payload with no session_id: there is no key to file the record under.
        assert!(Session::open(dir(), &payload(r#"{"hook_event_name":"Stop"}"#)).is_none());
        // A session_id that could choose the path is refused outright.
        assert!(Session::open(dir(), &payload(r#"{"session_id":"../../x"}"#)).is_none());
        assert!(Session::open(dir(), &payload(r#"{"session_id":""}"#)).is_none());
        // Restored, because `cargo test` runs these in threads of ONE process
        // and a removed variable would outlive this test.
        let saved = [("XDG_RUNTIME_DIR", std::env::var_os("XDG_RUNTIME_DIR"))];
        std::env::remove_var("CCTAB_STATE_DIR");
        std::env::remove_var("XDG_RUNTIME_DIR");
        assert!(dir().is_none());
        assert!(Session::open(dir(), &main_ev()).is_none());
        for (k, v) in saved {
            if let Some(v) = v {
                std::env::set_var(k, v);
            }
        }
    }

    #[test]
    fn a_record_that_is_not_ours_is_ignored_rather_than_misread() {
        for text in ["", "junk\nb w\n", "cts9\nb w\n"] {
            assert_eq!(Record::parse(text), None, "{text:?}");
        }
        // Our tag with nothing usable under it IS ours, and reads as fresh.
        assert_eq!(Record::parse("cts5\nb\nw\n"), Some(Record::fresh()));
        // A base letter this version cannot paint - the purple seam - degrades to
        // idle, and a reserved key is skipped rather than failing the parse.
        let r = Record::parse("cts5\nb p\ng aaa bbb\nn 5 a title\nw -:999\n")
            .expect("our own tag parses");
        assert_eq!(r.base, Glyph::Idle);
        assert_eq!(r.waits, vec![Wait { who: Owner::Main, raised: 999 }]);
        // A `w` word in the older single-epoch shape is dropped rather than read
        // with an invented clock, so such a record degrades to "nothing waiting".
        let old = Record::parse("cts5\nb w\nw 1790459815 aec99e1f\n").expect("our own tag");
        assert_eq!(old.waits, Vec::new());
    }

    /// The shapes a file in the directory can be, which is what `doctor` reports
    /// and what decides whether the reaper may touch it at all.
    #[test]
    fn a_future_tag_is_told_apart_from_something_that_is_not_a_record() {
        let f = Fixture::new("shapes");
        fs::create_dir_all(&f.dir).expect("a writable state dir");
        let at = |name: &str, body: &[u8]| {
            let p = f.dir.join(name);
            fs::write(&p, body).expect("a writable state dir");
            p
        };
        assert!(matches!(stored_at(&at("ours", b"cts5\nb w\n")), Stored::Ours(_)));
        assert!(matches!(stored_at(&at("later", b"cts9\nb w\n")), Stored::Future));
        assert!(matches!(stored_at(&at("empty", b"")), Stored::Alien));
        assert!(matches!(stored_at(&at("bin", b"\x00\x01\x02junk")), Stored::Alien));
        // Our tag on its own IS ours - an empty record - because temp-then-rename
        // means a half-written record is not a shape that can reach the disk.
        assert!(matches!(stored_at(&at("bare", b"cts5")), Stored::Ours(_)));
        assert!(matches!(stored_at(&at("torn", b"ct")), Stored::Alien));
        assert!(matches!(
            stored_at(&at("big", &vec![b'x'; MAX_RECORD + 1])),
            Stored::Absent
        ));
        // A first line that merely BEGINS with the family cannot claim the benefit
        // of the doubt a real newer tag gets.
        let mut long = b"cts".to_vec();
        long.extend(std::iter::repeat_n(b'9', 40));
        assert!(matches!(stored_at(&at("longtag", &long)), Stored::Alien));
        // A directory and a missing file are both absent rather than a failure, and
        // neither one blocks.
        assert!(matches!(stored_at(&f.dir), Stored::Absent));
        assert!(matches!(stored_at(&f.dir.join("nope")), Stored::Absent));
    }

    /// What the file NAME decides, before anything is read.
    #[test]
    fn only_our_own_names_are_ever_reap_candidates() {
        let name = |n: &str| Named::of(OsStr::new(n));
        assert_eq!(name("aec0f2b1-4d31-4e11-9a41-2c7d55e1a900"), Named::Record);
        assert_eq!(name("s1"), Named::Record);
        assert_eq!(name("s1.1234.tmp"), Named::Tmp);
        // A user's own files, in a CCTAB_STATE_DIR they pointed somewhere shared.
        assert_eq!(name("notes.txt"), Named::Foreign);
        assert_eq!(name("id_rsa.pub"), Named::Foreign);
        assert_eq!(name(".cctab-probe.7"), Named::Foreign);
        assert_eq!(name("s1.tmp"), Named::Foreign);
        assert_eq!(name("s1.abc.tmp"), Named::Foreign);
        assert_eq!(name(&"x".repeat(MAX_ID + 1)), Named::Foreign);
    }

    /// Every field the wire carries survives a render and a parse unchanged. This
    /// is the test that stops a new field being written and then silently dropped
    /// by the reader - which is the failure the purple and title seams are one line
    /// of code away from each.
    #[test]
    fn every_field_survives_a_render_and_a_parse() {
        let cases = [
            Record::fresh(),
            Record {
                base: Glyph::Working,
                ..Record::fresh()
            },
            Record {
                base: Glyph::Waiting,
                background: Some(123),
                origin: Some(Origin { pid: 3_709_427, start: 84_460_384 }),
                waits: vec![
                    Wait { who: Owner::Agent("aec99e1f4bda1972b".to_owned()), raised: 999_995 },
                    Wait { who: Owner::Main, raised: 999_998 },
                    Wait { who: Owner::Unknown, raised: 1_000_000 },
                ],
                completed: Vec::new(),
            },
            // The edges of the numbers, because all of them go through `itoa` and
            // `digits` rather than the formatter and `parse`.
            Record {
                base: Glyph::Idle,
                background: Some(u64::MAX),
                origin: Some(Origin { pid: u32::MAX, start: u64::MAX }),
                waits: vec![Wait { who: Owner::Main, raised: u64::MAX }],
                completed: Vec::new(),
            },
        ];
        for r in cases {
            assert_eq!(Record::parse(&r.render()), Some(r.clone()), "{:?}", r.render());
        }
        // And the shape is the documented one, not merely self-consistent.
        assert_eq!(
            Record {
                base: Glyph::Working,
                background: None,
                origin: Some(Origin { pid: 42, start: 7 }),
                waits: vec![Wait { who: Owner::Agent("ab-c_D".to_owned()), raised: 99 }],
                completed: Vec::new(),
            }
            .render(),
            "cts5\nb w\np 42 7\nw ab-c_D:99\n"
        );
    }

    /// The reaper's decision, which is the one piece of this module that deletes
    /// something. Two properties matter: a record whose origin is a live process is
    /// never reapable however old it is, and nothing the reaper cannot PROVE is
    /// ours is reapable at all.
    #[test]
    fn the_reaper_reaps_a_dead_session_and_provably_not_a_live_one() {
        let f = Fixture::new("reap");
        fs::create_dir_all(&f.dir).expect("a writable state dir");
        let write = |name: &str, text: &str| {
            let p = f.dir.join(name);
            fs::write(&p, text).expect("a writable state dir");
            p
        };
        let age = |p: &Path| {
            let old = SystemTime::now() - (REAP_AFTER + Duration::from_secs(3600));
            fs::File::options()
                .write(true)
                .open(p)
                .and_then(|h| h.set_times(fs::FileTimes::new().set_modified(old)))
                .expect("a writable state dir");
        };
        let verdict =
            |p: &Path| reapable(p, Named::of(p.file_name().expect("a named file")));

        // THE LIVE CASE. Our own pid, with its real start time: this is exactly
        // what a live session's record looks like, and no age can make it stale.
        let mine = Origin::mine_from(std::process::id()).expect("our own /proc entry");
        let live = write(
            "live",
            &Record { origin: Some(mine), ..Record::fresh() }.render(),
        );
        age(&live);
        assert!(mine.alive());
        assert_eq!(verdict(&live), None);

        // A pid that cannot be running: the kernel's own maximum plus nothing.
        // /proc/<pid> is absent, so the origin is gone.
        let dead = write(
            "dead",
            &Record {
                origin: Some(Origin { pid: u32::MAX, start: 1 }),
                ..Record::fresh()
            }
            .render(),
        );
        assert!(verdict(&dead).is_some());

        // THE RECYCLED PID. The pid is alive, but it is not the process that wrote
        // this record - which is precisely what the start time is in the file for.
        let recycled = write(
            "recycled",
            &Record {
                origin: Some(Origin { pid: mine.pid, start: mine.start + 1 }),
                ..Record::fresh()
            }
            .render(),
        );
        assert!(verdict(&recycled).is_some());

        // No origin at all - a record from before this field. Freshly written, so
        // the mtime rule keeps it: mtime cannot prove a session dead, so it must
        // not be the rule that reaps a young file. Aged, it goes.
        let anon = write("anon", "cts5\nb w\n");
        assert_eq!(verdict(&anon), None);
        let old_anon = write("oldanon", "cts5\nb w\n");
        age(&old_anon);
        assert!(verdict(&old_anon).is_some());
        // A `.tmp` from a crashed write, same rule - and a FRESH one must survive,
        // because unlinking it out from under a live write loses that write.
        let tmp = write("t.1.tmp", "half a rec");
        assert_eq!(verdict(&tmp), None);
        let old_tmp = write("t.2.tmp", "half a rec");
        age(&old_tmp);
        assert!(verdict(&old_tmp).is_some());

        // WHAT THE REAPER MAY NOT TOUCH, whatever its age: a user's own file in a
        // CCTAB_STATE_DIR they pointed at a shared directory, an unparseable file
        // with a record's NAME - the grammar cannot tell `id_rsa` from a session
        // id - and a directory, which `remove_file` cannot take and which doctor
        // must therefore not promise.
        let mut untouchable = Vec::new();
        for (name, body) in [
            ("notes.txt", "a shopping list"),
            ("id_rsa", "-----BEGIN OPENSSH PRIVATE KEY-----"),
            ("garbage", "not a record at all"),
        ] {
            let p = write(name, body);
            age(&p);
            untouchable.push(p);
        }
        let sub = f.dir.join("a-subdir");
        fs::create_dir(&sub).expect("a writable state dir");
        untouchable.push(sub);
        for p in &untouchable {
            assert_eq!(verdict(p), None, "{}", p.display());
        }
        // A NEWER version's record is on the mtime clock rather than untouchable:
        // the version that wrote it may be running, so a young one stays.
        let newer = write("newer", "cts9\nb w\n");
        assert_eq!(verdict(&newer), None);
        let old_newer = write("oldnewer", "cts9\nb w\n");
        age(&old_newer);
        assert!(verdict(&old_newer).is_some());

        // And the reaper acts on exactly those verdicts, skipping its own file.
        let s = f.session(&main_ev());
        s.reap();
        for kept in [&live, &anon, &tmp, &newer] {
            assert!(kept.exists(), "{}", kept.display());
        }
        for kept in &untouchable {
            assert!(kept.exists(), "{}", kept.display());
        }
        for gone in [&dead, &recycled, &old_anon, &old_tmp, &old_newer] {
            assert!(!gone.exists(), "{}", gone.display());
        }
    }

    /// `doctor`'s report is generated from the same verdicts the reaper acts on.
    #[test]
    fn doctor_reports_where_the_records_are_what_they_hold_and_which_are_stale() {
        let f = Fixture::new("survey");
        fs::create_dir_all(&f.dir).expect("a writable state dir");
        let mine = Origin::mine_from(std::process::id()).expect("our own /proc entry");
        fs::write(
            f.dir.join("live"),
            Record {
                base: Glyph::Working,
                background: None,
                origin: Some(mine),
                waits: vec![Wait {
                    who: Owner::Agent("aec99e1f".to_owned()),
                    raised: 999_990,
                }],
                completed: Vec::new(),
            }
            .render(),
        )
        .expect("a writable state dir");
        fs::write(
            f.dir.join("dead"),
            Record {
                origin: Some(Origin { pid: u32::MAX, start: 1 }),
                ..Record::fresh()
            }
            .render(),
        )
        .expect("a writable state dir");
        fs::write(f.dir.join("huge"), vec![b'x'; MAX_RECORD + 1]).expect("a writable state dir");
        fs::write(f.dir.join("corrupt"), b"\x00\x01\x02").expect("a writable state dir");
        fs::write(f.dir.join("newer"), b"cts9\nb w\n").expect("a writable state dir");
        fs::write(f.dir.join("notes.txt"), b"mine, not yours").expect("a writable state dir");

        let s = survey();
        assert_eq!(s.dir.clone().ok(), Some(f.dir.clone()));
        assert_eq!(s.writable, None);
        assert_eq!(s.stale, 1);
        let find = |n: &str| {
            s.records
                .iter()
                .find(|(name, _)| name == n)
                .map(|(_, what)| what.clone())
                .unwrap_or_default()
        };
        let live = find("live");
        assert!(live.contains("base working"), "{live}");
        assert!(live.contains(&format!("session pid {} live", mine.pid)), "{live}");
        assert!(live.contains("waiting on 1 (aec99e1f raised 10s ago)"), "{live}");
        assert!(!live.contains("STALE"), "{live}");
        let dead = find("dead");
        assert!(dead.contains("GONE") && dead.contains("STALE"), "{dead}");
        // The broken shapes are told apart rather than all reported as a healthy
        // idle record, which is what made this report the wrong place to look.
        assert!(find("huge").contains("unreadable"), "{}", find("huge"));
        assert!(
            find("corrupt").contains("not a record in any version's shape"),
            "{}",
            find("corrupt")
        );
        assert!(find("newer").contains("NEWER version"), "{}", find("newer"));
        assert!(
            find("notes.txt").contains("the reaper leaves it alone"),
            "{}",
            find("notes.txt")
        );

        // And with nowhere to keep a record the report says DISABLED and why,
        // which is the silent answer to almost every question about this layer.
        std::env::remove_var("CCTAB_STATE_DIR");
        std::env::remove_var("XDG_RUNTIME_DIR");
        assert_eq!(survey().dir, Err("no CCTAB_STATE_DIR and no XDG_RUNTIME_DIR"));
    }

    /// The one failure mode that looks healthy in every other line of the report: a
    /// readable directory nothing can be written to records no wait at all, so a
    /// subagent's dialog stays orange until the Task returns.
    #[test]
    fn doctor_reports_a_state_directory_that_cannot_be_written() {
        let f = Fixture::new("nowrite");
        fs::create_dir_all(&f.dir).expect("a writable state dir");
        let ro = |on: bool| {
            let mut m = fs::metadata(&f.dir).expect("our own directory").permissions();
            m.set_readonly(on);
            fs::set_permissions(&f.dir, m).expect("our own directory");
        };
        ro(true);
        let s = survey();
        // Restored first, so a failing assertion below cannot leave the fixture
        // undeletable.
        ro(false);
        assert!(s.dir.is_ok());
        assert!(
            s.writable.is_some_and(|w| w.contains("not writable")),
            "{:?}",
            s.writable
        );
        assert_eq!(survey().writable, None);
    }

    /// `uninstall` takes the records and nothing else, which is the same line the
    /// reaper draws: a `CCTAB_STATE_DIR` the user pointed at a shared directory must
    /// not be emptied by an uninstaller either.
    #[test]
    fn purge_takes_our_records_and_leaves_everything_else() {
        let f = Fixture::new("purge");
        fs::create_dir_all(&f.dir).expect("a writable state dir");
        for (name, body) in [
            ("s1", "cts5\nb w\n"),
            ("s2", "cts5\nb i\n"),
            ("s2.99.tmp", "half"),
            ("notes.txt", "mine"),
        ] {
            fs::write(f.dir.join(name), body).expect("a writable state dir");
        }
        let (where_, gone, dir_gone) = purge().expect("a state dir");
        assert_eq!((where_, gone, dir_gone), (f.dir.clone(), 3, false));
        let left: Vec<String> = fs::read_dir(&f.dir)
            .expect("still there")
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(left, vec!["notes.txt".to_owned()]);
        // With only our own files in it, the directory goes too.
        fs::remove_file(f.dir.join("notes.txt")).expect("our own file");
        fs::write(f.dir.join("s3"), "cts5\nb i\n").expect("a writable state dir");
        assert_eq!(purge(), Some((f.dir.clone(), 1, true)));
        assert!(!f.dir.exists());
        // And an absent directory is not a failure, just nothing to do.
        assert_eq!(purge(), None);
    }

    #[test]
    fn the_wait_set_is_bounded() {
        let mut r = Record::fresh();
        for i in 0..MAX_WAITS + 3 {
            r.raise(Owner::Agent(format!("a{i}")), 7);
        }
        assert_eq!(r.waits.len(), MAX_WAITS + 1);
        // Old waits survive; one overflow marker aggregates the extra requests.
        assert_eq!(
            r.waits.first().map(|w| w.who.clone()),
            Some(Owner::Agent("a0".to_owned()))
        );
        assert_eq!(r.waits.last().map(|w| &w.who), Some(&Owner::Overflow));
        // Re-raising refreshes that wait's own epoch and NO other, which is the
        // whole of the per-wait-epoch fix.
        let n = r.waits.len();
        r.raise(Owner::Agent("a0".to_owned()), 9);
        assert_eq!(r.waits.len(), n);
        assert_eq!(r.waits.first().map(|w| w.raised), Some(9));
        assert_eq!(r.waits.last().map(|w| w.raised), Some(7));
    }

    #[test]
    fn an_owner_may_not_impersonate_the_two_reserved_words() {
        assert_eq!(Owner::parse("-"), Some(Owner::Main));
        assert_eq!(Owner::parse("?"), Some(Owner::Unknown));
        assert_eq!(Owner::parse(""), None);
        assert_eq!(Owner::parse("a b"), None);
        // A colon in an owner would make the wait word ambiguous.
        assert_eq!(Owner::parse("a:b"), None);
        assert_eq!(Owner::parse(&"a".repeat(MAX_ID + 1)), None);
        assert_eq!(
            Owner::parse("aec99e1f4bda1972b"),
            Some(Owner::Agent("aec99e1f4bda1972b".to_owned()))
        );
        // And a wait word has to carry both halves.
        assert_eq!(Wait::parse("aaa"), None);
        assert_eq!(Wait::parse("aaa:"), None);
        assert_eq!(Wait::parse(":7"), None);
        assert_eq!(
            Wait::parse("-:7"),
            Some(Wait { who: Owner::Main, raised: 7 })
        );
    }

    #[test]
    fn itoa_matches_the_formatter() {
        for n in [0u64, 1, 9, 10, 99, 1_790_380_630, u64::MAX] {
            assert_eq!(itoa(n), n.to_string());
        }
    }

    #[test]
    fn digits_refuses_what_is_not_a_plain_number() {
        assert_eq!(digits("0"), Some(0));
        assert_eq!(digits("1790380630"), Some(1_790_380_630));
        assert_eq!(digits(""), None);
        assert_eq!(digits("-1"), None);
        assert_eq!(digits("1x"), None);
        assert_eq!(digits(&"9".repeat(21)), None);
        // Twenty nines overflow u64 and must not wrap.
        assert_eq!(digits(&"9".repeat(20)), None);
    }

    /// The two payload discriminators this module adds, including the direction
    /// they fail in: not found has to mean "I do not know", never "empty".
    #[test]
    fn the_payload_discriminators_fail_conservatively() {
        assert!(nothing_running(&payload(
            r#"{"session_id":"s1","background_tasks":[]}"#
        )));
        assert!(nothing_running(&payload(
            r#"{"session_id":"s1","background_tasks": []}"#
        )));
        assert!(nothing_running(&payload(
            r#"{"session_id":"s1","background_tasks":[{"type":"monitor","description":"claude.ai/artifact/x"}]}"#
        )));
        assert!(!nothing_running(&payload(
            r#"{"session_id":"s1","background_tasks":[{"id":"a","status":"running"}]}"#
        )));
        assert!(!nothing_running(&payload(r#"{"session_id":"s1"}"#)));
        assert!(at_the_prompt(&user_prompt()));
        assert!(!at_the_prompt(&main_tool()));
        assert!(!at_the_prompt(&payload(r#"{"session_id":"s1"}"#)));
        // The injected shapes, and the conservative answer when there is no prompt
        // at all or an empty one.
        assert!(!at_the_prompt(&task_notification()));
        assert!(!at_the_prompt(&payload(
            r#"{"session_id":"s1","hook_event_name":"UserPromptSubmit","prompt":"<system-reminder>x"}"#
        )));
        assert!(!at_the_prompt(&payload(
            r#"{"session_id":"s1","hook_event_name":"UserPromptSubmit"}"#
        )));
        assert!(!at_the_prompt(&payload(
            r#"{"session_id":"s1","hook_event_name":"UserPromptSubmit","prompt":""}"#
        )));
        // A late top-level `background_tasks` is found regardless of order, because
        // `Stop` serializes it after an unbounded `last_assistant_message`.
        let big = "x".repeat(8192 + 512);
        assert!(nothing_running(&payload(&format!(
            r#"{{"session_id":"s1","last_assistant_message":"{big}","background_tasks":[]}}"#
        ))));
    }
}
