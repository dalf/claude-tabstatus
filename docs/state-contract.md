# State and wait-ownership contract, version 2

This specifies the four-state model used by the registered hooks. It
consolidates issues [#5](https://github.com/dalf/claude-tabstatus/issues/5),
[#6](https://github.com/dalf/claude-tabstatus/issues/6),
[#7](https://github.com/dalf/claude-tabstatus/issues/7),
[#8](https://github.com/dalf/claude-tabstatus/issues/8), and the direct elicitation
lifecycle added by [#9](https://github.com/dalf/claude-tabstatus/issues/9).
Background lifecycle and compatibility added by [#15](https://github.com/dalf/claude-tabstatus/issues/15)
follow the [indicator policy](indicator-semantics.md).
## Meaning of state and output

Each session has a **base** (`working` or `idle`), a bounded set of outstanding
waits, a background snapshot epoch, and a bounded history of completed elicitation identities. The initial
base is idle. A wait overlays the base without replacing it. The **logical
state** is waiting while any recorded wait remains; otherwise working if the
main base is working, background if its snapshot epoch is present, else idle.
The reader also accepts a legacy `waiting` base; ordinary transitions never
create that base.

A hook separately decides whether to **paint**. Silence means no terminal
update, not an idle paint or an empty title. After a silent hook, a plain terminal
keeps its previous display even if the hook persisted an expiry or changed the
base. For example, an unrelated agent completion can expire the last wait and
still emit nothing. A later painting hook reconciles the display. Session end
clears the title instead of painting a glyph.

The trace fixtures distinguish `logical`, `paint`, and `displayed`. `displayed`
is the last requested display, assuming emitted output was delivered; a dry-run
trace is not evidence of actual Claude Code hook delivery. tmux has an additional,
independent display clock: it decays the last pane carrier without running hooks
or modifying the session record. Its carrier epoch is the latest paint, whereas
each persisted wait has its own epoch. Using the same TTL does **not** synchronize
those clocks. Repeated waiting paints can refresh the carrier without refreshing
the underlying wait.

## Envelope interpretation

Classification and ownership use only decoded top-level metadata. Nested
application data and escaped copies inside values cannot impersonate fields.
Legal JSON whitespace, member order, escaped field names/values, and position
within the accepted document do not change meaning.

Recognized fields are `session_id`, `agent_id`, `source`, `notification_type`,
`hook_event_name`, `prompt`, `background_tasks`, `mcp_server_name`,
`elicitation_id`, `mode`, and `action`. Each accepts a string or null, except
`background_tasks`, which accepts an array or null. Duplicate recognized fields,
including equivalent escaped spellings, reject the entire input. Missing and
null fields are absent; empty session and agent IDs are also absent. Only the
first decoded prompt character and the background array's emptiness are retained
from those fields. Unknown fields are skipped.

The parser accepts one complete UTF-8 JSON object, at most 16 MiB including
whitespace. It buffers that complete input and drains oversized input to EOF.
This bounds the input buffer, not total allocations or processing time. Unused
values use Serde `IgnoredAny`: skipped strings can contain an unpaired surrogate
escape, and skipped container depth is not bounded by the ordinary recursive
deserialization limit. Retained strings must decode; invalid raw UTF-8 anywhere
rejects the document. These are explicit parser limits, not full validation of
application data.

Read errors, invalid input, wrong types, oversized input, and trailing documents
produce no paint and no state access, and exit successfully. Zero-byte stdin
retains manual invocation behavior; whitespace alone is invalid. With no state
directory configured, metadata-independent edges (`waiting`, `idle`,
`session-end`, and unknown command names) only drain stdin and do not validate
JSON. With a configured directory every edge parses. Interactive stdin is not
read. The edge argument selects the operation; the hook manifest supplies that
argument. Direct elicitation edges additionally verify `hook_event_name`.
Unknown manually supplied command names retain the compatibility idle paint;
they are outside the registered-hook ownership invariant and do not update state.

## Wait identities

| Identity | Meaning and bound |
|---|---|
| `main` | A permission/blocking-tool wait with absent agent ID. |
| `agent:<id>` | A permission wait owned by a usable agent ID. |
| `unknown-permission` | An unattributed permission notification backstop. |
| `anonymous-permission` | Permission requests with an unusable supplied agent ID, represented together and independent of the notification backstop. |
| `notification-mcp` | An elicitation notification without a usable server/request pair. |
| `anonymous-direct-mcp` | All direct requests lacking a usable server/request pair, represented together. |
| `direct-mcp(server,id)` | Exact server/request pair, scoped to this session. |
| `overflow` | Requests that could not fit within the eight individual wait slots. |

Session and agent IDs use 1–64 ASCII letters, digits, underscores or hyphens.
The single hyphen is reserved as a main-owner wire token and cannot identify an
agent. A supplied nonempty unusable agent ID must not gain main-thread recovery
powers: working/completion edges with it cannot clear waits. An unusable supplied
permission owner is recorded as anonymous permission; absent/null/empty remains
main. Named requests and completions cannot identify or replace that anonymous
aggregate.

Each direct identity component is a nonempty string of at most 64 UTF-8 bytes,
without control characters. Invalid string identities become unavailable, never
truncated or prefix-matched. Wrong JSON types reject the input. The record stores
hex-encoded complete identity components, not messages, schemas, answers, URLs,
credentials, or tool data.

## Transitions and retirement

Rules below operate after expiry on an edge that successfully locks and loads
state. Unless stated otherwise, other waits remain. Painting the base is allowed
only when no wait remains.

| Hook/edge | State change | Paint |
|---|---|---|
| Non-compaction session start | Reset base to idle; clear background, waits and completion history. | Idle startup. |
| Compaction session start | None, including no expiry/reaping. | None. |
| Permission request / registered blocking-tool wait | Raise its owner, or anonymous permission if its supplied owner is unusable; leave base unchanged. | Waiting. |
| Main working edge | Base becomes working; retire main and unknown permission/MCP-notification waits, subject to prompt rule below. | Base if clear; otherwise none. |
| Usable agent working edge | Retire only that agent's permission wait; leave base unchanged. | Base only if it cleared the last live wait. |
| `SubagentStop` | Retire only the matching agent permission wait. | Base only if it cleared the last live wait. |
| Idle (`Stop`, `StopFailure`, idle notification) | Base becomes idle; retire main wait. If `background_tasks` is exactly `[]`, also retire agent, anonymous permission, and unknown notification/permission waits. | Purple or white if clear; otherwise orange only if background presence changed. |
| Supported direct request | Raise exact identity, or anonymous direct aggregate, unless completion history suppresses it. | Waiting unless suppressed. |
| Supported identified direct result | Retire only its exact identity and remember completion, even if no request was recorded. | Base only if it cleared the last live wait. |
| Other notification | None, including no expiry. | None. |
| Session end | Remove the session record; see lifecycle limitation below. | Clear title. |

An empty background array is a **recovery heuristic**, not an MCP response and
not proof that an arbitrary notification was answered. Missing, null and
nonempty arrays do not trigger this recovery. They still permit retirement of
the main wait. Only main `Stop` uses the array as a complete activity snapshot: nonempty sets
the background epoch, empty removes it, and absent/null preserves it. Task entries
are not retained individually. Other idle edges preserve background knowledge.
Where the table says paint the base, an idle base with known background paints
purple. See the [lifecycle evidence](indicator-semantics.md#background-lifecycle-and-reconciliation).

A main `UserPromptSubmit` is treated as a human prompt only when `prompt` is
nonempty and its **first character** is not `<`. That heuristic clears every
wait, records completion for the identified direct waits it clears, and sets the
base to working. Missing/empty prompts and prompts starting with `<` do not clear
waits; they still set the base to working. No whitespace trimming or authentication
of human origin is implied. This excludes the observed injected task notification
and conservatively excludes human prompts starting with `<` too.

Direct requests/results support absent mode, `form`, and `url`. Results require
`accept`, `decline`, or `cancel`, plus both identity components. Unsupported modes,
other actions, wrong event names, and unidentified results are silent and do not
process expiry. Direct waits and overflow survive main tool activity, quiet Stop,
and unrelated agent completion. Human-prompt recovery, expiry, and session reset
or end can remove them.

There is no independent interruption transition. The historical observations of
Esc, declined dialogs and Ctrl+C include cases with no hook. Silence cannot clear
state: owner completion, a subsequent human prompt, quiet-Stop recovery where
applicable, expiry, or session cleanup must resolve it.

## Duplicates, overlap, ordering and capacity

Permission waits coalesce by owner. Reasserting an owned permission wait refreshes
only that owner's epoch. A generic permission notification adds unknown permission
only when no permission wait exists; otherwise it neither adds a wait nor refreshes
its clock, but still paints waiting. A later owned permission request replaces an
unknown wait when that is the only **permission** owner. Independent elicitation
and anonymous permission waits do not participate in this deduplication or prevent
that replacement. Here, the deduplication set consists only of main, named-agent,
and unknown-notification permission owners. This is notification deduplication
based on the observed backstop pattern, not exact request correlation.
Permission completions have no tombstones: a late permission request after its
owner's completion is treated as a new wait and needs a subsequent retirement.

Anonymous permission requests retain their aggregate's first-observed epoch;
repeated requests paint waiting without refreshing that clock. They survive named
permission requests/completions and main tool progress. Human-prompt recovery,
quiet Stop with `[]`, expiry, and session reset/end can retire them. A permission
notification cannot absorb this independently recorded request.

MCP notification kinds are `elicitation_dialog` and `elicitation_url_dialog`.
With a complete identity they share the direct-request key. Without it they are
independent of both direct requests and permission waits, and reassertion refreshes
their own notification epoch. A delayed unidentified notification can therefore
raise waiting after a direct result. The implementation must not guess that it
belongs to a known request. Legacy `cts1` unknown waits have lost that provenance;
they remain unknown permission waits and participate in its recovery/deduplication.

Repeated exact direct starts, identified notifications, and anonymous direct starts
retain their first outstanding epoch. Completion tombstones suppress starts with
the same exact key, including result-before-start delivery. Repeated results do
not refresh the tombstone. An ID reused during retention is a duplicate, not a
new request. Expired or evicted tombstones no longer suppress it. There is no
equivalent exact deduplication guarantee for unidentified notifications.

Eight wait identities are retained across all kinds. Additional distinct waits
create one overflow aggregate instead of evicting an existing live wait. Further
overflow does not refresh its epoch. Individual completions cannot clear overflow;
even after slots become available it remains until its own recovery. Anonymous
direct requests likewise share one aggregate rather than an unbounded count.
Completion history independently retains at most eight keys and evicts the oldest
inserted key on overflow. These are bounded conservative summaries, not a complete
history of all dialogs.

## Time and persistence

Each wait and tombstone has a hook-sampled Unix-second epoch. On a state-loading
transition, it expires when `max(now - epoch, 0)` is **greater than**
`CCTAB_TTL_WAITING`; equality remains live. The default is 900 seconds. The
shared TTL grammar accepts one to six decimal digits without a leading zero;
`0` selects the never-expire sentinel, and other values select the default.
Future epochs have zero age until the clock catches up. Unrelated waits cannot
refresh one another. A successfully persisted expiry cannot be resurrected by
raising the TTL later.

Time passing alone does not rewrite the record or paint. Some silent operations
load state and persist expiry; rejected payloads, unrelated notification kinds,
unsupported direct events, and other early returns do not. tmux's independent
display decay is described above.

Persistence requires a valid session ID and either the dedicated
`CCTAB_STATE_DIR` override or `$XDG_RUNTIME_DIR/claude-tabstatus`; there is no HOME
fallback. Missing/invalid session IDs, no configured directory, or failure to
create the directory select the stateless resolver. It cannot protect overlapping
owners: waiting requests paint waiting, main working paints working, idle paints
idle, agent working/stop and direct results remain silent. Metadata parsing rules
still apply where parsing is required.

An existing but unusable record is different from no configured state. Updates
that need a lock refuse symlinks, nonregular files, records over 8 KiB, and unknown
`cts`-family versions. Requests can still paint waiting; failed working/completion
updates stay silent. Idle paints without a lock only if the record is genuinely
absent. Startup can paint even if state cannot be saved. Writes are best effort:
I/O failure cannot provide durable ownership guarantees. Handled persistence
failures exit successfully without a blocking hook response or an elicitation
answer. Filesystem operations and the blocking record lock have no guaranteed
latency bound.

Ordinary state updates lock the session's own record, including its first creation,
then verify the path still names the locked inode. Atomic temporary-file rename
prevents partially written records; unchanged state is not rewritten. Different
sessions have separate locks. Serialized updates preserve distinct waits and exact
completion history within the capacity rules; they do not impose a deterministic
winner for simultaneous unrelated base transitions or order terminal output after
the lock is released.

Session-end deletion is currently **unlocked** and requires lifecycle quiescence:
no more hooks should update that session. It may unlink a future-version or other
unexpected path bearing that session filename. An overlapping update can recreate
the record, so no ordering guarantee is made across teardown. Reaping on startup
and explicit uninstall are also separate from the ordinary update guards.
This is an existing lifecycle limitation, not an ownership-retirement rule.

The wire writer emits `cts5`, preserving both anonymous permission provenance
(`?p`) and known background (`g <epoch>`). Readers also accept `cts1`–`cts4`,
without interpreting their reserved `g` fields. Guarded older readers refuse
ordinary updates of newer records rather than silently dropping activity.
Recognized older records migrate on a changed write; missing historical owner
provenance or background knowledge cannot be reconstructed. Unknown fields are ignored; invalid individual
wait tokens are dropped and malformed/foreign readable record contents fall back
to a fresh base rather than becoming reliable wait evidence. Accepted records are
not an authenticated input boundary. The dedicated directory is trusted; the
path checks do not promise resistance to malicious concurrent filesystem changes.

Startup examines at most 256 directory entries for stale records. Known records
with a stored process ID/start-time pair can be reaped when that origin no longer
matches. Records without origin, future-version records and recognized temporary
files use a 24-hour mtime recovery rule; future mtimes count as stale. Recognized
background records without origin are exempt: missing process metadata cannot
prove their work ended. Alien record
contents and unrelated filenames are not startup-reaped. Explicit uninstall has
broader name-based cleanup and does not preserve live sessions' records.

## Evidence and regression coverage

[The versioned scenarios](../tests/fixtures/state-contract-v1.json) are executed by
[the semantic runner](../tests/test_state_contract.py), independently of the
historical shell golden oracle. Every event specifies base, owners and clocks,
completion history, logical state, paint, and last displayed state. Observation
steps establish that elapsed time alone is not a state transition. Synthetic
events are not described as product captures.

| Contract area | Scenario IDs in `state-contract-v1.json` |
|---|---|
| Historical reconstruction | `capture_s4_reconstruction` |
| Overlap and owner-specific completion | `overlap_reverse_completions`, `main_and_agent_owners`, `unknown_notification_ownership` |
| Permission deduplication and ordering | `permission_backstop_both_orders`, `duplicate_named_request_refreshes_own_clock`, `late_permission_request_has_no_tombstone` |
| Recovery and interruption | `stop_background_distinctions`, `prompt_recovery_requires_human_content`, `interruption_without_a_hook` |
| Expiry versus display | `per_owner_expiry_and_stale_display`, `clock_rollback_and_ttl_zero` |
| Session lifecycle | `reset_compaction_and_quiescent_end` |
| Structural envelope rules (#5/#7) | `nested_metadata_cannot_impersonate_envelope`, `rejected_json_is_a_state_and_paint_noop`, `equivalent_json_notification_layouts` |
| Capacity | `bounded_permission_overflow` |
| Direct and notification provenance (#6/#9) | `direct_request_permission_interop`, `anonymous_direct_and_notification_recovery`, `tombstone_expiry_does_not_refresh_on_replay` |
| Invalid versus absent permission owner | `invalid_permission_owner_reserved`, `invalid_permission_owner_punctuated`, `invalid_permission_owner_oversized`, `invalid_permission_owner_unicode`, `absent_and_null_permission_owner_is_main` |
| Anonymous permission overlap | `known_permission_then_anonymous`, `anonymous_permission_then_known` |

The capture-derived permission sequence is a **reconstruction of the existing
`src/state.rs` summary**, not a newly preserved raw capture. Source label:
`slice3/log/s4`; Claude Code version: **UNKNOWN**. Its reported order is main Stop
with background work, an agent permission request, its ownerless notification
backstop, an unrelated ghost-agent stop, and the owner's tool completion. Relative
times and omitted hook fields are historical observations, not guaranteed product
timing or a complete envelope. The unit replay
`the_captured_subagent_sequence_ends_orange_and_then_restores_the_base` preserves
the same rationale.

Direct MCP evidence has a separate boundary: Claude Code 2.1.274 implementation
inspection and synthetic lifecycle tests, **not validated live interactive event
delivery**. See the [#9 evidence record](https://github.com/dalf/claude-tabstatus/issues/9#issuecomment-5854485979).

Supplemental coverage is in [state guarantees](../tests/test_state_guarantees.py)
for fallback/concurrency, [payload regressions](../tests/test_payload.py) for
envelope rejection and draining, [elicitation regressions](../tests/test_elicitation.py)
for identity and bounded lifecycle behavior, Rust state unit tests for exact clocks,
wire parsing and reaping, and [private tmux tests](../tests/test_tmux_status.py)
for actual display decay and rendering. Physical output, record semantics and
capture provenance are tested and reported separately.
