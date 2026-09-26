//! claude-tabstatus - put the Claude Code session state into the terminal tab
//! title, beside the location.
//!
//!     tabstatus <edge>
//!
//! Edges:  session-start | working | waiting | idle | notify | session-end
//!
//! The first five paint; `notify` decides between idle, waiting and painting
//! nothing at all by looking at the notification kind, and session-end unpaints.
//! Which hook event maps to which edge is `hooks/hooks.json`'s business, not this
//! binary's.
//!
//! It is STATELESS: what it paints is a pure function of the edge argument plus,
//! for three edges, substring tests on two bounded windows of the raw payload.
//! Nothing is written, so nothing is stale after a SIGKILL.
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
//! `tests/corpus/` is the behaviour specification: 292 reproducible cases pinning
//! argv, environment, cwd and stdin to exact stdout bytes and an exit code, with
//! `tests/oracle/tabstatus.sh` retained so any of them can be re-derived rather
//! than trusted.

mod config;
mod edge;
mod emit;
mod git;
mod json;
mod location;
mod manage;
mod payload;
mod render;
mod settings;
mod text;

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
    // Three edges have to SEE the payload - notify, session-start and working -
    // and the rest only have to let stdin reach EOF, or Claude Code's writer sees
    // EPIPE on a pipe it is still filling.
    //
    // The isatty test is there so that running this by hand from a terminal does
    // not block on a stdin that never reaches EOF. A real hook always gets a pipe.
    // It leaves the payload empty, which every test below reads as "no
    // discriminator present", i.e. paint the edge as asked.
    let payload = if io::stdin().is_terminal() {
        Payload::empty()
    } else if edge.reads_payload() {
        Payload::read()
    } else {
        payload::drain_stdin();
        Payload::empty()
    };

    // Resolved BEFORE the location walk, so an edge that paints nothing costs
    // nothing beyond the payload it had to read anyway.
    let Some(paint) = edge.resolve(&payload) else {
        return Ok(());
    };

    let cfg = Config::from_env();
    let title = render::compose(paint, &cfg);

    if cfg.dry_run {
        return emit::dry_run(&title);
    }
    match paint {
        Paint::SessionStart => emit::session_start(&title, &cfg),
        Paint::SessionEnd => emit::session_end(&cfg),
        Paint::Line(_) => emit::json_line(&title),
    }
}
