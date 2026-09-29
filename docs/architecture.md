# Architecture

How claude-tabstatus works inside, and why it is built the way it is. The
[README](../README.md) is the user guide; this file is for anyone changing the
code or wanting to know why a behaviour is what it is.

Two documents are normative and are linked rather than repeated here:

- [docs/state-contract.md](state-contract.md) - the state model, wait
  identities, transitions, retirement, expiry, persistence and their limits.
- [docs/indicator-semantics.md](indicator-semantics.md) - what each colour
  means, precedence, the background-work lifecycle and compatibility.

Working conventions, tests and the roadmap are in [AGENTS.md](../AGENTS.md);
the slice-by-slice history, including the shell-versus-Rust comparison, is in
[docs/history.md](history.md); [COMPARISON.md](../COMPARISON.md) compares this
project with similar ones.

## Contents

- [Overview](#overview)
- [Hook registration](#hook-registration)
- [Hook input](#hook-input)
- [Wait ownership](#wait-ownership)
  - [Why abandoned dialogs stay orange](#why-abandoned-dialogs-stay-orange)
  - [Direct MCP elicitation](#direct-mcp-elicitation)
- [The per-session record](#the-per-session-record)
- [Stale records and the reaper](#stale-records-and-the-reaper)
- [Locking and atomic writes](#locking-and-atomic-writes)
- [Windows console-title painting](#windows-console-title-painting)
- [Location resolution](#location-resolution)
- [Install and uninstall mechanics](#install-and-uninstall-mechanics)
  - [The plugin directory is build output](#the-plugin-directory-is-build-output)
  - [How the generated tree is written](#how-the-generated-tree-is-written)
  - [How doctor and uninstall find the tree](#how-doctor-and-uninstall-find-the-tree)
  - [Keeping the embedded manifests honest](#keeping-the-embedded-manifests-honest)
  - [Install and uninstall are ordered, both ways](#install-and-uninstall-are-ordered-both-ways)
  - [The settings.json splice](#the-settingsjson-splice)
  - [What uninstall removes, and what it declines to](#what-uninstall-removes-and-what-it-declines-to)
  - [Native Windows](#native-windows)
  - [Migrating from a checkout symlink](#migrating-from-a-checkout-symlink)
  - [A deleted tree behind a live link](#a-deleted-tree-behind-a-live-link)
- [The three axes](#the-three-axes)
- [Konsole arming](#konsole-arming)
- [tmux integration](#tmux-integration)
- [TTL rationale](#ttl-rationale)
- [Performance](#performance)
- [Build choices](#build-choices)
- [Environment read internally](#environment-read-internally)

## Overview

There are four moving parts:

- **One binary, `tabstatus`.** Every hook runs it with an *edge* argument
  (`session-start`, `working`, `waiting`, `notify`, `idle`, `subagent-stop`,
  `elicitation`, `elicitation-result`, `session-end`); it reads the hook's JSON on
  stdin, decides what the tab should say and writes it. The same binary carries the
  management verbs: `install`, `uninstall`, `doctor`, `tmux-format`, `tmux-arm` and
  `print-embedded`. Platform differences live behind `src/sys` (`unix.rs`,
  `windows.rs`); Windows is a native port, not a compatibility layer.
- **Hooks.** `hooks/hooks.json` registers thirteen hooks, all pointing at
  `${CLAUDE_PLUGIN_ROOT}/bin/tabstatus` with a 5-second timeout (1 second for
  `SessionEnd`). It and
  `.claude-plugin/plugin.json` are compiled into the binary.
- **A generated plugin tree.** `install` writes the two manifests and a copy of the
  binary into a directory it owns (default `~/.local/share/claude-tabstatus`), links
  `~/.claude/skills/claude-tabstatus` to it, and sets
  `env.CLAUDE_CODE_DISABLE_TERMINAL_TITLE` in `settings.json` so Claude Code stops
  painting over the title. See [Install and uninstall mechanics](#install-and-uninstall-mechanics).
- **A state layer.** One small record per session, under `$XDG_RUNTIME_DIR/claude-tabstatus`
  (`%LOCALAPPDATA%\claude-tabstatus` on Windows) or `CCTAB_STATE_DIR`, remembers who
  raised each outstanding wait and whether background work is known. Without a
  usable directory every edge falls back to the stateless answer described in the
  [contract](state-contract.md#time-and-persistence).

The title itself reaches the terminal in one of three ways: as the hook's JSON
`terminalSequence` (the ordinary case), written straight to Claude Code's pty
(`session-start` and `session-end` on Linux, which the hook protocol cannot carry,
and every paint inside tmux), or as a console title on native Windows.

## Hook registration

| Event | Matcher or scope | Edge | Paints |
|---|---|---|---|
| `SessionStart` | matcher `startup\|resume\|clear\|fork` | `session-start` | ⚪ idle, and arms the Konsole tab |
| `SessionStart` | payload `source` is `compact` | `session-start` | *nothing* |
| `UserPromptSubmit` | | `working` | 🔵 working |
| `PreToolUse` | matcher `AskUserQuestion\|ExitPlanMode` | `waiting` | 🟠 waiting |
| `PermissionRequest` | | `waiting` | 🟠 waiting |
| `PostToolUse` | | `working` | 🔵 working |
| `PostToolUseFailure` | | `working` | 🔵 working |
| `Notification` | `permission_prompt`, `worker_permission_prompt`, `agent_needs_input`, `elicitation_dialog`, `elicitation_url_dialog` | `notify` | 🟠 waiting |
| `Notification` | `idle_prompt` | `notify` | 🟣 background if known, otherwise ⚪ idle |
| `Notification` | any other kind | `notify` | *nothing* |
| `Stop` | | `idle` | 🟣 while background remains, otherwise ⚪ idle |
| `StopFailure` | | `idle` | 🟣 background if known, otherwise ⚪ idle |
| `SubagentStop` | | `subagent-stop` | *nothing*, unless that agent owned the wait |
| `Elicitation` | form/URL request | `elicitation` | 🟠 waiting; duplicate completed requests stay silent |
| `ElicitationResult` | matching server and request ID, `accept`, `decline` or `cancel` | `elicitation-result` | restores the base only when no other wait remains |
| `SessionEnd` | | `session-end` | clears the title, restores the tab |

Live waits take precedence over everything in the right-hand column, and idle
transitions preserve known background work; the exact rules are the
[transitions table](state-contract.md#transitions-and-retirement).

The `SessionStart` and `PreToolUse` scopes are hook *matchers*, so those hooks do
not even run outside them. The `Notification` kinds and the `compact` source are
read from parsed top-level metadata inside the binary, so those hooks run and then
decide - which costs one ~0.6ms process on a notification, and buys a decision the
test suite can assert rather than one that lives only in a config file.

Each registration has a reason, and several are less obvious than they look:

- **`PostToolUse` is registered unmatched**, so it runs on every tool call, and it
  is the recovery from waiting: 24-42ms after you answer a dialog the tab is blue
  again. It reads `agent_id` and `session_id` from its payload, and a *subagent's*
  tool call paints nothing unless that subagent is the one you were waiting on,
  because a background subagent's tool call firing in the main session must not
  repaint over a dialog you are looking at. A matcher could not do either: a
  matcher sees only `tool_name`. [Wait ownership](#wait-ownership) is the whole of
  that distinction. The payload carries the whole `tool_response`, which can be
  hundreds of KB on a large read; how it is parsed and bounded is in the
  [contract](state-contract.md#envelope-interpretation).
- **`PreToolUse` is matched to exactly the two tools that always block on you.**
  Unmatched, it would paint waiting on every tool call. `PermissionRequest` fires
  for both of those tools anyway, 11-19ms later, so this edge is really only
  insurance for a permission path that gets bypassed.
- **A `Notification` never paints waiting on `idle_prompt`.** That kind is the
  quiet-turn nudge, fired `messageIdleNotifThresholdMs` (default 60s) after a turn
  ends, so mapping it to waiting would turn every idle tab orange a minute later
  and collapse idle and input-required into one. It is also the only recovery from
  an interrupted turn - see [Why abandoned dialogs stay orange](#why-abandoned-dialogs-stay-orange).
- **The waiting notifications are a backstop, not the fast path.**
  `permission_prompt` is scheduled 6.00s after the dialog appears, fires at most
  once per dialog, and is suppressed outright by
  `CLAUDE_CODE_DISABLE_PERMISSION_PROMPT_NOTIFY_HOOKS`. `PermissionRequest` is the
  real-time signal. What the notifications add is what `PermissionRequest` does not
  cover: a worker prompt, an agent asking for input, an MCP elicitation dialog, and
  the dialogs that are not tool calls at all - the managed-settings review, the
  sandbox network request - which is why `permission_prompt` is kept even though it
  is redundant for every tool dialog. The product does cancel the notification when
  you answer first, but best-effort: measured once firing in the same millisecond as
  the answering keystroke, so its orange and `PostToolUse`'s blue were emitted
  concurrently and landed 38ms apart. The order came out right; nothing guarantees it.
- **A subagent's `PermissionRequest` paints waiting too.** An asynchronous `Task`
  returns in ~6ms, so the main session's `Stop` has usually already fired:
  measured, a subagent's dialog arrived a second *after* the tab went idle, and
  nothing repainted until a human answered it. Treating it as a no-op would leave
  the tab idle while you are the one blocking.
- **`SubagentStop` is registered and paints nothing of its own.** A subagent
  finishing must not read as the session going idle - and it also fires for agents
  nothing announced: measured, a `SubagentStop` for `a8e90c10`, with an empty
  `agent_type` and no matching `SubagentStart`, nine seconds *before* the user
  answered a different agent's dialog, and another one immediately before a
  `SessionStart`/`compact`. No matcher could filter those. The owner test does:
  this edge clears the wait whose `agent_id` matches and is otherwise a complete
  no-op, which is also what it is when there is nowhere to keep a record. It is
  here because it is the only signal for a dialog you **declined** - no hook fires
  for a denial, the tool never runs, and no `PostToolUse` ever arrives.
- **`StopFailure` is there because `Stop` is not.** When a turn dies on an API
  error - `rate_limit`, `overloaded` - only `StopFailure` fires, so without it a
  rate-limited turn would leave the tab blue indefinitely.
- **`SessionEnd` owns the restore.** With `CLAUDE_CODE_DISABLE_TERMINAL_TITLE` set,
  Claude Code no longer clears the title on exit, so this plugin does.

## Hook input

What the binary paints depends on the edge, **top-level JSON metadata**, and the
per-session record. Hook input is parsed with `serde_json::from_slice` and a
selective visitor: `IgnoredAny` skips unused tool inputs and results without
constructing a JSON tree, so nested `agent_id`, `source` or notification fields
cannot impersonate hook metadata. The accepted grammar, the 16 MiB input bound,
which fields are recognised and how rejected input becomes a silent no-op are all
specified in the [envelope interpretation](state-contract.md#envelope-interpretation)
section of the contract and are not repeated here. Work grows with the input size;
the bounded-window reader that came before it is described in
[docs/history.md](history.md).

Stateful edges parse the same metadata as stateless ones: the record is filed
under the decoded top-level `session_id`, and wait ownership uses the same
`agent_id` accessor that suppresses stateless subagent repaints. The payload
parser fixes metadata interpretation; wait retirement and the settings-file editor
have separate contracts, and the background lifecycle has its own captured and
synthetic tests, described in the [indicator policy](indicator-semantics.md#background-lifecycle-and-reconciliation).

## Wait ownership

The normative rules - which edge adds or retires which wait, what each paints,
deduplication, capacity and expiry - are the
[state and wait-ownership contract](state-contract.md), and its
[versioned semantic traces](../tests/fixtures/state-contract-v1.json) assert the
base, outstanding owners, clocks, emitted update and displayed indicator after
every event. This section is the motivation.

### The problem

Statelessly, the binary filtered on `agent_id`: a subagent's `PostToolUse` painted
nothing. The two cases that filter cannot tell apart are both a subagent's
`PostToolUse`: one arriving while *your* dialog is open (must not paint) and one
arriving after you answered the *subagent's* dialog (should un-paint). No field
separates them - the difference is in what came before - so the stateless filter
took the safe half and paid for it: after approving a subagent's dialog nothing
repainted until the `Task` returned, so the tab read orange for the rest of that
subagent's run, which is minutes. Inside tmux the decay healed it after
`CCTAB_TTL_WAITING`; in a plain Konsole tab nothing healed it at all. It is now
resolved by recording *who* raised the wait.

The model is partly taken from
[Yannis-Adn/terminal-addons](https://github.com/Yannis-Adn/terminal-addons) (MIT),
whose `wt-tab-status` keeps one state file per session holding a state *and the
owner of a wait*, and only lets the waiting agent end the wait. `src/state.rs`
credits the exact functions, and records the four places this diverges - the
largest being that its `PermissionRequest` branch no-ops on a subagent's dialog,
which the capture below shows would paint idle while a human is being asked.

A wait is an **overlay on activity**. Main work shows blue; otherwise known
background shows purple and idle shows white. A dialog covers that activity;
clearing the *last* dialog restores it. Who raised a wait is therefore part of the
state, and it cannot be recovered from any one payload - which is why it is
persisted alongside background knowledge.

### The capture that forced it

Timings are relative to that session's `SessionStart`:

| t | event | `agent_id` | stateless | with ownership |
|---|---|---|---|---|
| 66.283 | `PreToolUse` `tool_name=Agent` | - | 🔵 | 🔵 base `w` |
| 68.946 | `Stop` `background_tasks=[subagent:running:aec99e]` | - | ⚪ | 🟣 base `i`, background recorded |
| 69.960 | `PermissionRequest` | `aec99e1f` | 🟠 | 🟠 wait owned by `aec99e1f` |
| 75.983 | `Notification` `permission_prompt` | *absent* | 🟠 | 🟠 no second owner added |
| 98.459 | `SubagentStop` | `a8e90c10` | - | *nothing*: owns no wait |
| 107.496 | `PostToolUse` (you approved) | `aec99e1f` | *nothing* | 🟣 background remains |
| 110.230 | `SubagentStop` | `aec99e1f` | - | *nothing*: already cleared |

The contract records the provenance of this sequence: it is a reconstruction of
the `src/state.rs` summary, not a preserved raw capture
([evidence](state-contract.md#evidence-and-regression-coverage)).

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

### Consequences

- **Two dialogs at once are both remembered.** A main-thread dialog queued behind a
  subagent's is the realistic overlap, and a single owner slot gets it wrong
  whichever one it keeps: answering the main one must not restore the base while
  the agent's dialog is still on screen. So the record holds a *set*, newest last,
  and the base comes back when the set empties.
- **A main-thread tool call no longer repaints over a subagent's dialog.** This is
  the half of the fix that matters most, and statelessly it was not even possible to
  attempt: the blunt `agent_id` filter only caught *subagent* tool calls.
- **`Stop` does not paint idle over an outstanding dialog** - not while something
  is actually running. It always clears a *main* wait, because the loop could not
  have stopped while a main-thread dialog blocked it (a rejected `ExitPlanMode`,
  whose `PostToolUse` never comes, is exactly that). What it does with the others
  depends on `background_tasks`. "Absent" is not "empty", deliberately: a
  `Notification` carries no `background_tasks` at all, and a Claude Code that
  renamed the member would carry none either, so a missing array preserves non-main
  waits; a *non-empty* one leaves them standing for the same reason, which is
  exactly the capture's 68.946 `Stop`.
- **`UserPromptSubmit` clears everything, but only when you typed it.** You cannot
  type at the prompt while a modal is up, so a prompt of yours proves the screen is
  clear whoever owned the dialog. The trap is that not every `UserPromptSubmit` is
  yours: in the capture, an event at 110.251 carries
  `"prompt":"<task-notification>..."`, which the *product* injects when an async
  agent finishes, and with two agents running the first one's completion would then
  retire the second one's live dialog. Hence the `<` heuristic in the
  [contract](state-contract.md#transitions-and-retirement): based on the captured
  injected prompt, not proof of human authorship, so a human prompt beginning with
  `<` also fails the recovery check. In the capture, that agent's own `SubagentStop`
  fires 20ms earlier and already handles its completion.

### Overlays must be liftable

**An overlay nothing can lift is worse than no overlay at all.** While any wait is
held, neither `working` nor `idle` paints - that is the whole mechanism - so a wait
that outlives its dialog freezes the tab orange, and outside tmux nothing decays
it. Each retirement path exists for a reason:

- **The owner's own completion** - its `PostToolUse`, or its `SubagentStop` when you
  declined and no tool ever ran. An arbitrary agent never owns an anonymous wait,
  because notification-only waits carry no request identity.
- **Main-thread tool progress**, for main and anonymous notification waits.
- **A prompt you typed**, because a modal dialog and a usable prompt cannot both be
  on screen.
- **An empty `background_tasks` at `Stop`**, which recovers permission and
  notification waits when nothing is left running.
- **Its own `CCTAB_TTL_WAITING` expiry, per wait.** Expiry is processed when a later
  hook loads the record; no daemon repaints a quiet terminal.

The recovery paths exist because the captures are emphatic that *abandoning* a
dialog fires no hook whatsoever: Esc on a live dialog (`s2`), declining one (`s7`)
and Ctrl+C mid-tool (`s5`) each emit nothing at all until the next prompt. Nothing
the owner does can be waited for, because the owner does nothing. Which identities
each path retires, and why direct MCP waits survive most of them, is the
[transitions table](state-contract.md#transitions-and-retirement).

**Per-wait epochs.** With one epoch for the whole list, raising any dialog
refreshed it, so every later dialog pushed a *stale* wait's expiry out with it:
measured on that shape with the TTL set to 3s, one abandoned subagent wait plus four
ordinary main turns 2s apart left **16 consecutive edges painting nothing**, 8s into
a 3s horizon - a tab frozen orange for the rest of the session, with the TTL unable
to rescue it. Each wait now carries its own clock, and an expiry is written back to
the record rather than recomputed, so raising the TTL later cannot resurrect a wait
already declared dead.

### Why abandoned dialogs stay orange

Ctrl+C emits no hook at all, and neither does walking away from a permission
dialog. It is not for want of an event. `PostToolUseFailure` exists and carries an
`is_interrupt` flag, but the hook is handed the turn's own abort signal and bails
before spawning anything, so an interrupt - the thing that aborts that signal -
skips it by construction. Measured end to end: a pre-approved `ping -c 40`
interrupted mid-run produced no `PostToolUseFailure`, no `PostToolUse` and no
`Stop`. Interrupting a *thinking* turn is the same, and so is pressing Esc at a
permission dialog - measured twice, no hook of any kind fires.
`PostToolUseFailure` is still registered, because it does fire for a tool that
*reports* an error, after which the turn continues - so it paints `working`, not
idle.

Declining with "No, and tell Claude what to do differently" and then actually
submitting the feedback is a different path, and is unverified: the probe that
tried it selected the option and never submitted, so what it measured was the
feedback box - itself a wait, correctly orange. Reading the code, a denial is not an
abort, so it should reach `PostToolUseFailure` and then a normal `Stop`, which would
need no recovery at all.

**The interrupt heals itself in about a minute; the abandoned dialog does not.**
When a turn ends, aborted or not, the product schedules its `idle_prompt`
notification, and this plugin maps that to idle - so a tab left blue by Ctrl+C goes
white about 60s later (`messageIdleNotifThresholdMs`, configurable) if you leave
the keyboard alone, and immediately if you type your next prompt. But that notifier
checks, when its timer fires, whether you have touched the keyboard since the turn
ended, drops the notification if you have, and never re-arms. Dismissing a dialog
*is* touching the keyboard, so nothing repaints: measured, Esc at a `Write` dialog
followed by 110s of absolute quiet - 1.8x the 60s threshold - produced no hook and
no repaint, and the tab was still orange when the session ended 113s later. Wait
ownership cannot fix the colour, because nothing fires; what it does is stop the
stale *record* from outliving the stale colour, so the next edge that does fire is
not also suppressed by it.

Some dialogs are invisible to every hook: the LSP recommendation, the plugin hint
and the auto-mode-default upsell fire no `PermissionRequest`, no matched
`PreToolUse` and no `Notification`, and the 60s nudge is gated on no dialog being on
screen. A white tab over one of those modals is the boundary of what hooks can see.

### Direct MCP elicitation

`Elicitation` and `ElicitationResult` are registered without a server matcher.
They observe requests and responses and emit only the existing title protocol; they
never answer forms, replace responses, change permission decisions, or exit with a
blocking status, and message text, form answers, schemas, credentials and
authentication URLs are skipped and never persisted. Identities, bounds,
tombstones and overflow are specified in the contract's
[wait identities](state-contract.md#wait-identities) and
[capacity](state-contract.md#duplicates-overlap-ordering-and-capacity) sections.

The one design decision worth its rationale here is that **identity-free
notifications are never coalesced with a direct request.** Claude Code 2.1.274's
notification construction omits the server and request IDs, so there is no
reliable way to tell a delayed duplicate from a new independent dialog. Such a
notification can raise the anonymous backstop again after a direct result, and main
progress, a quiet `Stop`, a human prompt, expiry or session cleanup recovers it.
Suppressing it solely because another request is known would hide independent
dialogs, which is the worse error.

Validation evidence, version information, and the distinction between synthetic
replays and live captures are recorded in the
[elicitation evidence](https://github.com/dalf/claude-tabstatus/issues/9#issuecomment-5854485979);
the input schema follows the
[official hook reference](https://code.claude.com/docs/en/hooks#elicitationresult).
Both events are present in the inspected Claude Code 2.1.274 executable; earlier
versions and live interactive delivery have not been validated. Without usable
persistence, requests still paint waiting, but results stay silent because they
cannot prove what is outstanding.

## The per-session record

One file per `session_id`, so concurrent sessions never contend. An example:

```text
cts5                                           the tag: version 5 of the wire
b i                                            base = w | a | i
g 1790380620                                   last main Stop reporting background
p 3709427 84460384                             the session's (pid, start time)
w aec99e1f4bda1972b:1790380630 -:1790380631    one wait per word: owner, then epoch
s konsole                                      the surface this session ARMED
```

On Windows the origin line is `q <pid> <creation FILETIME>` instead of `p`, and
each platform reads the other's key as an unknown field - no origin, the mtime rule
- never as a pid of its own.

Owner tokens on the `w` line map onto the contract's
[wait identities](state-contract.md#wait-identities):

| token | identity |
|---|---|
| `<agent_id>` | `agent:<id>` |
| `-` | `main` |
| `?` | `unknown-permission` (a permission/input notification) |
| `?p` | `anonymous-permission` (a supplied owner that is unusable) |
| `?!` | `notification-mcp` (the anonymous MCP notification backstop) |
| `!?` | `anonymous-direct-mcp` |
| `!+` | `overflow` |
| `!<hex-server>.<hex-request>` | `direct-mcp(server,id)` |

The optional `e` line holds completed request keys with their completion epochs.
An `agent_id` is accepted only as `[A-Za-z0-9_-]{1,64}` - the same test that stops
a `session_id` from choosing the path it is filed under. Unknown keys are
**skipped**, and a base letter this version cannot paint reads as idle, so a newer
version's record degrades rather than being misread; and a record this version did
not *change* is not rewritten, so it keeps the fields it did not understand. The
whole record fits in 8 KiB.

The `g` line is a bounded aggregate of in-flight work from main `Stop` snapshots:
no individual task list or count is stored. What sets, clears and preserves it is in
the [contract](state-contract.md#transitions-and-retirement), and why child
completion alone does not prove a workflow ended is in the
[indicator policy](indicator-semantics.md#background-lifecycle-and-reconciliation).

The optional `s` line is written **only** by a session that actually armed its
terminal - today, Konsole - and it is what `SessionEnd` restores from when there is
no tmux server to hold the same fact. A surface name this build has no row for
reads as **absent**, never as a different terminal, so the end hook falls back to
its own environment rather than writing bytes at random. Its silence therefore
means "nothing is recorded" and not "nothing was armed": a session that armed
nothing writes exactly the record it always wrote, which is what keeps every record
already on disk, and the 312-case golden corpus, byte for byte unchanged. The
normative rule is in [time and persistence](state-contract.md#time-and-persistence).

The `n` key is reserved, last in the file so free-form text would arrive whole, for
a cached session title; it is not implemented (see the roadmap in
[AGENTS.md](../AGENTS.md)).

Version compatibility - `cts1` to `cts4` stay readable and migrate on the next
change, and the `cts5` tag makes guarded older binaries leave a newer record alone
rather than drop background knowledge - is specified in
[time and persistence](state-contract.md#time-and-persistence) and the
[indicator policy](indicator-semantics.md#compatibility-and-delivery). Releases
without those guards can still drop it, which is why every hook should run the
same binary.

## Stale records and the reaper

**A stale record is normal, not exceptional.** A session killed with `SIGKILL`
fires no `SessionEnd`, so the record has three independent bounds and no daemon:

- An outstanding **wait** older than `CCTAB_TTL_WAITING` (900s), measured per wait
  against its own epoch, on a subsequent eligible hook.
- **A `SessionStart` in any session reaps the others**, by asking whether the
  process that wrote each record is still running. Every write stamps the record
  with `$CLAUDE_PID` *and that pid's start time* - field 22 of `/proc/<pid>/stat`;
  on Windows the process's creation FILETIME - and the reaper unlinks a record only
  when that pair no longer names a running process. Stamping on every write, not
  only at `SessionStart`, covers a session whose `SessionStart` ran before this
  plugin was installed, which would otherwise be left on the one-day mtime rule for
  its whole life.
- The directory is under `$XDG_RUNTIME_DIR`, which the OS empties at logout. On
  Windows it is `%LOCALAPPDATA%\claude-tabstatus`, which nothing empties; after a
  reboot no origin is alive, so the next `SessionStart` reaps every record.

The reaper **provably cannot delete a live session's record that carries an
origin**, and the pair is why. A hook process is a child of `$CLAUDE_PID`, so that
process exists whenever any of its own hooks are firing, and a start time is
immutable for the life of a process. So the unlink predicate is false for every
live session, whatever the record's age - and *age is what a naive reaper would
have used*. An mtime horizon cannot do this job: a session sitting at its prompt
touches nothing, so any horizon short enough to be useful would eventually delete
the record of a session that is merely idle. The start time is also what makes pid
recycling harmless - a recycled pid reads as *gone*, not as alive, because the
start time under it differs. The error the rule *can* make is the harmless one:
keeping a dead session's record if a new process were handed the same pid inside
the same 10ms tick, which needs 4194304 intervening spawns (`pid_max`, measured, at
`CLK_TCK` 100). On Windows the same proof holds: `$CLAUDE_PID` is the live
`claude.exe` (measured), its creation time never changes, and a pid is not reissued
while any handle to its process is open, so the error needs a reused pid inside the
same 100ns creation stamp. A process the reaper may not ask about counts as alive
there too.

The `/proc/<pid>/stat` parse has one trap worth naming, because the obvious
`awk '{print $22}'` falls into it: field 2 is the executable name in parentheses,
and it may itself contain spaces and parentheses. Measured, one process read
`(npm exec chrome...)` - exactly the sort of thing a `claude` session spawns - so
the field is read *after the last* `") "`, in the binary and in the test alike.

Records without an origin fall back to a 24-hour mtime rule, with the exemption for
known background; the exact rule is in
[time and persistence](state-contract.md#time-and-persistence). The horizon is
deliberately long for the reason above.

**The reaper deletes only what it can prove is ours, and everything else is left
alone forever.** A file whose name is not a session id, a file that is not a record
in any version's shape, a file it could not read, and anything that is not a
regular file or a symlink: none of those is a candidate at any age. That is stricter
than it looks necessary and the reason is concrete - `CCTAB_STATE_DIR` is a
documented knob, the name grammar `[A-Za-z0-9_-]{1,64}` cannot tell a session id
from `id_rsa`, and an earlier rule that fell back to mtime for anything it could not
parse deleted a 30-day-old private key out of a directory that had other things in
it. The price is bounded litter in a tmpfs that logout empties; `doctor` names every
file it will not take, and says why.

Correctness never depends on any of it: `SessionStart` rewrites its *own* record
before reaping, so a resumed or reused session id can never read a dead session's
record as authoritative. Reaping is hygiene, and `tabstatus doctor` names every
record it would take.

## Locking and atomic writes

The record is written temp-then-rename, because two hooks of one turn do overlap -
measured 1ms apart - and a reader must see the old record or the new one, never a
torn one. 240 concurrent hooks against one record leave one well-formed file and
no temporary. The temporary is `<id>.<pid>.<16 hex digits>.tmp`, the hex a fresh
random nonce for each attempt, and it is *created*, never opened
(`O_CREAT|O_EXCL`, `CREATE_NEW`): a file, hard link or symlink someone planted at
that name is skipped, left alone, and never written through. Older versions named
it `<id>.<pid>.tmp`, opened it with a truncating create, and so would overwrite
whatever a hard link planted in a shared `CCTAB_STATE_DIR` pointed at; leftovers
of that shape are still recognized and reaped by the 24-hour mtime rule in
[time and persistence](state-contract.md#time-and-persistence).

**Each read-modify-write holds an exclusive lock on that session's own record.** "A
lost update self-corrects on the next edge" - which the documentation used to claim
- is false in the one direction that matters. When the lost update is a *clear*,
the record keeps a phantom wait, and then `working`, `idle` and the idle nudge all
decline to paint: the tab is orange until the TTL, which outside tmux is the
fifteen-minute lie this whole layer exists to remove. Measured on the same code with
the lock disabled, 400 rounds of a subagent's un-painting `PostToolUse` launched
simultaneously with main's `Stop`: **76 of 400 lost the clear**, and 36 of 400 in
the reverse direction lost the wait. With the lock, 0 of 1200 across three runs,
and five concurrent sessions never touch each other's file.

Measured later with the in-crate barrier harness (each hook a separate process, all
released together once ready; the command is in [AGENTS.md](../AGENTS.md)), 400
rounds of the clear race, 400 of the wait race and 50 eight-way rounds lose **0, 0
and 0** with the lock on Windows and on Linux, and without it 400, 400 and 50 on
Windows and 399, 400 and 50 on Linux. That harness lines the hooks up far more
tightly than the one behind the 76 of 400 above, so the two sets of numbers are not
comparable.

What the lock does *not* order - terminal writes after it is released, and the
unlocked `SessionEnd` deletion - is stated in
[time and persistence](state-contract.md#time-and-persistence).

Two details of the lock are load-bearing:

- **It re-checks the inode.** The lock is taken on the record path, and because
  `write_if_changed` renames over that path the inode can change under a waiter -
  which would leave it holding an exclusive lock on an unlinked inode while a third
  hook held the new one - so after locking it checks that it holds the file the path
  names *now*, and retries when it does not. Creation-capable edges atomically
  create the record before locking, so concurrent first requests and results are
  serialised too. Unrelated agent events still create no file. If a lock cannot be
  obtained, the hook makes no unlocked state change.
- **It costs no dependency.** On Unix it is `std::fs::File::lock`, in std since
  1.89; that is the reason `rust-version` moved from 1.74 to 1.89. The alternative
  was a bounded compare-and-retry loop: more code, and only probably correct.

**On Windows it is the same algorithm with three different primitives**, all in
`src/sys/windows.rs`:

- The lock is `LockFileEx` on *one byte* at offset 2^62, far past any record,
  because that lock is mandatory: over the record's bytes it would refuse every
  other handle's read - the reaper's, `doctor`'s, and the hook's own read by path,
  which would then write a fresh record over the real one. std's own lock on
  Windows covers the whole file for the same reason, so it is not used there.
- The identity check compares the volume serial and 128-bit file id
  (`FILE_ID_INFO`), read from handles.
- The replace is a POSIX-semantics rename (`FileRenameInfoEx`), the only kind that
  replaces a file its writer still holds open. A volume without it (FAT, exFAT, the
  9P share WSL exports, some SMB servers) cannot keep a record, so the layer is off
  there and `doctor` says why.

One thing can still refuse that replace on Windows, and never on Linux: another
program holding the record open *without* delete sharing - Python's `open()`, a
.NET `File.OpenRead`, `Get-Content -Wait`, some editors and backup tools. The hook
retries for half a second under its lock; past that the write is lost, like any
failed write (best effort), and the hook still paints the edge's answer. When the
lost write was a subagent's clear, the tab shows the truth but the record keeps the
wait, so main-thread edges paint nothing until that agent's next tool call or its
`SubagentStop` clears it (measured), or a prompt, a quiet `Stop` or the TTL does.
And because NTFS compares names without case, two session ids differing only in
case would share one record; Claude Code's ids are lowercase UUIDs.

## Windows console-title painting

`session-start` and `session-end`, which the hook protocol cannot carry, reach the
tab on Linux by writing to Claude Code's pty through `/proc/$CLAUDE_PID/fd/1`. On
Windows they reach it as a **console title** instead: the hook leaves its own
hidden console, attaches to Claude Code's (`$CLAUDE_PID`), calls
`SetConsoleTitleW` - the idle title, or an empty one at the end - and detaches; the
pseudo console under Windows Terminal forwards that as an OSC 0.

The headless guard is three proofs, and any "no" paints nothing:

1. `$CLAUDE_PID` is a running ancestor of the hook.
2. Its current stdout, read out of its PEB, is a character device (refusing
   `> file` and `| jq`).
3. Once attached, that handle is a screen buffer of that console (refusing `> NUL`).

Only once the walk has found it is `$CLAUDE_PID` opened for more than a query,
once, and that handle - kept only if its process was created when the one the walk
found was - serves the PEB read and is held until the attach is over: Windows does
not reissue a pid while its process has an open handle, so a Claude that exits
mid-hook cannot have its pid, and the attach with it, land on another console.

The console part is abandoned after **250ms**, and a console call it left in
flight is cancelled within another 50ms so the hook can exit, so a terminal that
has stopped reading output cannot hold a hook past `SessionEnd`'s 1s budget; the tab
then keeps its previous title. A Ctrl+C in Claude's console while a hook is attached
is ignored by the hook, and so is a Ctrl+Break - except in the instant between the
attach and the hook's handler taking effect, which no in-process order closes: one
landing there ends that hook, not Claude, and the title is not set. Where the PEB
read is denied - endpoint security software, a 32-bit or WOW64 `claude` - nothing
is painted. The Konsole arming has no console form and is not sent.

A console title enters Claude's output stream at once. If Claude is midway through
writing a split escape sequence at that moment - possible for the `SessionStart` of
`/clear`, `/resume` or a fork, not at startup - a few characters of it could print.
The window is about a millisecond, and a VT write would share it.

## Location resolution

Inside a git repository the location is `<repo>@<branch>`, without the
subdirectory: the branch is the thing that changes under you, and every tab of the
same repo staying recognisably the same tab is the point. Outside a repository the
location is the whole home-relative path.

The repository is found by walking up for a `.git`, and `.git/HEAD` (or a `gitdir:`
pointer) is read and parsed directly in Rust (`src/git.rs`). `git` is never
executed: one `git rev-parse` costs 15-40ms, where the whole location was measured
at about 0.12ms, and the location is computed on every painting edge, including
every main-thread `PostToolUse`.

- **A `.git` that is not a working repository is not one here either.** An empty
  `.git` directory, a `gitdir:` pointer to somewhere that no longer exists, or a
  `HEAD` that does not parse all fall through to the path form, and the walk
  continues upward - the same thing git does. `/tmp/.git` exists on more machines
  than you would expect, and without this every path under `/tmp` would claim to be
  a repo called `tmp`.
- **The branch is read from `HEAD`, not resolved.** That is the branch you are on,
  which is what a tab should say, but it means a location can be a branch that has
  no commits yet, and `@` in a branch name is not escaped. A `HEAD` pointing outside
  `refs/heads/` keeps its namespace minus the `refs/` prefix, so a bisect reads
  `bisect/bad`, and a detached checkout reads a 7-character short sha. A first line
  longer than 255 bytes is not treated as a `HEAD` at all - no real one is, and the
  parse is not free on a huge string - so the walk continues past it.
- **An exported `GIT_DIR` wins over the walk**, exactly as it does for git.
- **On Windows a gitdir on a network share is refused.** A `.git` names where the
  real git directory lives - a gitfile's `gitdir:` line, a `GIT_DIR`, or a reparse
  point standing in for `.git` or `HEAD` - and a hostile checkout can aim that at a
  share. The first `stat` of a path on a share opens an SMB connection whose
  automatic NTLM handshake leaks the user's credentials off-box, and stalls the hook
  for seconds. So before any resolved gitdir, `GIT_DIR` or `HEAD` is probed, a
  network or device path is refused and the tab falls back to the plain path,
  exactly as for a directory that is not a repository: a UNC path (`\\server\share`,
  a mapped network drive, `\\?\UNC\…`) or a device path (`\\.\…`,
  `\\?\GLOBALROOT…`) is allowed only when it is the very volume or share the session
  already sits on, so a repository deliberately kept on a share still paints
  `repo@branch` while one that jumps to another share does not; a drive mapped to
  that same share counts as the share, so a worktree opened through a mapped drive
  still resolves. A reparse point is walked to the end of its chain by reading each
  link's target rather than following it, so a `.git` junction to a local directory
  resolves while one that leads - directly or through a local link - to a share is
  refused. On Unix a bare `stat` reaches no share, so the check is a no-op and the
  hot path is unchanged.
- **The repository is found on the physical path.** The walk starts from
  `getcwd()` - what a shell's `cd -P .` would give - without a fork, so the tab
  reports the same repository and branch `git` does, named after the real toplevel
  rather than after a symlink. The `~` abbreviation still uses the logical `$PWD`,
  so a distro whose `/home` is a symlink keeps its `~`.
- **The ssh hostname comes from `/proc/sys/kernel/hostname`**, which keeps it
  fork-free on Linux. Elsewhere it falls back to `$HOSTNAME` and then to a
  `hostname` fork, the only fork on the paint path outside tmux; `CCTAB_HOST` skips the guessing. If
  none of the three answers, the prefix becomes a literal `ssh:` rather than
  nothing, because no prefix means "local".

**Elision.** A path is cut at the front on a component boundary, and a
`repo@branch` at the end, so both Konsole (which elides from the left) and Windows
Terminal (which truncates from the right) show the informative part. The host has
a cap of its own (`CCTAB_MAX_HOST`, default 16) because without one a 63-character
single-label cloud hostname rendered a 91-column title, and Windows Terminal showed
the host and nothing else. Konsole's measured tab budget is ~49-60 columns; a local
title is ~27-37.

**Characters, not columns.** Both caps count characters, in every locale. The unit
used to be a byte or a character depending on the shell and `$LANG`, which is why
the shell implementation skipped the cap for any location holding a byte outside
printable ASCII - not only a non-ASCII one, but also one carrying an ASCII control
character or DEL, since the old guard was a single `*[!\ -~]*` test. A character is
still not a column: a CJK or emoji location is cut to 32 *characters*, up to 64
display columns, and a multi-column `CCTAB_ELLIPSIS` overshoots by its extra width.
Counting East-Asian wide and fullwidth code points as two would fix that and is not
done: it needs a width table this binary deliberately does not carry.

## Install and uninstall mechanics

### The plugin directory is build output

**`install` never links your checkout. It writes a plugin directory and links
that.** `hooks/hooks.json` and `.claude-plugin/plugin.json` are *source*: tracked
files you read, diff and edit, and they reach a running session the way
`src/main.rs` does - through a build. They are compiled into the binary with
`include_str!`, and `install` writes them back out into

```text
~/.local/share/claude-tabstatus/
    .claude-plugin/plugin.json      the copy compiled in
    hooks/hooks.json                the copy compiled in
    bin/tabstatus                   a copy of the binary you just ran (bin\tabstatus.exe on Windows)
    .tabstatus-generated            the marker: version, target triple, file list
```

then sets the env key and points `~/.claude/skills/claude-tabstatus` at **that**.
The tree goes in `$XDG_DATA_HOME/claude-tabstatus`, or
`$HOME/.local/share/claude-tabstatus`; `install --tree <dir>` puts it anywhere
else. One code path, in a checkout, from a release download and on a bare VM alike
- there is no second verb and no mode. A directory under `skills/` containing
`.claude-plugin/plugin.json` auto-loads, so there is no marketplace entry and no
`enabledPlugins` line; there is deliberately no `SKILL.md`, so the plugin costs
essentially no model context.

It used to be a symlink **to the clone**, and that had to change:

- `git checkout` of a working branch whose `hooks.json` is broken broke **every
  prompt in every running session**, instantly. Deployment must be an explicit act,
  not a side effect of changing branches.
- `git pull` did something subtler and worse, because `bin/` is gitignored: you got
  the **new** `hooks.json` live at once while `bin/` still held the **old** binary -
  new hook edges pointing at a binary that had never heard of them. One rebuild plus
  one `install` moves both together, so that state is unreachable.

Nothing in the checkout is written by any install path, and the test suite asserts
that as a byte comparison after every install it performs. `bin/` in a checkout is
still the binary you *run*; it simply stops being the binary *hooks* run.

Shipping one file instead of a tree costs little on the wire, provided it is
compressed: the raw binary is 680 KB against the old four-entry tarball's 303 KB,
and `gzip -9` of it is 334 KB, so copying it to a remote machine with `scp -C`
costs about 31 KB more than the tarball did; without `-C` (scp does not compress by
default), 2.2x.

### How the generated tree is written

**`include_str!` rather than a crate.** It is a std macro, it costs no dependency,
and it is the direct analogue of Go's `//go:embed`: the JSON becomes a rustc build
**input**, so a `cargo build` binary cannot carry a copy that disagrees with the
tree it was built from. `rust-embed` and `include_dir` solve a different problem -
globbing an asset tree and iterating it at runtime - and both are proc-macro crates.
Two `include_str!` lines execute nothing at build time.

**Ownership is positive evidence, never inference.** A tree `install` generated
carries `.tabstatus-generated`; a directory without it is refused rather than
written into:

```text
error: /home/me/notes already exists, is not empty, and carries no
       .tabstatus-generated - so it was not written by `tabstatus install` and is
       not ours to overwrite. Pass a different directory with `--tree`. Nothing has
       been changed.
```

That refusal used to end with a copy-pasteable `rm -rf <whatever you typed>`, and
the subject of that sentence is an argument - so `--tree /` printed `rm -rf /`,
`--tree ~` printed it on the home directory, and a slipped `--tree ..` printed it on
a parent full of somebody's work. Bounding it to directories that looked like ours
was not enough either: by definition the directory carries no marker, so nothing
can tell its files from yours, and "sits beside the default tree" took in
`~/.local/share/claude` and every other sibling there. So no refusal carries a
command now. A directory named `claude-tabstatus` - never `$HOME`, never anything
fewer than three levels down - gets a sentence instead:

```text
error: /home/me/.local/share/claude-tabstatus already exists, is not empty, and
       carries no .tabstatus-generated - ... If it is an old plugin tree that lost
       its marker, nothing here can tell its files from yours: look at what it
       holds, and empty it yourself before re-running. Pass a different directory
       with `--tree`. Nothing has been changed.
```

Further refusals close the remaining doors:

- A directory with a `.git` in it is refused even if a marker appears there.
- A directory **inside** this checkout is refused - the last door by which the
  clone could become the plugin directory again. It looks for a `.git` beside a
  `.claude-plugin/plugin.json` specifically, because plenty of people keep `$HOME`
  itself in git and the default tree lives three levels under it.
- A target under `<config>/skills/` is refused, because `install` would then be
  asked to link a directory to itself.
- A path that exists and is *not a readable directory* is a named refusal rather
  than an errno from `create_dir_all` three lines later, and so is a path that
  exists as a **symlink**: a dangling one answered `NotFound` to `fs::metadata`,
  read as "absent, go ahead", and then failed `File exists (os error 17)` after the
  header had announced a repoint that never happened. A symlink to a real empty
  directory was worse, because it was accepted: the tree landed in the link's target
  and nothing could ever prove which files under it were ours to remove.
- Whether the tree or its nearest existing parent is **writable** is checked in the
  preflight, so a failure lands before the marker exists rather than half way
  through.

**`--tree` is made absolute and lexically normalised once, before anything looks at
it.** Everything downstream compares that path, writes through it and *records* it,
and a raw argument defeated all three. `--tree skills/claude-tabstatus` run from
`<config>` walked straight past the refusal whose job is to keep the tree out of
`skills/`, because that test is a component-prefix test on the string; the symlink
then got the relative string as its target, which resolves against the *link's*
directory rather than the shell's, so the link dangled while the env key was set and
`install` said "Done."; and the record kept the relative string, so a later
`uninstall` run from somewhere else removed files from whatever happened to be named
that there. `fs::canonicalize` is the wrong tool - it resolves symlinks, and it fails
on a path that does not exist yet, which the tree usually does not - so `.` and `..`
are folded textually. A `tree` field in the record that is not absolute can only
come from a hand edit and is ignored rather than resolved.

**The marker is written first, before the binary and before either manifest.** It
is the only evidence of ownership the refusal accepts, so a run killed in that
window - ENOSPC during the 680 KB copy on a small VM, a dropped ssh, an OOM - must not
leave a populated directory with no marker in it: that shape was classified as
somebody else's and refused *forever*, and only `rm -rf` recovered it. One write,
before the files, makes every partial state re-enterable by construction.

**Then the binary, and only then the manifests.** A refresh has no quiet moment, so
one hook event somewhere may see a half-updated pair, and of the two possible
in-between states only one is harmless. A **new binary with old manifests** paints
every edge the old `hooks.json` can name. An **old binary with new manifests** is
the broken one: a new edge word reaches `edge.rs`, which maps what it does not
recognise to `Edge::Unknown` and paints the *idle* glyph, so the tab goes quietly
wrong rather than loudly. On a first install into an empty directory the order is
indifferent, so binary-first is right in both cases.

**Every generated file is rewritten, and stale ones pruned.** Inside a tree it owns,
`install` rewrites every generated file unconditionally and prunes the ones an older
version generated. The alternative - "leave what is already there" - is the upgrade
that silently does nothing: a release adding a hook edge would install cleanly
against an old `hooks.json` and that edge would never fire. A file whose bytes
changed is overwritten *and named*, because a silent revert is the bug even where
overwriting is right:

```text
wrote:    hooks/hooks.json (2842 bytes, REPLACED, was 2851 bytes)
pruned:   hooks/extra.json (generated by an older version)
```

**An identical binary is not copied, and a new one is run before it goes live.** A
re-install whose binary is byte-identical to the deployed one skips the copy and
says `unchanged - identical bytes`, so a no-op install does not rename a fresh inode
over a file thirteen hooks are executing. Otherwise the binary is copied to a temp
file in the tree's own `bin/`, **exec'd there** - `install` refuses if it does not
answer with the expected version - and only then renamed onto `bin/tabstatus`. That
turns a `noexec` mount and a lost exec bit into one refusal at install time instead
of thirteen hooks failing silently in every later session, and the live hook path is
never absent for the length of a 680 KB copy. It does *not* cover a wrong
architecture and does not claim to: the copy is `std::env::current_exe`, so it is by
construction the same architecture as the process running the check.

**Concurrent installs are benign by construction rather than by locking.** Every
write is a same-directory temp file with a pid-suffixed name plus `rename(2)`, so no
file is ever torn, no temp name collides and no path is ever missing. Two runs of the
same version are a no-op; two *different* versions can leave a mixed tree, which one
more install repairs. A lockfile would be disproportionate for a one-user tool.

**Architecture.** Release binaries are x86_64 only. On an aarch64 machine the binary
fails at `exec` with the kernel's own *Exec format error* before a line of this
program runs, so nothing in it can improve that message. `doctor` also compares the
marker's target triple against the running one, which covers a tree materialised by
one architecture and later run by another.

### How doctor and uninstall find the tree

`doctor` and `uninstall` find the tree from **`<config>/skills/claude-tabstatus`**,
the link `install` itself wrote, then from the state record's `tree` field, then from
the default path. Walking up from the running executable is deliberately **not** one
of the answers: `./bin/tabstatus doctor` in a checkout would find
`.claude-plugin/plugin.json` above itself and report that the plugin is this checkout
- the exact wiring being abolished, restated by the tool as fact. The binary you
*run* and the directory Claude Code *loads* are two different things, and only the
config directory knows the second one. The link is followed even when it dangles,
and the record covers what the link cannot: a link repointed or removed by hand
would otherwise leave the tree an orphan nothing can name. `doctor` names an orphan
when it finds one.

`doctor` says where the plugin directory is and how it worked that out:

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

The `binary:` comparison is **unconditional**. It used to be reachable only for a
remotely installed tree, so the commonest real case - `./bin/tabstatus doctor` in
the checkout after a rebuild, asking whether the running build has actually been
deployed - never reached it. `source:` is the other half of that question: do the
manifests in the checkout this binary came out of still match the copies compiled
**into** it? A mismatch means somebody edited `hooks.json` and has not rebuilt, so an
install from there would deploy the older copy. `WARN`, never `FAIL` - a mid-edit
working tree is a normal state and the command people run daily must not cry wolf
over it. `install` prints the same warning at the moment those bytes become live
wiring.

Two byte counts make a drift line actionable without a diff - except when they are
the same number, which is common: 0.1.0 and 0.2.0 are the same length, so a plain
version bump makes `plugin.json` differ at identical size. That reads as `same 438
bytes, different content` rather than `(438 vs 438 bytes)`, which would look like a
bug in the report. The install path says it the same way: `REPLACED - same 438
bytes, different content`.

### Keeping the embedded manifests honest

Three layers, and the worst outcome - a stale embedded `hooks.json` silently
disagreeing with the repository - is caught by all three:

| layer | catches | where it fires |
|---|---|---|
| rustc's rebuild dependency | any build from an edited manifest | `cargo build` recompiles; both JSON files appear in `target/<triple>/release/tabstatus.d` |
| `bin/sources.sha256` | a **prebuilt** binary gone stale against an edited manifest, with nobody rebuilding | `sh tests/run.sh`, which recomputes the manifest |
| `tabstatus print-embedded <plugin\|hooks>` | the bytes themselves, in either direction | `sh tests/run.sh` diffs it against the file; `doctor` reports it on a machine with no source tree |

`print-embedded` writes the embedded bytes to stdout verbatim and nothing else - no
trailing newline of its own - so `tabstatus print-embedded hooks | diff -
hooks/hooks.json` is empty exactly when the two agree. It needs no hasher or
additional dependency.

### Install and uninstall are ordered, both ways

`install` writes the **tree** first, then `settings.json`, then the plugin link
**last**; `uninstall` is the mirror - settings first, the link next, the tree last.

The reason for the last two is the half-state between them: the env key switches
Claude Code's own title painting off and the plugin paints the replacement, so "key
set, plugin gone" is the one combination that paints **no tab title at all**. The
tree goes before both because it is **inert until the link points at it**, so an
abort anywhere before that last step leaves a first-time user exactly as they were.
And the link swap is the only irreversible step - it is the one write that changes
what code a running session executes - so it goes last *and* after the copy in the
tree has been exec'd, which is what proves the new target works.

The link is repointed with a temp link and `rename(2)`, not `unlink` then `symlink`:
`rename` over a symlink is atomic, so the name resolves to the old target or the new
one and never to *nothing* - a hook firing in that window would exec a missing file,
and a non-zero `PreToolUse` hook **blocks a tool**. Together with the exec'd
temp-then-rename binary, this is what makes `install` safe to run while sessions are
live. The link handles the three shapes that can be in its way distinctly: an
existing symlink elsewhere is repointed and its old target recorded, a *broken*
symlink is reported as broken and replaced, and a real directory is refused outright.

Both halves preflight every refusal - the tree's ownership and writability, the
`skills` directory and its writability, `settings.json`'s shape, mode and parent, a
`settings.json` symlink that does not resolve, and whether there is a state record
proving the key is ours - so nothing between the writes can decide to stop, and every
refusal still honestly ends **"Nothing has been changed."** A `settings.json` with
duplicate members at the top level or inside `env` is refused too: this tool resolves
first-wins and `JSON.parse` resolves last-wins, so editing it could set a key Claude
Code never reads.

What is preflighted cannot fail between the writes; what is left is a full disk or a
tampered tree, and both land at the **first** write - directly under a header that
may have just announced that the live plugin link "will ->" somewhere new. The bare
OS error alone leaves the only question that matters unanswered, so the failure
answers it:

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

`install` copies the running binary (`std::env::current_exe`) into the tree, so it
can never deploy hooks that point at a missing binary. `install --force` does not
mean "install anyway, I am about to build"; it means only "write settings.json
even where its Windows permissions cannot be kept" (see the settings.json notes
below).

`~/.claude/claude-tabstatus.state` is a small JSON record in two halves: what was
there before (written once, never rewritten) and which tree this install owns
(rewritten every install, because `--tree` moves it). `uninstall` removes it.

### The settings.json splice

The env key is not optional: Claude Code repaints its own terminal title roughly
every 960ms, straight over ours, and a plugin cannot set environment variables, so
the switch has to live in `settings.json`.

That file is edited as a **text splice, not a reprint**: it is parsed only to find
out where the member goes, and then one line is inserted. Every other byte survives
literally - key order, indentation, blank lines, your own escapes - so the diff of an
install is exactly one line and the diff of an uninstall is nothing at all. (The
jq-based shell installer this replaced reprinted the document, which reindented
hand-formatted files.)

Before that, it copies `settings.json` to `settings.json.cctab-preinstall`; after it,
the spliced text must parse *and*, with our one key removed from both documents,
compare structurally identical to the original - same keys, same order, same values -
or nothing is written. The file keeps its mode (a `settings.json` locked to 0600
stays 0600, and a 0644 one stays 0644: the temp file is created with the original's
mode rather than the umask's), a `settings.json` that is a symlink stays a symlink
with its target updated, and a read-only one, an unparseable one, or one whose top
level is not an object is refused rather than quietly overwritten. On Windows,
where there is no mode, it keeps its **ACL**: the temp file is created open to its
owner alone and gets the original's DACL - its entries, and whether it inherits -
before a byte is written; its owner and group are carried where no privilege is
needed. The backups get the same DACL, since they hold the same secrets.
`--restore-backup` keeps the live file's ACL, or takes the backup's if there is no
live file. An ACL `install` or `uninstall` cannot read is refused up front, with
nothing changed, and so is a `settings.json` on a filesystem that keeps no Windows
ACL (a symlink into a WSL share, where a rewrite from Windows would turn 0600 into
0644): edit that one from the system it lives on, or pass `--force` to `install`
or `uninstall` to write it anyway after a warning. `--force` lifts only that
refusal; an ACL that exists but cannot be read stays refused. Integrity labels and
auditing entries (the SACL) are not carried. A new `settings.json` inherits from
its directory.

### What uninstall removes, and what it declines to

**It removes the plugin tree by default**, because nothing else owns it: leaving it
behind leaves a whole plugin directory in `~/.local/share` that nothing will ever
mention again. `--keep-tree` opts out and names the `rm -rf` that finishes the job,
or, when the tree holds anything else, the generated files to delete instead.

It is conservative, and two rules make removing it by default defensible. Only a
directory carrying `.tabstatus-generated` is touched at all - a link still pointing
at a **checkout** is named and left alone, because that is somebody's source - and
within a tree it does own, only the files the marker lists plus the directories
those leave empty. Anything else is **named and kept**:

```text
tree:     removed 3 generated files from /home/me/.local/share/claude-tabstatus, which was
          left in place because it holds 1 file nothing here generated: NOTES.txt
```

It removes the **live** tree, the one the link points at, and nothing else. An
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

That command - for an orphan, after `--keep-tree`, and for the tree `install` just
moved away from - is offered only for a tree holding nothing but what its marker
lists, read without following a link and read whole. Anything else in there, and
the report names it, says not to delete the directory, and lists the generated
files to delete instead, the marker last so an interrupted clean-up is still a tree
`install` recognises:

```text
tree:     /home/me/.local/share/claude-tabstatus was kept (--keep-tree).
          It also holds 1 file nothing here generated: NOTES.txt, so
          do NOT delete the directory - delete only these, the marker last:
            /home/me/.local/share/claude-tabstatus/.claude-plugin/plugin.json
            /home/me/.local/share/claude-tabstatus/bin/tabstatus
            /home/me/.local/share/claude-tabstatus/hooks/hooks.json
            /home/me/.local/share/claude-tabstatus/.tabstatus-generated
```

There is no `remove_dir_all` anywhere on a path this program derived from a link.
Prune only ever removes what the marker lists, so a few refresh runs teach you by
behaviour that your own files are safe in that directory, and `uninstall` keeps that
promise rather than breaking it at the last moment. If you want the whole directory
gone including your own files, `rm -rf <the path it just named>` is the honest
instruction - and when the directory survives with only empty subdirectories in it,
it says exactly that rather than claiming the tree "is now empty". A tree it could
not read whole - one past its bound on entries, or with a directory it may not list -
gets no command at all.

**A marker-listed path is checked on disk, not just as a string.** The marker is
plain text in a directory anything able to write the tree can edit, so "we wrote it"
is not a bound on the blast radius. `safe_relative` rejects `..`, a leading `/` and
`.`, but a string made entirely of ordinary components still resolves through
whatever is on disk: with `<tree>/bin` replaced by a symlink, `bin/tabstatus` named a
file in *somebody else's* directory, and both operations reached it - `remove_file`
unlinked it, and the atomic write, whose temp file is created in the destination's
parent, created and renamed inside it. One helper is now the only way those paths are
built: the tree root and every directory component of the path must be a real
directory, and a component that is a link is refused by name. Missing components are
created one at a time with `create_dir`, never `create_dir_all`, which would accept
an existing symlink-to-directory as "already there" and reopen the hole from the
write side. The path's *ancestors* are deliberately not checked - `~/.local` is a
symlink on any machine with a dotfile manager - because what this defends is the
boundary of the tree, not the route to it.

```text
tree:     LEFT a file the marker listed - /home/me/.local/share/claude-tabstatus/bin is
          not a directory, so bin/tabstatus is not provably inside the plugin tree
          /home/me/.local/share/claude-tabstatus - following it would write to, or
          unlink, a file outside. Refusing.
```

Pruning also takes the directories it empties. An older version's `old/legacy.json`
left an empty `old/` that no later marker lists, so `remove` never took it and the
tree could never come down.

**Uninstall is an undo, not a delete**: it puts back whatever
`claude-tabstatus.state` says was there before. If you had already set
`CLAUDE_CODE_DISABLE_TERMINAL_TITLE` yourself, your value comes back byte for byte -
the state file records the value's original *text*. If that record is missing and
the key is present, the key is left alone unless you pass `--force`, because there is
then no way to tell it apart from your own setting. The one recorded thing it
declines to restore is a prior link target that is a checkout; see
[Migrating from a checkout symlink](#migrating-from-a-checkout-symlink).

Being right about that key can still leave you with a blank tab, and it says so. If
the record shows the key was already set to something Claude Code reads as "do not
paint the title" *before* `install` ran, `uninstall` correctly keeps your value - and
unlinks the plugin that painted the replacement in the same run. That used to be
reported as a neutral `unchanged`:

```text
settings: env.CLAUDE_CODE_DISABLE_TERMINAL_TITLE already holds the value install found - unchanged
          NOTE that value switches Claude Code's OWN title painting off, and the
          plugin that painted the replacement is unlinked below - so nothing will
          paint the tab. It was already set when install ran, so claude-tabstatus keeps
          it; unset it yourself in /home/me/.claude/settings.json if that was not deliberate.
```

It also removes the per-session records - `records:  removed
/run/user/1000/claude-tabstatus (2 record(s))` - the only other thing the running
plugin leaves on disk. Taking a still-live session's record is harmless: a session
with no record paints exactly what it painted before this plugin existed, and by that
point the plugin is unlinked so no hook will write another. It draws the same line
the reaper does: if `CCTAB_STATE_DIR` points at a directory holding anything else,
only the records go and it says so. Its cleanup is name-based and broader than the
reaper's; see [time and persistence](state-contract.md#time-and-persistence).

Two known defects in this area are reproduced and deferred; they are listed with
their fixes in [AGENTS.md](../AGENTS.md) and summarised for users in the README.

### Native Windows

The same verb, `tabstatus.exe install`, and the same tree; the differences are in
the primitives.

- **The plugin link is a directory junction** rather than a symlink, so it needs
  neither Developer Mode nor an elevated shell. `install` makes and removes a probe
  junction before its first write, so a volume that cannot hold one is refused with
  nothing changed. Repointing the junction is one rename on NTFS, atomic as on Unix;
  only a filesystem that refuses that falls back to a rename-aside.
- **The tree's binary is `bin\tabstatus.exe`.** `hooks.json` still names
  `.../bin/tabstatus`, and Git Bash - which Claude Code requires on Windows and runs
  hooks through - resolves that to the `.exe`.
- **Windows will not replace a program while it runs**, so a re-install during a
  hook renames the running binary aside first: there are two renames' worth of time
  with no `bin\tabstatus.exe`, and the aside copy is named when it is made and is
  deleted - and named again - by a later `install` or `uninstall` once nothing runs
  it.
- **`uninstall` run by the tree's own binary is refused**, because that file cannot
  be deleted while it runs: run it from another copy, or pass `--keep-tree`. An
  `uninstall` that meets a hook running the binary names the file it could not
  remove and keeps the tree's marker, so a later `install` still reuses the tree.
- **Paths are compared the way NTFS compares them** - any letter case, an 8.3 short
  name or a `\\?\` prefix is the same directory - and a removal hint is a PowerShell
  `Remove-Item -Recurse -Force -LiteralPath '...'` rather than `rm -rf`.

Wait ownership and the background indicator work as on Linux, with the record under
`%LOCALAPPDATA%\claude-tabstatus` and the same lock guarantees (see
[Locking and atomic writes](#locking-and-atomic-writes)); `doctor` shows the record
and each session's liveness.

### Migrating from a checkout symlink

Older versions linked `~/.claude/skills/claude-tabstatus` to the clone. If it still
points there, one `install` moves it, and it **says so before it writes anything**:

```text
plugin:   /home/me/.claude/skills/claude-tabstatus
          now  -> /home/me/code/claude-tabstatus (a checkout, not a generated tree)
          will -> /home/me/.local/share/claude-tabstatus
          That checkout stops being the live plugin. Its hooks.json and
          plugin.json are SOURCE from now on; `tabstatus install` is what
          deploys them. Nothing in it is modified.
```

A silent repoint of live wiring is the wrong behaviour whatever it is for. `doctor`
names the same thing, which is how it gets discovered - `doctor` is what you run when
a tab misbehaves:

```text
plugin:    WARN /home/me/.claude/skills/claude-tabstatus points at a CHECKOUT, not at a generated tree:
           /home/me/code/claude-tabstatus
           That is the wiring from before the plugin directory became build
           output - a `git checkout` there changes what every running session
           runs. `tabstatus install` repoints it at a generated tree.
```

It is safe to run while sessions are live, for the reasons in
[Install and uninstall are ordered](#install-and-uninstall-are-ordered-both-ways):
the atomic link rename, and a binary exec'd before it becomes live.

One thing an uninstall will **not** do afterwards: put the checkout link back. The
state record's `symlink_before.target` is written once, at the first install, and on
a machine that was wired the old way it names the clone - so restoring it would
rebuild exactly the arrangement this change exists to abolish. It is declined by
name:

```text
symlink:  removed /home/me/.claude/skills/claude-tabstatus
          (was -> /home/me/.local/share/claude-tabstatus)
          the recorded prior target was the checkout at /home/me/code/claude-tabstatus;
          a checkout is no longer a plugin directory, so the link is
          removed rather than pointed back at it.
```

That write-once record is also why `install` no longer promises an undo it cannot
deliver. When it replaces a link it did not create it used to say "uninstall puts the
old target back." - true on a *first* install, which is the run that writes the
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

### A deleted tree behind a live link

The two live effects differ, and `doctor` names both:

```text
tree:      /home/me/.local/share/claude-tabstatus (from the plugin symlink)
           FAIL the plugin directory is not there.
           A session already running has its hooks registered and now execs a
           missing file - 127 per event, and a PreToolUse 127 can block a tool.
           A NEW session loads no plugin at all and the tab stays BLANK, with no
           error anywhere, because env.CLAUDE_CODE_DISABLE_TERMINAL_TITLE is still set.
           `tabstatus install` writes it back.
```

`install` heals it *in place*: the link already names where the tree belongs, so the
tree is rewritten there and the link is not touched. An `install --tree <somewhere>`
you chose once is not silently abandoned for the default path.

## The three axes

Three things decide what a paint looks like and where it goes, and they are
independent: the **platform** this binary was built for, the **surface** - the leaf
terminal drawing the tab - and the **multiplexer**, if any, between them.
`tabstatus doctor` prints all three as one fixed-column capability table, so a
report from Linux and a report from Windows can be diffed against each other:

```text
platform    linux                        a record carries its session's origin as `p <pid> <start>`
  session terminal      ok    CLAUDE_PID=4242 - session-start and session-end write it directly
  record lock           ok    a record is written under an exclusive lock proven to hold that same file
  state dir             n/a   no CCTAB_STATE_DIR and no XDG_RUNTIME_DIR
surface     konsole                      Konsole, measured on a running terminal
  evidence              ok    $KONSOLE_VERSION
  elide                 left  the tab label is cut from the left, so a glyph goes last
  title (OSC 0)         ok    icon name and window title together
  arm / restore         ok    ESC]50;LocalTabTitleFormat=%w;RemoteTabTitleFormat=%w BEL
                              back to ESC]50;LocalTabTitleFormat=%d : %n;RemoteTabTitleFormat=(%u) %H BEL
multiplexer tmux
  outer title           ok    it re-renders its own format on a timer, which is what lets a glyph decay
```

The verdict column has exactly five words - `ok`, `n/a`, `off`, `?`, `fail` - and
each one means something different. `n/a` is "this cannot, ever"; `off` names a knob
of **ours** you can turn back on; `?` is "the terminal may or may not honour it and
nothing we can read says which", which is what Windows Terminal's
`compatibility.allowOSC777` and `profiles.suppressApplicationTitle` force; and `fail`
is an attempt the OS refused. Escape bytes are **named, never written** - doctor is
read in the terminal whose tab is misbehaving.

Each surface line says how far its row should be trusted, because six of the fourteen
have never had a byte delivered to them by this program. They exist because
`CCTAB_TERMINAL` can name them over ssh, and their table can be read without them:

```sh
tabstatus doctor --surface windows-terminal
```

That spelling needs no terminal, no session and no config directory - every input is
compiled-in data - and prints the surface axis alone, in the same columns, so it
diffs cleanly against the block inside the full report. The names are the fourteen
`CCTAB_TERMINAL` accepts: `unknown`, `konsole`, `vte`, `kitty`, `alacritty`,
`wezterm`, `foot`, `ghostty`, `xterm`, `iterm2`, `apple-terminal`,
`windows-terminal`, `conhost`, `vscode`.

## Konsole arming

Konsole's stock tab format is `%d : %n`, not the shell-supplied title. On
`SessionStart` the plugin sends an OSC 50 property change setting that tab's title
format to `%w`, and on `SessionEnd` sets both formats back to Konsole's defaults
(`%d : %n` and `(%u) %H`). Konsole applies profile properties per tab, at runtime,
in memory, and never inherits them into new tabs or writes them to disk. OSC 50
means "set font" in xterm, so it is sent only when Konsole is detected
(`KONSOLE_VERSION` or `KONSOLE_DBUS_SESSION`, outside tmux and screen) or
`CCTAB_TERMINAL=konsole` says so. Konsole arming is Linux-only: Windows has no
console form of it. The user-facing rules, including the ssh case, are in the README.

Konsole's elide direction is not configurable: it is
`QTabBar::setElideMode(Qt::ElideLeft)` at one hardcoded call site, with no config key
and nothing a Qt stylesheet can override. That is why the glyph goes last there.

## tmux integration

Inside tmux the tab stops being a latch and becomes **a function of time**.

An OSC 0 written inside a tmux pane never reaches the outer terminal. tmux stores it
as that pane's `pane_title` and emits a title of its *own*, computed from
`set-titles-string`, and it recomputes that title on a timer. `%s` - the epoch - is
available inside a tmux format, so if the paint carries the moment it happened, the
format can render the live glyph while the paint is fresh, the idle glyph once it is
stale, and nothing at all once it is old, unless background work is known. **No
process runs, no hook fires and nothing is notified**; the only input that moved is
tmux's own clock. That is worth more than convenience: the wrong titles a missing
hook leaves behind (an edge that paints with no matching un-paint) age out inside
tmux, while known background stays visible.

### Delivery

All tmux paints write raw OSC directly to the pane's verified terminal through
`/proc/$CLAUDE_PID/fd/1`, so tmux integration is Linux-only. Claude Code 2.1.274
wraps hook `terminalSequence` OSCs in tmux passthrough, which bypasses `pane_title`;
using that JSON delivery path would leave the startup idle record unchanged. A
missing or redirected terminal is a silent no-op. The non-tmux JSON delivery path is
unchanged.

### The carrier

Inside tmux the payload stops being a tab title and becomes a **record** in the pane
title:

```text
<location> ct1 <state> <epoch>          state = w (working) | a (waiting) | i (idle)
~/code/one ct1 w 1790443548
<location> ct2 <state> <epoch>          state = p (background) | W / A (working / waiting with background)
~/code/one ct2 p 1790443550
```

`SessionStart` installs formats that understand both versions before publishing a
carrier, so old panes remain readable; compatibility is covered in the
[indicator policy](indicator-semantics.md#compatibility-and-delivery).

The glyph is not in the record. `#{=1:}` counts *columns* and returns the empty
string for a width-2 emoji, so a glyph cannot be sliced back out of a title at all;
the glyph's position already varies with `CCTAB_GLYPH_POS`; and leaving it in would
paint the current pane's glyph twice, once in the strip and once in the label. A
state *letter* is one ASCII column, and the glyph it stands for is looked up in a
server option the same `SessionStart` wrote.

The outer title loop is `#{W:#{P:…}}` and not `#{W:…}`, because `#{pane_title}`
inside a window loop reads only that window's *active* pane, so two claudes split in
one window would otherwise show one cell. The session loop `#{S:…}` is deliberately
absent: one terminal tab shows one attached session, and looping every session would
put another tab's claudes into this tab's title.

Anything else that sets the pane title - a shell prompt, `vim`, `ssh` - overwrites
the record, and that pane's cell disappears until the next paint. Conversely a pane
title that *ends in* a well-formed record forges a cell: harmless, and the price of
using the title as the carrier instead of a tmux option, which would have cost
2.84ms on every tool call.

### What is configured at runtime

`SessionStart` configures the outer title in one `tmux` batch, then reads and
decorates its window's two status formats:

```text
set -s @cctab_gw/@cctab_ga/@cctab_gp/@cctab_gi     the four glyphs
set -s @cctab_tw/@cctab_ta/@cctab_tg     the three TTLs, in seconds
set -s @cctab_title                      the generated strip-and-label format
set -s @cctab_string                     the set-titles-string we installed
set -s @cctab_window_strip               the generated strip for one window
set -s @cctab_window_color               one status color for a themed window cap
set -t <our session> @cctab_armed        the surface this session armed, or `-`
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

`@cctab_armed` is the odd one out, and deliberately: it is a **session** option
rather than a server one. Every other option feeds `set-titles-string`, which is
server-wide and genuinely is "the last `SessionStart` wins". An arming is not - it
goes to the ptys of the clients attached to *one* session, so two tmux sessions on
one server are two different outer tabs, and a server-wide record would let either
one's `SessionEnd` erase the other's. It holds the surface's name, or `-` when that
`SessionStart` armed nothing, and `SessionEnd` reads it back instead of guessing
from its own environment - so a `CCTAB_TERMINAL` that changes mid-session no longer
loses the restore. A value naming no surface this build knows reads as **absent**,
never as a different terminal. `tabstatus uninstall` removes it with the hook.

`@cctab_window_color` returns the highest-priority visible state across all Claude
panes in the window (orange > blue > purple > white), sharing the strip's carrier
recognition and expiry rules, including background's protection from expiry; empty
means no visible Claude state.

**Options, not spliced text.** The glyphs and the TTLs travel as *options* rather
than as text spliced into the format, because an option's value is substituted
**literally**: measured, `#{host}` and `%H` reach the tab intact through `#{@opt}`,
where the same bytes written into the format itself would have been expanded. That
makes a hostile `CCTAB_GLYPH_*` inert. A pane title is never re-expanded either, so a
path holding `#{host}`, `#(…)`, `}`, `,`, `|` or `:` arrives as itself - and `#(…)`
is not a command-execution vector: tmux defangs it to `_(` when it *stores* the
title. (`select-pane -T`, which this plugin never uses, expands its argument at set
time and must never carry a location.)

**The hot path execs nothing.** Only `SessionStart` runs `tmux`, and `SessionEnd`
only in Konsole mode; ordinary paints write the pane carrier without invoking `tmux`.

**What is deliberately not set.** `SessionStart` does not turn on `status`, set
`status-interval`, or add terminal features, although decay depends on the first two.
Turning the status line on would run a user's `#(…)` in `status-right` on our
schedule, and guessing at somebody else's terminal features is not ours to do.
`doctor` reports all three with a one-line remedy. The measurements behind the
requirements: a paint reached the outer terminal in 4ms with `status off`, but the
decay stopped; `status on` with `status-interval 0` never decays either; and on
tmux 3.7c, `linux`, `vt220` and `vt100` all reported the `title` feature and got the
OSC 0, although `terminal-features` lists only `xterm*` and `screen*` - only
`TERM=dumb` failed, and it cannot attach at all.

**When does the flip land?** `#{e|>|:age,ttl}` is strictly greater over *integer*
seconds and the record's epoch is truncated to the second, so the earliest possible
flip is **TTL + 1s**, plus up to one `status-interval` on top. Measured with
`status-interval 1` and `CCTAB_TTL_WORKING=5` / `CCTAB_TTL_GONE=12`: white at
+6.01s, gone at +13.02s. Irrelevant at the shipped defaults, and the reason short
TTLs look off by one. `#{e|>=|:}` does exist on 3.7c and would move the flip one
second earlier, which is not worth a change to a format string two tests pin.

**Server-wide options.** The TTLs, `CCTAB_TERMINAL` and `CCTAB_GLYPH_POS` are
server-wide, so the last `SessionStart` on a server wins for every window on it: a
plain `SessionStart` after a `CCTAB_TERMINAL=konsole` one flips the strip back to the
elided end while the Konsole arming stays in force. `doctor`'s `layout: WARN` line
exists because the `title: OK` test cannot catch it (the same `SessionStart` rewrites
`@cctab_string`, so those two always agree). A per-session policy would need the
deadlines carried in the record instead.

### Save and restore

For each decorated window, `SessionStart` saves the original normal and current
status formats independently, including whether each was explicitly local or
inherited. Repeated starts do not replace those backups. While installed, each
decorator uses the saved label format; changing a global format takes effect in that
window after the decorator is removed. `uninstall` restores an originally local
format exactly, including an empty value, and unsets an originally inherited one so
inheritance resumes. If saved metadata is incomplete, or an edited format still uses
the plugin's saved values, it reports that it could not finish restoration and keeps
the shared tmux options rather than removing data the label still needs.
`uninstall` restores tracked windows across every session on the tmux server. If you
replace either decorator yourself, later starts and uninstall preserve your
replacement. Ordinary windows and global window formats are not changed.
`SessionEnd` clears its pane's indicator; decorators remain until uninstall so other
Claude panes in the same window keep working.

`SessionStart` copies your own `set-titles` and `set-titles-string` into
`@cctab_prev_titles` / `@cctab_prev_string` **before** it overwrites them, and only
if nothing is saved yet - so a second claude starting later cannot record *our*
string as yours. tmux serialises commands on one event loop, so the save and the
install are one atomic batch; measured, thirty concurrent `SessionStart`s left the
saved value intact.

A saved string that points at `@cctab_title` is **not** put back, and uninstall says
so instead of claiming a restore. That is not hypothetical: pinning the output of
`tabstatus tmux-format` into your own `~/.tmux.conf`, which the README offers as an
option, means the first `SessionStart` saves *our* string as yours, and restoring it
after `@cctab_title` has been unset renders the empty string, leaving the tab title
permanently blank until the server restarts. The test is the whole `@cctab_`
namespace, because a server whose last `SessionStart` used the other glyph position
holds a different string of ours pointing at the same option.

`SessionEnd` deliberately does not restore them: the options are server-wide, and
another claude window may still be painting through them. If the saved value was
tmux's own compiled-in default, the option is left *unset* rather than pinned to a
string a later tmux may change. A tmux server restart loses the installed format and
the saved values together, which is self-consistent: nothing to restore, nothing left
behind.

### Konsole over ssh into tmux

`KONSOLE_*` does not survive an ssh, so in the topology this exists for - Konsole →
ssh → tmux → claude - there is nothing to detect, and `CCTAB_TERMINAL=konsole` says
it explicitly. The OSC 50 arming then goes to **each attached client's pty**, named
by `tmux list-clients -F '#{client_tty}'`, never to our own pane (where tmux would
swallow it).

That route was chosen over `allow-passthrough`, which does work - `on` passes the
active pane, `all` also passes background windows, every inner ESC has to be doubled
- but turning it on lets any program in any pane write arbitrary bytes to your
terminal, which is not a decision a tab-title plugin should be taking for you.

**A reattach is re-armed.** An arming sent while *detached* reaches nobody, and on
reattach tmux replays the title but never the arming - measured, a reattached tab was
governed by `RemoteTabTitleFormat=(%u) %H` again and the glyph was invisible. So in
Konsole mode `SessionStart` also installs a `client-attached[1971]` hook that runs
`tabstatus tmux-arm` with the attaching client's pty. Measured on a reattach: the new
terminal receives `CSI 22;0;0t`, the current title, and the OSC 50 arming 1ms later.
The hook is set on *our session*, not globally, so a tab whose tmux session holds no
claude is never armed; it is removed with the restore at the last `SessionEnd`, and
by `uninstall`.

The path travels in `@cctab_exe` rather than inside the hook's text, because a hook
value is parsed when it is *set*: measured, `$rd` inside tmux's double quotes is
expanded there, so a path holding `$` would lose a piece of itself. An option's value
is substituted literally instead - measured, a path holding `#`, `#{host}`, `%Y` and a
space arrived byte for byte. A path holding a single quote has no representation
inside the hook's shell quoting and drops the re-arm, keeping everything else.

The restore is sent only when no *other* claude pane is left in the session, because
the arming is per tab and inside tmux one tab holds every window. Our own pane is
excluded from that count rather than relied on to have been cleared already.

### screen and nested tmux

`$STY` gets nothing: screen's title machinery has no arithmetic and no strftime in
its format, so there is no decay to buy. When both `$TMUX` and `$STY` are set, tmux
wins - tmux-inside-screen is the plausible order, and then the record is right.

In nested tmux the inner server owns `$TMUX`, so that is the one configured, which
is correct. With our own string installed on the outer server too, measured: the
inner tmux re-emits its own *rendered* title into the outer pane's `pane_title`,
which no longer matches the record pattern, so the outer tab loses that window's cell
and its label falls back to `session:index:window`. The inner tmux's own tab - if it
has one - is the one that paints. `doctor` cannot see an outer
server from inside and does not claim to.

## TTL rationale

The defaults are `CCTAB_TTL_WORKING=1200`, `CCTAB_TTL_WAITING=900` and
`CCTAB_TTL_GONE=3600`. The tmux decay uses all three; the state record uses
`CCTAB_TTL_WAITING` for wait expiry, on its own clock (see the
[contract](state-contract.md#meaning-of-state-and-output)).

`working` is repainted by every tool call, so its TTL only has to outlast the longest
ordinary gap between two paints - one long tool call, one long thinking phase - and
decaying sooner would lie in the "it finished" direction. `waiting` is a *summons*:
it gets exactly one paint and is never refreshed, so it has to survive a coffee
break. `idle` needs no TTL of its own, because white is already what the other two
decay *into*. The disappear horizon is shared: once a cell has whitened it says the
same thing whatever it decayed from. Purple has no TTL: elapsed time cannot prove
background work ended.

`CCTAB_TTL_WORKING` is **measured**, not guessed. `working` is repainted by
`UserPromptSubmit`, `PostToolUse` and `PostToolUseFailure` only, so during one long
tool call nothing paints at all. Over the 91 most recent real transcripts on the
development machine - 7709 within-turn gaps between two successive `working` paints:

| p50 | p95 | p99 | p99.9 | max | over 300s | over 1200s |
|---|---|---|---|---|---|---|
| 9.9s | 77s | 173s | 646s | 27751s | 39 (0.51%) | 4 (0.05%) |

`300` - the first default - whitened a genuinely working tab about once in every 200
gaps, which is the *damaging* direction: "finished, come back" about a build that is
still running. `1200` covers 99.95% of them, and the four above it are 3343s, 5677s,
23700s and 27751s - sessions resumed the next day rather than a tool call still
going. A lingering blue is the cheap lie: the next paint corrects it in seconds, and
the 3600s disappear horizon still catches a genuinely stuck one.

## Performance

**Startup.** The Linux binary is musl static-pie: it starts in 243us against the gnu
build's 556us, below even `/bin/true`'s 312us, because it never enters `ld.so`.
(The comment in `scripts/build.sh` records a second best-of-N run: 211us against
528us over a 319us exec floor.)

**Response.** After you answer a dialog, `PostToolUse` turns the tab blue again in
24-42ms. Konsole repaints its tab on a ~2s tick, not when the title arrives, so in
Konsole that tick, not the hook, is the responsiveness ceiling.

**The state layer.** Remembered numbers did not reproduce: a per-edge cost of tens of
microseconds against a fork-and-exec floor of several hundred is below the noise of a
desktop that is also doing something else, and an earlier harness that forked three
times per invocation put an 1100us floor under a 500us measurement. So the numbers
live in a script:

```sh
sh scripts/bench-state.sh                      # the layer against itself, switched off
sh scripts/bench-state.sh <baseline-binary>    # ...and against another build
```

It interleaves every arm within each round, reports the spread as well as the
minimum, and gives the **median of the per-round paired deltas** rather than a
difference of two minima. Two independent 21-round passes of 200 execs on the
development machine, no baseline argument:

| arm | state I/O | min us | spread | vs layer-off |
|---|---|---|---|---|
| `new-nostate` — the layer switched off | none | 460 | 42 | +0 |
| **`working`, main thread, record already current** | read | 475 | 145 | **+15us** |
| `working`, main thread, a wait held by somebody else | read | 453 | 62 | −8us |
| `working`, a transition | read + write | 479 | 54 | **+22us** |
| `working`, a subagent that owns no wait | read | 436 | 48 | −24us |
| `working`, the subagent that owns the wait | read + write | 446 | 42 | −14us |
| `idle` / `Stop` | read | 474 | 176 | **+16us** |

The negative arms are not noise: an edge that owns no wait, or that holds one
somebody else owns, returns before the location walk and before the emit, so it is
genuinely cheaper than a painting edge. The layer's own cost is +15us on the hot
`working` read and +15 to +22us on a transition, against a ~460us floor - under 5%
there, and under 3% of the 767us a real hook costs including the harness fork. These
numbers predate the Serde parser; re-run the script for current figures.

Write avoidance is cheap to **check** rather than to believe, and it holds: 50
steady-state main-thread `working` edges leave the record's mtime untouched, and a
subagent tool call that owns no wait returns without writing - or creating - anything.
That asymmetry is why `write_if_changed` earns its place: after the first main-thread
tool call of a turn sets the base, every later one in that turn is a read, so
transitions are a handful per turn against hundreds of tool calls.

**tmux.** Before window-list support, one `tmux set-option` cost 2.84ms against a
0.37ms fork floor, and the title-only `SessionStart` batch cost 3.1ms end to end.
Those are historical measurements, not timings for the additional window-format
queries and installation batch, which also happen only at `SessionStart`.

**Location.** One `git rev-parse` costs 15-40ms; the fork-free location was measured
at about 0.12ms.

The shell-versus-binary comparison from the Rust port is in
[docs/history.md](history.md).

## Build choices

- **musl, not gnu, on Linux.** Faster startup (above), and no `GLIBC_2.34`
  requirement, which matters because the binary's main home is a remote box reached
  over ssh whose glibc you do not control: a gnu build simply refuses to start on an
  older distro. The gnu build is still produced and released.
- **MSVC on Windows, with a static CRT.** `x86_64-pc-windows-msvc` is rustup's
  default there and needs no mingw, so `x86_64-pc-windows-gnu` is not a target.
  `.cargo/config.toml` sets `+crt-static`, so the `.exe` needs no Visual C++
  Redistributable; CI checks that it imports no Visual C++ runtime DLL. Windows is a
  native port behind `src/sys`, not a cross-compile, so it is built and tested on
  Windows - CI does not ship an `.exe` a Windows machine never ran. Each host's
  `scripts/build.sh` lists only the targets it can build, so `--all` stays a success
  signal.
- **`include_str!` for the manifests** - see
  [How the generated tree is written](#how-the-generated-tree-is-written).
- **`rust-version = "1.89"`**, raised from 1.74 for `std::fs::File::lock` - see
  [Locking and atomic writes](#locking-and-atomic-writes).
- **Serde and serde_json, without `serde_derive`**, parse hook metadata. Application
  code is optimised for size (`opt-level = "s"`); the measured parser dependencies
  (`serde`, `serde_core`, `serde_json`, `memchr`) use `opt-level = 3` for speed. The
  lockfile and `--locked` conventions are in [AGENTS.md](../AGENTS.md).
- **Release binaries are the tested binaries.** The Release workflow publishes only
  binaries that passed the Test workflow: each is checked against the digest its own
  test job recorded, and `SHA256SUMS` is those recorded lines.

## Environment read internally

The user-facing variables are in the README. These are read too:

- **`CLAUDE_PID`** is exported into every hook subprocess, and is how
  `session-start`, `session-end` and tmux paints find the pty - on Windows, the
  console whose title they set - and how a record is stamped with its origin.
- **`XDG_RUNTIME_DIR`** (`LOCALAPPDATA` on Windows) selects the state directory,
  deliberately with no `$HOME` fallback, because that would put a record inside the
  golden corpus's fixture `HOME` and make every case carrying a `session_id`
  order-dependent. That it reaches a *hook* subprocess at all is measured, not
  assumed: a temporary probe build logged what a real `PostToolUse` hook sees, and it
  was `xdg=Some("/run/user/1000")`.
- **`XDG_DATA_HOME`** is read by `install` alone, for where the generated tree goes.
  The runtime half never looks at it.
- **`TMUX`, `TMUX_PANE`, `STY`, `KONSOLE_VERSION`, `KONSOLE_DBUS_SESSION`,
  `SSH_CONNECTION`, `SSH_TTY`, `HOME`, `PWD`, `HOSTNAME` and `GIT_DIR`** are read as
  they are.
- **`CCTAB_NOW`** is test-only: it pins the epoch the tmux record and the state record
  carry.
