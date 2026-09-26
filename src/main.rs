//! claude-tabstatus - put the Claude Code session state into the terminal tab
//! title, beside the location.
//!
//!     tabstatus <edge>
//!
//! Edges:  session-start | working | waiting | idle | notify | session-end
//!
//! The first five paint; `notify` decides between idle, waiting and painting
//! nothing at all by looking at the notification kind, and session-end unpaints.
//! Which hook event maps to which edge is hooks.json's business, not this
//! binary's. It is STATELESS: what it paints is a pure function of the edge
//! argument plus, for two edges, a handful of substring tests on the raw
//! payload. Nothing is written, so nothing is stale after a SIGKILL and there is
//! nothing to prune.
//!
//! This binary always exits 0. A non-zero PreToolUse hook can block a tool, and
//! a non-zero hook anywhere is noise. `panic = "abort"` is in the release
//! profile, so every fallible call here is handled rather than unwrapped.
//!
//! This is a port of the 875-line POSIX sh implementation removed in this same
//! change, proven byte for byte against it: the shell is preserved verbatim as
//! `tests/oracle/tabstatus.sh` and the 292-case golden corpus in `tests/corpus/`
//! pins the two together. Where the shell was wrong it is still wrong here, on
//! purpose: a port that is simultaneously a bugfix cannot be proven against its
//! oracle. The exceptions are enumerated - ten corpus cases carry a `fixed` field
//! naming the limitation they close, `tests/corpus/refreeze_fixed.py` is the table
//! that re-recorded them, and `cases.jsonl.before-fixes` is the pre-fix freeze.

mod emit;
mod json;
mod location;
mod manage;
mod render;
mod settings;
mod sh;

use std::io::IsTerminal;
use std::os::unix::ffi::OsStringExt;

fn main() {
    let mut argv = std::env::args_os().skip(1).map(|a| a.into_vec());
    let first = argv.next().unwrap_or_default();

    // ------------------------------------------------------------------
    // The management half - install, uninstall, doctor, version, help - is in
    // this same binary, which was measured rather than assumed: merging it cost
    // -9us on the hot edge, because code that never runs is never paged in. It
    // is dispatched FIRST and by exact name, so it reads no stdin, and no edge
    // and no typo of one can reach it.
    //
    // Those names are deliberately disjoint from the edge names. Anything else
    // still falls through to the paint path, including an unknown word: an edge
    // this version does not know about paints the idle form rather than nothing,
    // which is the forward-compatibility choice hooks.json's table and
    // tests/run.sh both pin.
    // ------------------------------------------------------------------
    if manage::is_subcommand(&first) {
        let rest: Vec<Vec<u8>> = argv.collect();
        std::process::exit(manage::dispatch(&first, &rest));
    }

    run(first);
    // Nothing in run() is allowed to fail; exit 0 always.
    std::process::exit(0);
}

fn run(edge: Vec<u8>) {
    // ------------------------------------------------------------------
    // 0. Take the hook payload off stdin, BOUNDED.
    //
    // Three edges have to SEE the payload - notify, session-start and now
    // working - and all three search only two BOUNDED windows of the first line:
    // its first `sh::PAYLOAD_PREFIX` bytes and, for notify alone, its last
    // `sh::PAYLOAD_TAIL` bytes. Nothing between them is ever searched, so a 1 MB
    // tool_response costs the same as an empty payload. The tail exists because a
    // Notification serializes `notification_type` LAST, after the unbounded
    // `message`; the other two discriminators are serialized before anything
    // unbounded and are read from the prefix only. That bound is what makes
    // the hot edge affordable: in the shell, reading the line and running `case`
    // globs over it cost 165ms under dash and 20ms under bash-as-sh on a 1 MB
    // payload, measured on this machine, against a 5s hook timeout - which is why
    // the shell's hot edge could not afford to look at its payload at all, and
    // why a background subagent's tool calls repainted over an open permission
    // dialog.
    //
    // session-end and idle have nothing to read, so they only drain.
    //
    // The isatty test is there so that running this by hand from a terminal
    // does not block on a stdin that never reaches EOF. A real hook always gets
    // a pipe. It leaves the payload empty, which every test in 0b treats as
    // "no discriminator present", i.e. paint the edge as asked.
    // ------------------------------------------------------------------
    let mut payload = sh::Payload::default();
    if !std::io::stdin().is_terminal() {
        match &edge[..] {
            b"notify" | b"session-start" | b"working" => payload = sh::payload_prefix(),
            _ => sh::drain_stdin(),
        }
    }

    // ------------------------------------------------------------------
    // 0b. The two edges whose state the payload decides.
    //
    // Both are substring tests on the raw FIRST LINE, never jq: a fork per
    // notification, to parse 845 bytes that can be matched literally, plus a
    // dependency the rest of the plugin does not have. They carry the compact
    // spelling Claude Code actually writes - JSON.stringify output, no space
    // after `:` - and the session-start one also tolerates one space after the
    // colon. A pretty-printed payload therefore matches nothing, and the two
    // edges then fail in OPPOSITE directions: notify falls through to silence,
    // which is safe because an unpainted tab keeps the state it already showed,
    // while session-start falls through to painting AND arming. hooks.json's
    // `"matcher": "startup|resume|clear|fork"` is the load-bearing guard
    // against that; this test is only a belt to it.
    //
    // NOTIFY is a three-way and the third branch is silence:
    //
    //   * idle_prompt is the quiet-turn nudge, fired ~60s after a turn ends. It
    //     is the one kind that MUST NOT paint waiting - it arrives after EVERY
    //     quiet turn end, so mapping it to waiting would turn every idle tab
    //     orange a minute later and collapse two of the three states into one.
    //     Matched first for that reason.
    //   * The waiting kinds are a BACKSTOP, not the fast path:
    //     permission_prompt is scheduled 6s after the dialog goes up, fires at
    //     most once per dialog, and is suppressed outright by
    //     CLAUDE_CODE_DISABLE_PERMISSION_PROMPT_NOTIFY_HOOKS. PermissionRequest,
    //     23ms after PreToolUse, is the real-time signal. permission_prompt is
    //     kept anyway because it is the ONLY signal for the dialogs that are not
    //     tool calls, and the other four kinds are what PermissionRequest does
    //     not cover either.
    //   * Everything else is deliberately silent - agent_completed, the
    //     elicitation RESPONSE pair, computer_use_exit, push_notification,
    //     auth_success, and whatever kind a later Claude Code invents. None of
    //     them is a state change.
    // ------------------------------------------------------------------
    let mut edge = edge;
    if &edge[..] == b"notify" {
        const WAITING: [&[u8]; 5] = [
            b"\"notification_type\":\"permission_prompt\"",
            b"\"notification_type\":\"worker_permission_prompt\"",
            b"\"notification_type\":\"agent_needs_input\"",
            b"\"notification_type\":\"elicitation_dialog\"",
            b"\"notification_type\":\"elicitation_url_dialog\"",
        ];
        if payload.has(b"\"notification_type\":\"idle_prompt\"") {
            edge = b"idle".to_vec();
        } else if WAITING.iter().any(|k| payload.has(k)) {
            edge = b"waiting".to_vec();
        } else {
            // Not a state change. Emit nothing at all - not an empty title,
            // which would blank the tab - and do not spend the location walk.
            return;
        }
    } else if &edge[..] == b"working" {
        // THE defect this port exists to fix. PostToolUse is registered
        // unmatched, so a SUBAGENT's tool calls fire it in the main session:
        // while you are looking at a main-thread permission dialog, a background
        // subagent doing N tool calls used to paint `working` over the orange N
        // times, and the one-shot `permission_prompt` notification restored it
        // once, 6s in, and never again. A tab then reads "busy, do not bother"
        // for as long as that subagent runs, while Claude is in fact blocked on
        // you - the damaging direction.
        //
        // `agent_id` is documented as "present only when the hook fires inside a
        // subagent call", so its PRESENCE is the discriminator, and it is checked
        // as a non-empty string rather than as a substring so that an empty value
        // could never silence the edge for the whole session.
        //
        // Only `working` is filtered. A subagent's PermissionRequest keeps
        // painting `waiting` on purpose: an asynchronous Task returns in ~6ms, so
        // the main session's Stop has usually already fired, and you are the one
        // blocking on that dialog whoever asked for it.
        if sh::has_string_field(&payload.head, b"agent_id") {
            return;
        }
    } else if &edge[..] == b"session-start" {
        // An auto-compaction re-fires SessionStart MID-TURN, which would repaint
        // the idle dot while Claude is still working AND arm the tab a second
        // time with no matching unarm.
        if payload.head_has(b"\"source\":\"compact\"")
            || payload.head_has(b"\"source\": \"compact\"")
        {
            return;
        }
    }

    let cwd = location::cwd();
    let (place, in_repo) = location::place(&cwd);
    let place = render::apply_length_cap(place, in_repo);
    let place = render::apply_ssh_prefix(place);
    let place = render::sanitize(place);
    let konsole = render::is_konsole();
    let title = render::title(&edge, place, konsole);

    if &sh::env_str("CCTAB_DRY_RUN")[..] == b"1" {
        emit::dry_run(&title);
        return;
    }

    match &edge[..] {
        b"session-start" => emit::session_edge(true, &title, konsole),
        b"session-end" => emit::session_edge(false, &title, konsole),
        _ => emit::json_line(&title),
    }
}
