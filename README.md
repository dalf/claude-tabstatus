# claude-tabstatus

Puts the Claude Code session state into the terminal tab title, in front of the
location, so a row of tabs tells you which session wants you without losing
track of where each one is.

```text
🔵 streaming-browser@master       working  - Claude is running
🟠 streaming-browser@master       waiting  - Claude needs an answer
⚪ streaming-browser@master       idle     - Claude has stopped
⚪ srv:streaming-browser@master   the same session, over ssh
⚪ ~/code/bug_fedora              not a repo, so the path instead
⚪ ~                              at $HOME
```

Claude Code's own title cannot tell you the second line from the third: it
renders one static glyph whether Claude is thinking or waiting for an answer
from you. Telling those two apart across a row of tabs is what this exists for.

## States

Three states, and the hook events that paint them:

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
| `Notification` | `idle_prompt` | ⚪ idle |
| `Notification` | any other kind | *nothing* |
| `Stop` | | ⚪ idle |
| `StopFailure` | | ⚪ idle |
| `SubagentStop` | | *nothing*, unless that agent owned the wait |
| `SessionEnd` | | clears the title, restores the tab |

The `SessionStart` and `PreToolUse` scopes are hook *matchers*, so those hooks
do not even run outside them. The `Notification` kinds and the `compact` source
are substring tests inside the binary, so those hooks run and then decide -
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
  of that distinction. The cost of the edge is the process
  spawn, about 0.61ms measured, so 900 tool calls in a heavy session cost ~0.55s
  spread over minutes - invisible beside the tool calls themselves. The payload
  carries the whole `tool_response`, hundreds of KB on a large read, and only its
  first 8 KiB is ever searched; the rest is drained unread, so a 4 MB response
  costs 2.4ms instead of the 165ms the shell version spent on 1 MB.
- **`PreToolUse` is matched to exactly the two tools that always block on you.**
  Unmatched, it would paint waiting on every tool call. `PermissionRequest`
  fires for both of those tools anyway, 11-19ms later, so this edge is really
  only insurance for a permission path that gets bypassed.
- **A `Notification` never paints waiting on `idle_prompt`.** That kind is the
  quiet-turn nudge, fired `messageIdleNotifThresholdMs` (default 60s) after a
  turn ends, so mapping it to waiting would turn every idle tab orange a minute
  later and collapse two of the three states into one. It is also the only
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

What the binary paints is a function of the edge name, a handful of substring
tests on the raw payload line, and - for the four edges that can be part of a
wait - one small record per session. Wait ownership, below, is why that record
exists and what is in it; with nowhere to keep it, every edge falls back to the
stateless answer, which is what the whole golden corpus still pins byte for byte.
`Notification`, `SessionStart`, `PostToolUse` and `SubagentStop` look at their
payload for a discriminator of their own, and all of them search only **bounded
windows** of
the first line: its first 8 KiB, plus - for `Notification` alone - its last
8 KiB. Nothing between them is ever searched, so the cost does not depend on what
a tool returned. The back window is not symmetry: a `Notification` serializes
`notification_type` **last**, after the unbounded `message`, so with a front
window alone an MCP elicitation carrying a long message lost its own
discriminator and the tab painted nothing at all. `agent_id` and `source` are
read from the front window only, because they are serialized before anything
unbounded (byte 760 of 1360 and byte 713 of 769 in real captures) and because a
false positive on `agent_id` would silence every `working` repaint for the rest of
the session. `session_id` is the payload's **first** member in every capture, so
it is in the front window by construction. The tests carry the compact `"key":"value"` spelling Claude Code
actually writes, and a payload that matches nothing falls through to painting
nothing, which leaves whatever the tab already showed.

### Wait ownership

The model here is partly taken from
[Yannis-Adn/terminal-addons](https://github.com/Yannis-Adn/terminal-addons) (MIT),
whose `wt-tab-status` keeps one state file per session holding a state *and the
owner of a wait*, and only lets the waiting agent end the wait. `src/state.rs`
credits the exact functions, and records the four places this diverges - the
largest being that its `PermissionRequest` branch no-ops on a subagent's dialog,
which the capture below shows would paint idle while a human is being asked.


A wait is an **overlay on a base**. The base is what the tab shows when nothing
is waiting; a dialog covers it; clearing the *last* dialog restores it. Who
raised a wait is therefore part of the state, and it cannot be recovered from any
one payload - which is why this is the one thing the binary writes down.

The capture that forced it, timings relative to that session's `SessionStart`:

| t | event | `agent_id` | stateless | with ownership |
|---|---|---|---|---|
| 66.283 | `PreToolUse` `tool_name=Agent` | - | 🔵 | 🔵 base `w` |
| 68.946 | `Stop` `background_tasks=[subagent:running:aec99e]` | - | ⚪ | ⚪ base `i` |
| 69.960 | `PermissionRequest` | `aec99e1f` | 🟠 | 🟠 wait owned by `aec99e1f` |
| 75.983 | `Notification` `permission_prompt` | *absent* | 🟠 | 🟠 no second owner added |
| 98.459 | `SubagentStop` | `a8e90c10` | - | *nothing*: owns no wait |
| 107.496 | `PostToolUse` (you approved) | `aec99e1f` | *nothing* | ⚪ the base comes back |
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
| `waiting` | adds this owner (`agent_id`, or the main loop), with its own epoch | 🟠 always |
| `waiting` from a `Notification` | adds an *unknown* owner, and only when nothing is waiting; an owned wait raised while `?` is the only one outstanding replaces it | 🟠 always |
| `working`, a `PostToolUse` on the main thread | base ← `w`; clears the main and unknown waits | the base, or nothing if a wait remains |
| `working`, a `UserPromptSubmit` **you typed** | base ← `w`; clears **every** wait | the base |
| `working`, a subagent | clears the wait it owns, or a lone `?`; base untouched | the base if that emptied the set, else nothing |
| `idle` | base ← `i`; clears a main wait, and **every** wait when `background_tasks` is `[]` | ⚪, or nothing if a wait remains |
| `subagent-stop` | clears the wait it owns, or a lone `?` | the base if that emptied the set, else nothing |
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
  begin with `<` - deliberately broader than the one spelling measured, because
  every injected prompt shape is an XML-ish tag, and the injected event is redundant
  anyway: that agent's own `SubagentStop` fires 20ms earlier.

**An overlay nothing can lift is worse than no overlay at all.** While any wait is
held, neither `working` nor `idle` paints - that is the whole mechanism - so a wait
that outlives its dialog freezes the tab orange, and outside tmux nothing decays it.
Every wait therefore has four independent retirement conditions, and each is a
proof rather than a guess:

| what retires it | why it is a proof |
|---|---|
| the owner's own completion | that agent's `PostToolUse`, or its `SubagentStop` when you declined and no tool ever ran |
| a `UserPromptSubmit` you typed | a modal dialog and a usable prompt cannot both be on screen |
| `background_tasks` empty at `Stop` | the array holds one entry per live subagent - the capture's 68.946 `Stop` lists the very agent that raises the dialog a second later - so an empty one proves nothing outside the main loop is running, and therefore that no subagent's dialog and no unattributable dialog is outstanding |
| its own expiry | `CCTAB_TTL_WAITING`, **per wait** |

The last three exist because the captures are emphatic that *abandoning* a dialog
fires no hook whatsoever: Esc on a live dialog (`s2`), declining one (`s7`) and
Ctrl+C mid-tool (`s5`) each emit nothing at all until the next prompt. Nothing the
owner does can be waited for, because the owner does nothing.

"Absent" is not "empty", and that asymmetry is deliberate in both directions. A
`Notification` carries no `background_tasks` at all, and a Claude Code that renamed
the member would carry none either, so a missing array leaves every wait standing -
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

- An outstanding **wait** older than `CCTAB_TTL_WAITING` (900s - the same knob, and
  the same grammar, that tmux decays an orange title with, so inside tmux the
  record and the title stop lying at the same moment), measured **per wait** against
  that wait's own epoch.
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
long for the reason above.

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

Two details of the lock are load-bearing. It is taken on the record path, and
because `write_if_changed` renames over that path the inode can change under a
waiter - which would leave it holding an exclusive lock on an unlinked inode while a
third hook held the new one - so after locking it checks that it holds the inode the
path names *now*, and retries when it does not. And it does **not** create the file:
with no record there is nothing to lock, but a record that does not exist also holds
no wait, so the update that can still be lost there is a first wait or a base, never
a clear. `std::fs::File::lock` ships in std (1.89), so this costs no dependency; it
is the reason `rust-version` moved from 1.74 to 1.89.

```text
cts1                                           the tag: version 1 of the wire
b i                                            base = w | a | i
p 3709427 84460384                             the session's (pid, start time)
w aec99e1f4bda1972b:1790380630 -:1790380631    one wait per word: owner, then epoch
```

`-` is the main loop and `?` is an unknown owner, which is why an `agent_id` is
accepted only as `[A-Za-z0-9_-]{1,64}` - the same test that stops a `session_id`
from choosing the path it is filed under. Unknown keys are **skipped**, and a base
letter this version cannot paint reads as idle, so a newer version's record
degrades rather than being misread; and a record this version did not *change* is
not rewritten, so it keeps the fields it did not understand.

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

**The edges that used to only drain stdin now read a window, and that had to be
bounded.** `waiting`, `idle` and `session-end` have no discriminator of their own;
they read a payload only because the record is filed under `session_id`. `waiting`
and `session-end` read the **front** window alone - `session_id` and `agent_id` are
both front members. The distinction is not cosmetic, because building a tail means
scanning every byte of the payload for the end of the line, and a
`PermissionRequest` for a `Write` carries the whole file in `tool_input`: the first
version of this read the full window on those edges and cost **+555us** on a 1 MiB
payload, rising with payload size.

`idle` is the one exception, and it is a deliberate trade. It reads
`background_tasks`, which a `Stop` serializes *after* `last_assistant_message`, so a
front window alone loses it on any turn that ended with a long message - and losing
it means every wait stands when it should have been retired, which is the tab staying
orange. It therefore builds a tail: once per turn, not once per tool call, and
measured at +16us on a 2 KB payload and +44 to +53us against a pre-slice build. A
300 KB `Stop` replayed through both binaries is byte-identical.

**Seams, designed and deliberately not built.** `Stop` carries `background_tasks`,
which is `[]` when the session is genuinely idle and otherwise holds
`{id, type:"subagent", status:"running"}` per live agent, where `id` *is* the
`agent_id` of the hooks that agent fires. Half the read is already here: `idle` asks
that array the only question a wait needs - whether it is empty - with one needle and
no parse. Purple needs the **ids**, which is the part not built. A reserved
`g <id>...` line records that set, a fourth base letter paints it, and
`subagent-stop` - already wired - removes the id and repaints the base. That is the
un-paint the purple state needs, and it is the same mechanism above. It also sharpens
the rule above: an agent wait whose id is absent from `g` is stale even while *other*
agents run, where today an empty array is the only signal. A reserved `n <epoch> <text>` line, last in the
record because everything above it is ASCII words, caches the session title
`aiTitle` from a bounded tail read of `transcript_path`. Both keys are already
skipped by this version's parser. `src/state.rs` carries the detail.

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
rebuild, and a new session - the installed plugin is a symlink to the clone, so
there is nothing to reinstall.

The build step is temporary: binaries are not committed, and will ship as
GitHub release assets so that installing needs no toolchain.

**No dependencies.** `bin/tabstatus` is a single binary - one executable, zero
crates, nothing linked but libc - and it is both the hook and the installer. There is no `jq`, no Python, and no shell script left in the runtime
path. Building it needs cargo; running it needs nothing. [Build](#build) covers
the targets.

Avoid changing configuration in other Claude Code sessions while the installer
runs. It reads, merges and renames `settings.json`, and although it re-reads the
file immediately before the rename and aborts if it changed, the safe habit is to
install when nothing else is writing that file.

```sh
bin/tabstatus doctor      # is it linked, is the key set, what would this tab say
bin/tabstatus version
```

`doctor` is the thing to run when a tab is not painting: it reports the plugin
link, the env key, the terminal it detected, which end of the title the glyph
therefore goes on, and the title this directory would render right now.

Its `record:` lines cover [wait ownership](#wait-ownership): where the records live,
how many there are, what each one holds in words rather than in wire format, and
which of them the next `session-start` will reap and why.

```text
record:    /run/user/1000/claude-tabstatus (3 records, 1 stale)
           8e6d6eb9-...: base working, session pid 3709427 live, waiting on 1 (aec99e1f raised 10s ago)
           a1b2c3d4-...: base idle, session pid 4242 GONE, nothing waiting; STALE (pid 4242 is not
           that process any more), the next session-start reaps it
           notes.txt: not a record in any version's shape - treated as absent; not a name this
           writes, so the reaper leaves it alone
```

A record it cannot parse is reported as such rather than as a healthy idle one, which
is what it used to do: an empty file, a binary one, a *newer* version's record and one
too big to be a record each printed the same line as an idle session, so the report
was the wrong place to look when something was wrong.

It also answers the one question every other line renders as healthy - **can a record
be written at all?** A state directory that is readable but not writable records no
wait, so a subagent's `PostToolUse` finds none to clear and paints nothing, and the
tab stays orange until the Task returns. That is the whole defect this layer exists to
fix, back in silence, under a report that says `nothing recorded, which is also what a
session that has raised no dialog leaves behind`:

```text
record:    /run/user/1000/claude-tabstatus (1 record, 0 stale)
           FAIL not writable - no wait is ever recorded, so a subagent's dialog stays
           orange until the Task returns
```

Two things about that report are deliberate. It is **read-only** - the command you
run when something is already wrong must not be the command that deletes the
evidence, so it names the stale files instead of taking them, and the verdicts come
from the same function the reaper calls so the two cannot drift. The one exception is
the writability probe above, which creates and immediately unlinks a file named for
its own pid; that destroys no evidence, and a report that cannot answer the question
is worse than useless. And when there is nowhere to keep a record it says so, with
the reason:

```text
record:    disabled - no CCTAB_STATE_DIR and no XDG_RUNTIME_DIR
           so every edge falls back to the stateless answer, which is the behaviour
           from before wait ownership existed
```

That line exists because "disabled" is the silent answer to almost every question
this layer can raise: a tab behaving exactly as it did before wait ownership is
indistinguishable from a layer that is working correctly.

### install and uninstall are ordered, both ways

`install` writes `settings.json` **first** and the plugin symlink **last**;
`uninstall` is the mirror, settings first and the link last. The reason is the
half-state between the two writes: the env key switches Claude Code's own title
painting off and the plugin paints the replacement, so "key set, plugin gone" is
the one combination that paints **no tab title at all**. Both halves therefore
preflight every refusal - the repo shape, `bin/tabstatus`, the `skills` directory
and its writability, `settings.json`'s shape, mode and parent, a `settings.json`
symlink that does not resolve, and whether there is a state record proving the key
is ours - so nothing between the two writes can decide to stop. A `settings.json`
with duplicate members at the top level or inside `env` is refused too: this tool
resolves first-wins and `JSON.parse` resolves last-wins, so editing it could set a
key Claude Code never reads.

## Uninstall

```sh
~/code/claude-tabstatus/bin/tabstatus uninstall
~/code/claude-tabstatus/bin/tabstatus uninstall --force            # no state record: remove anyway
~/code/claude-tabstatus/bin/tabstatus uninstall --restore-backup   # roll settings.json back wholesale
```

The uninstaller is an undo, not a delete: it puts back whatever
`claude-tabstatus.state` says was there before. If you had already set
`CLAUDE_CODE_DISABLE_TERMINAL_TITLE` yourself, your value comes back, byte for
byte - the state file records the value's original *text*. If that record is
missing and the key is present, the key is left alone unless you pass `--force`,
because there is then no way to tell it apart from your own setting.

It also removes the wait-ownership records - `records:  removed
/run/user/1000/claude-tabstatus (2 record(s))` - which is the only other thing the
running plugin leaves on disk. Taking a still-live session's record is harmless: a
session with no record paints exactly what it painted before this plugin existed, and
by that point the plugin is unlinked so no hook will write another. It draws the same
line the reaper does, though: if `CCTAB_STATE_DIR` points at a directory holding
anything else, only the records go and it says so.

## What it changes

Three things, and nothing else:

1. One key in `~/.claude/settings.json`:
   `env.CLAUDE_CODE_DISABLE_TERMINAL_TITLE = "1"`.
2. A symlink `~/.claude/skills/claude-tabstatus` pointing at this repo. A
   directory there containing `.claude-plugin/plugin.json` auto-loads; there is
   no marketplace entry and no `enabledPlugins` line. There is deliberately no
   `SKILL.md`, so the plugin costs essentially no model context.
3. `~/.claude/claude-tabstatus.state`, a small JSON record of what was there
   before, written once and removed by `uninstall`.

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
glyph once it is stale, and nothing at all once it is old. **No process runs, no
hook fires and nothing is notified**; the only input that moved is tmux's own
clock.

That is worth much more than tmux convenience. The wrong titles in [Known
limitations](#known-limitations) are all the same shape - an edge that paints
with no matching un-paint. Inside tmux every one of them heals itself.

So inside tmux the payload the plugin emits stops being a tab title and becomes a
**record**:

```text
<location> ct1 <state> <epoch>          state = w (working) | a (waiting) | i (idle)
~/code/one ct1 w 1790443548
```

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

### What is configured at runtime

`SessionStart` does it, in **one** `tmux` invocation, so you need no
`~/.tmux.conf` edit:

```text
set -s @cctab_gw/@cctab_ga/@cctab_gi     the three glyphs
set -s @cctab_tw/@cctab_ta/@cctab_tg     the three TTLs, in seconds
set -s @cctab_title                      the generated strip-and-label format
set -s @cctab_string                     the set-titles-string we installed
set -g set-titles on
set -g set-titles-string '#{s|^ ||:#{T:@cctab_title}}'
```

In Konsole mode only, two more - this binary's path, and the hook that re-arms a
reattached tab with it:

```text
set -s @cctab_exe                        this binary, for the hook to run
set-hook -t <our session> 'client-attached[1971]' \
    'run-shell -b "'\''#{@cctab_exe}'\''  tmux-arm '\''#{client_tty}'\''"'
```

`tabstatus tmux-format` prints the last two lines' values, if you would rather
pin them in your own config than have them set at runtime.

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
only in Konsole mode. Measured here: one `tmux set-option` costs 2.84ms against a
0.37ms fork floor, and the whole twelve-command `SessionStart` batch costs 3.1ms
end to end - thirteen commands and 4.9ms in Konsole mode, where it also lists the
clients and writes the arming. The cost is the fork and the socket round trip, not
the commands, which is why batching is free and why a per-tool-call `tmux`
invocation would have been a tenfold regression on a binary that runs in 370µs.

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

- **The tmux status line.** A glyph per window in `window-status-format` uses
  exactly this carrier and the cell expression drops into it unchanged, but the
  target here is the *tab*, and the status line is your real estate.
- **A fourth glyph for background work**, and **OSC 9;4 progress**. The record
  reserves the `g` key for the first, and the state layer's `subagent-stop` edge is
  already the un-painter it needs; the seam is designed in full at the bottom of
  `src/state.rs` and in [Wait ownership](#wait-ownership), and built not at all.
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
- **MCP elicitation is backstop-only.** A server asking the user its own
  question has real-time events, `Elicitation` and `ElicitationResult`, and this
  plugin registers neither (see [Not yet built](#slices)), so the tab turns
  orange only when the `elicitation_dialog` notification arrives about 6s later,
  and only if you have not touched the keyboard in the meantime.
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
- **`session-start` and `session-end` are Linux-only.** They resolve the pty
  through `/proc/$CLAUDE_PID/fd/1`, which macOS and Git Bash do not have, so on
  those platforms Konsole arming does not happen (fine, they are not Konsole)
  and, more importantly, the title is not cleared at the end of a session while
  `CLAUDE_CODE_DISABLE_TERMINAL_TITLE` is set.
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
| `CCTAB_GLYPH_WORKING` / `_WAITING` / `_IDLE` | 🔵 / 🟠 / ⚪ | the three glyphs; empty drops the glyph and its space |
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

`CLAUDE_PID` is exported into every hook subprocess and is how `session-start`
and `session-end` find the pty. `XDG_RUNTIME_DIR` is read for the state
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
path `hooks/hooks.json` invokes. A fresh clone has no `bin/` until you build,
and `tabstatus install` refuses rather than half-installing.

Binaries ship as **GitHub release assets** rather than in git history, so that
installing needs no toolchain. The [Test workflow](.github/workflows/test.yml)
builds both Linux targets and runs Rust unit tests, the shell integration suite
(including tmux), and the golden corpus on every branch push and pull request.
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

**Zero dependencies, std only**, and `rust-version` is **1.89** - raised from 1.74
for `std::fs::File::lock`, which is what makes the state layer's read-modify-write
atomic without a crate. The alternative was a bounded compare-and-retry loop: more
code, and only probably correct.

A digest is written **per triple built**, `bin/sources.<triple>.sha256`, plus an
unsuffixed copy for the host because `bin/tabstatus` is the host binary. A single
unsuffixed manifest covering all sources let a build of only the host leave the other
triple's binary behind while a verify read fully green.

| Target | State |
|---|---|
| `x86_64-unknown-linux-musl` | **default**, 589 KB, static-pie |
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
own title painting off, and all eleven hooks would then resolve to a command that
exits 127 - a tab nothing paints at all, which is strictly worse than no install.
`install --force` overrides it for the case where you are about to build.
Zero crates, so `cargo build` needs no network.

## Tests

```sh
sh tests/run.sh
CCTAB_TEST_BIN=target/release/tabstatus sh tests/run.sh   # a build you just made
```

463 assertions, and what they drive is `bin/tabstatus` - the same binary
`hooks/hooks.json` invokes, so a stale committed binary fails here rather than in
somebody's tab. The suite used to run the shell implementation under three shells
in four locales, because its answer depended on both; a binary has no
interpreter, and since the length cap became locale-independent it has no locale
dependence either, so that whole axis is gone.

Beside the two shell harnesses there are in-crate unit tests, which those
harnesses cannot replace: they pin the argv and environment parsing, the location
walk, the length cap and its elision, the two payload windows and the JSON writers
at FUNCTION granularity, so a refactor can be checked a piece at a time instead of
only end to end. Some of what they assert is invisible from outside the binary at
all - that `repair` answers differently from `String::from_utf8_lossy` on a
truncated sequence, for one.

```sh
cargo test   # 150 tests, beside the 463 assertions and the 312 corpus cases
```

The state section pins `CLAUDE_PID` per case rather than inheriting it, and that is
a gate property rather than tidiness: the layer reads that variable to stamp a
record's origin, so run from inside a Claude Code session - which is how this project
is developed, and the only place a developer would run it - the ambient session's pid
used to land in records two cases assert byte for byte, and the declared gate was RED
in the one environment it is actually invoked from. It is not unset globally, because
the headless-guard and pty sections need the ambient one.

Eighty-one of the assertions are the tmux section, and they drive a PRIVATE
tmux server - `tmux -L cctabprobe -f /dev/null`, killed afterwards, with no
client ever attached, so no pty is touched and the user's own server is never
listed, configured or killed. They are SKIPPED, never failed, where there is no
`tmux` binary. What they cannot see is tmux re-EMITTING the title to an attached
client, which needs a pty this suite cannot allocate; what they do assert is the
whole of the server side, including that re-rendering the same paint after a wait
gives a different answer with no process running and no hook firing.

Two of the assertions exist only to guard the committed binaries: `bin/` carries
a digest of the `src/*.rs` and `Cargo.toml` it was built from, *per triple built*
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

The two payload-reading edges are assertable the same way, with the payload on
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
present, nothing else is registered, eleven hooks exactly, one command per group,
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
allocated pty and compares bytes. And the belt is pinned at its real reach rather than a wished-for
one: `{"source": "compact"}` on one line is caught, the same payload
pretty-printed over three lines is not, because only the first line is read.

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

- The fourth glyph: **background work running, main loop free**. `Stop` carries
  `background_tasks`, so painting it is easy and un-painting it is the problem the
  record already solves. Half the read exists: `idle` already asks that array whether
  it is empty, with one needle. The rest of the shape is designed - a reserved `g`
  line for the ids, a fourth base
  letter, and the `subagent-stop` edge that is already wired - and deliberately not
  built here.
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
- The `Elicitation` and `ElicitationResult` events. An MCP server asking the
  user is a true waiting state, and today it is covered only by the
  `elicitation_dialog` notification kinds, which no session here has been able to
  reproduce. Registering the events directly would be the real signal; their
  match query is the MCP server name, not the kind, so they would go in
  unmatched.
- Konsole `TabColor`, which rides on the same OSC 50 property list as the
  arming and would let the tab itself carry the colour. Whoever adds it also
  has to add `TabColor=#000000` to the `SessionEnd` list, or the colour
  outlives the session.

## Licence

GPL-3.0-or-later. See [LICENSE](LICENSE).
