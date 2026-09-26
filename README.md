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
| `SessionEnd` | | clears the title, restores the tab |

The `SessionStart` and `PreToolUse` scopes are hook *matchers*, so those hooks
do not even run outside them. The `Notification` kinds and the `compact` source
are substring tests inside the binary, so those hooks run and then decide -
which costs one ~0.6ms process on a notification, and buys a decision the test
suite can assert rather than one that lives only in a config file.

Some of that is less obvious than it looks:

- **`PostToolUse` is registered unmatched**, so it runs on every tool call, and
  it is the recovery from waiting: 24-42ms after you answer a dialog the tab is
  blue again. It reads one thing from its payload - `agent_id` - and paints
  nothing when it is there, because a *subagent's* tool call firing in the main
  session must not repaint over a dialog you are looking at. A matcher could not
  do that: a matcher sees only `tool_name`. The cost of the edge is the process
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
- **`SubagentStop` is deliberately not registered.** A subagent finishing must
  not read as the session going idle - and it also fires for the internal
  compaction summarizer, with an empty `agent_type` that no matcher could
  filter, right before a `SessionStart`/`compact`.
- **The `agent_id` filter has a cost, and it is a tab left orange.** The two
  cases it cannot tell apart are both a subagent's `PostToolUse`: one arriving
  while *your* dialog is open (must not paint) and one arriving after you
  answered the *subagent's* dialog (should paint blue). Stateless, there is no
  field that separates them, so the filter takes the safe half and pays for it:
  after you approve a subagent's dialog, nothing repaints until the `Task`
  returns, so the tab **reads orange for the rest of that subagent's run**.
  Registering `SubagentStop` → `working` does not fix it - measured, not assumed:
  a real `SubagentStop` payload carries `agent_id` (byte 760 of 1439), so the
  `working` edge silences it exactly like any other subagent event. An edge that
  skipped the filter would close this window and reopen the worse one, painting
  blue over a main-thread dialog that is still open. Orange-while-working is the
  benign direction - you glance, find nothing to answer, move on - and
  blue-while-blocked is the damaging one, so this is the trade taken on purpose.
- **`StopFailure` is there because `Stop` is not.** When a turn dies on an API
  error - `rate_limit`, `overloaded` - only `StopFailure` fires, so without it a
  rate-limited turn would leave the tab blue indefinitely.

The binary is **stateless**: what it paints is a pure function of the edge name
plus, on three edges, a handful of substring tests on the raw payload line.
Nothing is written anywhere, so nothing is stale after a `kill -9` and there is
nothing to prune. `Notification`, `SessionStart` and `PostToolUse` look at their
payload and nothing else does, and all three search only **bounded windows** of
the first line: its first 8 KiB, plus - for `Notification` alone - its last
8 KiB. Nothing between them is ever searched, so the cost does not depend on what
a tool returned. The back window is not symmetry: a `Notification` serializes
`notification_type` **last**, after the unbounded `message`, so with a front
window alone an MCP elicitation carrying a long message lost its own
discriminator and the tab painted nothing at all. `agent_id` and `source` are
read from the front window only, because they are serialized before anything
unbounded (byte 760 of 1360 and byte 713 of 769 in real captures) and because a
false positive on `agent_id` would silence every `working` repaint for the rest of
the session. The tests carry the compact `"key":"value"` spelling Claude Code
actually writes, and a payload that matches nothing falls through to painting
nothing, which leaves whatever the tab already showed.

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
environment and the session is not inside tmux or screen.

**Windows Terminal needs no configuration**, and neither does any other
terminal that honours a plain OSC 0 title.

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
  the damaging direction: a tab claiming it needs you when nothing does.
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

Binaries are meant to ship as **GitHub release assets** rather than in git
history, so that installing needs no toolchain. That is not wired up yet.

```sh
sh scripts/build.sh          # the host target, refresh bin/ and the digests
sh scripts/build.sh --all    # every target in the list
```

| Target | State |
|---|---|
| `x86_64-unknown-linux-musl` | **default**, 533 KB, static-pie |
| `x86_64-unknown-linux-gnu` | builds, 441 KB |
| `x86_64-pc-windows-gnu` | **does not build**, see below |

Windows is deliberately **not** in the build script's target list. The target and
its mingw linker are both installed here and the failure is not theirs: the source
is Unix-only by construction. 28 compile errors across five source files, all of
them `std::os::unix` - byte-oriented paths (`OsStrExt`), file modes, symlinks, and
the `/proc/$CLAUDE_PID/fd/1` lookup the two direct-write edges need. Windows has no
byte paths at all (its `OsString` is WTF-16), so this is a port, not a
cross-compile, and it is not faked with an untested `.exe`. Listing it made
`sh scripts/build.sh --all` exit 1 on every run even when the host build had
succeeded, which made the documented release step useless as a success signal.

**On a platform with no committed binary**, build one *before* installing:

```sh
sh scripts/build.sh
bin/tabstatus install
```

`install` **refuses** when `bin/tabstatus` is missing, and says the same thing.
That refusal is not pedantry: the env key it would write switches Claude Code's
own title painting off, and all ten hooks would then resolve to a command that
exits 127 - a tab nothing paints at all, which is strictly worse than no install.
`install --force` overrides it for the case where you are about to build.
Zero crates, so `cargo build` needs no network.

## Tests

```sh
sh tests/run.sh
CCTAB_TEST_BIN=target/release/tabstatus sh tests/run.sh   # a build you just made
```

279 assertions, and what they drive is `bin/tabstatus` - the same binary
`hooks/hooks.json` invokes, so a stale committed binary fails here rather than in
somebody's tab. The suite used to run the shell implementation under three shells
in four locales, because its answer depended on both; a binary has no
interpreter, and since the length cap became locale-independent it has no locale
dependence either, so that whole axis is gone.

Two of the assertions exist only to guard the committed binaries: `bin/` carries
a digest of the `src/*.rs` and `Cargo.toml` it was built from, and the suite
recomputes it. Git does not preserve mtimes, so "is the binary older than the
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
present, nothing else is registered, ten hooks exactly, one command per group,
`SubagentStop` still absent, and no edge name the binary does not implement (an
unknown edge falls back to idle, so a typo there would silently paint the wrong
state on every notification). Presence checks alone are not enough, and that is
measured rather than argued: a copy of this tree with `Stop` → working and
`UserPromptSubmit` → idle passed an earlier "every event appears somewhere"
version of these assertions, and `claude plugin validate` passed it too. Eight
deliberate mutations of `hooks.json` - transposed edges, a matcher added to
`PostToolUse`, the matcher dropped from `PreToolUse`, a missing timeout, a
changed timeout, an added `SubagentStop`, a second hook smuggled into a group,
an edge-name typo - now each fail at least one assertion.

Two things the suite deliberately does not assert. The real emitting path of
`session-start` and `session-end` needs an allocated pty, which would cost a
dependency; it is checked by hand instead - 82 bytes for a startup, the OSC 50
arming pair of [Konsole](#konsole) followed by the idle title, and 0 bytes for a
compaction - and by the 292-case golden corpus in
[`tests/corpus/`](tests/corpus/), which replays every edge over a freshly
allocated pty and compares bytes. And the belt is pinned at its real reach rather than a wished-for
one: `{"source": "compact"}` on one line is caught, the same payload
pretty-printed over three lines is not, because only the first line is read.

### The golden corpus

```sh
sh tests/corpus/replay.sh bin/tabstatus     # 292 passed, 0 failed
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

**Not yet built:**

- A tmux branch, for when the session is inside tmux rather than a bare tab.
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
