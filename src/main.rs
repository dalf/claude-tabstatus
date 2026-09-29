//! claude-tabstatus - put the Claude Code session state into the terminal tab
//! title, beside the location.
//!
//!     tabstatus <edge>
//!
//! Edges:  session-start | working | waiting | idle | notify | subagent-stop
//!         | session-end | elicitation | elicitation-result
//!
//! The first five paint; `notify` decides between idle, waiting and painting
//! nothing at all by looking at the notification kind, and session-end unpaints.
//! Which hook event maps to which edge is `hooks/hooks.json`'s business, not this
//! binary's.
//!
//! What it paints depends on the edge, structurally parsed top-level hook
//! metadata, and a small per-session wait-ownership record. Without a state
//! directory or session id, the edge uses its stateless decision. Invalid parsed
//! input is a silent no-op before any state record is opened.
//!
//! It always exits 0 on the paint path, because a non-zero hook is noise and a
//! non-zero PreToolUse hook blocks a tool. That is a property of `main`, not a
//! licence for every function below it to swallow its own failure: "nothing to
//! paint" is a value (`Ok(())`), a failed write is an `io::Error` that travels up
//! through [`paint`], and `main` is the single place either one becomes exit 0.
//!
//! Absence below that is `Option`, and in exactly two places absence and failure
//! are deliberately the same answer - `git::first_line` (no HEAD vs. an unreadable
//! one, so the walk climbs past it) and `emit::session_tty` (no hook subprocess
//! vs. an unresolvable fd 1). Both reproduce the reference implementation, and
//! telling them apart would change what paints, so it belongs to a slice allowed
//! to change behaviour. `panic = "abort"` is in the release profile, so nothing
//! here indexes or unwraps.
//!
//! `tests/corpus/` is the behaviour specification: 312 reproducible cases pinning
//! argv, environment, cwd and stdin to exact stdout bytes and an exit code, with
//! `tests/oracle/tabstatus.sh` retained so any of them can be re-derived rather
//! than trusted.

mod config;
mod edge;
mod embedded;
mod emit;
mod git;
mod json;
mod location;
mod manage;
mod payload;
mod render;
mod settings;
mod state;
mod text;
mod tmux;
mod tree;

use config::Config;
use edge::{Edge, Paint};
use payload::Payload;
use std::ffi::OsString;
use std::io::{self, IsTerminal};

fn main() {
    let mut args = std::env::args_os().skip(1);
    let first = args.next();
    // Free on the paint path: `ArgsOs` is an ExactSizeIterator, so collecting a
    // tail that is empty allocates nothing.
    let rest: Vec<OsString> = args.collect();

    // The management half shares this binary: merging it cost -9us on the hot edge,
    // because code that never runs is never paged in. It is dispatched FIRST and by
    // exact name, so it reads no stdin and no edge can reach it; anything else falls
    // through to the paint path, an unknown word included.
    if let Some(cmd) = manage::Subcommand::parse(first.as_deref(), &rest) {
        std::process::exit(cmd.run());
    }

    // THE boundary. Above this line a failure is an error with a cause; below it
    // there is no such thing, because a hook that reports one is a hook that
    // spoils a turn.
    let _ = paint(Edge::parse(first.as_deref()));
    std::process::exit(0);
}

/// Paint the tab, or deliberately do not. `Ok(())` covers both: an unrecognised
/// notification kind, a subagent's tool call, a mid-turn compaction and an absent
/// pty are all "nothing to paint", which is a decision rather than a failure - an
/// unpainted tab keeps the state it already showed. The `Err` is the write itself
/// failing, and only `main` ever sees it.
fn paint(edge: Edge) -> io::Result<()> {
    // Manual terminal invocations never wait for stdin. Metadata-independent
    // stateless edges only drain it; they do not validate JSON. All other paths
    // parse one complete object, bounded to 16 MiB, and reject bad input BEFORE
    // opening a session (including reaping, locking or changing its record).
    let state_dir = state::dir();
    let payload = if io::stdin().is_terminal() {
        Payload::empty()
    } else if edge.reads_payload() || state_dir.is_some() {
        match Payload::read() {
            Ok(payload) => payload,
            Err(_) => return Ok(()),
        }
    } else {
        payload::drain_stdin();
        Payload::empty()
    };

    // Resolved BEFORE the location walk, so an edge that paints nothing costs
    // nothing beyond the payload it had to read anyway. With a record, the same
    // decision is taken against it; without one, this is exactly the stateless
    // decision. Deliberate parsing changes have named golden-corpus updates.
    let resolved = match state::Session::open(state_dir, &payload) {
        Some(session) => session.resolve(edge, &payload),
        None => edge.resolve(&payload),
    };
    let Some(paint) = resolved else {
        return Ok(());
    };

    let mut cfg = Config::from_env();
    let composed = render::compose(paint, &cfg);

    // Dry run prints the TAB TITLE whatever else is true, so it stays the way to
    // see what a directory would paint - and so the golden corpus's dry-run cases
    // are unchanged by anything below.
    if cfg.dry_run {
        return emit::dry_run(&composed.title);
    }

    // Inside tmux an OSC 0 emitted in a pane never reaches the outer terminal: it
    // sets `pane_title`, and tmux re-emits a title of its OWN. So inside tmux the
    // same sequence becomes the record `tmux::title_format` reads back. Both
    // carrier composition and direct pane delivery execute no subprocess.
    let payload = match cfg.tmux {
        Some(_) => tmux::carrier(paint, &composed.place),
        None => composed.title,
    };
    match paint {
        Paint::SessionStart => {
            // Inside a LOCAL tmux the inherited KONSOLE_* says nothing, but the
            // process that owns the attached client's pty does. Asking costs one
            // round trip on an edge that already makes four, and nothing at all
            // on the edges that paint. Safe after compose(): `composed.place` is the
            // location alone, and the title compose() also built is thrown away
            // inside tmux in favour of the carrier above. Outside tmux this is a
            // no-op, so detection can only ever change what happens INSIDE tmux.
            let ev = tmux::adopt_konsole(&mut cfg);
            // Before the first title lands, for the same reason the Konsole
            // arming precedes it: a tab painted before it can show the paint.
            tmux::session_start(&cfg, &ev);
            tmux::arm_konsole(&cfg, &ev);
            emit::session_start(&payload, &cfg)
        }
        Paint::SessionEnd => {
            let r = emit::session_end(&cfg);
            tmux::session_end(&cfg);
            r
        }
        // Claude Code may wrap terminalSequence in tmux passthrough, which
        // bypasses pane_title. Our carrier must reach the pane's pty as raw OSC.
        Paint::Line(_) | Paint::LineWithBackground(_) if cfg.tmux.is_some() => emit::pane_title(&payload, &cfg),
        Paint::Line(_) | Paint::LineWithBackground(_) => emit::json_line(&payload),
    }
}
