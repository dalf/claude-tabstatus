//! claude-tabstatus - put the Claude Code session state into the terminal tab
//! title, beside the location.
//!
//!     tabstatus <edge>
//!
//! Edges:  session-start | working | waiting | idle | notify | subagent-stop
//!         | session-end
//!
//! The first five paint; `notify` decides between idle, waiting and painting
//! nothing at all by looking at the notification kind, and session-end unpaints.
//! Which hook event maps to which edge is `hooks/hooks.json`'s business, not this
//! binary's.
//!
//! What it paints is a function of the edge argument, substring tests on two
//! bounded windows of the raw payload, and - for the four edges that can be part
//! of a WAIT - one small record per session under `$XDG_RUNTIME_DIR`. That record
//! is the whole of the state, and `src/state.rs` carries why a stateless answer
//! could not express wait ownership. With nowhere to keep it, or with no
//! `session_id` in the payload, every edge falls back to the stateless answer, so
//! a SIGKILL leaves at most one stale file that the next `SessionStart` reaps.
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
mod standalone;
mod state;
mod text;
mod tmux;

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
    // The state layer needs `session_id`, which is the payload's FIRST member, so
    // an edge with no discriminator of its own still has to read a window once
    // there is somewhere to keep a record. Resolved BEFORE stdin, so the no-state
    // path still only drains, and threaded down rather than asked for twice.
    //
    // FRONT window in that case, with one exception, and that is not a
    // micro-optimisation. `session_id` and `agent_id` are both front members by
    // construction, so `waiting` and `session-end` need nothing else. Building a
    // tail means scanning every byte of the payload for the end of the line, and a
    // `PermissionRequest` for a `Write` carries the whole file in `tool_input`:
    // MEASURED on a 1 MiB payload, the full window costs 885us against a drain's
    // 330us, and the front alone costs 337us. The exception is `idle`, which reads
    // `background_tasks` - a Stop's LAST member - to decide whether a wait it does
    // not own can be retired; `Edge::reads_state_tail` is that one edge, and it
    // fires once per turn rather than once per tool call.
    let state_dir = state::dir();
    let payload = if io::stdin().is_terminal() {
        Payload::empty()
    } else if edge.reads_payload() {
        Payload::read()
    } else if state_dir.is_some() {
        // The one exception to the front window is `idle`: `state::free` reads
        // `background_tasks`, which a Stop serializes last, so that edge needs a
        // tail as well - once, per turn, and only when there is a record for the
        // answer to change.
        if edge.reads_state_tail() {
            Payload::read()
        } else {
            Payload::read_head()
        }
    } else {
        payload::drain_stdin();
        Payload::empty()
    };

    // Resolved BEFORE the location walk, so an edge that paints nothing costs
    // nothing beyond the payload it had to read anyway. With a record, the same
    // decision is taken against it; without one, this is exactly the stateless
    // program - which is what keeps the golden corpus byte-identical.
    let resolved = match state::Session::open(state_dir, &payload) {
        Some(session) => session.resolve(edge, &payload),
        None => edge.resolve(&payload),
    };
    let Some(paint) = resolved else {
        return Ok(());
    };

    let cfg = Config::from_env();
    let composed = render::compose(paint, &cfg);

    // Dry run prints the TAB TITLE whatever else is true, so it stays the way to
    // see what a directory would paint - and so the golden corpus's dry-run cases
    // are unchanged by anything below.
    if cfg.dry_run {
        return emit::dry_run(&composed.title);
    }

    // Inside tmux an OSC 0 emitted in a pane never reaches the outer terminal: it
    // sets `pane_title`, and tmux re-emits a title of its OWN. So inside tmux the
    // same sequence stops being a tab title and becomes the record
    // `tmux::title_format` reads back - the ONE branch tmux puts on the hot path,
    // and it execs nothing.
    let payload = match cfg.tmux {
        Some(_) => tmux::carrier(paint, &composed.place),
        None => composed.title,
    };
    match paint {
        Paint::SessionStart => {
            // Before the first title lands, for the same reason the Konsole
            // arming precedes it: a tab painted before it can show the paint.
            tmux::session_start(&cfg);
            tmux::arm_konsole(&cfg);
            emit::session_start(&payload, &cfg)
        }
        Paint::SessionEnd => {
            let r = emit::session_end(&cfg);
            tmux::session_end(&cfg);
            r
        }
        Paint::Line(_) => emit::json_line(&payload),
    }
}
