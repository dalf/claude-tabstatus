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
sh ~/code/claude-tabstatus/install.sh
```

Then start a **new** Claude Code session. Updating later is `git pull` plus a
new session - the installed plugin is a symlink to the clone, so there is
nothing to reinstall.

`jq` is required; the installer refuses to run without it, because it edits
`settings.json` with jq specifically so that no existing key can be clobbered.

Avoid changing configuration in other Claude Code sessions while the installer
runs. It reads, merges and renames `settings.json`, and although it re-checks
the file's identity immediately before the rename and aborts if it moved, the
safe habit is to install when nothing else is writing that file.

## Uninstall

```sh
sh ~/code/claude-tabstatus/uninstall.sh
sh ~/code/claude-tabstatus/uninstall.sh --force            # no state record: remove anyway
sh ~/code/claude-tabstatus/uninstall.sh --restore-backup   # roll settings.json back wholesale
```

The uninstaller is an undo, not a delete: it puts back whatever
`claude-tabstatus.state` says was there before. If you had already set
`CLAUDE_CODE_DISABLE_TERMINAL_TITLE` yourself, your value comes back. If that
record is missing and the key is present, the key is left alone unless you pass
`--force`, because there is then no way to tell it apart from your own setting.

## What it changes

Three things, and nothing else:

1. One key in `~/.claude/settings.json`:
   `env.CLAUDE_CODE_DISABLE_TERMINAL_TITLE = "1"`.
2. A symlink `~/.claude/skills/claude-tabstatus` pointing at this repo. A
   directory there containing `.claude-plugin/plugin.json` auto-loads; there is
   no marketplace entry and no `enabledPlugins` line. There is deliberately no
   `SKILL.md`, so the plugin costs essentially no model context.
3. `~/.claude/claude-tabstatus.state`, a small JSON record of what was there
   before, written once and removed by `uninstall.sh`.

The first one is not optional. Claude Code repaints its own terminal title
roughly every 960ms, straight over ours, and a plugin cannot set environment
variables - so the switch has to live in `settings.json`. The installer copies
`settings.json` to `settings.json.cctab-preinstall` before touching it, merges
with jq, verifies the result parses, and refuses to proceed if anything other
than that one key would change. The file keeps its mode (a `settings.json`
locked to 0600 stays 0600), a `settings.json` that is a symlink stays a symlink
with its target updated, and a read-only one is refused rather than quietly
overwritten.

jq reprints the whole document, so a hand-formatted `settings.json` comes back
reindented to 2-space JSON. Values and key order survive; the installer says so
when it happens, and the original formatting is in the backup.

`settings.json` is written first and the symlink last, so a failure while
editing settings cannot leave the plugin loaded with the built-in title still
repainting over it.

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

- **Ctrl+C mid-turn emits no hook.** Interrupting Claude fires neither `Stop`
  nor anything else, so the tab keeps reading `working` until the next prompt
  is submitted or the session ends.
- **Konsole repaints the tab on a ~2s tick**, not when the title arrives, so
  the dot trails the actual state change by up to about two seconds. That, not
  the ~2ms hook, is the responsiveness ceiling.
- **A session killed ungracefully leaves the tab armed.** `SessionEnd` runs on
  a clean shutdown, on `/clear` and on `/resume`, but not after `kill -9`, an
  OOM kill or a crash: that Konsole tab keeps `LocalTabTitleFormat=%w` and its
  last title until the tab is closed. To fix one by hand, from a shell in that
  tab:

  ```sh
  CLAUDE_PID=$$ sh ~/code/claude-tabstatus/scripts/tabstatus.sh session-end
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
- **A working directory, branch or hostname whose name is not valid UTF-8**
  (legal on Linux) produces a hook line that is not valid JSON, because JSON
  text must be UTF-8. Quotes, backslashes and control characters are stripped
  from all three; invalid byte sequences are not, because the only cheap way to
  detect them would also cost a fork for every perfectly good accented or emoji
  name.
- Konsole's tab bar elides from the left, so a very narrow tab could in
  principle clip the leading dot. Measured budget is ~49-60 columns. A local
  title is ~27-37 columns; over ssh the host prefix adds its own width, which is
  why the host carries a cap of its own (`CCTAB_MAX_HOST`, default 16) - without
  one, a 63-character single-label cloud hostname rendered a 91-column title and
  Windows Terminal, which truncates from the right, showed the host and nothing
  else.
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
- **The location cap counts what the running shell counts.** The unit is bytes
  unless the shell has multibyte support *and* the locale is UTF-8: bash and
  BusyBox ash count characters in a UTF-8 locale, dash counts bytes always. So a
  non-ASCII path elides soonest under dash, and under any shell in the C locale.
  The cap also budgets one column for the ellipsis, so a multi-column
  `CCTAB_ELLIPSIS` overshoots it by its extra width.
- **A location containing non-ASCII characters is never cut mid-string**, only
  at a `/`. Cutting by count is only safe where the unit is a byte *and* a
  character, and a half-written UTF-8 sequence would be invalid JSON. So an
  accented or emoji name with no `/` left to cut at keeps its full length and
  the terminal elides it instead. The one hard ceiling is 256 units: the
  quote-and-control-character stripper is a quadratic shell loop, so it stops
  there and marks the cut rather than spending seconds on a name nobody can
  read - dropping any trailing high bytes first, so even that cut lands on a
  character boundary.
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
CCTAB_MAX_LOCATION=32   # columns before the location is elided; 0 = no limit
CCTAB_MAX_HOST=16       # columns for the ssh host prefix; 0 = no limit
CCTAB_ELLIPSIS="…"      # the elision marker; "..." for an ASCII-only terminal
CCTAB_HOST="srv"        # the ssh prefix, instead of this machine's hostname
```

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

## Tests

```sh
sh   tests/run.sh
dash tests/run.sh
CCTAB_TEST_SH=/bin/sh busybox sh tests/run.sh   # multi-call shells
```

The suite runs `scripts/tabstatus.sh` under the same interpreter that is
running the suite, so `dash tests/run.sh` really exercises dash. On a
multi-call shell (BusyBox ash, toybox) that self-detection cannot work -
`/proc/$$/exe` is the `busybox` binary, which reads its first argument as an
applet name - so it falls back to `sh`; `CCTAB_TEST_SH` overrides it outright.

Dependency-free, and nothing in the suite can write to a real terminal: every
assertion goes through `CCTAB_DRY_RUN=1` (which prints the computed title and
emits nothing) or runs with `CLAUDE_PID` unset.

```sh
CCTAB_DRY_RUN=1 sh scripts/tabstatus.sh working   # -> 🔵 claude-tabstatus@main
```

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

**Not yet built:**

- A tmux branch, for when the session is inside tmux rather than a bare tab.
- The full 13-edge state machine. Today's four edges cannot see a tool call, a
  permission prompt, or a notification, which is why `waiting` is implemented
  but never actually emitted yet.
- A compaction edge. `SessionStart` here carries
  `"matcher": "startup|resume|clear|fork"`, which deliberately leaves out
  `compact`: an auto-compaction fires mid-turn, and without the matcher it
  repainted the idle dot while Claude was still working, and armed the tab a
  second time with no matching unarm. A compaction edge should be a *second*
  `SessionStart` group with `"matcher": "compact"` (or the first-class
  `PreCompact` / `PostCompact` events), selected declaratively rather than by
  parsing the payload's `source` field.
- Konsole `TabColor`, which rides on the same OSC 50 property list as the
  arming and would let the tab itself carry the colour. Whoever adds it also
  has to add `TabColor=#000000` to the `SessionEnd` list, or the colour
  outlives the session.

## Licence

MIT. See [LICENSE](LICENSE).
