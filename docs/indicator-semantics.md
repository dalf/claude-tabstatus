# Indicator meanings and background-work decision

Decision for [#10](https://github.com/dalf/claude-tabstatus/issues/10): background
activity is a standard state, enabled by default since
[#15](https://github.com/dalf/claude-tabstatus/issues/15). A workflow can run for a
long time while the main agent accepts more prompts. Availability for another
prompt must not make that workflow appear finished.

This policy is implemented by the registered stateful hooks. Background activity
is a separate persisted fact beneath the main state and outstanding waits.
Without a usable session ID and state directory, the legacy stateless fallback
cannot retain that fact across hooks.

## Meanings and precedence

| Indicator | Approved meaning |
|---|---|
| Orange: input required | A known unresolved request needs a response. Work may continue elsewhere in the session. |
| Blue: working | The main agent is actively working. |
| Purple: background | The main agent is idle and session-owned background work is known to remain in flight. |
| White: idle | No foreground or background work is known to remain active. This is not a success signal. |

Precedence is **input required > main working > background > idle**, using the
resolved state after the ownership and recovery rules in the
[state contract](state-contract.md). Raw events do not bypass live waits.
Terminal titles, tmux window labels and the tmux outer title must use those same
meanings. The default strip retains an indicator per pane. The optional rounded-cap theme
summarizes all panes in a window using the same priority, so an inactive pane
waiting for input still makes the cap orange. Its outer title retains the
individual pane indicators.

The intended long-workflow sequence is:

```text
launch: blue -> main stops, workflow continues: purple
ask for progress: blue -> main answers, workflow continues: purple
workflow needs approval: orange -> resolved, workflow continues: purple
last tracked work finishes, main idle: white
```

Completion means tracked activity ended, not that it succeeded. `StopFailure`
must not erase a surviving wait or background activity. No additional failure
color is selected here; the conversation remains the source of the result.
Normal completion and the quiet `idle_prompt` notification are not requests for
input.

## Background lifecycle and reconciliation

The authoritative snapshot is **main `Stop.background_tasks`**. Claude Code's
[hook reference](https://code.claude.com/docs/en/hooks) describes it as the
session's in-flight background registry. A nonempty array records known activity;
an explicit empty array clears it. Absent or null arrays preserve the previous
fact. Ordinary main progress, replies without metadata, idle notifications,
`StopFailure`, focus and elapsed time do not clear it.

This is one bounded aggregate, not a task inventory or a count. It includes
session-owned workflows, subagents and background shells, including pending work
already registered to an active workflow. Unknown task kinds also count because
they occupy the in-flight registry, except artifact watches, which never end
(see [state contract](state-contract.md)). No entry is evicted when many tasks are
present. `session_crons` and scheduled future occurrences do not count by
existence alone; their registered in-flight executions do. Unrelated OS processes
do not count. The shared 16 MiB payload limit still applies; rejected payloads
make no state change.

`SubagentStop` clears only its existing permission-wait ownership. It does not
clear background activity, even if its array is empty: a child can finish while
its enclosing workflow or another task continues. Main `Stop` supplies the next
complete snapshot. This is independent from the older permission-wait recovery
heuristic in the [state contract](state-contract.md).

Live interactive Claude Code **2.1.274** traces establish the clearing path:

| Captured task | Observed clearing evidence |
|---|---|
| Background shell | Automatic task-notification turn, then main `Stop` with `[]`, without another human prompt. |
| Background subagent | Its own `SubagentStop` still lists it; automatic task-notification turn then main `Stop` with `[]`. |
| Workflow completing normally | Child completion still lists the workflow; automatic turn then main `Stop` with `[]`, without another human prompt. |
| Workflow failing intentionally | Failure notification wakes the main agent; subsequent main `Stop` has `[]`. |
| Workflow cancelled with `TaskStop` | Main `Stop` has `[]` after cancellation, without a child `SubagentStop`. The UI briefly retained a cancelled child's cleanup entry; this tracks the registry, not OS-process termination. |
| Workflow spanning turns | Three main `Stop` snapshots remain nonempty through two additional human prompts, then the automatic completion turn reports `[]`. |

The [sanitized captures](../tests/fixtures/background-v1.json) retain lifecycle
metadata and authored expectations, with original prompts/tool text removed.
The multi-turn live probe used a bounded 20-second task; year-long duration is
covered by synthetic clocks, not claimed as a live observation. Ordinary hooks
in the traces omit background metadata. Missing/null **Stop** metadata and
`StopFailure` preservation are additionally tested synthetically.

No daemon polls tasks. A later main `Stop` reconciles the aggregate. If the
completion notification, main turn or hook never arrives, retain the last known
activity instead of inventing completion. `doctor` reports when background was
last reported and explicitly labels it as last-known, unexpired evidence.
Session end/reset can discard tracking without claiming success; compaction
preserves it. Startup reaping also preserves a background record with no process
origin, regardless of age; a known dead origin can still be cleaned up. State write failures and missing/unusable state storage retain the
existing best-effort limits. Interrupts that emit no hook cannot be detected
immediately.

## Long duration and input precedence

Waits keep their existing ownership, recovery and expiry rules. Orange overrides
blue and purple. Resolving the final wait restores blue if the main agent is
working, purple if only background is known, otherwise white. If a main snapshot
changes background knowledge beneath an outstanding wait, repaint orange to
update tmux's fallback too.

Purple has **no TTL**. tmux carriers with known background also cannot disappear
through `CCTAB_TTL_GONE`; blue/orange can age to purple using their existing TTLs.
Carriers without known background retain the existing white/disappearance decay.
A stale display or a recovered wait is not proof of success. Focus does not
resolve requests. Terminal delivery remains best effort; after a silent hook the
plain terminal retains its previous display.

## Attention and acknowledgement

Additional attention channels remain disabled by default and belong to
[#14](https://github.com/dalf/claude-tabstatus/issues/14). Their initial policy is
to alert on entering input-required state, coalescing repeated hooks while it
remains waiting. Completion alerts need a separate explicit option. Focusing a
tab can acknowledge an alert if the backend supports it; it never establishes
that an input request was answered. Alert delivery or failure does not mutate
the state record.

## Compatibility and delivery

- `CCTAB_GLYPH_BACKGROUND` defaults to 🟣 and follows the other glyph settings,
  including an empty value. Existing working/waiting/idle customization remains.
- The record writer emits `cts5`. Its optional `g <epoch>` records the last main
  Stop reporting active work, independently of `b` (main base) and `w` (waits).
  Readers accept `cts1`–`cts4` without inventing background history, even if an
  old record contains a formerly reserved `g` field. Migration occurs on a
  changed write. Guarded older binaries refuse ordinary updates of `cts5`, so
  they cannot silently drop this activity; explicit teardown and much older
  unguarded versions remain outside that guarantee. Update all installed copies.
- tmux consumers still read `ct1 w/a/i`. Background-aware paints use **ct2**:
  `p` means background, `W` working with background, `A` waiting with background.
  `SessionStart` installs compatible server formats before publishing its title.
  New readers support old panes alongside new ones; old readers ignore ct2
  rather than interpreting it as white. `doctor` warns when installed formats
  cannot consume background. Update all plugin copies and start a new Claude
  session to refresh an existing tmux server; old sessions can otherwise replace
  the shared formats. Refresh the optional [theme](../examples/tmux.conf) too.
- Session locks serialize the aggregate with waits and completion history.
  Atomic record replacement prevents partial reads. Concurrent independent
  snapshots have no source sequence number, so the last processed snapshot wins;
  terminal output after unlocking has the same existing ordering limitation.
  Different sessions remain isolated.

[Background tests](../tests/test_background.py) replay all six live scenarios and
exercise duration, missing metadata, lifecycle cleanup, unknown kinds, large
registries, concurrency, isolation, diagnostics and record migration. The
[tmux tests](../tests/test_tmux_status.py) check four-state parity with plain
terminal output, age-to-purple behavior, mixed carriers, custom glyphs, theme
labels and incompatible-consumer diagnostics. Existing wait-ownership scenarios
remain in the [state contract fixtures](../tests/fixtures/state-contract-v1.json).
