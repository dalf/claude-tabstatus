# Indicator meanings and background-work decision

Decision for [#10](https://github.com/dalf/claude-tabstatus/issues/10): background
activity is a standard state, enabled by default when implemented in
[#15](https://github.com/dalf/claude-tabstatus/issues/15). A workflow can run for a
long time while the main agent accepts more prompts. Availability for another
prompt must not make that workflow appear finished.

This is the approved product policy. The current release still renders three
states and does not track background tasks. Its white indicator means the main
conversation is idle, even when children remain active. That known gap belongs
to #15; this document does not claim it is implemented.

## Meanings and precedence

| Indicator | Approved meaning |
|---|---|
| Orange: input required | A known unresolved request needs a response. Work may continue elsewhere in the session. |
| Blue: working | The main agent is actively working. |
| Purple: background | The main agent is idle and session-owned background work is known to remain in flight. Planned in #15. |
| White: idle | No foreground or background work is known to remain active. This is the target meaning after #15, not a success signal. |

Precedence is **input required > main working > background > idle**, using the
resolved state after the ownership and recovery rules in the
[state contract](state-contract.md). Raw events do not bypass live waits.
Terminal titles, tmux window labels and the tmux outer title must use those same
meanings. A window containing several Claude panes retains an indicator per pane;
the priority is within a session, not permission to hide another session's wait.

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

## Scenario decisions and current limits

| Scenario | Current three-state behavior | Approved behavior / interpretation |
|---|---|---|
| Main stops with running children and no wait | White. Ordinary child progress is silent. | Purple in #15; main availability alone is insufficient for white. |
| Main stops with an outstanding child wait | Idle base; orange remains. | Orange remains. Resolving the last wait restores purple if work continues, otherwise the appropriate base. |
| Main turn fails | Same idle edge as normal Stop, subject to waits. | Failure is an outcome, not proof that all session work ended. Preserve waits/background work. |
| Quiet idle notification | Idle base, subject to waits. | No alert or orange merely because a turn ended; missing task metadata cannot erase known background work. |
| Child completes | Clears only its own wait, if any; does not track remaining children. | #15 updates the matching activity or a validated complete snapshot. Other work survives; the last completion removes purple without requiring another prompt. |
| Interrupt produces no hook | Display persists until a recovery event or applicable expiry. | Do not promise immediate cancellation detection. Silence supplies no completion evidence. |
| A TTL expires | State waits expire on eligible hooks; tmux independently ages its carrier. | Recovery is not proof of an answer or successful completion. The future background state must not decay to white solely because a workflow is long. |
| User focuses a waiting tab | Focus does not resolve state. | An alert may be acknowledged separately; orange remains while the wait remains. |
| Background metadata is absent/null | No background tracking; quiet-Stop wait recovery is not triggered. | Unknown information, not an empty activity set. Preserve previously established activity until retirement evidence or explicit lifecycle cleanup. |

## Activity scope and lifecycle requirements for #15

Count in-flight work registered to this Claude session: workflows, subagents,
and background shell tasks. Running and pending work already belonging to an
active workflow count. Scheduled future occurrences (`session_crons`, schedules
or an inactive loop) do not count merely because they exist. A schedule's actual
in-flight execution does count. Unrelated operating-system processes do not.

Task kinds share the same user-facing meaning but can require different evidence.
Do not infer that a shell exit emits a subagent completion hook, or equate one
child finishing with the enclosing workflow finishing. Unknown task kinds in a
validated in-flight snapshot must not silently become evidence of an idle session.

Use validated session-scoped identities and authoritative in-flight snapshots or
matching lifecycle events. An empty array can clear the tracked set only where
the event's snapshot completeness is established. An unrelated event, missing
array, main reply, focus change or elapsed duration cannot clear it. This does
not change the current permission-wait recovery heuristic.

#15 must capture normal completion without another prompt, cancellation, failure,
missing metadata, work spanning many turns, and shell versus subagent behavior.
Those captures must establish both creation and removal of activity. Existing
synthetic tests and the historical permission capture do not establish that
background lifecycle.

Do not reuse the working-state timeout to turn purple white. When evidence grows
stale, retain the last known activity; report uncertainty through diagnostics.
Session end/reset may discard the session's tracking without claiming the work
succeeded. A missing reliable clearing signal remains an implementation blocker
for #15: it needs a validated reconciliation mechanism before release. An expiry
that merely calls the session idle would reintroduce the reported problem.

## Attention and acknowledgement

Additional attention channels remain disabled by default and belong to
[#14](https://github.com/dalf/claude-tabstatus/issues/14). Their initial policy is
to alert on entering input-required state, coalescing repeated hooks while it
remains waiting. Completion alerts need a separate explicit option. Focusing a
tab can acknowledge an alert if the backend supports it; it never establishes
that an input request was answered. Alert delivery or failure does not mutate
the state record.

## Compatibility and delivery

#10 changes documentation and verification, not the emitted states or existing
glyphs. #15 is the separately approved behavior change to four default states.
It must ship state resolution, rendering, configuration and migration together:

- Add a configurable background glyph/color with purple as the default, following
  existing configuration conventions. Custom working/waiting/idle settings stay
  intact. Theme and example configuration must include the fourth state.
- Version the tmux carrier when extending today's `ct1` grammar (`w`, `a`, `i`).
  New consumers must still read `ct1`; install compatible formats before emitting
  the new carrier and test upgrade/restoration of existing servers. Mixed-version
  or unsupported consumers must be diagnosed, not silently translate active
  background work into white.
- Version persisted state when adding task ownership and a background base.
  Read existing `cts1`–`cts4` records without inventing background history. Older
  readers must not silently discard new activity on an ordinary update. Retain
  the documented session-cleanup limits and bounded-record requirements.
- Define conservative overflow/unknown-task handling, concurrency and per-session
  isolation. A full record must not evict live activity and falsely render idle.
- Verify the full sequence above in plain-terminal output, tmux pane/window and
  outer-title rendering, including long duration and the final completion event.

## Existing validation

These checks establish the current behavior and its documented gap, not an
implemented purple state:

| Requirement | Existing regression coverage |
|---|---|
| Main Stop with running children; child completion | `capture_s4_reconstruction`, `overlap_reverse_completions` in [semantic fixtures](../tests/fixtures/state-contract-v1.json) |
| Live waits, StopFailure, absent/null/nonempty background metadata | `stop_background_distinctions` |
| Interruption, lazy expiry, display uncertainty | `interruption_without_a_hook`, `per_owner_expiry_and_stale_display`, `clock_rollback_and_ttl_zero` |
| Plain-terminal output and real tmux wait precedence, main/child completion, failure and focus | `test_current_state_semantics_agree_with_plain_terminal` in [tmux integration tests](../tests/test_tmux_status.py) |
| Actual tmux carrier decay | `test_native_window_format_decays_without_hook_activity` |

The semantic fixtures run independently of the historical golden oracle. Future
background scenarios must extend them once #15 has the required lifecycle
evidence; changing the existing white expectations alone does not implement it.
