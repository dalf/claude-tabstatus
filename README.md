# claude-tabstatus

Puts the Claude Code session state into the terminal tab title, in front of the
location, so a row of tabs tells you which session wants you without losing
track of where each one is.

```text
🔵 streaming-browser     working  - Claude is running
🟠 streaming-browser     waiting  - Claude needs an answer
⚪ streaming-browser     idle     - Claude has stopped
⚪ ~                     at $HOME
⚪ /                     at the filesystem root
```

The location in this slice is the basename of the working directory. Richer
locations (`repo@branch`, an ssh host prefix) are listed under
[Slices](#slices) below.

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
- **A working directory whose name is not valid UTF-8** (legal on Linux)
  produces a hook line that is not valid JSON, because JSON text must be UTF-8.
  Quotes, backslashes and control characters are stripped; invalid byte
  sequences are not, because the only cheap way to detect them would also cost
  a fork for every perfectly good accented or emoji directory name.
- Konsole's tab bar elides from the left, so a very narrow tab could in
  principle clip the leading dot. Measured budget is ~49-60 columns against
  titles of ~27-37, so in practice it survives.

## Glyphs

Override any of them - for a terminal with no emoji font, or just to taste:

```sh
# in ~/.claude/settings.json under "env", or exported before launching claude
CCTAB_GLYPH_WORKING=">"
CCTAB_GLYPH_WAITING="?"
CCTAB_GLYPH_IDLE="."
```

Setting one to the empty string drops the glyph and its separating space.

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
CCTAB_DRY_RUN=1 sh scripts/tabstatus.sh working   # -> 🔵 claude-tabstatus
```

## Slices

Built here (slice 1): the plugin skeleton, four hook edges
(`SessionStart`, `UserPromptSubmit`, `Stop`, `SessionEnd`), Konsole per-tab
arming and restore, and the location as the working directory's basename.

**Not yet built:**

- `repo@branch` - the git repository name and current branch as the location.
- An ssh hostname prefix, e.g. `srv:streaming-browser@master`.
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
