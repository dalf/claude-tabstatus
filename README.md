# claude-tabstatus

Puts the Claude Code session state into the terminal tab title, in front of the
location, so a row of tabs tells you which session wants you without losing
track of where each one is.

```text
🔵 streaming-browser@master       working  - the main agent is working
🟠 streaming-browser@master       waiting  - Claude needs an answer
🟣 streaming-browser@master       background - work continues after the main reply
⚪ streaming-browser@master       idle     - no known active work
⚪ srv:streaming-browser@master   the same session, over ssh
⚪ ~/code/bug_fedora              not a repo, so the path instead
⚪ ~                              at $HOME
```

The default priority is **input required > main working > background > idle**.
A long workflow remains purple after the main agent answers, including when you
can keep chatting with it. Main activity temporarily shows blue; unresolved
input requests stay orange. White means no known active work, not success.
Focusing a tab does not resolve a request.

Background tracking uses main `Stop` snapshots of the session's in-flight work.
Missing metadata and elapsed time cannot clear known activity. The
[indicator policy](docs/indicator-semantics.md) documents captured lifecycle
evidence, stale-state behavior, and compatibility. It requires a valid session
ID and writable state directory, as configured by the installed plugin; the
legacy stateless fallback cannot remember background work between hooks.

## States

The four states and the hook events that request transitions (live waits take
precedence, and idle transitions preserve known background work):

| Event | Scoped to | Paints |
|---|---|---|
| `SessionStart` | `startup\|resume\|clear\|fork` | ⚪ idle, and arms the Konsole tab |
| `SessionStart` | `source` is `compact` | *nothing* |
| `UserPromptSubmit` | | 🔵 working |
| `PreToolUse` | `AskUserQuestion\|ExitPlanMode` | 🟠 waiting |
| `PermissionRequest` | | 🟠 waiting |
| `PostToolUse` | | 🔵 working |
| `PostToolUseFailure` | | 🔵 working |
| `Notification` | `permission_prompt`, `worker_permission_prompt`, `agent_needs_input`, `elicitation_dialog`, `elicitation_url_dialog` | 🟠 waiting |
| `Notification` | `idle_prompt` | 🟣 background if known, otherwise ⚪ idle |
| `Notification` | any other kind | *nothing* |
| `Stop` | | 🟣 while background remains, otherwise ⚪ idle |
| `StopFailure` | | 🟣 background if known, otherwise ⚪ idle |
| `SubagentStop` | | *nothing*, unless that agent owned the wait |
| `Elicitation` | form/URL request | 🟠 waiting; duplicate completed requests stay silent |
| `ElicitationResult` | matching server and request ID, `accept`, `decline`, or `cancel` | restores the base only when no other wait remains |
| `SessionEnd` | | clears the title, restores the tab |

The `SessionStart` and `PreToolUse` scopes are hook *matchers*, so those hooks
do not even run outside them. The `Notification` kinds and the `compact` source
use parsed top-level metadata inside the binary, so those hooks run and then decide -
which costs one ~0.6ms process on a notification, and buys a decision the test
suite can assert rather than one that lives only in a config file.

Some of that is less obvious than it looks:

- **`PostToolUse` is registered unmatched**, so it runs on every tool call, and
  it is the recovery from waiting: 24-42ms after you answer a dialog the tab is
  blue again. It reads two things from its payload - `agent_id` and `session_id` -
  and a *subagent's* tool call paints nothing unless that subagent is the one you
  were waiting on, because a background subagent's tool call firing in the main
  session must not repaint over a dialog you are looking at. A matcher could not
  do either: a matcher sees only `tool_name`. Wait ownership, below, is the whole
  of that distinction. The payload carries the whole `tool_response`, which can
  be hundreds of KB on a large read. The shared reader buffers and parses the
  complete envelope up to 16 MiB, skipping unneeded values; larger input is
  drained and rejected. Work grows with the input size. Earlier timings for the
  prefix-scanning reader do not measure this parser.
- **`PreToolUse` is matched to exactly the two tools that always block on you.**
  Unmatched, it would paint waiting on every tool call. `PermissionRequest`
  fires for both of those tools anyway, 11-19ms later, so this edge is really
  only insurance for a permission path that gets bypassed.
- **A `Notification` never paints waiting on `idle_prompt`.** That kind is the
  quiet-turn nudge, fired `messageIdleNotifThresholdMs` (default 60s) after a
  turn ends, so mapping it to waiting would turn every idle tab orange a minute
  later and collapse idle and input-required into one. It is also the only
  recovery from an interrupted turn - see the Ctrl+C limitation below.
- **The waiting notifications are a backstop, not the fast path.**
  `permission_prompt` is scheduled 6.00s after the dialog appears, fires at most
  once per dialog, and is suppressed outright by
  `CLAUDE_CODE_DISABLE_PERMISSION_PROMPT_NOTIFY_HOOKS`. `PermissionRequest` is
  the real-time signal. What the notifications add is what `PermissionRequest`
  does not cover: a worker prompt, an agent asking for input, an MCP elicitation
  dialog, and the dialogs that are not tool calls at all - the managed-settings
  review, the sandbox network request - which is why `permission_prompt` is kept
  even though it is redundant for every tool dialog. The product does cancel the
  notification when you answer first, but best-effort: measured once firing in
  the same millisecond as the answering keystroke, so its orange and
  `PostToolUse`'s blue were emitted concurrently and landed 38ms apart. The
  order came out right; nothing guarantees it.
- **A subagent's `PermissionRequest` paints waiting too.** An asynchronous
  `Task` returns in ~6ms, so the main session's `Stop` has usually already
  fired: measured, a subagent's dialog arrived a second *after* the tab went
  idle, and nothing repainted until a human answered it. Treating it as a no-op
  would leave the tab idle while you are the one blocking.
- **`SubagentStop` is registered and paints nothing of its own.** A subagent
  finishing must not read as the session going idle - and it also fires for
  agents nothing announced: measured, a `SubagentStop` for `a8e90c10`, with an
  empty `agent_type` and no matching `SubagentStart`, nine seconds *before* the
  user answered a different agent's dialog, and another one immediately before a
  `SessionStart`/`compact`. No matcher could filter those. The owner test does:
  this edge clears the wait whose `agent_id` matches and is otherwise a complete
  no-op, which is also what it is when there is nowhere to keep a record. It is
  here because it is the only signal for a dialog you **declined** - no hook
  fires for a denial, the tool never runs, and no `PostToolUse` ever arrives.
- **The `agent_id` filter used to cost a tab left orange, and that is what wait
  ownership fixes.** The two cases the filter cannot tell apart are both a
  subagent's `PostToolUse`: one arriving while *your* dialog is open (must not
  paint) and one arriving after you answered the *subagent's* dialog (should
  un-paint). No field separates them - the difference is in what came before - so
  stateless the filter took the safe half and paid for it: after approving a
  subagent's dialog, nothing repainted until the `Task` returned, so the tab read
  orange for the rest of that subagent's run, which is minutes. Inside tmux the
  decay healed it after `CCTAB_TTL_WAITING`; in a plain Konsole tab nothing
  healed it at all. It is now resolved by recording *who* raised the wait.
- **`StopFailure` is there because `Stop` is not.** When a turn dies on an API
  error - `rate_limit`, `overloaded` - only `StopFailure` fires, so without it a
  rate-limited turn would leave the tab blue indefinitely.

What the binary paints depends on the edge, **top-level JSON metadata**, and a
small record per session for wait ownership. Hook input is parsed with
`serde_json::from_slice` and a selective visitor: `IgnoredAny` skips unused tool
inputs and results without constructing a JSON tree. Nested `agent_id`, `source`
or notification fields cannot impersonate hook metadata. Valid JSON whitespace,
member order and escapes have the same meaning, including fields far from either
end of a large payload. Skipped strings are syntax-checked rather than decoded:
an unpaired surrogate escape in unused data is accepted by `IgnoredAny`, while
retained string fields require successful decoding. Raw invalid UTF-8 is rejected
everywhere. Skipped arrays and objects are traversed iteratively, so Serde's
ordinary recursive-deserialization depth limit does not bound their nesting.

The complete input is buffered up to **16 MiB, including whitespace**. Oversized
input is drained to EOF and ignored. This bounds the buffered input, not total
memory or processing time: parsing and draining still require work, and parsing
may allocate additional storage. Parsed input must be UTF-8 and contain one
complete JSON object, with no trailing document or garbage. Recognized fields
must be strings (or null), except `background_tasks`, which must be an array (or
null). Duplicate recognized keys are rejected, including escaped spellings of the
same key; unknown keys are skipped. Missing and null fields are absent; empty
session and agent IDs are absent too. Only the first decoded prompt character and
whether the background array is empty are retained from those two fields.
Artifact watches do not count (see [state contract](docs/state-contract.md)).

Read errors, malformed input, wrong field types and oversized input are **silent
no-ops, exiting zero before any state record is opened or changed**. Zero-byte
stdin and interactive terminal invocations retain the manual paint behavior;
whitespace-only input is invalid. Metadata-independent edges (`waiting`, `idle`,
`session-end` and unknown edges) still only drain stdin when no state directory
is configured, so JSON validation does not apply on that stateless fast path.
With a configured state directory every edge parses its metadata.

The payload parser fixes metadata interpretation; wait retirement and the
settings-file editor have separate contracts. Background lifecycle has its own
captured and synthetic tests, described in the indicator policy.

### Wait ownership

The [state and wait-ownership contract](docs/state-contract.md) specifies the
supported transitions, recovery policies, persistence guarantees and limitations.
Its [versioned semantic traces](tests/fixtures/state-contract-v1.json) assert the
base, outstanding owners, clocks, emitted update and displayed indicator after
every event, independently of the historical golden oracle.

The model here is partly taken from
[Yannis-Adn/terminal-addons](https://github.com/Yannis-Adn/terminal-addons) (MIT),
whose `wt-tab-status` keeps one state file per session holding a state *and the
owner of a wait*, and only lets the waiting agent end the wait. `src/state.rs`
credits the exact functions, and records the four places this diverges - the
largest being that its `PermissionRequest` branch no-ops on a subagent's dialog,
which the capture below shows would paint idle while a human is being asked.


A wait is an **overlay on activity**. Main work shows blue; otherwise known
background shows purple and idle shows white. A dialog covers that activity;
clearing the *last* dialog restores it. Who
raised a wait is therefore part of the state, and it cannot be recovered from any
one payload - which is why it is persisted alongside background knowledge.

The capture that forced it, timings relative to that session's `SessionStart`:

| t | event | `agent_id` | stateless | with ownership |
|---|---|---|---|---|
| 66.283 | `PreToolUse` `tool_name=Agent` | - | 🔵 | 🔵 base `w` |
| 68.946 | `Stop` `background_tasks=[subagent:running:aec99e]` | - | ⚪ | 🟣 base `i`, background recorded |
| 69.960 | `PermissionRequest` | `aec99e1f` | 🟠 | 🟠 wait owned by `aec99e1f` |
| 75.983 | `Notification` `permission_prompt` | *absent* | 🟠 | 🟠 no second owner added |
| 98.459 | `SubagentStop` | `a8e90c10` | - | *nothing*: owns no wait |
| 107.496 | `PostToolUse` (you approved) | `aec99e1f` | *nothing* | 🟣 background remains |
| 110.230 | `SubagentStop` | `aec99e1f` | - | *nothing*: already cleared |

Three things in that sequence decide the design, and each one rules out a simpler
answer:

- The main thread's `Stop` lands **before** the subagent's dialog. So no-oping a
  subagent `PermissionRequest` is not the fix either - it would paint idle while
  something genuinely wants your approval.
- The event that resolves the dialog is the **subagent's own** `PostToolUse`,
  which the stateless filter has to discard.
- `SubagentStop` fires for agents nothing announced. A wait is cleared only by an
  `agent_id` that **matches**, or the ghost at 98.459 would have un-painted a live
  dialog nine seconds early.

The rules, in full:

| edge | what it does to the record | what it paints |
|---|---|---|
| `waiting` | adds the usable `agent_id`, or the main loop when absent; a supplied unusable/reserved ID becomes an independent anonymous permission wait | 🟠 always |
| `waiting` from a permission/input `Notification` | adds an unknown permission owner `?` unless a permission wait already exists; an owned permission wait replaces that anonymous permission backstop | 🟠 always |
| `waiting` from an MCP `Notification` | adds an anonymous backstop `?!`; a complete server/request identity instead shares its direct wait | 🟠 unless an identified request has already completed |
| `elicitation` | adds a wait keyed by server/request ID, or an anonymous direct wait | 🟠 unless that key has already completed |
| `elicitation-result` | removes only the matching server/request wait for a recognized response | the base if that emptied the set, else nothing |
| `working`, a `PostToolUse` on the main thread | base ← `w`; clears main and anonymous notification waits, preserving direct elicitations | the base, or nothing if a wait remains |
| `working`, a `UserPromptSubmit` **you typed** | base ← `w`; clears **every** wait | the base |
| `working`, a subagent | clears only the wait it owns; base untouched | the base if that emptied the set, else nothing |
| `idle` | base ← `i`; clears a main wait, and remaining permission/notification waits when `background_tasks` is `[]`; direct elicitations remain | 🟣 if background remains, otherwise ⚪; orange refresh when background presence changes beneath a wait |
| `subagent-stop` | clears only the wait it owns | the base if that emptied the set, else nothing |
| `session-start` | resets the record, and reaps | ⚪ as before |
| `session-end` | removes the record | clears the title |

Four consequences worth naming:

- **Two dialogs at once are both remembered.** A main-thread dialog queued behind
  a subagent's is the realistic overlap, and a single owner slot gets it wrong
  whichever one it keeps: answering the main one must not restore the base while
  the agent's dialog is still on screen. So the record holds a *set*, newest last,
  and the base comes back when the set empties.
- **A main-thread tool call no longer repaints over a subagent's dialog.** This is
  the half of the fix that matters most, and stateless it was not even possible to
  attempt: the blunt `agent_id` filter only caught *subagent* tool calls.
- **`Stop` does not paint idle over an outstanding dialog** - not while something
  is actually running. It always clears a *main* wait, because the loop could not
  have stopped while a main-thread dialog blocked it (a rejected `ExitPlanMode`,
  whose `PostToolUse` never comes, is exactly that). What it does with the others
  depends on `background_tasks`: see the next block.
- **`UserPromptSubmit` clears everything, but only when you typed it.** You cannot
  type at the prompt while a modal is up, so a prompt of yours proves the screen is
  clear whoever owned the dialog. The trap is that not every `UserPromptSubmit` is
  yours: the capture's 110.251 event carries
  `"prompt":"<task-notification>..."`, which the *product* injects when an async
  agent finishes, and with two agents running the first one's completion would then
  retire the second one's live dialog. So the prompt must be present and must not
  begin with `<` (without trimming whitespace) - a heuristic based on the captured
  injected prompt, not proof of human authorship. A human prompt beginning with `<` also
  fails this recovery check. In the capture, that agent's own `SubagentStop`
  fires 20ms earlier and already handles its completion.

**An overlay nothing can lift is worse than no overlay at all.** While any wait is
held, neither `working` nor `idle` paints - that is the whole mechanism - so a wait
that outlives its dialog freezes the tab orange, and outside tmux nothing decays it.
The retirement conditions include ownership evidence and stale-wait recovery:

| what retires it | evidence or recovery policy |
|---|---|
| the known owner's own completion | that agent's `PostToolUse`, or its `SubagentStop` when you declined and no tool ever ran; an arbitrary agent never owns an anonymous wait |
| main-thread tool progress | preserves recovery for main and anonymous notification waits; direct elicitation waits remain |
| a `UserPromptSubmit` you typed | a modal dialog and a usable prompt cannot both be on screen |
| `background_tasks` empty at `Stop` | recovers permission and notification waits when no background tasks remain; direct elicitation waits remain |
| its own expiry | `CCTAB_TTL_WAITING`, **per wait** |

The recovery paths exist because the captures are emphatic that *abandoning* a dialog
fires no hook whatsoever: Esc on a live dialog (`s2`), declining one (`s7`) and
Ctrl+C mid-tool (`s5`) each emit nothing at all until the next prompt. Nothing the
owner does can be waited for, because the owner does nothing.

Notification-only waits carry no request identity. An unrelated subagent's
`PostToolUse`, `PostToolUseFailure`, or `SubagentStop` therefore leaves them intact,
even when there is only one wait. Injected or missing-content `UserPromptSubmit`
events also preserve anonymous waits. MCP notifications retain separate provenance
so a permission request cannot replace them, and a matching subagent permission completion
leaves the MCP wait standing. Both identity-free MCP notification kinds share one
anonymous backstop slot, separate from direct elicitation requests.

### Direct MCP elicitation

`Elicitation` and `ElicitationResult` are registered without a server matcher.
They observe requests and responses and emit only the existing title protocol;
they never answer forms, replace responses, change permission decisions, or exit
with a blocking status. Message text, form answers, requested schemas, credentials,
and authentication URLs are skipped and never persisted.

The identity is `(session_id, mcp_server_name, elicitation_id)`. Server and request
IDs must each be nonempty, at most 64 UTF-8 bytes, and contain no control characters.
They are stored whole using an unambiguous encoding, never truncated. Form and URL
modes are supported; an absent mode is supported too. Unsupported modes and malformed
JSON are silent no-ops. A valid result with `accept`, `decline`, or `cancel` removes
only its matching wait. The visible title comes from the remaining waits and the
current working/idle base, so answering request A cannot hide request B or a
permission dialog.

Requests without a usable server/ID pair share one anonymous **direct** wait.
Repeated anonymous direct requests keep that aggregate's first-observed clock.
An uncorrelated result clears nothing, even if server and mode match the only
pending request. Direct waits survive main-thread tool progress, a quiet `Stop`,
and unrelated agent completion. A new human prompt, per-wait `CCTAB_TTL_WAITING`
expiry, or session reset/end recovers them. Expiry is processed on subsequent
events; no daemon repaints an otherwise quiet terminal. Setting the TTL to `0`
disables expiry, leaving prompt/session recovery available.

The state retains up to eight completion keys as tombstones, with the same TTL.
This makes repeated starts/results and a result arriving before its start
idempotent while the key is retained. Duplicate events do not extend their clocks.
Reusing a completed ID in the same server/session during that retention period is
treated as a duplicate. A key evicted from the bounded history or expired from it
can be observed as a new request again.

Notifications coalesce with a direct request **only** when they carry the same
complete server/ID pair. Identity-free notifications stay independent, whether
they arrive before or after a direct event. Claude Code 2.1.274's notification
construction omits those identifiers, so there is no reliable way to distinguish
a delayed duplicate from a new independent dialog. Such a notification can raise
the anonymous backstop again after a direct result; main progress, a quiet `Stop`,
a human prompt, expiry, or session cleanup recovers it. Suppressing it solely
because another request is known would hide independent dialogs.

At most eight individual waits are retained. Further requests add one aggregate
overflow wait rather than evicting an unrelated wait; it survives individual
completions and uses prompt/session recovery or its own expiry. Further overflow
does not refresh that clock. Pending state and completion history fit within an
8 KiB record. Concurrent creation and updates use the session record lock.

Validation evidence, version information, and the distinction between synthetic
replays and live captures are recorded in
[elicitation evidence](https://github.com/dalf/claude-tabstatus/issues/9#issuecomment-5854485979). The input schema follows the
[official hook reference](https://code.claude.com/docs/en/hooks#elicitationresult).
Both events are present in the inspected Claude Code 2.1.274 executable; earlier
versions and live interactive delivery have not been validated for this feature.
Without usable persistence, requests can still paint waiting, but results stay
silent because they cannot prove what is outstanding.

"Absent" is not "empty", and that asymmetry is deliberate in both directions. A
`Notification` carries no `background_tasks` at all, and a Claude Code that renamed
the member would carry none either, so a missing array preserves non-main waits -
the conservative answer. A *non-empty* one leaves them standing for the same reason,
which is exactly the capture's 68.946 `Stop`.

The per-wait epoch is the other half. With one epoch for the whole list, raising any
dialog refreshed it, so every later dialog pushed a *stale* wait's expiry out with
it: measured on that shape with the TTL set to 3s, one abandoned subagent wait plus
four ordinary main turns 2s apart left **16 consecutive edges painting nothing**, 8s
into a 3s horizon - a tab frozen orange for the rest of the session, with the TTL
unable to rescue it. Each wait now carries its own clock, and an expiry is written
back to the record rather than recomputed, so raising the TTL later cannot resurrect
a wait already declared dead.

**A stale record is normal, not exceptional.** A session killed with `SIGKILL`
fires no `SessionEnd`, so the record has three independent bounds and no daemon:

- An outstanding **wait** older than `CCTAB_TTL_WAITING` (900s), measured **per
  wait** against that wait's own epoch. State expiry requires a subsequent
  eligible hook. Tmux title decay uses the same setting and grammar, but follows
  its own carrier clock.
- A **`SessionStart` in any session reaps the others**, by asking whether the
  process that wrote each record is still running. Every write stamps the record
  with `$CLAUDE_PID` *and that pid's start time* - field 22 of `/proc/<pid>/stat` -
  and the reaper unlinks a record only when that pair no longer names a running
  process. (`SessionStart` is where it usually happens; doing it on any write is
  what covers a session whose `SessionStart` ran before this plugin was installed,
  which would otherwise be left on the one-day mtime rule for its whole life.)
- The directory is under `$XDG_RUNTIME_DIR`, which the OS empties at logout.

The reaper **provably cannot delete a live session's record that carries an
origin**, and the pair is why.
A hook process is a child of `$CLAUDE_PID`, so that process exists whenever any of
its own hooks are firing, and a start time is immutable for the life of a process.
So the unlink predicate is false for every live session, whatever the record's age -
and *age is what a naive reaper would have used*. An mtime horizon cannot do this
job: a session sitting at its prompt touches nothing, so any horizon short enough to
be useful would eventually delete the record of a session that is merely idle. The
start time is also what makes pid recycling harmless - a recycled pid reads as
*gone*, not as alive, because the start time under it differs. The error the rule
*can* make is the harmless one: keeping a dead session's record if a new process
were handed the same pid inside the same 10ms tick, which needs 4194304 intervening
spawns (`pid_max`, measured, at `CLK_TCK` 100).

The parse has one trap worth naming, because the obvious `awk '{print $22}'` falls
into it: field 2 of `/proc/<pid>/stat` is the executable name in parentheses, and it
may itself contain spaces and parentheses. Measured on this machine, one process
reads `(npm exec chrome...)` - exactly the sort of thing a `claude` session spawns -
so the field is read *after the last* `") "`, in the binary and in the test alike.

A file that names no origin - a record written before this field existed, or a
`.tmp` from a crashed write, or a *newer* version's record whose writer may be
running right now - falls back to mtime with a **one-day** horizon, deliberately
long for the reason above. A recognized record with known background is exempt
when its origin is missing: elapsed time alone cannot prove that work ended.
It remains until a clearing snapshot, session reset/end, or explicit cleanup.

**The reaper deletes only what it can prove is ours, and everything else is left
alone forever.** A file whose name is not a session id, a file that is not a record
in any version's shape, a file it could not read, and anything that is not a regular
file or a symlink: none of those is a candidate at any age. That is stricter than it
looks necessary and the reason is concrete - `CCTAB_STATE_DIR` is a documented knob,
the name grammar `[A-Za-z0-9_-]{1,64}` cannot tell a session id from `id_rsa`, and an
earlier rule that fell back to mtime for anything it could not parse deleted a
30-day-old private key out of a directory that had other things in it. The price is
bounded litter in a tmpfs that logout empties; `doctor` names every file it will not
take, and says why. **Point `CCTAB_STATE_DIR` at a dedicated directory.**

Correctness never depends on any of it: `SessionStart` rewrites its *own* record
before reaping, so a resumed or reused session id can never read a dead session's
record as authoritative. Reaping is hygiene, and `tabstatus doctor` names every
record it would take.

The record is one file per `session_id`, so five concurrent sessions never contend.
It is written temp-then-`rename`, because two hooks of one turn do overlap -
measured 1ms apart - and a reader must see the old record or the new one, never a
torn one. 240 concurrent hooks against one record leave one well-formed file and no
temporary.

**Each read-modify-write holds an exclusive `flock` on that session's own record**,
and "a lost update self-corrects on the next edge" - which this section used to
claim - is false in the one direction that matters. When the lost update is a
*clear*, the record keeps a phantom wait, and then `working`, `idle` and the 3s idle
nudge all decline to paint: the tab is orange until the TTL, which outside tmux is
the fifteen-minute lie this whole layer exists to remove. Measured on the same code
with the lock disabled, 400 rounds of a subagent's un-painting `PostToolUse`
launched simultaneously with main's `Stop`: **76 of 400 lost the clear**, and 36 of
400 in the reverse direction lost the wait. With the lock, 0 of 1200 across three
runs, and five concurrent sessions never touch each other's file.

These locks serialize record updates, not terminal writes after the locks are
released. `SessionEnd` is cleanup after a session has stopped issuing hooks; it
unlinks the session path without this lock. Concurrent teardown and new hooks
reusing the same session ID have no ordering guarantee.

Two details of the lock are load-bearing. It is taken on the record path, and
because `write_if_changed` renames over that path the inode can change under a
waiter - which would leave it holding an exclusive lock on an unlinked inode while a
third hook held the new one - so after locking it checks that it holds the inode the
path names *now*, and retries when it does not. Creation-capable edges atomically
create the record before locking, so concurrent first requests and results are
serialized too. Unrelated agent events still create no file. If a lock cannot be
obtained, the hook makes no unlocked state change. `std::fs::File::lock` ships in
std (1.89), so this costs no dependency; it
is the reason `rust-version` moved from 1.74 to 1.89.

```text
cts5                                           the tag: version 5 of the wire
b i                                            base = w | a | i
g 1790380620                                  last main Stop reporting background
p 3709427 84460384                             the session's (pid, start time)
w aec99e1f4bda1972b:1790380630 -:1790380631    one wait per word: owner, then epoch
```

`-` is the main loop, `?` an unknown permission/input notification, `?p` an anonymous
permission request with an unusable supplied owner, and `?!` an anonymous
MCP notification backstop. `!?` is an anonymous direct MCP request, `!+` an overflow
aggregate, and `!<hex-server>.<hex-request>` an identified request. The optional
`e` line holds completed request keys with their completion epochs. An `agent_id` is
accepted only as `[A-Za-z0-9_-]{1,64}` - the same test that stops a `session_id`
from choosing the path it is filed under. Unknown keys are **skipped**, and a base
letter this version cannot paint reads as idle, so a newer version's record
degrades rather than being misread; and a record this version did not *change* is
not rewritten, so it keeps the fields it did not understand.

Versions 1–4 (`cts1`, `cts2`, `cts3`, `cts4`) remain readable and become `cts5`
on the next state change. Version 1 `?` waits lack provenance and retain the permission-backstop
deduplication policy; the original notification kind cannot be recovered.
The `cts5` tag prevents guarded older binaries from dropping known background
activity: their ordinary update guards leave the record untouched. The optional
`g` epoch never expires. Older records do not acquire background knowledge during
migration, even if they contain a formerly reserved `g` field. Session teardown and older releases without those guards remain outside
that guarantee. Use the updated binary for all hooks.

Anonymous permission requests remain independent of named requests and notification
backstops. They keep their first epoch and survive main tool activity and agent
completion. A human prompt, explicit quiet Stop (`background_tasks: []`), expiry,
or session reset/end can clear them.

**What it costs, per edge - and how to re-measure it.** This section used to carry
remembered numbers, and they did not reproduce: a per-edge cost of tens of
microseconds against a fork-and-exec floor of several hundred is below the noise of a
desktop that is also doing something else, and an earlier harness that forked three
times per invocation put an 1100us floor under a 500us measurement. So the numbers
live in a script instead:

```sh
sh scripts/bench-state.sh                      # the layer against itself, switched off
sh scripts/bench-state.sh <baseline-binary>    # ...and against a pre-slice build
```

It interleaves every arm within each round, reports the spread as well as the
minimum, and gives the **median of the per-round paired deltas** rather than a
difference of two minima. Two independent 21-round passes of 200 execs on this
machine, no baseline argument, so the comparison is the layer against itself with no
state directory:

| arm | state I/O | min us | spread | vs layer-off |
|---|---|---|---|---|
| `new-nostate` — the layer switched off | none | 460 | 42 | +0 |
| **`working`, main thread, record already current** | read | 475 | 145 | **+15us** |
| `working`, main thread, a wait held by somebody else | read | 453 | 62 | −8us |
| `working`, a transition | read + write | 479 | 54 | **+22us** |
| `working`, a subagent that owns no wait | read | 436 | 48 | −24us |
| `working`, the subagent that owns the wait | read + write | 446 | 42 | −14us |
| `idle` / `Stop` | read | 474 | 176 | **+16us** |

The two arms that come out *negative* are not noise and are worth understanding: an
edge that owns no wait, or that holds one somebody else owns, returns before the
location walk and before the emit, so it is genuinely cheaper than a painting edge.
The layer's own cost is the +15us on the hot `working` read and the +15 to +22us on a
transition, against a ~460us floor - so under 5% there, and under 3% of the 767us a
real hook costs including the harness fork.

The half of the claim that is cheap to **check** rather than to believe is write
avoidance, and it holds: 50 steady-state main-thread `working` edges leave the
record's mtime untouched, and a subagent tool call that owns no wait returns without
writing - or creating - anything at all. That asymmetry is why `write_if_changed`
earns its place: after the first main-thread tool call of a turn sets the base, every
later one in that turn is a read and nothing else, so transitions are a handful per
turn against hundreds of tool calls.

**Stateful edges parse the same metadata as stateless edges.** The record is
filed under the decoded top-level `session_id`, and wait ownership uses the same
`agent_id` accessor that suppresses stateless subagent repaints. `idle` checks
whether the top-level `background_tasks` array is empty; absent or null means
unknown. The parser scans unused values but does not keep their contents. Its
work grows with input size, unlike the historical bounded-window reader.

**Background work.** Main `Stop` snapshots record a bounded aggregate of in-flight
work: a nonempty registry sets `g <epoch>`, an explicit empty registry clears it,
and missing/null metadata preserves it. No individual task list or count is
stored. Unknown kinds and arbitrarily many entries within the payload limit stay
conservatively active. Child completion alone does not prove that a workflow
ended. The [indicator policy](docs/indicator-semantics.md) documents the observed
automatic completion turns and the last-known diagnostic when a hook is missed.

**Planned extension.** A reserved `n <epoch> <text>` line, last in the record,
could cache the session's `aiTitle` from the transcript. It remains unimplemented.

## Location

Inside a git repository the location is `<repo>@<branch>`, and the
subdirectory is deliberately not shown: the branch is the thing that changes
under you, and every tab of the same repo staying recognizably the same tab is
the point. Outside a repository there is no branch to show, so the location is
the whole home-relative path instead of just a basename.

| Situation | Location |
|---|---|
| in a repo, any subdirectory of it | `streaming-browser@master` |
| a branch name with slashes | `claude-tabstatus@feature/tab-title` |
| a detached HEAD | `streaming-browser@b56583d` |
| a linked worktree, or a submodule | its own directory name and its own branch |
| not a repo, under `$HOME` | `~/code/bug_fedora` |
| not a repo, elsewhere | `/srv/www` |
| `$HOME` itself | `~` |
| over ssh | `srv:` in front of any of the above |

**A local session has no prefix at all** - that absence is how you recognize
it. Only `SSH_CONNECTION` or `SSH_TTY` puts a host in front.

Long locations are elided to 32 columns, and the two forms lose different
ends, because different halves carry the information:

```text
~/code/one/two/three/four/five/six   ->  …/two/three/four/five/six
repo@some-very-long-branch-name      ->  repo@some-very-long-branch-nam…
```

A path is cut at the front on a component boundary, so both Konsole (which
elides from the left) and Windows Terminal (which truncates from the right)
show the same informative tail. See [Location tuning](#location-tuning) to
change the cap.

The repository is found by walking up for a `.git`, reading `.git/HEAD`
directly and parsing it with shell parameter expansion. `git` is never
executed: one `git rev-parse` costs 15-40ms, where the whole location costs
about 0.12ms, and a later slice will run this on every tool call.

## Install

```sh
git clone <this repo> ~/code/claude-tabstatus
cd ~/code/claude-tabstatus
sh scripts/build.sh          # needs cargo, or mise with a rust tool
./bin/tabstatus install
```

Then start a **new** Claude Code session. Updating later is `git pull`, a
rebuild, **an install**, and a new session:

```sh
cd ~/code/claude-tabstatus && git pull && sh scripts/build.sh && ./bin/tabstatus install
```

On a machine with **no checkout** - a remote VM reached over ssh - it is the same
verb with nothing else on it, because the two manifests Claude Code needs are
compiled into the binary:

```sh
scp -C bin/tabstatus-x86_64-unknown-linux-musl vm:~/tabstatus
ssh vm 'chmod +x ~/tabstatus && ~/tabstatus install && ~/tabstatus doctor'
```

The build step is temporary: binaries are not committed, and will ship as
GitHub release assets so that installing needs no toolchain.

### The plugin directory is build output

**`install` never links your checkout. It writes a plugin directory and links
that.** `hooks/hooks.json` and `.claude-plugin/plugin.json` are *source*: tracked
files you read, diff and edit, and they reach a running session the way
`src/main.rs` does - through a build. `scripts/build.sh` compiles them into the
binary with `include_str!`, and `install` writes them back out into

```text
~/.local/share/claude-tabstatus/
    .claude-plugin/plugin.json      the copy compiled in
    hooks/hooks.json                the copy compiled in
    bin/tabstatus                   a copy of the binary you just ran
    .tabstatus-generated            the marker: version, target triple, file list
```

then sets the env key and points `~/.claude/skills/claude-tabstatus` at **that**.
The tree goes in `$XDG_DATA_HOME/claude-tabstatus`, or
`$HOME/.local/share/claude-tabstatus`; `install --tree <dir>` puts it anywhere
else. One code path, in a checkout and on a bare VM alike - there is no second
verb and no mode.

It used to be a symlink **to the clone**, and that had to change:

- `git checkout` of a working branch whose `hooks.json` is broken broke **every
  prompt in every running session**, instantly. Deployment must be an explicit
  act, not a side effect of changing branches.
- `git pull` did something subtler and worse, because `bin/` is gitignored: you
  got the **new** `hooks.json` live at once while `bin/` still held the **old**
  binary - new hook edges pointing at a binary that had never heard of them. One
  rebuild plus one `install` moves both together, so that state is unreachable.

Nothing in the checkout is written by any install path, and the test suite asserts
that as a byte comparison after every install it performs. `bin/` keeps its job:
it is still the binary you *run*, and `./bin/tabstatus install` is the documented
command. It simply stops being the binary *hooks* run.

`-C` on the `scp`, because it does not compress by default and one file is not
automatically fewer bytes than the tarball it replaces: the raw binary is 680 KB
against the old four-entry tarball's 303 KB, and `gzip -9` of it is 334 KB. The
whole wire cost of shipping one file instead of a tree is about 31 KB, with the
flag. Without it, 2.2x.

### Migrating from a checkout symlink

If `~/.claude/skills/claude-tabstatus` currently points at your clone, one
`install` moves it, and it **says so before it writes anything**:

```text
plugin:   /home/me/.claude/skills/claude-tabstatus
          now  -> /home/me/code/claude-tabstatus (a checkout, not a generated tree)
          will -> /home/me/.local/share/claude-tabstatus
          That checkout stops being the live plugin. Its hooks.json and
          plugin.json are SOURCE from now on; `tabstatus install` is what
          deploys them. Nothing in it is modified.
```

A silent repoint of live wiring is the wrong behaviour whatever it is for.
`doctor` names the same thing, which is how it gets discovered - `doctor` is what
you run when a tab misbehaves:

```text
plugin:    WARN /home/me/.claude/skills/claude-tabstatus points at a CHECKOUT, not at a generated tree:
           /home/me/code/claude-tabstatus
           That is the wiring from before the plugin directory became build
           output - a `git checkout` there changes what every running session
           runs. `tabstatus install` repoints it at a generated tree.
```

It is safe to run **while sessions are live**, and two mechanics make that true
rather than hopeful. The symlink is repointed with a temp link and `rename(2)`,
not `unlink` then `symlink`: `rename` over a symlink is atomic, so the name
resolves to the old target or the new one and never to *nothing* - a hook firing
in that window would exec a missing file, and a non-zero `PreToolUse` hook
**blocks a tool**. The binary is copied to a temp file in the tree's own `bin/`,
**exec'd there**, and only then renamed onto `bin/tabstatus`, so the live hook
path is never absent for the length of a 680 KB copy and nothing becomes live
wiring without having been run first.

One thing an uninstall will **not** do afterwards: put the checkout link back.
The state record's `symlink_before.target` is written once, at the first install,
and on a machine that was wired the old way it names the clone - so restoring it
would rebuild exactly the arrangement this change exists to abolish. It is
declined by name:

```text
symlink:  removed /home/me/.claude/skills/claude-tabstatus
          (was -> /home/me/.local/share/claude-tabstatus)
          the recorded prior target was the checkout at /home/me/code/claude-tabstatus;
          a checkout is no longer a plugin directory, so the link is
          removed rather than pointed back at it.
```

That write-once record is also why `install` no longer promises an undo it cannot
deliver. When it replaces a link it did not create it used to say "uninstall puts
the old target back." - true on a *first* install, which is the run that writes the
record, and false on every one after it, because from then on the target being
replaced and the target `uninstall` reads are different paths. It was printed in
exactly the case it exists for: live wiring being repointed. It now says what the
record actually holds:

```text
symlink:  WARNING - repointed a symlink this installer did not create
          /home/me/.claude/skills/claude-tabstatus
          was  /home/me/somewhere/else
          now  /home/me/.local/share/claude-tabstatus
          uninstall will NOT put this target back: the record is
          write-once, so it restores /home/me/first-install-found-this - what the
          FIRST install found here.
```

### If you delete the tree but keep the link

The two live effects differ, and `doctor` names both, because only naming both
answers the question:

```text
tree:      /home/me/.local/share/claude-tabstatus (from the plugin symlink)
           FAIL the plugin directory is not there.
           A session already running has its hooks registered and now execs a
           missing file - 127 per event, and a PreToolUse 127 can block a tool.
           A NEW session loads no plugin at all and the tab stays BLANK, with no
           error anywhere, because env.CLAUDE_CODE_DISABLE_TERMINAL_TITLE is still set.
           `tabstatus install` writes it back.
```

`install` heals it *in place*: the link already names where the tree belongs, so
the tree is rewritten there and the link is not touched. An
`install --tree <somewhere>` you chose once is not silently abandoned for the
default path.

### How the generated tree is written

`include_str!` rather than a crate. It is a std macro, it costs no dependency,
and it is the direct analogue of Go's `//go:embed`: the JSON becomes a rustc
build **input**, so a `cargo build` binary cannot carry a copy that disagrees
with the tree it was built from. `rust-embed` and `include_dir` solve a different
problem - globbing an asset tree and iterating it at runtime - and both are
proc-macro crates. Two `include_str!` lines execute nothing at build time.

**Ownership is positive evidence, never inference.** A tree `install` generated
carries `.tabstatus-generated`; a directory without it is refused rather than
written into. That is the marker's only job now - it used to double as a mode
discriminator, and there is one shape of plugin directory left, so it does not.

```text
error: /home/me/notes already exists, is not empty, and carries no
       .tabstatus-generated - so it was not written by `tabstatus install` and is
       not ours to overwrite. Pass a different directory with `--tree`. Nothing has
       been changed.
```

That refusal used to end with a copy-pasteable `rm -rf <whatever you typed>`, and
the subject of that sentence is an argument - so `--tree /` printed `rm -rf /`,
`--tree ~` printed it on the home directory, and a slipped `--tree ..` printed it
on a parent full of somebody's work. The refusal itself was right and wrote
nothing; the defect was that the suggested remedy for a typo was unrecoverable, in
the one message a hurried operator copies. The hint is now offered **only** for a
directory that looks like a stale tree of ours - named `claude-tabstatus`, or
sitting exactly where the default one would - and never for `$HOME` or for anything
fewer than three levels down, whatever it is called:

```text
error: /home/me/.local/share/claude-tabstatus already exists, is not empty, and
       carries no .tabstatus-generated - ... If it is nothing you need, remove it -
       `rm -rf /home/me/.local/share/claude-tabstatus` - and re-run. Pass a
       different directory with `--tree`. Nothing has been changed.
```

A directory with a `.git` in it is refused a second time even if a marker appears
there, a directory **inside** this checkout is refused a third time - that is the
last door by which the clone could become the plugin directory again, and it looks
for a `.git` beside a `.claude-plugin/plugin.json` specifically, because plenty of
people keep `$HOME` itself in git and the default tree lives three levels under
it - and a target under `<config>/skills/` is refused too, because `install` would
then be asked to symlink a directory to itself. A path that exists and is *not a
readable directory* is a named refusal rather than an errno from `create_dir_all`
three lines later, and so is a path that exists as a **symlink**: a dangling one
answered `NotFound` to `fs::metadata`, read as "absent, go ahead", and then failed
`File exists (os error 17)` - the exact errno these refusals exist to replace,
printed *after* the header had announced a repoint that never happened. A symlink
to a real empty directory was worse than a bad error, because it was accepted: the
tree landed in the link's target and nothing could ever prove which files under it
were ours to remove. Whether the tree or its nearest existing parent is
**writable** is checked in the preflight as well, so a failure lands before the
marker exists rather than half way through materialising.

**`--tree` is made absolute and lexically normalised once, before anything looks at
it.** Everything downstream compares that path, writes through it and *records* it,
and a raw argument defeated all three. `--tree skills/claude-tabstatus` run from
`<config>` walked straight past the refusal whose whole job is to keep the tree out
of `skills/`, because that test is a component-prefix test on the string; the
symlink then got the relative string as its target, which resolves against the
*link's* directory rather than the shell's, so the link dangled while the env key
was set and `install` said "Done."; and the record kept the relative string, so a
later `uninstall` run from somewhere else removed files from whatever happened to be
named that there. `fs::canonicalize` is the wrong tool here - it resolves symlinks,
and it fails on a path that does not exist yet, which the tree usually does not - so
`.` and `..` are folded textually. A `tree` field in the record that is not absolute
can only come from a hand edit and is ignored rather than resolved.

**The marker is written first, before the binary and before either manifest.** It
is the only evidence of ownership the refusal above accepts, so a run killed in
that window - ENOSPC during the 680 KB copy on a small VM, a dropped ssh, an OOM -
must not leave a populated directory with no marker in it: that shape was
classified as somebody else's and refused *forever*, and only `rm -rf` recovered
it. One write, before the files, makes every partial state re-enterable by
construction.

**Then the binary, and only then the manifests.** That order is the opposite of
what it was, and it flipped for a reason that only applies now that the tree is
live wiring. A refresh has no quiet moment, so one hook event somewhere may see a
half-updated pair - and of the two possible in-between states only one is
harmless. A **new binary with old manifests** paints every edge the old
`hooks.json` can name. An **old binary with new manifests** is the broken one: a
new edge word reaches `edge.rs`, which maps what it does not recognise to
`Edge::Unknown` and paints the *idle* glyph, so the tab goes quietly wrong rather
than loudly. On a first install into an empty directory the order is indifferent,
so binary-first is right in both cases.

Inside a tree it does own, `install` rewrites **every** generated file
unconditionally and prunes the ones an older version generated. The alternative -
"leave what is already there" - is the upgrade that silently does nothing: a
release adding a twelfth hook edge would install cleanly against an old
`hooks.json` and that edge would never fire. A file whose bytes changed is
overwritten *and named*, because a silent revert is the bug even where
overwriting is right:

```text
wrote:    hooks/hooks.json (2842 bytes, REPLACED, was 2851 bytes)
pruned:   hooks/extra.json (generated by an older version)
```

A re-install whose binary is **byte-identical** to the running one skips the copy
and says `unchanged - identical bytes`, so a no-op install genuinely does not
disturb live wiring rather than renaming a fresh inode over a file thirteen hooks
are executing for no reason at all. Before any copy becomes that file, `install`
**execs it** and refuses if it does not answer with the expected version - which
turns a `noexec` mount and a lost exec bit into one refusal at install time
instead of thirteen hooks failing silently in every later session. It does *not*
cover a wrong architecture and does not claim to: the copy is
`std::env::current_exe`, so it is by construction the same architecture as the
process running the check.

Two concurrent installs are benign by construction rather than by locking: every
write is a same-directory temp file with a pid-suffixed name plus `rename(2)`, so
no file is ever torn, no temp name collides and no path is ever missing. Two runs
of the same version are a no-op; two *different* versions can leave a mixed tree,
which one more install repairs. A lockfile would be disproportionate for a
one-user tool.

`doctor` says where the plugin directory is and how it worked that out, because
the repo is no longer the answer and there are three places it can come from:

```text
tree:      /home/me/.local/share/claude-tabstatus (from the plugin symlink)
           generated by tabstatus 0.1.0 (x86_64-unknown-linux-musl)
binary:    OK   /home/me/.local/share/claude-tabstatus/bin/tabstatus
           WARN the live tree is running a different build of tabstatus than this one
                (same 680496 bytes, different content)
           deploy this build: tabstatus install
source:    OK   /home/me/code/claude-tabstatus - both manifests match the copies compiled in
embedded:  OK   .claude-plugin/plugin.json matches the copy compiled in
embedded:  WARN hooks/hooks.json differs from the copy compiled in (2842 vs 2851 bytes)
           the tree is stale: `tabstatus install` rewrites it
```

The `binary:` comparison is **unconditional**, which it was not: it used to be
reachable only for a remotely installed tree, so the commonest real case of all -
`./bin/tabstatus doctor` in the checkout after a rebuild, asking whether the
running build has actually been deployed - never reached it. `source:` is the
other half of that question and the only part of the old two-mode report that was
ever actionable: do the manifests in the checkout this binary came out of still
match the copies compiled **into** it? A mismatch means somebody edited
`hooks.json` and has not rebuilt, so an install from there would deploy the older
copy. `WARN`, never `FAIL` - a mid-edit working tree is a normal state and the
command people run daily must not cry wolf over it. `install` prints the same
warning at the moment those bytes become live wiring.

Two byte counts are the detail that makes a drift line actionable without a diff -
except when they are the same number, which is the commonest case of all: 0.1.0
and 0.2.0 are the same length, so a plain version bump makes `plugin.json` differ
at identical size. That reads as `same 438 bytes, different content` rather than
`(438 vs 438 bytes)`, which would look like a bug in the report rather than the
answer. The install path says it the same way: `REPLACED - same 438 bytes,
different content`.

`doctor` and `uninstall` find the tree from **`<config>/skills/claude-tabstatus`**,
the symlink `install` itself wrote, then from the state record's `tree` field, then
from the default path. Walking up from the running executable is deliberately
**not** one of the answers: `./bin/tabstatus doctor` in a checkout would find
`.claude-plugin/plugin.json` above itself and report that the plugin is this
checkout - the exact wiring being abolished, restated by the tool as fact. The
binary you *run* and the directory Claude Code *loads* are two different things
now, and only the config directory knows the second one. The link is followed even
when it dangles, and the record covers what the link cannot: a link repointed or
removed by hand would otherwise leave the tree an orphan that nothing can name.
`doctor` names an orphan when it finds one.

**The binary is `x86_64-unknown-linux-musl`.** On an aarch64 machine it fails at
`exec` with the kernel's own *Exec format error* before a line of this program
runs, so nothing in it can improve that message. The fix is an
`aarch64-unknown-linux-musl` entry in `scripts/build.sh`'s `TARGETS`; until then,
check `uname -m` on the VM first. `doctor` also compares the marker's target
triple against the running one, which covers a tree materialised by one
architecture and later run by another.

Three layers keep the embedded copies honest, and the worst outcome - a stale
embedded `hooks.json` silently disagreeing with the repo - is caught by all
three:

| layer | catches | where it fires |
|---|---|---|
| rustc's rebuild dependency | any build from an edited manifest | `cargo build` recompiles; both JSON files appear in `target/<triple>/release/tabstatus.d` |
| `bin/sources.sha256` | a **prebuilt** binary gone stale against an edited manifest, with nobody rebuilding | `sh tests/run.sh`, which recomputes the manifest |
| `tabstatus print-embedded <plugin\|hooks>` | the bytes themselves, in either direction | `sh tests/run.sh` diffs it against the file; `doctor` reports it on a machine with no source tree |

`print-embedded` writes the embedded bytes to stdout verbatim and nothing else -
no trailing newline of its own - so `tabstatus print-embedded hooks | diff -
hooks/hooks.json` is empty exactly when the two agree. It needs no hasher or
additional dependency.

### install and uninstall are ordered, both ways

`install` writes the **tree** first, then `settings.json`, then the plugin symlink
**last**; `uninstall` is the mirror - settings first, the link next, the tree last.

The reason for the last two is the half-state between them: the env key switches
Claude Code's own title painting off and the plugin paints the replacement, so
"key set, plugin gone" is the one combination that paints **no tab title at all**.
The tree goes before both because it is **inert until the symlink points at it**,
so an abort anywhere before that last step leaves a first-time user exactly as
they were. And the symlink swap is the only irreversible step - it is the one write
that changes what code a running session executes - so it goes last *and* after
the copy in the tree has been exec'd, which is what proves the new target works.

Both halves preflight every refusal - the tree's ownership and writability, the
`skills` directory and its writability, `settings.json`'s shape, mode and parent, a
`settings.json` symlink that does not resolve, and whether there is a state record
proving the key is ours - so nothing between the writes can decide to stop, and
every refusal still honestly ends **"Nothing has been changed."** A
`settings.json` with duplicate members at the top level or inside `env` is refused
too: this tool resolves first-wins and `JSON.parse` resolves last-wins, so editing
it could set a key Claude Code never reads.

What is preflighted cannot fail between the writes; what is left is a full disk or
a tampered tree, and both land at the **first** write - directly under a header that
may have just announced, in three loud lines, that the live plugin link "will ->"
somewhere new. The bare OS error alone leaves the only question that matters
unanswered, so the failure answers it:

```text
error: /home/me/.local/share/claude-tabstatus/hooks is not a directory, so
hooks/hooks.json is not provably inside the plugin tree ... Refusing.

The plugin symlink /home/me/.claude/skills/claude-tabstatus was NOT touched, and
neither were settings.json or the state record at
/home/me/.claude/claude-tabstatus.state - so live sessions still paint through
whatever the header above printed as `now`, and nothing has become live wiring.
Part of /home/me/.local/share/claude-tabstatus may have been written. It carries
.tabstatus-generated, so a re-run resumes into it rather than refusing.
```

## Uninstall

```sh
~/code/claude-tabstatus/bin/tabstatus uninstall
~/code/claude-tabstatus/bin/tabstatus uninstall --force            # no state record: remove anyway
~/code/claude-tabstatus/bin/tabstatus uninstall --restore-backup   # roll settings.json back wholesale
~/code/claude-tabstatus/bin/tabstatus uninstall --keep-tree        # leave the plugin tree on disk
```

**It removes the plugin tree by default**, because nothing else owns it: leaving it
behind leaves a whole plugin directory in `~/.local/share` that nothing will ever
mention again. `--keep-tree` opts out and names the `rm -rf` that finishes the job.

It is conservative, and the two rules are what make removing it by default
defensible. Only a directory carrying `.tabstatus-generated` is touched at all - a
link still pointing at a **checkout** is named and left alone, because that is
somebody's source - and within a tree it does own, only the files the marker lists
plus the directories those leave empty. Anything else is **named and kept**:

```text
tree:     removed 3 generated files from /home/me/.local/share/claude-tabstatus, which was
          left in place because it holds 1 file nothing here generated: NOTES.txt
```

It removes the **live** tree, the one the symlink points at, and nothing else. An
`install --tree <somewhere else>` leaves the previous tree behind as an orphan, and
both commands name one whenever it is still discoverable - the default path, or the
`tree` field of the state record:

```text
tree:     /home/me/.local/share/claude-tabstatus is another generated tree and was NOT
          the live one, so it is left behind. Remove it with `rm -rf ...`.
```

`install` names it at the moment of the move too, which is where it is actionable. A
previous *custom* path is not discoverable afterwards and this does not pretend
otherwise: the record names the tree an install owns, not a history of them.

There is no `remove_dir_all` anywhere on a path this program derived from a
symlink. Prune only ever removes what the marker lists, so a few refresh runs
teach you by behaviour that your own files are safe in that directory, and
`uninstall` keeps that promise rather than breaking it at the last moment. If you
want the whole directory gone including your own files, `rm -rf <the path it just
named>` is the honest instruction - and when the directory survives with nothing
this report can name in it, only empty subdirectories, it says exactly that rather
than claiming the tree "is now empty" over a directory still on disk.

**A marker-listed path is checked on disk, not just as a string.** The marker is
plain text in a directory anything able to write the tree can edit, so "we wrote it"
is not a bound on the blast radius. `safe_relative` rejects `..`, a leading `/` and
`.`, and a string made entirely of ordinary components still resolves through
whatever is on disk: with `<tree>/bin` replaced by a symlink, `bin/tabstatus` named
a file in *somebody else's* directory, and both operations here reached it -
`remove_file` unlinked it, and the atomic write, whose temp file is created in the
destination's parent, created and renamed inside it. Outside the tree, silently, and
reported as inside it. One helper is now the only way those paths are built: the
tree root and every directory component of the path must be a real directory, and a
component that is a link is refused by name. Missing components are created one at a
time with `create_dir`, never `create_dir_all`, which would accept an existing
symlink-to-directory as "already there" and reopen the hole from the write side. The
path's *ancestors* are deliberately not checked - `~/.local` is a symlink on any
machine with a dotfile manager - because what this defends is the boundary of the
tree, not the route to it.

```text
tree:     LEFT a file the marker listed - /home/me/.local/share/claude-tabstatus/bin is
          not a directory, so bin/tabstatus is not provably inside the plugin tree
          /home/me/.local/share/claude-tabstatus - following it would write to, or
          unlink, a file outside. Refusing.
```

Pruning also takes the directories it empties. An older version's
`old/legacy.json` left an empty `old/` that no later marker lists, so `remove` never
took it and the tree could never come down.

The uninstaller is an undo, not a delete: it puts back whatever
`claude-tabstatus.state` says was there before. If you had already set
`CLAUDE_CODE_DISABLE_TERMINAL_TITLE` yourself, your value comes back, byte for
byte - the state file records the value's original *text*. If that record is
missing and the key is present, the key is left alone unless you pass `--force`,
because there is then no way to tell it apart from your own setting. The one
recorded thing it declines to restore is a prior symlink target that is a
**checkout**; see [Migrating from a checkout symlink](#migrating-from-a-checkout-symlink).

Being right about that key can still leave you with a blank tab, and it says so.
If the record shows the key was already set to something Claude Code reads as "do
not paint the title" *before* `install` ran, `uninstall` correctly keeps your value -
and it unlinks the plugin that painted the replacement in the same run. That is the
"a tab nothing paints" state the install path spells out in full when a late write
fails, reached here by being scrupulous rather than by failing, and it used to be
reported as a neutral `unchanged`:

```text
settings: env.CLAUDE_CODE_DISABLE_TERMINAL_TITLE already holds the value install found - unchanged
          NOTE that value switches Claude Code's OWN title painting off, and the
          plugin that painted the replacement is unlinked below - so nothing will
          paint the tab. It was already set when install ran, so claude-tabstatus keeps
          it; unset it yourself in /home/me/.claude/settings.json if that was not deliberate.
```

It also removes the wait-ownership records - `records:  removed
/run/user/1000/claude-tabstatus (2 record(s))` - which is the only other thing the
running plugin leaves on disk. Taking a still-live session's record is harmless: a
session with no record paints exactly what it painted before this plugin existed, and
by that point the plugin is unlinked so no hook will write another. It draws the same
line the reaper does, though: if `CCTAB_STATE_DIR` points at a directory holding
anything else, only the records go and it says so.

## What it changes

Four things, and nothing else:

1. A generated plugin tree at `$XDG_DATA_HOME/claude-tabstatus`, or
   `$HOME/.local/share/claude-tabstatus` - [build output](#the-plugin-directory-is-build-output),
   like `bin/`, written from the copies compiled into the binary.
2. One key in `~/.claude/settings.json`:
   `env.CLAUDE_CODE_DISABLE_TERMINAL_TITLE = "1"`.
3. A symlink `~/.claude/skills/claude-tabstatus` pointing at **that tree**, never
   at your checkout. A directory there containing `.claude-plugin/plugin.json`
   auto-loads; there is no marketplace entry and no `enabledPlugins` line. There
   is deliberately no `SKILL.md`, so the plugin costs essentially no model
   context.
4. `~/.claude/claude-tabstatus.state`, a small JSON record in two halves: what was
   there before (written once, never rewritten) and which tree this install owns
   (rewritten every install, because `--tree` moves it). `uninstall` removes it.

Plus one thing that is not configuration: the running plugin keeps a small
per-session record under `$XDG_RUNTIME_DIR/claude-tabstatus` (see
[wait ownership](#wait-ownership)). `uninstall` removes that directory too, and says
so; it is on a tmpfs the OS empties at logout in any case.

The first one is not optional. Claude Code repaints its own terminal title
roughly every 960ms, straight over ours, and a plugin cannot set environment
variables - so the switch has to live in `settings.json`.

That file is edited as a **text splice, not a reprint**: it is parsed only to
find out where the member goes, and then one line is inserted. Every other byte
survives literally - key order, indentation, blank lines, your own escapes - so
the diff of an install is exactly one line:

```diff
   "env": {
+    "CLAUDE_CODE_DISABLE_TERMINAL_TITLE": "1",
     "ANTHROPIC_API_KEY": "sk-...",
```

and the diff of an uninstall is nothing at all. (The jq-based shell installer
reprinted the document, which reindented hand-formatted files and needed a
warning to say so.)

Before that, it copies `settings.json` to `settings.json.cctab-preinstall`; after
it, the spliced text must parse *and*, with our one key removed from both
documents, compare structurally identical to the original - same keys, same
order, same values - or nothing is written. The file keeps its mode (a
`settings.json` locked to 0600 stays 0600, and a 0644 one stays 0644: the temp
file is created with the original's mode rather than the umask's), a
`settings.json` that is a symlink stays a symlink with its target updated, and a
read-only one, an unparseable one, or one whose top level is not an object is
refused rather than quietly overwritten.

`settings.json` is written first and the symlink last, so a failure while
editing settings cannot leave the plugin loaded with the built-in title still
repainting over it. The symlink handles the three shapes that can be in its way
distinctly: an existing symlink elsewhere is repointed and its old target
recorded, a *broken* symlink is reported as broken and replaced, and a real
directory is refused outright.

Disabling the built-in title also means Claude Code no longer clears the title
on exit, so this plugin owns the restore on `SessionEnd`.

## Konsole

Konsole ignores the title a shell sets, because its stock tab format is
`%d : %n` (directory and name) rather than the shell-supplied title - which is
why Claude's own title has never been visible in a Konsole tab. On
`SessionStart` this plugin sends Konsole an OSC 50 property change setting that
tab's title format to `%w`, so the title we send becomes the whole tab text,
and on `SessionEnd` it sets both formats back to Konsole's defaults. Konsole
applies profile properties per tab, at runtime, in memory, and never inherits
them into new tabs or writes them to disk, so **every other tab keeps
Konsole's default tab title** and nothing survives closing the tab.

OSC 50 means "set font" in xterm and is unrecognised in most other terminals,
so it is sent only when `KONSOLE_VERSION` or `KONSOLE_DBUS_SESSION` is in the
environment and the session is not inside tmux or screen - or when
`CCTAB_TERMINAL=konsole` says so explicitly, which is the only signal that
survives an ssh. Inside tmux the arming goes to the attached tmux client's pty
instead of to our own pane; see [tmux](#tmux).

**Windows Terminal needs no configuration**, and neither does any other
terminal that honours a plain OSC 0 title.

## tmux

Inside tmux the tab stops being a latch and becomes **a function of time**.

An OSC 0 written inside a tmux pane never reaches the outer terminal. tmux
stores it as that pane's `pane_title` and emits a title of its *own*, computed
from `set-titles-string`, and it recomputes that title on a timer. `%s` - the
epoch - is available inside a tmux format, so if the paint carries the moment it
happened, the format can render the live glyph while the paint is fresh, the idle
glyph once it is stale, and nothing at all once it is old, **unless background
work is known**. Background-aware blue/orange age to purple; purple never expires
or disappears through a TTL. **No process runs, no
hook fires and nothing is notified**; the only input that moved is tmux's own
clock.

All tmux paints write raw OSC directly to the pane's verified terminal through
`/proc/$CLAUDE_PID/fd/1`. Claude Code 2.1.274 wraps hook `terminalSequence` OSCs
in tmux passthrough, which bypasses `pane_title`; using that JSON delivery path
would leave the startup idle record unchanged. A missing or redirected terminal
is a silent no-op. The non-tmux JSON delivery path is unchanged.

That is worth much more than tmux convenience. The wrong titles in [Known
limitations](#known-limitations) are all the same shape - an edge that paints
with no matching un-paint. Inside tmux those transient carriers can age; known background remains visible.

So inside tmux the payload the plugin emits stops being a tab title and becomes a
**record**:

```text
<location> ct1 <state> <epoch>          state = w (working) | a (waiting) | i (idle)
~/code/one ct1 w 1790443548
<location> ct2 <state> <epoch>          state = p (background) | W / A (working / waiting with background)
~/code/one ct2 p 1790443550
```

SessionStart installs formats that understand both versions before publishing a
carrier. Old panes remain readable. Upgrade all installed copies and start a new
Claude session to refresh the server; `doctor` warns about incompatible formats.
Update the reference theme too if its label strips only `ct1`.

The glyph is not in it. `#{=1:}` counts *columns* and returns the empty string
for a width-2 emoji, so a glyph cannot be sliced back out of a title at all; the
glyph's position already varies with `CCTAB_GLYPH_POS`; and leaving it in would
paint the current pane's glyph twice, once in the strip and once in the label. A
state *letter* is one ASCII column, and the glyph it stands for is looked up in a
server option the same `SessionStart` wrote.

What the outer tab then shows is one cell per claude **pane** of the attached
session, then where you are:

```text
🔵🟠⚪ ~/code/one      three claudes: one working, one waiting for you, one idle
⚪🟠⚪ ~/code/one      the first one has gone quiet past its TTL
🟠⚪ ~/code/one        and then dropped out of the strip entirely
🔵🟠⚪ t:2:shell       looking at a plain shell, so tmux's own label
t:0:w0                 no claude anywhere on the server
```

A window's non-active panes count too - the loop is `#{W:#{P:…}}` and not
`#{W:…}`, because `#{pane_title}` inside a window loop reads only that window's
*active* pane, so two claudes split in one window would otherwise show one cell.
The session loop `#{S:…}` is deliberately absent: one terminal tab shows one
attached session, and looping every session would put another tab's claudes into
this tab's title.

The **tmux window list** also shows each window's pane indicators before its
existing label, for example `🟠⚪ 2:editor*`. Both the current and background
window formats are decorated, so a waiting Claude remains visible while you work
in another window. Only windows where Claude runs `SessionStart` are changed.
Window names and `automatic-rename` are untouched; custom labels, flags and styles
continue through the original formats. The outer terminal keeps the session-wide
aggregate shown above.

For a theme with rounded tabs or embedded colours, place
`#{T:@cctab_window_strip}` inside the styled body of both window formats. For example:

```tmux
set -g window-status-format "#[fg=white,bg=colour238] #{T:@cctab_window_strip} #I:#W "
set -g window-status-current-format "#[fg=black,bg=cyan,bold] #{T:@cctab_window_strip} #I:#W "
```

The plugin recognizes this explicit placement and does not prepend another
strip. If the window already has the plugin's local decorators, reload your
configuration and remove those two local overrides from inside that window:

```sh
tmux source-file ~/.tmux.conf
tmux set-option -wu -t "$TMUX_PANE" window-status-format
tmux set-option -wu -t "$TMUX_PANE" window-status-current-format
```

Uninstall preserves these custom formats; the strip reference becomes empty
when its shared option is removed, leaving the window label and styles intact.

An optional [reference tmux configuration](examples/tmux.conf) includes rounded
tabs with a **status-colored left cap**, a light active label, and
`repository@branch` labels for Claude panes. The left cap and two filled character cells replace the separate
circle, making a wider status band; the right cap and label still distinguish the selected window. Shell panes keep their usual window names. In split
windows the label follows the active pane, while the strip includes every Claude
pane. Copy the settings you want into `~/.tmux.conf`; the plugin does not install
this configuration. The colours are a provisional example, not a required theme.

The theme uses `#{T:@cctab_window_color}` in the left cap's foreground. This
format returns the highest-priority visible state across all Claude panes in
that window: orange `#fb923c` > blue `#60a5fa` > purple `#c084fc` > white
`#e5e7eb`. It shares the strip's carrier recognition and expiry rules, including
background's protection from expiry, and works even with customized or empty
glyphs. Empty means no visible Claude state; the theme supplies a neutral color.
A single cap summarizes the window, so individual pane states remain available
in the outer title's dot strip. Keep the color reference directly in both window
formats so the plugin recognizes the custom placement and adds no extra dots.
After uninstall, the color reference becomes empty and the neutral cap remains.

For an existing installation, update the plugin and restart Claude to install
the color format, then reload the theme. Remove old plugin-owned local decorators
as described above if they still override the new global theme.

This uses the same four-state precedence as the outer title. Known background
has no TTL; completion does not create an alert. [Issue #19](https://github.com/dalf/claude-tabstatus/issues/19)
is not explicitly placed in the [roadmap](https://github.com/dalf/claude-tabstatus/issues/16);
this implementation leaves the state-contract decisions in
[issue #10](https://github.com/dalf/claude-tabstatus/issues/10) unchanged.

### What is configured at runtime

`SessionStart` configures the outer title in one `tmux` batch, then reads and
decorates its window's two status formats. You need no `~/.tmux.conf` edit:

```text
set -s @cctab_gw/@cctab_ga/@cctab_gp/@cctab_gi     the four glyphs
set -s @cctab_tw/@cctab_ta/@cctab_tg     the three TTLs, in seconds
set -s @cctab_title                      the generated strip-and-label format
set -s @cctab_string                     the set-titles-string we installed
set -s @cctab_window_strip               the generated strip for one window
set -s @cctab_window_color               one status color for a themed window cap
set -g set-titles on
set -g set-titles-string '#{s|^ ||:#{T:@cctab_title}}'
set -w -t <window> window-status-format          a strip plus the saved normal format
set -w -t <window> window-status-current-format  a strip plus the saved current format
```

In Konsole mode only, two more - this binary's path, and the hook that re-arms a
reattached tab with it:

```text
set -s @cctab_exe                        this binary, for the hook to run
set-hook -t <our session> 'client-attached[1971]' \
    'run-shell -b "'\''#{@cctab_exe}'\''  tmux-arm '\''#{client_tty}'\''"'
```

`tabstatus tmux-format` prints the outer `set-titles-string` and generated title
format, if you would rather pin them in your own config than have them set at runtime.

The glyphs and the TTLs travel as *options* rather than as text spliced into the
format, because an option's value is substituted **literally**: measured,
`#{host}` and `%H` reach the tab intact through `#{@opt}`, where the same bytes
written into the format itself would have been expanded. That makes a hostile
`CCTAB_GLYPH_*` inert. A pane title is never re-expanded either, so a path
holding `#{host}`, `#(…)`, `}`, `,`, `|` or `:` arrives as itself - and `#(…)`
is not a command-execution vector: tmux defangs it to `_(` when it *stores* the
title. (`select-pane -T`, which this plugin never uses, expands its argument at
set time and must never carry a location.)

**The hot path execs nothing.** Only `SessionStart` runs `tmux`, and `SessionEnd`
only in Konsole mode. Before window-list support, measured here: one
`tmux set-option` cost 2.84ms against a 0.37ms fork floor, and the title-only
`SessionStart` batch cost 3.1ms end to end. Those are historical measurements,
not timings for the additional window-format queries and installation batch.
That extra work happens only at `SessionStart`; ordinary state paints still
write the pane carrier without invoking `tmux`.

### What you must have on

| setting | why |
|---|---|
| `status on` (tmux's default) | **painting** still works without it - measured, a paint reached the outer terminal in 4ms with `status off` - but the **decay** stops, and a stale glyph then survives until the next paint |
| `status-interval > 0` (default `15`) | this is the decay clock, and it is needed *independently*: measured, `status on` with `status-interval 0` never decays either |
| an outer terminal tmux grants the `title` feature | else tmux emits no OSC 0 at all. Informational rather than a gate on a current tmux: measured on 3.7c, `linux`, `vt220` and `vt100` all reported `title` and all got the OSC 0, although `terminal-features` lists only `xterm*` and `screen*`; only `TERM=dumb` failed, and it cannot attach at all. On an older tmux the remedy is `set -as terminal-features ",$TERM:title"` |

**When does the flip land?** `#{e|>|:age,ttl}` is strictly greater over *integer*
seconds and the record's epoch is truncated to the second, so the earliest
possible flip is **TTL + 1s**, plus up to one `status-interval` on top. Measured
with `status-interval 1` and `CCTAB_TTL_WORKING=5` / `CCTAB_TTL_GONE=12`: white at
+6.01s, gone at +13.02s. Irrelevant at the shipped defaults, and the reason short
TTLs look off by one. `#{e|>=|:}` does exist on 3.7c and would move the flip one
second earlier, which is not worth a change to a format string two tests pin.

`SessionStart` does **not** set any of them. Turning the status line on would run
a user's `#(…)` in `status-right` on our schedule, and guessing at somebody
else's terminal features is not ours to do. `tabstatus doctor` reports all three
with the one-line remedy:

```text
tmux:      OK   tmux 3.7c on /tmp/tmux-1000/default, pane %3
           decay: OK   status on, status-interval 15s - the tab re-renders on that timer
           title: OK   set-titles-string is the one SessionStart installed
           client: /dev/pts/3 xterm-256color HASTITLE
           konsole: off  set CCTAB_TERMINAL=konsole when the outer terminal is Konsole
           ttl: working 1200s, waiting 900s, gone 3600s (0 = never)
```

In Konsole mode it also reports the re-arm hook, and it warns when the strip on
the server is on the other end from the one this session would install:

```text
           arm: OK   client-attached re-arms this tab's Konsole format on every reattach
           layout: WARN the server has the strip last, but this session would install it first
```

### The TTLs

```sh
CCTAB_TTL_WORKING=1200   # blue decays to white after 20 min; 0 = never
CCTAB_TTL_WAITING=900    # orange decays to white after 15 min; 0 = never
CCTAB_TTL_GONE=3600      # any cell disappears after 1 h;      0 = never
```

`CCTAB_TTL_WORKING` is **measured**, not guessed. `working` is repainted by
`UserPromptSubmit`, `PostToolUse` and `PostToolUseFailure` only, so during one
long tool call nothing paints at all. Over the 91 most recent real transcripts on
this machine - 7709 within-turn gaps between two successive `working` paints:

| p50 | p95 | p99 | p99.9 | max | over 300s | over 1200s |
|---|---|---|---|---|---|---|
| 9.9s | 77s | 173s | 646s | 27751s | 39 (0.51%) | 4 (0.05%) |

`300` - the first default - whitened a genuinely working tab about once in every
200 gaps, which is the *damaging* direction: "finished, come back" about a build
that is still running. `1200` covers 99.95% of them, and the four above it are
3343s, 5677s, 23700s and 27751s - sessions resumed the next day rather than a tool
call still going. A lingering blue is the cheap lie: the next paint corrects it in
seconds, and the 3600s disappear horizon still catches a genuinely stuck one.

`working` is repainted by every tool call, so its TTL only has to outlast the
longest ordinary gap between two paints - one long tool call, one long thinking
phase - and decaying sooner would lie in the "it finished" direction. `waiting`
is a *summons*: it gets exactly one paint and is never refreshed, so it has to
survive a coffee break. `idle` needs no TTL of its own, because white is already
what the other two decay *into*. The disappear horizon is shared: once a cell has
whitened it says the same thing whatever it decayed from, so what it decayed from
is no reason for it to linger longer.

They are server-wide, so the last `SessionStart` on a server wins for every
window on it. So are `CCTAB_TERMINAL` and `CCTAB_GLYPH_POS`, which decide the
*layout*: a plain `SessionStart` after a `CCTAB_TERMINAL=konsole` one flips the
strip back to the elided end while the Konsole arming stays in force. Set them the
same for every claude on one tmux server - `doctor`'s `layout: WARN` line is there
because the `title: OK` test cannot catch it (the same `SessionStart` rewrites
`@cctab_string`, so those two always agree).

### Save and restore

For each decorated window, `SessionStart` saves the original normal and current
status formats independently, including whether each was explicitly local or
inherited. Repeated starts do not replace those backups. While installed, each
decorator uses the saved label format; changing a global format takes effect in
that window after the decorator is removed.

`tabstatus uninstall` restores tracked windows across every session on the tmux
server. An originally local format is restored exactly, including an empty
value; an originally inherited format is unset locally so inheritance resumes.
If you replace either decorator yourself, later starts and uninstall preserve
your replacement. Ordinary windows and global window formats are not changed.
If saved metadata is incomplete, or an edited format still uses the plugin's
saved values, uninstall reports that it could not finish restoration and retains
the shared tmux options instead of removing data the label still needs.
`SessionEnd` clears its pane's indicator; decorators remain until uninstall so
other Claude panes in the same window continue to work.

`SessionStart` copies your own `set-titles` and `set-titles-string` into
`@cctab_prev_titles` / `@cctab_prev_string` **before** it overwrites them, and
only if nothing is saved yet - so a second claude starting later cannot record
*our* string as yours. tmux serialises commands on one event loop, so the save
and the install are one atomic batch; measured, thirty concurrent `SessionStart`s
left the saved value intact.

A saved string that points at `@cctab_title` is **not** put back, and uninstall
says so instead of claiming a restore. That is not hypothetical: pinning the pair
from `tabstatus tmux-format` into your own `~/.tmux.conf` - which this README
invites two sections up - means the first `SessionStart` saves *our* string as
yours, and restoring it after `@cctab_title` has been unset renders the empty
string, leaving the tab title permanently blank until the server restarts. The
test is the whole `@cctab_` namespace, because a server whose last `SessionStart`
used the other glyph position holds a different string of ours pointing at the
same option.

`tabstatus uninstall` is the **only** thing that puts them back, and it says what
it restored. It also counts the other claude panes in the session and warns that
their tab cells stop updating, since the options it removes are server-wide. `SessionEnd` deliberately does not: the options are server-wide, and
another claude window may still be painting through them. If the saved value was
tmux's own compiled-in default, the option is left *unset* rather than pinned to
a string a later tmux may change. Run from outside tmux, uninstall says it could
not restore rather than claiming it did. A tmux server restart loses the
installed format and the saved values together, which is self-consistent:
nothing to restore, nothing left behind.

### Konsole over ssh

`KONSOLE_*` does not survive an ssh, so in the topology this exists for - Konsole
→ ssh → tmux → claude - there is nothing to detect. `CCTAB_TERMINAL=konsole`
says it explicitly, and then the OSC 50 arming goes to **each attached client's
pty**, named by `tmux list-clients -F '#{client_tty}'`, never to our own pane
(where tmux would swallow it). The strip also moves to the end Konsole does not
elide.

That route was chosen over `allow-passthrough`, which does work - `on` passes the
active pane, `all` also passes background windows, every inner ESC has to be
doubled - but turning it on lets any program in any pane write arbitrary bytes to
your terminal, which is not a decision a tab-title plugin should be taking for
you. `CCTAB_TERMINAL` also works outside tmux, which is the same fix for a plain
ssh out of a Konsole tab.

**A reattach is re-armed.** The arming goes to the ptys `list-clients` names, so
an arming sent while *detached* reaches nobody, and on reattach tmux replays the
title but never the arming - measured, a reattached tab was governed by
`RemoteTabTitleFormat=(%u) %H` again and the glyph was invisible. "Close the
laptop while Claude keeps working" is exactly this topology, so in Konsole mode
`SessionStart` also installs a `client-attached[1971]` hook that runs
`tabstatus tmux-arm` with the attaching client's pty. Measured on a reattach: the
new terminal receives `CSI 22;0;0t`, the current title, and the OSC 50 arming 1ms
later. The hook is set on *our session*, not globally, so a tab whose tmux session
holds no claude is never armed; it is removed with the restore at the last
`SessionEnd`, and by `uninstall`.

The path travels in `@cctab_exe` rather than inside the hook's text, because a
hook value is parsed when it is *set*: measured, `$rd` inside tmux's double quotes
is expanded there, so a path holding `$` would lose a piece of itself. An option's
value is substituted literally instead - measured, a path holding `#`, `#{host}`,
`%Y` and a space arrived byte for byte. A path holding a single quote has no
representation inside the hook's shell quoting and drops the re-arm, keeping
everything else.

The restore is sent only when no *other* claude pane is left in the session,
because the arming is per tab and inside tmux one tab holds every window. Our own
pane is excluded from that count rather than relied on to have been cleared
already.

### Anything else that writes a title

A shell prompt, `vim` or `ssh` setting the pane title overwrites the record, and
that pane's cell disappears until the next paint. Conversely a pane title that
*ends in* a well-formed record forges a cell - harmless, and the price of using
the title as the carrier instead of a tmux option, which would have cost 2.84ms
on every tool call.

**If your tab shows `~/code/one ct1 w 1790…`,** the carrier has become visible:
something replaced our `set-titles-string` while `set-titles` stayed on, and
tmux's own default string contains `#T`. A `tmux source-file ~/.tmux.conf` after
`SessionStart` does it, so does `uninstall` while another claude is still
painting, and so does a claude that was SIGKILLed on a server whose config sets
`set-titles on`. Nothing re-arms in band - `doctor` says `title: WARN
set-titles-string is not ours any more` - so start a new claude session, or
`/clear`, either of which re-runs `SessionStart`.

### Deliberately not built

- **OSC 9;4 progress** is not implemented.
- **A cached session title.** The record reserves the `n` key for it, last in the
  file so that its free-form text arrives whole. Same seam, same status: designed,
  not built. It is the one future field that would put a record read on the
  *painting* path rather than only on the edges listed above, because the title goes
  into the tab text - budget it at the ~+13us a read measures here.
- **A per-session policy.** The options are server-wide; per-session glyphs or
  TTLs would need the deadlines carried in the record instead.
- **Re-arming a server that lost our OPTIONS** some other way than a restart
  (which takes its panes with it). Nothing re-runs `SessionStart`, and putting a
  `tmux` invocation on the hot path to check is exactly the cost this design
  refuses. The `client-attached` hook is now installed in Konsole mode, for the
  Konsole *arming* only; re-checking the options from it is the remaining seam,
  and `doctor` is the manual answer.
- **screen.** `$STY` gets nothing: screen's title machinery has no arithmetic
  and no strftime in its format, so there is no decay to buy. When both `$TMUX`
  and `$STY` are set, tmux wins - tmux-inside-screen is the plausible order, and
  then the record is right.
- **Nested tmux.** The inner server owns `$TMUX`, so that is the one configured,
  which is correct. With our own string installed on the OUTER server too, the
  concrete answer is measured and worse than "it depends": the inner tmux re-emits
  its own *rendered* title into the outer pane's `pane_title`, which no longer
  matches the record pattern, so the outer tab loses that window's cell entirely
  and its label falls back to `session:index:window`. The inner tmux's own tab - if
  it has one - is the one that paints. `doctor` cannot see an outer server from
  inside and does not claim to.

`CCTAB_NO_TMUX=1` backs the whole of this out of the way: no record, no `tmux`
invocation, no arming.

## Known limitations

- **Ctrl+C emits no hook at all, and neither does walking away from a
  permission dialog.** These are the two ways a turn can end with nothing to
  paint, and they are the wrong titles this design can produce: the tab keeps
  reading `working` (after an interrupt) or `waiting` (after a dialog you never
  answer) with nothing actually happening.

  It is not for want of an event. `PostToolUseFailure` exists and carries an
  `is_interrupt` flag, but the hook is handed the turn's own abort signal and
  bails before spawning anything, so an interrupt - the thing that aborts that
  signal - skips it by construction. Measured end to end: a pre-approved
  `ping -c 40` interrupted mid-run produced no `PostToolUseFailure`, no
  `PostToolUse` and no `Stop`. Interrupting a *thinking* turn is the same, and
  so is pressing Esc at a permission dialog - measured twice, no hook of any
  kind fires. `PostToolUseFailure` is still registered, because it does fire for
  a tool that *reports* an error, after which the turn continues - so it paints
  `working`, not idle.

  (Declining with "No, and tell Claude what to do differently" and then actually
  submitting the feedback is a different path, and this README does not claim it
  either way: the probe that tried it selected the option and never submitted,
  so what it measured was the feedback box - itself a wait, correctly orange.
  Reading the code, a denial is not an abort, so it should reach
  `PostToolUseFailure` and then a normal `Stop`, which would need no recovery at
  all. Unverified.)

  **The interrupt heals itself in about a minute. The abandoned dialog does
  not.** When a turn ends, aborted or not, the product schedules its
  `idle_prompt` notification, and this plugin maps that to idle - so a tab left
  blue by Ctrl+C goes white about 60s later (`messageIdleNotifThresholdMs`,
  configurable) if you leave the keyboard alone, and immediately if you just type
  your next prompt. But that notifier checks, when its timer fires, whether you
  have touched the keyboard since the turn ended, drops the notification if you
  have, and never re-arms - and that is precisely what breaks the other case.
  Dismissing a dialog *is* touching the keyboard, so nothing repaints: the tab
  stays **orange until the next `UserPromptSubmit`, which may be never.**
  Measured: Esc at a `Write` dialog, then
  110s of absolute quiet - 1.8x the 60s threshold - produced no hook and no
  repaint, and the tab was still orange when the session ended 113s later. So the stuck colour can be orange, which is
  the damaging direction: a tab claiming it needs you when nothing does. Wait
  ownership does not fix this one, and cannot: nothing fires, so nothing repaints.
  What it does is stop the stale *record* from outliving the stale colour, so the
  next edge that does fire is not also suppressed by it - and there are now four ways
  out rather than one: the next prompt you type, the next `Stop` whose
  `background_tasks` is empty, that agent's own `SubagentStop`, and the wait's own
  `CCTAB_TTL_WAITING` expiry.
- **Some dialogs are invisible to every hook.** A dialog that is neither a tool
  call nor a notification - the LSP recommendation, the plugin hint, the
  auto-mode-default upsell - fires no `PermissionRequest`, no matched
  `PreToolUse` and no `Notification`, so the tab keeps whatever it last showed,
  usually idle white, while a modal waits for you. The product's own 60s nudge
  cannot correct it either: that notifier is gated on no dialog being on screen.
  A white tab over one of those modals is the boundary of what hooks can see,
  not a bug.
- **MCP requests without IDs cannot be matched to responses.** Direct hooks
  paint waiting immediately, but an anonymous result clears nothing. Recovery
  and the ambiguity of identity-free notifications are described under
  [Direct MCP elicitation](#direct-mcp-elicitation).
- **Konsole repaints the tab on a ~2s tick**, not when the title arrives, so
  the dot trails the actual state change by up to about two seconds. That, not
  the ~2ms hook, is the responsiveness ceiling.
- **A session killed ungracefully leaves the tab armed.** `SessionEnd` runs on
  a clean shutdown, on `/clear` and on `/resume`, but not after `kill -9`, an
  OOM kill or a crash: that Konsole tab keeps `LocalTabTitleFormat=%w` and its
  last title until the tab is closed. To fix one by hand, from a shell in that
  tab:

  ```sh
  CLAUDE_PID=$$ ~/code/claude-tabstatus/bin/tabstatus session-end
  ```

- **The Konsole restore puts back Konsole's stock formats**, `%d : %n` and
  `(%u) %H`, not whatever a customized profile had. If your profile sets its
  own tab title format, a Claude session in that tab replaces it for the life
  of the tab.
- **`KONSOLE_*` is only inherited environment.** An xterm or alacritty launched
  from a Konsole shell still carries it, and so does every pane of a tmux
  server that was first started under Konsole. `$TMUX` and `$STY` take the
  multiplexer case out; the launched-from-Konsole case would still misfire, and
  in xterm OSC 50 sets the font rather than being ignored.

  `CCTAB_TERMINAL=konsole` is the explicit answer, and `CCTAB_TERMINAL=<anything
  else>` is how a leaked `KONSOLE_*` is turned off. It is also the only way to
  know, over ssh or inside tmux, that the tab at the far end is Konsole's.
- **`session-start`, `session-end`, and all tmux paints are Linux-only.** They resolve the pty
  through `/proc/$CLAUDE_PID/fd/1`, which macOS and Git Bash do not have, so on
  those platforms Konsole arming does not happen (fine, they are not Konsole)
  and, more importantly, the title is not cleared at the end of a session while
  `CLAUDE_CODE_DISABLE_TERMINAL_TITLE` is set. Tmux pane records cannot be updated
  there through this direct-write path either.
- **`claude -p` typed straight at a terminal is retitled too.** Its stdout
  really is that tab's pty, so a one-shot run arms the tab, retitles it and
  restores it at `SessionEnd`. Only the redirected or piped form
  (`claude -p ... | jq`, or a call from a script) is detected as headless and
  skipped. A `-p` run killed before `SessionEnd` leaves the tab armed, as
  above.
- Konsole's tab bar elides from the left, so a very narrow tab could in
  principle clip the leading dot. Measured budget is ~49-60 columns. A local
  title is ~27-37 columns; over ssh the host prefix adds its own width, which is
  why the host carries a cap of its own (`CCTAB_MAX_HOST`, default 16) - without
  one, a 63-character single-label cloud hostname rendered a 91-column title and
  Windows Terminal, which truncates from the right, showed the host and nothing
  else. Both caps count **characters, not columns**: one character is one unit
  whatever it draws as, so a CJK or emoji location fills about twice the tab width
  the count implies, and a multi-column `CCTAB_ELLIPSIS` overshoots by its extra
  width. Counting columns needs a width table this binary deliberately does not
  carry.
- **A `.git` that is not a working repository is not one here either.** An
  empty `.git` directory, a `gitdir:` pointer to somewhere that no longer
  exists, or a `HEAD` that does not parse all fall through to the path form,
  and the walk continues upward - the same thing git does. `/tmp/.git` exists
  on more machines than you would expect, and without this every path under
  `/tmp` would claim to be a repo called `tmp`.
- **The branch is read from `HEAD`, not resolved.** That is the branch you are
  on, which is what a tab should say, but it means a location can be a branch
  that has no commits yet, and `@` in a branch name is not escaped. A `HEAD`
  pointing outside `refs/heads/` keeps its namespace minus the `refs/` prefix,
  so a bisect reads `bisect/bad` and a detached checkout reads a 7-character
  short sha. A first line longer than 255 bytes is not treated as a `HEAD` at
  all - no real one is, and the parse is not free on a huge string - so the walk
  continues past it and the tab shows the path.
- **An exported `GIT_DIR` wins over the walk**, exactly as it does for git, so
  every tab of a shell that exports one (a habit for bare dotfiles repos) reads
  that repository regardless of the working directory. Unset it per session if
  that is not what you want.
- **The repository is found on the physical path.** A working directory reached
  through a symlink is resolved with `cd -P .` before the walk - fork-free - so
  the tab reports the same repository and branch `git` does, and names it after
  the real toplevel rather than after the symlink. The `~` abbreviation still
  uses the logical path, so a distro whose `/home` is a symlink keeps its `~`.
- **The ssh hostname comes from `/proc/sys/kernel/hostname`**, which keeps it
  fork-free on Linux. Elsewhere it falls back to `$HOSTNAME` and then to a
  `hostname` fork; set `CCTAB_HOST` to skip the guessing. If none of the three
  answers, the prefix becomes a literal `ssh:` rather than nothing, because no
  prefix means "local".

## Glyph position

Konsole's tab bar elides the label from the **left**, so a leading glyph is the
first thing cut - a 23-cell title in a 19-cell tab renders `…de-tabstatus@main`
with the dot gone. Windows Terminal truncates from the **right**. So the glyph
goes on whichever end that terminal preserves:

| Terminal | Position | Crushed to 19 cells |
|---|---|---|
| Konsole (detected automatically) | last | `…tatus@main ⚪` |
| Windows Terminal, and anything unrecognised | first | `⚪ claude-tabst…` |

Detection uses `KONSOLE_VERSION` / `KONSOLE_DBUS_SESSION`, and is deliberately
suppressed inside `tmux` or `screen`, where those variables leak in from
whichever terminal first started the server and say nothing about the one
drawing the tab.

**Over ssh the local terminal cannot be detected** - its variables do not
travel - so a remote session defaults to `prefix`. If you ssh *from* Konsole,
set the override in the remote shell:

```sh
CCTAB_GLYPH_POS=suffix   # last; what Konsole is given automatically
CCTAB_GLYPH_POS=prefix   # first; the default when the terminal is unknown
CCTAB_GLYPH_POS=both     # both ends, immune to either, costs two columns

Inside tmux it picks which end of the tab the glyph STRIP goes on, and `both` is
not doubled there: a strip is not a marker, and six cells at each end is the
whole tab.
```

An unrecognised value falls back to `prefix`.

Konsole's elide direction is not configurable: it is
`QTabBar::setElideMode(Qt::ElideLeft)` at one hardcoded call site, with no
config key and nothing a Qt stylesheet can override. Widening the tabs
(*Settings -> Configure Konsole -> Tab Bar*, or `setTabWidthToText false` over
D-Bus) buys room but is undone by opening more tabs.

## Glyphs

Override any of them - for a terminal with no emoji font, or just to taste:

```sh
# in ~/.claude/settings.json under "env", or exported before launching claude
CCTAB_GLYPH_WORKING=">"
CCTAB_GLYPH_WAITING="?"
CCTAB_GLYPH_IDLE="."
```

Setting one to the empty string drops the glyph and its separating space.

## Location tuning

```sh
CCTAB_MAX_LOCATION=32   # characters before the location is elided; 0 = no limit
CCTAB_MAX_HOST=16       # characters for the ssh host prefix; 0 = no limit
CCTAB_ELLIPSIS="…"      # the elision marker; "..." for an ASCII-only terminal
CCTAB_HOST="srv"        # the ssh prefix, instead of this machine's hostname
```

Both caps count **characters**, in every locale, and cut every location the same
way: `~/étéétéétéétété` elides exactly like an ASCII path of the same length. The
unit used to be a byte or a character depending on the shell and `$LANG`, which is
why the cap used to be skipped for any location holding a byte outside printable
ASCII - not only a non-ASCII one, but also one carrying an ASCII control character
or DEL, since the old guard was a single `*[!\ -~]*` test.

A character is **not** a column. A CJK or emoji location is now cut, but to 32
*characters*, which can be up to 64 display columns, so a wide-script tab still
elides in the terminal on top of being cut here. Counting East-Asian wide and
fullwidth code points as two would fix that and is not done: it needs a width
table this binary deliberately does not carry.

`CCTAB_MAX_LOCATION` bounds **the location only**, not the whole title: the
rendered title is that plus 3 columns for the glyph and its space, plus
`host:` on an ssh session. So sizing it to a tab width undershoots by 3 columns
locally and by the host width again over ssh - size the two caps together. It is
clamped up to 8, and a non-numeric value falls back to the default.

`CCTAB_MAX_HOST` is clamped up to 4 and behaves the same way. `CCTAB_HOST` loses
everything from the first dot, so `srv.example.com` still renders as `srv:`,
unless the name is all digits and dots, where `192.168.1.5` would otherwise
become `192:`. It is still only used when `SSH_CONNECTION` or `SSH_TTY` says this
is an ssh session, so exporting it globally is safe.


## Environment

Everything the runtime half reads, in one place:

| variable | default | what it does |
|---|---|---|
| `CCTAB_GLYPH_WORKING` / `_WAITING` / `_BACKGROUND` / `_IDLE` | 🔵 / 🟠 / 🟣 / ⚪ | the four glyphs; empty drops the glyph and its space |
| `CCTAB_GLYPH_POS` | terminal-dependent | `prefix`, `suffix` or `both`; inside tmux, which end the strip goes on |
| `CCTAB_MAX_LOCATION` | `32` | characters before the location elides; `0` = no limit |
| `CCTAB_MAX_HOST` | `16` | characters for the ssh host prefix; `0` = no limit |
| `CCTAB_ELLIPSIS` | `…` | the elision marker |
| `CCTAB_HOST` | `/proc`'s hostname | the ssh prefix, instead of this machine's name |
| `CCTAB_TERMINAL` | unset | `konsole` (matched case-insensitively) arms Konsole's per-tab format even over ssh or inside tmux, and moves the strip to the end Konsole does not elide; any other value says explicitly NOT Konsole. **The one knob here that changes what paints outside tmux as well as in.** |
| `CCTAB_TTL_WORKING` | `1200` | seconds before 🔵 decays to ⚪ in a tmux tab; `0` = never |
| `CCTAB_TTL_WAITING` | `900` | seconds before 🟠 decays to ⚪ in a tmux tab, **and** before an outstanding wait in the state record expires; `0` = never |
| `CCTAB_TTL_GONE` | `3600` | seconds before a cell leaves the tmux strip; `0` = never |
| `CCTAB_NO_TMUX` | unset | set to anything: no record, no `tmux` invocation, no arming |
| `CCTAB_DRY_RUN` | unset | `1` prints the computed tab title and emits nothing |
| `CCTAB_STATE_DIR` | `$XDG_RUNTIME_DIR/claude-tabstatus` | where the per-session wait record lives. Unset **and** no `XDG_RUNTIME_DIR` means no record at all, and every edge falls back to the stateless answer. **Use a dedicated directory:** `session-start` reaps in it. It deletes only files it can prove are its own records ([wait ownership](#wait-ownership)), but it is still the wrong place to keep anything else |
| `CCTAB_NOW` | unset | test only: pins the epoch the tmux record and the state record carry |

`XDG_DATA_HOME` is read by `install` alone, for where the [generated plugin
tree](#the-plugin-directory-is-build-output) goes - `$XDG_DATA_HOME/claude-tabstatus`,
falling back to `$HOME/.local/share/claude-tabstatus`, and `install --tree <dir>`
overrides both. The runtime half never looks at it. Anything that exercises
`install` or `uninstall` must redirect it along with `HOME`, `CLAUDE_CONFIG_DIR` and
`CCTAB_STATE_DIR`: it is the one that decides where a real 680 KB tree lands.

`CLAUDE_PID` is exported into every hook subprocess and is how `session-start`,
`session-end`, and tmux state paints find the pty. `XDG_RUNTIME_DIR` is read for the state
directory - deliberately with no `$HOME` fallback, because that would put a record
inside the golden corpus's fixture `HOME` and make every case carrying a
`session_id` order-dependent. That it reaches a *hook* subprocess at all is
measured, not assumed: a temporary probe build logged what a real `PostToolUse`
hook sees, and it was `xdg=Some("/run/user/1000")`. `TMUX`, `TMUX_PANE`, `STY`,
`KONSOLE_VERSION`,
`KONSOLE_DBUS_SESSION`, `SSH_CONNECTION`, `SSH_TTY`, `HOME`, `PWD`, `HOSTNAME`
and `GIT_DIR` are read as they are.

## Build

The default target is **musl, not gnu.** A static-pie binary starts in 243us
against the gnu build's 556us - below even `/bin/true`'s 312us, because it never
enters `ld.so` - and it carries no `GLIBC_2.34` requirement, which matters when
the binary's main home is a remote box reached over ssh whose glibc you do not
control.

`bin/` is **build output and is gitignored.** It holds a binary per platform plus
`bin/tabstatus`, a relative symlink to the one for this machine - that is the
binary you **run**, and `./bin/tabstatus install` is the documented command. It is
no longer the binary *hooks* run: `install` copies it into the
[generated plugin tree](#the-plugin-directory-is-build-output), and
`hooks/hooks.json` invokes the copy there. A fresh clone has no `bin/` until you
build.

`hooks/hooks.json` and `.claude-plugin/plugin.json` are **source, not deployed
files**. They are compiled into the binary with `include_str!`, so editing one is a
rustc rebuild input - and, exactly like editing `src/main.rs`, it reaches a running
session only after a rebuild and an `install`. That is the whole point: stale source
is stale source, and there is no reason the JSONs should be special.

Binaries ship as **GitHub release assets** rather than in git history, so that
installing needs no toolchain. The [Test workflow](.github/workflows/test.yml)
builds both Linux targets and runs Rust unit tests, the shell integration suite
(including tmux), the semantic state traces and persistence checks, and the golden
corpus on every branch push and pull request.
The tested binaries are also available as workflow artifacts.

Pushing a Git tag runs the [Release workflow](.github/workflows/release.yml).
It runs the same checks on the tagged commit, then creates a GitHub release with
`tabstatus-x86_64-unknown-linux-musl`, `tabstatus-x86_64-unknown-linux-gnu`, and
`SHA256SUMS` attached. For example, after updating the package and plugin versions
and committing the changes, push `v0.1.0` with `git tag v0.1.0` followed by
`git push origin v0.1.0`. All tag names trigger a release; use a new tag for each
release. macOS support is tracked in [issue #1](https://github.com/dalf/claude-tabstatus/issues/1).

After downloading a binary and `SHA256SUMS` from the same release, verify it with
`sha256sum --check --ignore-missing SHA256SUMS` and make it executable with
`chmod +x tabstatus-x86_64-unknown-linux-musl` (adjust the name for the GNU build).

```sh
sh scripts/build.sh          # the host target, refresh bin/ and its digests
sh scripts/build.sh --all    # every target in the list
```

**Serde and serde_json parse hook metadata**, without enabling `serde_derive`.
`Cargo.lock` pins their dependency graph and release builds use `--locked`.
Application code remains optimized for size (`opt-level = "s"`); the measured
parser dependencies (`serde`, `serde_core`, `serde_json`, `memchr`) use
`opt-level = 3` for speed. `rust-version` is **1.89** - raised from 1.74
for `std::fs::File::lock`, which is what makes the state layer's read-modify-write
atomic without a crate. The alternative was a bounded compare-and-retry loop: more
code, and only probably correct.

A digest is written **per triple built**, `bin/sources.<triple>.sha256`, plus an
unsuffixed copy for the host because `bin/tabstatus` is the host binary. A single
unsuffixed manifest covering all sources let a build of only the host leave the other
triple's binary behind while a verify read fully green.

| Target | State |
|---|---|
| `x86_64-unknown-linux-musl` | **default**, static-pie |
| `x86_64-unknown-linux-gnu` | builds |
| `x86_64-pc-windows-gnu` | **does not build**, see below |

Windows is deliberately **not** in the build script's target list. The target and
its mingw linker are both installed here and the failure is not theirs: the source
is Unix-only by construction. Measured, it is 53 compile errors across six source
files, every one a `std::os::unix` error - byte-oriented paths (`OsStrExt`), file
modes, symlinks, and the `/proc/$CLAUDE_PID/fd/1` lookup the two direct-write edges
need. Windows has no byte paths at all (its `OsString` is WTF-16), so this is a
port, not a cross-compile, and it is not faked with an untested `.exe`. Listing it
made `sh scripts/build.sh --all` exit 1 on every run even when the host build had
succeeded, which made the documented release step useless as a success signal.

**On a platform with no committed binary**, build one *before* installing:

```sh
sh scripts/build.sh
bin/tabstatus install
```

`install` **refuses** when `bin/tabstatus` is missing, and says the same thing.
That refusal is not pedantry: the env key it would write switches Claude Code's
own title painting off, and all thirteen hooks would then resolve to a command that
exits 127 - a tab nothing paints at all, which is strictly worse than no install.
`install --force` overrides it for the case where you are about to build.
The first build needs registry access (or a populated Cargo cache). Once cached,
`cargo build --locked --offline` works without network access. The lockfile can
list optional derive/proc-macro packages that are not compiled; inspect
`cargo tree -e normal,build` for the active dependency graph.

## Tests

```sh
sh tests/run.sh
CCTAB_TEST_BIN=target/release/tabstatus sh tests/run.sh   # a build you just made
```

596 assertions, and what they drive is `bin/tabstatus` - the same binary
`hooks/hooks.json` invokes, so a stale committed binary fails here rather than in
somebody's tab. The suite used to run the shell implementation under three shells
in four locales, because its answer depended on both; a binary has no
interpreter, and since the length cap became locale-independent it has no locale
dependence either, so that whole axis is gone.

Beside the two shell harnesses there are in-crate unit tests, which those
harnesses cannot replace: they pin the argv and environment parsing, the location
walk, the length cap and its elision, structural hook parsing and the JSON writers
at FUNCTION granularity, so a refactor can be checked a piece at a time instead of
only end to end. Some of what they assert is invisible from outside the binary at
all - that `repair` answers differently from `String::from_utf8_lossy` on a
truncated sequence, for one.

```sh
cargo test --locked
python3 tests/test_payload.py   # subprocess parsing and state-preservation regressions
```

The state section pins `CLAUDE_PID` per case rather than inheriting it, and that is
a gate property rather than tidiness: the layer reads that variable to stamp a
record's origin, so run from inside a Claude Code session - which is how this project
is developed, and the only place a developer would run it - the ambient session's pid
used to land in records two cases assert byte for byte, and the declared gate was RED
in the one environment it is actually invoked from. It is not unset globally, because
the headless-guard and pty sections need the ambient one.

The install sections perform **real** installs, which is why what they pin matters
more than in any other part of this suite. `install` now materialises a 680 KB
plugin tree, so **four** variables have to be redirected and each one covers
something the others do not. `HOME` and `CLAUDE_CONFIG_DIR` are where the key, the
link and the record go. `XDG_DATA_HOME` is where the **tree** goes, and it is the
newest of the four and the one that would otherwise reach a real
`~/.local/share/claude-tabstatus` on any machine where that variable is set - it
happens to be unset on the machine this was written on, so the bug would not have
shown locally. It is unset once at the top of the file and pinned per case as well.
`CCTAB_STATE_DIR` is the one that is not derived from any of the others:
`state::purge` resolves the record directory from `XDG_RUNTIME_DIR` /
`CCTAB_STATE_DIR` **alone**, so an `uninstall` with only the first three redirected
deletes the *real* wait records of whoever is running it. Harmless and
self-healing - a session with no record degrades to the stateless answer and the
next edge writes another - but any script or session exercising `uninstall` should
redirect all four, exactly as this suite does.

One section drops a **bare copy** of the binary in a directory with no plugin tree
above it - the shape of a binary scp'd to a VM - and drives a first install,
`doctor`, every refusal, a refresh and `uninstall` against it. Another rebuilds the
**pre-change wiring** exactly: a skills symlink pointing at a checkout and a
`state_version 2` record whose `symlink_before.target` is that same checkout, then
asserts the loud repoint, that the checkout gains nothing at all, and that a later
`uninstall` declines to point the link back at it. The load-bearing assertion of
the whole file is at the end of that section and is stronger than it was, because
`install` is now the verb under suspicion: not one byte of a tracked manifest was
written by any install path above, and nothing any install *linked* holds a `.git`.

Eighty-one of the assertions are the tmux section, and they drive a PRIVATE
tmux server - `tmux -L cctabprobe -f /dev/null`, killed afterwards, with no
client ever attached; only disposable pane ptys are written, and the user's own server is never
listed, configured or killed. They are SKIPPED, never failed, where there is no
`tmux` binary. What they cannot see is tmux re-EMITTING the title to an attached
client, which needs a pty this suite cannot allocate; what they do assert is the
whole of the server side, including that re-rendering the same paint after a wait
gives a different answer with no process running and no hook firing.

`tests/test_background.py` replays six sanitized live lifecycle traces and tests
long duration, missing metadata, concurrent waits, bounded tracking and migration.

`tests/test_tmux_status.py` adds isolated private-server tests for window-list
rendering, split panes, background windows, TTL decay and exact format restoration.
It drives ordinary hook updates directly into disposable pane terminals, then
attaches a disposable PTY client and checks the displayed status text,
excluding outer-title escape sequences so they cannot satisfy the assertion.
Each test uses a unique socket and isolated configuration directories.

Two of the assertions exist only to guard the committed binaries: `bin/` carries
a digest including `src/*.rs`, `Cargo.toml` and `Cargo.lock` it was built from, *per triple built*
plus an unsuffixed copy for the host, and the suite recomputes it. The per-triple
split matters: one manifest written for all sources after building only the host left
the other triple's binary silently behind while `sha256sum -c` read fully green. Git does not preserve mtimes, so "is the binary older than the
newest source file" cannot be answered after a clone - a digest can, and it also
catches an edit that kept its timestamp. The binary's own `version` is checked
against `Cargo.toml` as well.

Dependency-free, and nothing in the suite can write to a real terminal: every
assertion goes through `CCTAB_DRY_RUN=1` (which prints the computed title and
emits nothing) or runs with `CLAUDE_PID` unset. `install` and `uninstall` are
asserted against a throwaway `CLAUDE_CONFIG_DIR` under `mktemp -d`, never a real
config.

```sh
CCTAB_DRY_RUN=1 bin/tabstatus working   # -> 🔵 claude-tabstatus@main
```

Payload-dependent behavior is assertable the same way, with the payload on
stdin - including the cases that must paint *nothing*, which print no bytes at
all rather than an empty title:

```sh
echo '{"notification_type":"idle_prompt"}'  | CCTAB_DRY_RUN=1 bin/tabstatus notify
echo '{"notification_type":"agent_needs_input"}' | CCTAB_DRY_RUN=1 bin/tabstatus notify
echo '{"notification_type":"agent_completed"}' | CCTAB_DRY_RUN=1 bin/tabstatus notify
echo '{"agent_id":"a1","hook_event_name":"PostToolUse"}' | CCTAB_DRY_RUN=1 bin/tabstatus working
echo '{"hook_event_name":"SessionStart","source":"compact"}' | CCTAB_DRY_RUN=1 bin/tabstatus session-start
```

Every no-op in the state table has its own assertion, because a no-op that
quietly paints is the failure this table is most likely to hide: it does not
crash, it just overwrites a correct state with a wrong one a minute later.

`hooks/hooks.json` is asserted as a **table**, not as a bag of strings. The
suite parses it into one `event matcher edge timeout` row per registered hook
and checks that against the table above in both directions: every row is
present, nothing else is registered, thirteen hooks exactly, one command per group,
and no edge name the binary does not implement (an
unknown edge falls back to idle, so a typo there would silently paint the wrong
state on every notification). Presence checks alone are not enough, and that is
measured rather than argued: a copy of this tree with `Stop` → working and
`UserPromptSubmit` → idle passed an earlier "every event appears somewhere"
version of these assertions, and `claude plugin validate` passed it too. Eight
deliberate mutations of `hooks.json` - transposed edges, a matcher added to
`PostToolUse`, the matcher dropped from `PreToolUse`, a missing timeout, a
changed timeout, an added event, a second hook smuggled into a group, an
edge-name typo - now each fail at least one assertion.

Two things the suite deliberately does not assert. The real emitting path of
`session-start` and `session-end` needs an allocated pty, which would cost a
dependency; it is checked by hand instead - 82 bytes for a startup, the OSC 50
arming pair of [Konsole](#konsole) followed by the idle title, and 0 bytes for a
compaction - and by the 312-case golden corpus in
[`tests/corpus/`](tests/corpus/), which replays every edge over a freshly
allocated pty and compares bytes. The compact-session guard now recognizes every
valid JSON whitespace layout, including pretty-printed input. Named golden-case
changes are documented in `tests/corpus/refreeze_fixed.py`; the original shell
oracle and `cases.jsonl.before-fixes` remain historical evidence.

### The golden corpus

```sh
sh tests/corpus/replay.sh bin/tabstatus     # 312 passed, 0 failed
```

`tests/corpus/cases.jsonl` is the frozen, byte-level record of what the POSIX sh
implementation did - argv, cwd, environment, stdin, and the exact stdout, stderr
and pty bytes it answered with - and it is what makes "byte-for-byte port" a claim
anyone can re-check rather than one you have to believe. The shell itself is kept
beside it as [`tests/oracle/tabstatus.sh`](tests/oracle/tabstatus.sh), byte
identical to the deleted `scripts/tabstatus.sh`, so the corpus can be re-frozen
from the real oracle; `tests/oracle/run.sh` is the shell-era suite. Ten cases were
deliberately re-recorded after the port, each named in `refreeze_fixed.py` for the
limitation it closes, and the pre-fix freeze is kept as
`cases.jsonl.before-fixes`.

The fixture tree is built under `$TMPDIR`, not in the repo, and that is
load-bearing: the corpus `HOME` is the fixture root, and since `tabstatus` walks
*up* from the cwd looking for a `.git`, a fixture tree inside a checkout makes
every location case answer `repo@branch` instead of `~/...`.

The repository fixtures are hand-built - a `.git` directory and a one-line
`HEAD` - so the suite needs no git binary and can assert HEAD bytes that git
will not write on request, such as a missing trailing newline or a CRLF line
ending. A cross-check against a real `git init`, `git worktree add` and
`git checkout --detach` runs at the end when a git binary happens to be
present, and is skipped, not failed, when it is not.

## Slices

Built in slice 1: the plugin skeleton, four hook edges (`SessionStart`,
`UserPromptSubmit`, `Stop`, `SessionEnd`), and Konsole per-tab arming and
restore.

Built in slice 2: the real location - `repo@branch` from a fork-free `.git`
walk, the home-relative path outside a repo, the left-eliding length cap, and
the ssh host prefix. See [Location](#location).

Built in slice 3: the `waiting` state and the recovery from it - six more hook
edges (`PreToolUse`, `PermissionRequest`, `PostToolUse`, `PostToolUseFailure`,
`Notification`, `StopFailure`), the notification-kind three-way, and the
`compact` guard on `SessionStart`. See [States](#states).

Built in slice 4: one Rust binary in place of 875 lines of POSIX sh and two
installer scripts, and the five limitations the shell had caused. It was ported
first and fixed second: a golden corpus of 292 cases recorded what the shell did,
byte for byte, including its bugs, and the port had to reproduce all of it before
anything was allowed to change - after which ten of those cases were re-recorded
on purpose, each named for the limitation it closed. What the language bought:

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
count and window-based parser. It is historical, not a current parser benchmark.
The binary column is best-of-300 on this machine with an exec floor of 288us
(`/bin/true` through the same harness), so the
interesting column is the difference, not the absolute. **Which machine and which
shell matters for the `shell` column and cannot be reproduced from this checkout:**
the only POSIX sh installed here is bash 5.3 as `/bin/sh` (there is no `dash` and
no `busybox`), so the bash figures are re-measurable and the `dash` ones are
historical - taken on the machine that had it during slices 1-3, and quoted rather
than re-run. The `binary` column and the whole ratio are reproducible here with
`sh tests/corpus/replay.sh` and the timing harness in the slice notes.

Built in slice 5: nothing. The port was transliterated from the shell
deliberately, so that it could be proved; this slice made it Rust without
letting it change its mind about anything. `src/sh.rs` - 608 lines
reimplementing shell string operators on bytes - is gone, argv and the
environment are parsed into types once at the boundary, the closed sets are
enums, absence is `Option`, and a failed write is an `io::Error` that only
`main` turns into exit 0. The contract was byte-identical output; what holds it
is the 292-case corpus, the 279 assertions, ~50,000 paired invocations against
the pre-refactor binary, and 101 new in-crate unit tests. Two defects were found
and deliberately NOT fixed, because mixing a fix into this slice would have cost
the proof - they are the two below.

Built in slice 6: tmux. Inside a tmux server the tab title becomes a function of
time - one cell per claude pane, decaying on tmux's own clock - so every one of
the "paints with no matching un-paint" defects above heals itself there. The
payload becomes a record, `SessionStart` configures the server in one invocation
and saves what it replaced, `uninstall` puts it back, and the hot path execs
nothing at all. Exactly one golden-corpus case changed - the
session-start-inside-tmux pty case, whose payload is now the record - and
nineteen were added; every case where `$TMUX` is unset is byte-identical, and so
is every dry run whatever `$TMUX` says. See [tmux](#tmux).

**Known defects, reproduced and deferred.** Both predate the Rust port, both are
reproduced by the current binary, and both belong to a slice that is allowed to
change behaviour:

- `doctor` aborts with exit 1 when `settings.json` is a **directory**. The read
  error propagates, so the env-key, settings, state, terminal, glyph, pty and
  title lines never print. Every other broken shape - empty, whitespace,
  unparseable, an array, missing, a dangling symlink - is reported and exits 0,
  which is what the command is for. Reproduce with `mkdir <config>/settings.json
  && tabstatus doctor`. The fix is to report an I/O error as one more `env key:
  FAIL` line and let the rest of the report run.
- `install` writes the state record (step 1) **before** it edits `settings.json`
  (step 2), so an install that aborts between the two - the
  concurrent-modification guard is one reachable way - leaves a record saying
  `env_had: false` with no key ever written. If you then set
  `CLAUDE_CODE_DISABLE_TERMINAL_TITLE` yourself, `uninstall` reads that orphan,
  concludes the key is ours and removes it; the refusal that exists to prevent
  exactly this only fires when there is no record at all. The damage is
  recoverable, because the `.cctab-preuninstall` copy is written first. The fix
  is to write the record only after settings.json has actually changed, or to
  cross-check the recorded `settings_path` before editing.

Built in slice 7: **wait ownership**, and with it the state layer the next two
slices need. The stateless invariant was the right constraint for six slices and
the wrong one for this defect: the two cases the `agent_id` filter could not tell
apart - a background subagent's tool call, and the tool call that resolved the
dialog you just approved - differ only in what came before. So there is now one
small record per session, and a wait is an overlay on a base rather than a colour.
What it closed: approving a subagent's dialog left the tab orange until the `Task`
returned, and a main-thread tool call repainted blue over a subagent's open dialog.
See [Wait ownership](#wait-ownership) for the capture, the rules, the per-edge cost
(+15us on the hot edge, a read; +22us on a transition; zero with the layer disabled)
and the two documented seams. Nothing in the golden corpus moved: all 312 cases are
byte-identical, because the corpus environment configures no state directory and the
reference implementation the corpus is frozen against is the shell one, which has no
record to consult. The new behaviour is pinned by assertions in `tests/run.sh` and by
unit tests instead.

A second pass then fixed what the first one got wrong, and every item is a *lift*
rather than a tweak: a wait now carries its own epoch (one shared one let unrelated
dialogs keep a stale wait alive for the whole session, 16 consecutive edges painting
nothing), an unattributable `?` is retired by an empty `background_tasks` at `Stop`
instead of holding the tab orange through every idle period, a `UserPromptSubmit` you
typed retires everything, each read-modify-write holds an `flock` on its own record
(76 of 400 racing rounds lost a clear without it), the reaper deletes only what it can
prove is its own, and `doctor` reports whether the directory can be written at all -
the one failure mode every other line rendered as healthy.

**Not yet built:**

- A **cached session title**. The transcript records carry `aiTitle`, readable from
  a bounded tail read, and it is the only field that distinguishes five concurrent
  sessions that all render as `streaming-browser@master`. A reserved `n` line
  caches it; the cap and the elision then belong to `render::compose`.
- A compaction *edge*, as opposed to today's guard. `SessionStart` carries
  `"matcher": "startup|resume|clear|fork"`, which leaves out `compact`, and the
  binary refuses a `compact` payload as well; neither of those paints anything
  while a compaction runs mid-turn. A tab that said so would be better, and the
  place for it is a second `SessionStart` group with `"matcher": "compact"` (or
  the first-class `PreCompact` / `PostCompact` events).
- Konsole `TabColor`, which rides on the same OSC 50 property list as the
  arming and would let the tab itself carry the colour. Whoever adds it also
  has to add `TabColor=#000000` to the `SessionEnd` list, or the colour
  outlives the session.

## Licence

GPL-3.0-or-later. See [LICENSE](LICENSE).
