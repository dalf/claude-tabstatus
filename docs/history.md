# History

How claude-tabstatus got to where it is, one slice at a time. Nothing here
describes current behaviour normatively: for that, see the
[README](../README.md), [the state and wait-ownership contract](state-contract.md),
[the indicator semantics](indicator-semantics.md) and
[the architecture notes](architecture.md). Work that is planned but not yet
built is listed in [AGENTS.md](../AGENTS.md).

## Slice 1: skeleton and Konsole

The plugin skeleton, four hook edges (`SessionStart`, `UserPromptSubmit`,
`Stop`, `SessionEnd`), and Konsole per-tab arming and restore.

## Slice 2: the real location

The real location - `repo@branch` from a fork-free `.git` walk, the
home-relative path outside a repo, the left-eliding length cap, and the ssh
host prefix.

## Slice 3: the waiting state

The `waiting` state and the recovery from it - six more hook edges
(`PreToolUse`, `PermissionRequest`, `PostToolUse`, `PostToolUseFailure`,
`Notification`, `StopFailure`), the notification-kind three-way, and the
`compact` guard on `SessionStart`. The states as they stand today are
described in [the indicator semantics](indicator-semantics.md).

## Slice 4: one Rust binary

One Rust binary in place of 875 lines of POSIX sh and two installer scripts,
and the five limitations the shell had caused. It was ported first and fixed
second: a golden corpus of 292 cases recorded what the shell did, byte for
byte, including its bugs, and the port had to reproduce all of it before
anything was allowed to change - after which ten of those cases were
re-recorded on purpose, each named for the limitation it closed. What the
language bought:

| | shell | binary |
|---|---|---|
| hot edge, small payload | 3.1ms | 0.61ms |
| `notify`, 1 MB payload | 20ms (bash), 165ms (dash) | 1.1ms |
| `working` or `notify`, 4 MB payload | linear, unbounded | 2.4ms |
| a subagent's `PostToolUse` | repaints over your dialog | paints nothing |
| `notify` with a long `message` | painted | painted (needed a tail window) |
| a non-UTF-8 name | invalid JSON, no title at all | U+FFFD, a title |
| the length cap's unit | bytes or characters, per shell and locale | characters |
| a location outside printable ASCII | never cut, overflows the tab | cut like any other |
| dependencies | `sh`, `jq` | none |

This table records the pre-Serde implementation, including its old dependency
count and window-based parser. It is historical, not a current parser
benchmark. The binary column is best-of-300 on the measuring machine with an
exec floor of 288us (`/bin/true` through the same harness), so the interesting
column is the difference, not the absolute. **Which machine and which shell
matters for the `shell` column and cannot be reproduced from this checkout:**
the only POSIX sh installed on the measuring machine was bash 5.3 as `/bin/sh`
(there was no `dash` and no `busybox`), so the bash figures were re-measurable
and the `dash` ones are historical - taken on the machine that had it during
slices 1-3, and quoted rather than re-run. The `binary` column and the whole
ratio were reproducible there with `sh tests/corpus/replay.sh` and a timing
harness kept in the slice notes, which are not part of this repository.

The test suite used to run the shell implementation under three shells in four
locales, because its answer depended on both; a binary has no interpreter, and
since the length cap became locale-independent it has no locale dependence
either, so that whole axis is gone.

## Slice 5: idiomatic Rust, same bytes

Nothing changed in behaviour. The port was transliterated from the shell
deliberately, so that it could be proved; this slice made it Rust without
letting it change its mind about anything. `src/sh.rs` - 608 lines
reimplementing shell string operators on bytes - is gone, argv and the
environment are parsed into types once at the boundary, the closed sets are
enums, absence is `Option`, and a failed write is an `io::Error` that only
`main` turns into exit 0. The contract was byte-identical output; what held it
was the 292-case corpus, the 279 assertions, ~50,000 paired invocations against
the pre-refactor binary, and 101 new in-crate unit tests. Two defects were
found and deliberately NOT fixed, because mixing a fix into this slice would
have cost the proof. Both are still open; they are listed under known
limitations in the [README](../README.md) and in [AGENTS.md](../AGENTS.md).

## Slice 6: tmux

Inside a tmux server the tab title becomes a function of time - one cell per
claude pane, decaying on tmux's own clock - so every one of the "paints with no
matching un-paint" defects heals itself there. The payload becomes a record,
`SessionStart` configures the server in one invocation and saves what it
replaced, `uninstall` puts it back, and the hot path execs nothing at all.
Exactly one golden-corpus case changed - the session-start-inside-tmux pty
case, whose payload is now the record - and nineteen were added; every case
where `$TMUX` is unset is byte-identical, and so is every dry run whatever
`$TMUX` says. The tmux setup as it stands today is in the
[README](../README.md).

## Slice 7: wait ownership

**Wait ownership**, and with it the state layer the slices after it need. The
stateless invariant was the right constraint for six slices and the wrong one
for this defect: the two cases the `agent_id` filter could not tell apart - a
background subagent's tool call, and the tool call that resolved the dialog you
just approved - differ only in what came before. So there is now one small
record per session, and a wait is an overlay on a base rather than a colour.

What it closed: approving a subagent's dialog left the tab orange until the
`Task` returned, and a main-thread tool call repainted blue over a subagent's
open dialog. The capture and the rules are now specified in
[the state and wait-ownership contract](state-contract.md); the per-edge cost
(+15us on the hot edge, a read; +22us on a transition; zero with the layer
disabled) and the design rationale are in
[the architecture notes](architecture.md).

Nothing in the golden corpus moved: all 312 cases are byte-identical, because
the corpus environment configures no state directory and the reference
implementation the corpus is frozen against is the shell one, which has no
record to consult. The new behaviour is pinned by assertions in `tests/run.sh`
and by unit tests instead.

### Second pass

A second pass then fixed what the first one got wrong, and every item is a
*lift* rather than a tweak:

- a wait now carries its own epoch (one shared one let unrelated dialogs keep a
  stale wait alive for the whole session, 16 consecutive edges painting
  nothing);
- an unattributable `?` is retired by an empty `background_tasks` at `Stop`
  instead of holding the tab orange through every idle period;
- a `UserPromptSubmit` you typed retires everything;
- each read-modify-write holds an `flock` on its own record (76 of 400 racing
  rounds lost a clear without it);
- the reaper deletes only what it can prove is its own;
- `doctor` reports whether the directory can be written at all - the one
  failure mode every other line rendered as healthy.
