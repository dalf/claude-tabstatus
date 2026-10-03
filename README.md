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

The priority is **input required > main working > background > idle**. A long
workflow stays purple after the main agent answers, including while you keep
chatting with it. Main activity temporarily shows blue; unresolved input
requests stay orange. White means no known active work, not success. Focusing a
tab does not resolve a request.

Purple is cleared only when Claude Code reports that the background work has
finished, never by a timeout. It needs a per-session state directory, by default
under `$XDG_RUNTIME_DIR` (`%LOCALAPPDATA%` on Windows); without one, background
work cannot be remembered between hooks. The full policy is in
[docs/indicator-semantics.md](docs/indicator-semantics.md).

- [Supported platforms](#supported-platforms)
- [Install](#install) · [Update](#update) · [Uninstall](#uninstall) · [What install changes](#what-install-changes)
- [When each colour appears](#when-each-colour-appears) · [Location](#location)
- [Terminal setup](#terminal-setup): [Windows Terminal](#windows-terminal) · [Konsole](#konsole) · [Konsole over ssh](#konsole-over-ssh) · [tmux](#tmux)
- [Configuration](#configuration) · [Troubleshooting](#troubleshooting) · [Known limitations](#known-limitations)
- [For contributors](#for-contributors) · [Credits](#credits) · [Licence](#licence)

## Supported platforms

| Platform | Status | Release asset |
|---|---|---|
| Linux x86_64 | supported; the musl build is static and recommended - it runs on any x86_64 Linux whatever its glibc | `tabstatus-x86_64-unknown-linux-musl` (or `-linux-gnu`) |
| Windows x86_64, native | supported; needs Git Bash, which Claude Code itself requires on Windows and runs hooks through | `tabstatus-x86_64-pc-windows-msvc.exe` |
| macOS | **experimental; native arm64 CI validated**. Native builds, process/state checks and disposable PTY delivery passed on macOS 15 arm64; Intel runtime behaviour remains unvalidated. Automated PTY tests do not establish behaviour in Terminal.app, iTerm2 or other terminal applications; see [issue #1](https://github.com/dalf/claude-tabstatus/issues/1). | none |
| Linux aarch64 / ARM | no build; an x86_64 binary fails with *Exec format error*. Check `uname -m` on a remote VM first | none |

| Terminal | What you get |
|---|---|
| Windows Terminal, and any terminal that honours a plain OSC 0 title | works with no configuration |
| Konsole | works; the tab is switched to show the title automatically ([Konsole](#konsole)) |
| Konsole, then ssh to a Linux host | set one variable on the remote side ([Konsole over ssh](#konsole-over-ssh)) |
| tmux (Linux; untested on macOS) | one indicator per Claude pane, decaying over time ([tmux](#tmux)) |
| GNU screen | no indicator; tmux running inside screen works |

Direct MCP elicitation tracking relies on two hook events found in the Claude
Code 2.1.274 executable; earlier versions and live interactive delivery have not
been validated (see [Known limitations](#known-limitations)). The reference tmux theme
was tested with tmux 3.7c.

## Install

Prebuilt binaries are attached to each
[GitHub release](https://github.com/dalf/claude-tabstatus/releases), with one
`SHA256SUMS` file covering all of them. No Rust toolchain is needed.

### Linux

```sh
asset=tabstatus-x86_64-unknown-linux-musl        # or tabstatus-x86_64-unknown-linux-gnu
base=https://github.com/dalf/claude-tabstatus/releases/latest/download
curl -LO "$base/$asset" -LO "$base/SHA256SUMS"
sha256sum --check --ignore-missing SHA256SUMS    # must print "<asset>: OK"
chmod +x "$asset"
./"$asset" install
./"$asset" doctor
```

On a remote VM reached over ssh it is the same verb with nothing else on it,
because the plugin manifests Claude Code needs are compiled into the binary:

```sh
scp -C tabstatus-x86_64-unknown-linux-musl vm:~/tabstatus
ssh vm 'chmod +x ~/tabstatus && ~/tabstatus install && ~/tabstatus doctor'
```

### Windows

Download `tabstatus-x86_64-pc-windows-msvc.exe` and `SHA256SUMS` from the same
release, then from **Git Bash**, in the folder holding them:

```sh
sha256sum --check --ignore-missing SHA256SUMS    # must print "...msvc.exe: OK"
./tabstatus-x86_64-pc-windows-msvc.exe install
./tabstatus-x86_64-pc-windows-msvc.exe doctor
```

In PowerShell, compare `(Get-FileHash tabstatus-x86_64-pc-windows-msvc.exe).Hash`
with its line in `SHA256SUMS`, then run `.\tabstatus-x86_64-pc-windows-msvc.exe install`
and `.\tabstatus-x86_64-pc-windows-msvc.exe doctor`. The `.exe` is not
code-signed, so SmartScreen may warn on first run. It needs no Visual C++
Redistributable.

`install` needs neither Developer Mode nor an elevated shell: the plugin link is
a directory **junction**, not a symlink. A volume that cannot hold a junction is
refused with nothing changed. **Keep the downloaded `.exe`**: on Windows you
uninstall with it (see [Uninstall](#uninstall)).

### Check that it works

Start a **new** Claude Code session. A white dot beside the location - at the
front, or at the end in Konsole - means it works; inside tmux, look for a white
cell in the strip.

### Build from source instead

Needs Rust **1.89 or newer** (`cargo` on `PATH`, or `mise` with a Rust tool). The
first build needs network access to crates.io, or a populated Cargo cache; after
that, `cargo build --locked --offline` works without network.

```sh
git clone https://github.com/dalf/claude-tabstatus ~/code/claude-tabstatus
cd ~/code/claude-tabstatus
rustup target add x86_64-unknown-linux-musl   # Linux only: the default build target
sh scripts/build.sh
./bin/tabstatus install
./bin/tabstatus doctor
```

On Windows run the build in **Git Bash** - `scripts/build.sh` does not run from
cmd or PowerShell (a released or built `.exe` runs from either) - and skip the
`rustup target add` line: the target there is `x86_64-pc-windows-msvc`, and
`bin/tabstatus.exe` is a copy of it rather than a symlink.

Your checkout is never the live plugin: after a `git pull` or any edit, rebuild
and run `install` again.

## Update

Get the new binary, run `install` with it, and start new Claude sessions:

```sh
# release: download and verify the new asset as in Install, then
./tabstatus-x86_64-unknown-linux-musl install
./tabstatus-x86_64-pc-windows-msvc.exe install   # Windows, Git Bash; keep this .exe for uninstall
# from source:
cd ~/code/claude-tabstatus && git pull && sh scripts/build.sh && ./bin/tabstatus install
```

`install` is safe to run while Claude sessions are running. Update **every**
installed copy - each machine, and each tree if you used `--tree` - so that no
hook runs an older release: an older binary can drop background tracking. Inside
tmux, start a new Claude session to refresh the server's formats, and reload
your theme if you copied [examples/tmux.conf](examples/tmux.conf).

## Uninstall

```sh
# Linux: the installed copy can remove itself
~/.local/share/claude-tabstatus/bin/tabstatus uninstall
# Windows (Git Bash): the copy you downloaded - not the installed one
./tabstatus-x86_64-pc-windows-msvc.exe uninstall
```

The installed copy is under `$XDG_DATA_HOME/claude-tabstatus/bin/` when that is
set, or in your `--tree` directory; `doctor` prints the live tree.

| option | effect |
|---|---|
| `--keep-tree` | leave the plugin tree on disk, and print how to remove it (the files to delete if it holds any of yours) |
| `--restore-backup` | roll `settings.json` back wholesale to `settings.json.cctab-preinstall` |
| `--force` | remove the env key even when there is no state record proving it is ours; on Windows, also write a `settings.json` whose filesystem keeps no ACL (see below) |

On Windows a running program's file cannot be deleted, so an `uninstall` run by
the installed tree's own `bin\tabstatus.exe` is refused with nothing changed:
run it from another copy, or pass `--keep-tree`. Removal hints there are given as
PowerShell `Remove-Item -Recurse -Force -LiteralPath '...'`.

Uninstall is an undo, not a delete:

- It removes only the files `install` generated. A file you added to the tree is
  named and kept, and so is a link that still points at a checkout.
- It removes the live tree. If you moved the tree with `install --tree`, the old
  one is left behind and named whenever it can still be found (the default path,
  or the tree the install record names), with the command to remove it - or, if
  it holds files of yours, the list of generated files to delete instead.
- If `CLAUDE_CODE_DISABLE_TERMINAL_TITLE` was already set before you installed,
  your value is put back byte for byte, and `uninstall` warns that nothing will
  then paint the tab. Unset it yourself if that was not deliberate.
- It deletes the per-session records directory. If `CCTAB_STATE_DIR` points at a
  directory holding anything else, only the records go, and it says so.
- Inside tmux it restores the window formats and title settings it changed; see
  [Uninstall and tmux](#uninstall-and-tmux).

## What install changes

| what | where |
|---|---|
| the generated plugin tree (manifests and a copy of the binary, `bin/tabstatus`; `bin\tabstatus.exe` on Windows) | `$XDG_DATA_HOME/claude-tabstatus`, default `~/.local/share/claude-tabstatus` (Windows: `%USERPROFILE%\.local\share\claude-tabstatus`); `install --tree <dir>` puts it elsewhere |
| one settings key | `env.CLAUDE_CODE_DISABLE_TERMINAL_TITLE = "1"` in `~/.claude/settings.json` |
| the plugin link | `~/.claude/skills/claude-tabstatus` → the tree: a symlink on Linux, a junction on Windows |
| the install record | `~/.claude/claude-tabstatus.state`: what was there before, and which tree this install owns |
| per-session records, written by the running plugin | `$XDG_RUNTIME_DIR/claude-tabstatus` on Linux, emptied at logout; `%LOCALAPPDATA%\claude-tabstatus` on Windows, which only `uninstall` empties |

The settings key is not optional: Claude Code repaints its own title about once
a second, straight over this one, and a plugin cannot set environment variables.
It also means Claude Code no longer clears the title on exit, so this plugin does
that at `SessionEnd`.

Your own `settings.json` formatting is kept, and so are its permissions (mode bits
on Unix, plus owner, group and ACLs on macOS; the DACL on Windows); a backup is
kept at `settings.json.cctab-preinstall`. Settings rewrites and backups refuse an ACL
they cannot read or preserve. On macOS, filesystems without ACL support and
operations whose owner or group cannot be preserved with the current privileges
are also refused; `--force` does not bypass these refusals. A restore keeps the
live file's protection, or the backup's when the live file is missing. When `install`
cannot proceed safely, it refuses, and every refusal ends with **"Nothing has
been changed."** On Windows that includes a `settings.json` on a filesystem that
keeps no ACL, such as a symlink into a WSL share: edit it from the system it
lives on, or pass `--force` to `install` or `uninstall` to write it anyway. The
details are in [docs/architecture.md](docs/architecture.md).

The tree must be outside `<config>/skills` and the source checkout, including
paths reached through ancestor symlinks. On macOS the guards recognise existing
case and Unicode-normalisation aliases and respect case-sensitive volumes.
The tree root itself must be a directory, and its generated directory components
must not be symlinks. Unresolvable paths are refused before installation writes.
For differing missing Unicode names on APFS or HFS+, management commands create
and remove private temporary filename probes beneath the deepest existing ancestor.
These checks keep no cache and do not run during hooks. The ancestor's timestamps
can change; interruption or cleanup failure can leave a `.cctab-name-probe-*`
directory. A probe failure or another filesystem retains uncertainty: create the
intended ancestor directory first so the filesystem can identify it, then retry.

## When each colour appears

| You see | When |
|---|---|
| 🔵 working | you submit a prompt, or a tool runs or fails |
| 🟠 waiting | a permission dialog, `AskUserQuestion`, a plan to approve, an agent asking for input, or an MCP form/URL request |
| 🟣 background | the turn ended but session-owned background work (an async agent, a workflow) is still running |
| ⚪ idle | the turn ended with no known work left |
| *(title cleared)* | the session ends; the tab gets its normal title back |

Nothing changes while Claude Code compacts the conversation. The event-by-event
table is in [docs/architecture.md](docs/architecture.md), and the normative rules
in [docs/state-contract.md](docs/state-contract.md).

A dialog is an overlay on activity. When several are open at once - a
subagent's and the main thread's - the tab stays orange until the last one is
answered, then returns to whatever the session is actually doing: blue, purple or
white. Typing a new prompt clears any stuck orange, because you cannot type while
a dialog is up.

For MCP elicitation, tabstatus only watches the request to paint orange: it never
answers or blocks it, and never stores its text, answers, URLs or credentials.

## Location

Inside a git repository the location is `<repo>@<branch>`; the subdirectory is
deliberately not shown, so every tab of one repo stays recognisably the same tab.
Outside a repository it is the home-relative path.

| Situation | Location |
|---|---|
| in a repo, any subdirectory of it | `streaming-browser@master` |
| a branch name with slashes | `claude-tabstatus@feature/tab-title` |
| a detached HEAD | `streaming-browser@b56583d` |
| during a bisect | `streaming-browser@bisect/bad` |
| a linked worktree, or a submodule | its own directory name and its own branch |
| a directory reached through a symlink | the real repository's name and branch, as `git` reports them |
| not a repo, under `$HOME` | `~/code/bug_fedora` |
| not a repo, elsewhere | `/srv/www` |
| `$HOME` itself | `~` |
| over ssh | `srv:` in front of any of the above |

**A local session has no prefix at all** - that absence is how you recognise it.
Only `SSH_CONNECTION` or `SSH_TTY` puts a host in front.

Long locations are elided to 32 characters, and the two forms lose different
ends, because different halves carry the information:

```text
~/one/two/three/four/five/six/seven/eight   ->  …/four/five/six/seven/eight
repo@feature/a-much-longer-branch-name      ->  repo@feature/a-much-longer-bran…
```

A path is cut at the front on a component boundary, so Konsole (which elides
from the left) and Windows Terminal (which truncates from the right) show the
same informative tail. See [Location tuning](#location-tuning).

## Terminal setup

### Windows Terminal

Nothing to configure, and the same goes for any terminal that honours a plain
OSC 0 title.

### Konsole

Nothing to configure. Konsole's stock tab format is `%d : %n` (directory and
name), so it normally ignores the title a program sets - which is why Claude's
own title has never shown in a Konsole tab. At session start this plugin sets
**that tab's** title format to `%w`, so the title becomes the whole tab text, and
at session end it sets the formats back to Konsole's defaults. Every other tab
keeps Konsole's default title, and nothing is written to disk.

Detection uses `KONSOLE_VERSION` / `KONSOLE_DBUS_SESSION`, and is ignored inside
tmux or screen, where those variables leak in from whichever terminal started the
server. Konsole elides tab labels from the left, so the glyph goes **last** there
(see [Glyph position](#glyph-position)).

### Konsole over ssh

`KONSOLE_*` does not travel over ssh, so the remote side cannot see that the tab
is Konsole's. Set one variable **on the remote machine**, in
`~/.claude/settings.json` under `"env"`:

```json
"env": { "CCTAB_TERMINAL": "konsole" }
```

(or export it in the remote shell before launching `claude`). That alone is
enough, with or without tmux on the remote side: it switches the tab to show the
title and puts the glyph - or, in tmux, the strip - on the end Konsole does not
elide. You do not need `CCTAB_GLYPH_POS` as well. Inside tmux the switch is sent
to each attached client's terminal, and a detach-and-reattach re-arms the tab
automatically. The tab stays armed until the last Claude pane in that tmux session
ends, even when other panes start with a different `CCTAB_TERMINAL` value.

`CCTAB_TERMINAL` set to anything else says explicitly that the terminal is
**not** Konsole, which is how you turn off a false detection (an xterm launched
from a Konsole shell inherits `KONSOLE_*`).

### tmux

Inside tmux the outer tab shows **one cell per Claude pane** of the attached
session, then where you are, and each cell ages on tmux's own clock:

```text
🔵🟠⚪ ~/code/one      three claudes: one working, one waiting for you, one idle
⚪🟠⚪ ~/code/one      the first one has gone quiet past its TTL
🟠⚪ ~/code/one        and then dropped out of the strip entirely
🔵🟠⚪ t:2:shell       looking at a plain shell, so tmux's own label
t:0:w0                 no claude anywhere on the server
```

Blue and orange decay to white (or to purple while background work is known),
and a cell disappears, after [configurable TTLs](#the-ttls). Purple never expires. Because of the decay, a
stale title left by one of the [known limitations](#known-limitations), such as
Ctrl+C, ages out on its own. Split panes each get a cell; only the attached
session's Claudes appear in that tab. There is no alert when background work
finishes - purple simply turns white.

**No `~/.tmux.conf` edit is needed.** Each Claude `SessionStart` sets tmux's
title options and decorates its window's status formats at runtime; `uninstall`
restores them. If you would rather pin the outer title in your own config,
`tabstatus tmux-format` prints the format strings - but read
[Uninstall and tmux](#uninstall-and-tmux) before you do.

**The tmux window list** also shows each window's pane indicators before its
label, for example `🟠⚪ 2:editor*`, in both the current and the other windows'
formats, so a waiting Claude stays visible while you work elsewhere. Only windows
where Claude starts are changed; window names, `automatic-rename`, custom labels,
flags and styles are left alone.

#### What you must have on

| setting | why |
|---|---|
| `status on` (the default) | painting works without it, but the decay stops |
| `status-interval` > 0 (default `15`) | this is the decay clock |
| an outer terminal tmux grants the `title` feature | otherwise tmux sends no title at all; on an older tmux, `set -as terminal-features ",$TERM:title"` |

`SessionStart` sets none of these for you. `tabstatus doctor` reports all three,
with the remedy:

```text
tmux:      OK   tmux 3.7c on /tmp/tmux-1000/default, pane %3
           decay: OK   status on, status-interval 15s - the tab re-renders on that timer
           title: OK   set-titles-string is the one SessionStart installed
           client: /dev/pts/3 xterm-256color HASTITLE
           konsole: off  set CCTAB_TERMINAL=konsole when the outer terminal is Konsole
           ttl: working 1200s, waiting 900s, gone 3600s (0 = never)
```

In Konsole mode it also reports the re-arm hook, and warns when the strip on the
server is on the other end from the one this session would install:

```text
           arm: OK   client-attached re-arms this tab's Konsole format on every reattach
           layout: WARN the server has the strip last, but this session would install it first
```

#### The TTLs

`CCTAB_TTL_WORKING` (20 min), `CCTAB_TTL_WAITING` (15 min) and `CCTAB_TTL_GONE`
(1 h) are in seconds, `0` meaning never; see [Environment](#environment). A flip
lands one second after the TTL at the earliest, plus up to one `status-interval`. These settings - and `CCTAB_TERMINAL` and `CCTAB_GLYPH_POS`,
which decide the layout - are **server-wide**: the last Claude to start on a tmux
server wins for every window on it. Set them the same for every Claude on one
server; `doctor` prints a `layout: WARN` line when they disagree.

#### Themes

For a theme with rounded tabs or embedded colours, place
`#{T:@cctab_window_strip}` inside the styled body of both window formats:

```tmux
set -g window-status-format "#[fg=white,bg=colour238] #{T:@cctab_window_strip} #I:#W "
set -g window-status-current-format "#[fg=black,bg=cyan,bold] #{T:@cctab_window_strip} #I:#W "
```

The plugin recognises this placement and does not prepend another strip, and
uninstall leaves these formats alone (the strip reference just becomes empty).

The optional [reference tmux configuration](examples/tmux.conf) adds rounded
tabs with a status-coloured left cap; it is not installed for you, and its
comments explain the colour cap and how to clear the plugin's own decorators from
a window that already has them. The internals are in
[docs/architecture.md](docs/architecture.md).

#### Uninstall and tmux

Run `tabstatus uninstall` **from inside tmux**: it puts every decorated window's
status formats back as they were (unless you replaced them yourself), restores
your previous `set-titles` and `set-titles-string`, and warns that other Claude
panes on that server stop updating. Run from outside tmux, it says it could not
restore. `SessionEnd` clears its own pane's cell but leaves the server-wide
settings, because other Claude panes may still be painting through them.

If you pinned the `tabstatus tmux-format` output in `~/.tmux.conf`, uninstall
cannot restore your previous title string - the saved one would be ours - and
says so: remove the pinned lines yourself.

#### Other programs, screen and nested tmux

- A shell prompt, `vim` or `ssh` setting the pane title hides that pane's cell
  until Claude's next update.
- **GNU screen** gets no indicator. When both `$TMUX` and `$STY` are set, tmux
  wins, so tmux inside screen works.
- **Nested tmux**: only the inner server shows indicators; the outer tmux tab
  loses that window's cell, and `doctor` cannot see the outer server.
- `CCTAB_NO_TMUX=1` turns the whole tmux integration off.

## Configuration

Set these in `~/.claude/settings.json` under `"env"`, or export them before
launching `claude`. Every variable is listed in [Environment](#environment);
the sections before it explain the ones that need more than a line.

```json
"env": { "CCTAB_GLYPH_WORKING": ">", "CCTAB_GLYPH_IDLE": ".", "CCTAB_ELLIPSIS": "..." }
```

### Glyphs

Override any of the four glyphs - for a terminal with no emoji font, or just to
taste. Setting one to the empty string drops the glyph and its separating space.

### Glyph position

The glyph goes on whichever end the terminal keeps when the title is too long.
Konsole elides from the left, so a leading glyph is the first thing cut: a
23-cell title in a 19-cell tab renders `…de-tabstatus@main` with the dot gone.

| Terminal | Position | Crushed to 19 cells |
|---|---|---|
| Konsole (detected, or `CCTAB_TERMINAL=konsole`) | last | `…tatus@main ⚪` |
| Windows Terminal, and anything unrecognised | first | `⚪ claude-tabst…` |

To override, set `CCTAB_GLYPH_POS` to `suffix` (last, what Konsole gets
automatically), `prefix` (first, the default when the terminal is unknown) or
`both` (both ends, immune to either, at the cost of two columns). An
unrecognised value falls back to `prefix`. Inside tmux it picks which end of the
tab the strip goes on, and `both` is not doubled there.

Konsole's left-eliding cannot be changed. Widening the tabs (*Settings ->
Configure Konsole -> Tab Bar*, or `setTabWidthToText false` over D-Bus) gives
more room, until you open more tabs.

### Location tuning

`CCTAB_MAX_LOCATION` (32) and `CCTAB_MAX_HOST` (16) cap the location and the ssh
host prefix; `CCTAB_ELLIPSIS` and `CCTAB_HOST` change the marker and the prefix.

- Both caps count **characters, not columns**, in every locale: an accented path
  elides like an ASCII one, but a CJK or emoji location can take up to twice the
  width, and a multi-column `CCTAB_ELLIPSIS` overshoots by its extra width.
- `CCTAB_MAX_LOCATION` bounds the location only. The title adds 3 columns for the
  glyph and its space, plus `host:` over ssh, so size the two caps together. It is
  clamped up to 8, and `CCTAB_MAX_HOST` up to 4; a non-numeric value falls back
  to the default.
- `CCTAB_HOST` loses everything from the first dot (`srv.example.com` renders as
  `srv:`), unless it is all digits and dots. It is used only in an ssh session,
  so exporting it globally is safe.

### Environment

| variable | default | what it does |
|---|---|---|
| `CCTAB_GLYPH_WORKING` / `_WAITING` / `_BACKGROUND` / `_IDLE` | 🔵 / 🟠 / 🟣 / ⚪ | the four glyphs; empty drops the glyph and its space |
| `CCTAB_GLYPH_POS` | terminal-dependent | `prefix`, `suffix` or `both`; inside tmux, which end the strip goes on |
| `CCTAB_MAX_LOCATION` | `32` | characters before the location elides; `0` = no limit |
| `CCTAB_MAX_HOST` | `16` | characters for the ssh host prefix; `0` = no limit |
| `CCTAB_ELLIPSIS` | `…` | the elision marker; `...` for an ASCII-only terminal |
| `CCTAB_HOST` | this machine's hostname | the ssh prefix |
| `CCTAB_TERMINAL` | unset | `konsole` (any case) arms Konsole's per-tab format even over ssh or inside tmux, and puts the glyph or strip on the end Konsole does not elide; any other value says explicitly NOT Konsole. The one knob here that changes what paints outside tmux as well as in |
| `CCTAB_TTL_WORKING` | `1200` | seconds before 🔵 decays to ⚪ (🟣 while background work is known) in a tmux tab; `0` = never |
| `CCTAB_TTL_WAITING` | `900` | seconds before 🟠 decays to ⚪ (🟣 while background work is known) in a tmux tab, **and** before an outstanding wait expires (applied at the next hook); `0` = never |
| `CCTAB_TTL_GONE` | `3600` | seconds before a cell leaves the tmux strip; `0` = never |
| `CCTAB_NO_TMUX` | unset | set to anything: no tmux integration at all |
| `CCTAB_DRY_RUN` | unset | `1` prints the computed tab title and emits nothing |
| `CCTAB_STATE_DIR` | `$XDG_RUNTIME_DIR/claude-tabstatus`; on macOS `$TMPDIR/claude-tabstatus`; on Windows `%LOCALAPPDATA%\claude-tabstatus` | where the per-session records live. Unset with no `XDG_RUNTIME_DIR` (`TMPDIR` on macOS, `LOCALAPPDATA` on Windows) means background and wait tracking are off. **Use a dedicated, empty directory**: stale records are cleaned up there, and `doctor` lists anything it leaves alone. On Windows give a drive-absolute path (`C:\...`) on NTFS - Git Bash rewrites a `\\server\share` value into a drive-rooted one - and do not share it with WSL: each reads the other's records as having no origin |

`XDG_DATA_HOME` is read only by `install`, to choose where the plugin tree goes.

## Troubleshooting

Start with **`tabstatus doctor`** - the binary you downloaded, `./bin/tabstatus`
in a checkout, or `~/.local/share/claude-tabstatus/bin/tabstatus` once installed
(under `$XDG_DATA_HOME`, or your `--tree` directory, if you used either).
It reports where the live plugin tree is, whether the deployed binary and
manifests match the build you are running, the settings key, the state
directory, the terminal it detected and why, the glyph position, and the tmux
checks above. `CCTAB_DRY_RUN=1 tabstatus working` prints the title it would
paint, without painting it.

`doctor` also prints a capability table for the platform, the terminal drawing
the tab (the *surface*) and the multiplexer. Its verdict column has five words:
`ok`, `n/a` (unsupported by this terminal or version), `off` (a knob of ours you
can turn back on),
`?` (the terminal may or may not honour it and nothing we can read says which)
or `fail` (an attempt the OS refused), with escape bytes named, never written.
A `?` entry still shows its known grammar, if any, alongside the setting to check.

For Konsole's versioned protocols, `ok` requires a valid `KONSOLE_VERSION` at
or above the documented minimum: notifications (OSC 777) need 23.04, tab colour
(OSC 34) needs 24.12 and progress (OSC 9;4) needs 26.04. Older versions show
`n/a`; missing, malformed or inherited multiplexer version evidence shows `?`.
`CCTAB_TERMINAL=konsole` names the family but does not establish a version.
These entries describe terminal protocols; this release does not emit notifications,
tab colour or progress.

`tabstatus doctor --surface <name>` prints one terminal's table with no terminal,
session or config directory, for any of the fourteen names `CCTAB_TERMINAL`
accepts: `unknown`, `konsole`, `vte`, `kitty`, `alacritty`, `wezterm`, `foot`,
`ghostty`, `xterm`, `iterm2`, `apple-terminal`, `windows-terminal`, `conhost`,
`vscode`. This offline catalogue shows versioned protocols as `?`, with their
minimum versions, regardless of the local environment. Why the table looks as it
does is in
[docs/architecture.md](docs/architecture.md#the-three-axes).

| symptom | cause and fix |
|---|---|
| tab stays blank in a new session | the plugin tree was deleted but the link and settings key remain; `doctor` says `FAIL the plugin directory is not there`. Run `install` |
| tab blank after uninstall | `CLAUDE_CODE_DISABLE_TERMINAL_TITLE` was set before you installed, so uninstall kept it; unset it in `~/.claude/settings.json` |
| `doctor`: `binary: WARN ... different build`, or `hooks.json differs` | the live tree is stale; run `install` with the binary you mean to deploy |
| `doctor`: `points at a CHECKOUT` | old wiring from an early version; run `install` |
| `install` refuses a `--tree` directory | it is not empty and was not created by `install`; choose an empty or new directory |
| *Exec format error* | wrong architecture: only x86_64 builds exist |
| Konsole tab shows the directory, not the title, over ssh | set `CCTAB_TERMINAL=konsole` on the remote side ([Konsole over ssh](#konsole-over-ssh)) |
| the ssh prefix is wrong, or a bare `ssh:` | set `CCTAB_HOST` |
| tab reads `~/code/one ct1 w 1790…` (tmux) | something replaced tmux's title string - a `tmux source-file`, an uninstall while another Claude runs, or a killed Claude. Start a new Claude session or `/clear` |
| tmux cells never decay | `status` is off or `status-interval` is 0; see [What you must have on](#what-you-must-have-on) |
| tab stays blue after Ctrl+C | see [Known limitations](#known-limitations) |
| tab stays orange | see [Known limitations](#known-limitations) |
| Konsole tab keeps the Claude title after `kill -9`, a crash or an OOM kill | from a shell in that tab: `CLAUDE_PID=$$ ~/.local/share/claude-tabstatus/bin/tabstatus session-end` (or the `bin/tabstatus` of the tree `doctor` prints) |
| Windows: background and wait tracking off | the state directory is not on NTFS (FAT, exFAT, the WSL share, some SMB shares); `doctor` says why |
| Windows: tab stops updating after a dialog | another program held a state-directory file open without delete sharing (Python `open()`, `Get-Content -Wait`, some editors or backup tools). It recovers at that agent's next tool call, your next prompt, or `CCTAB_TTL_WAITING`; leave the state directory's files alone while sessions run |
| Windows: no title at session start or end | endpoint security blocked reading Claude's process, or Claude is a 32-bit (WOW64) build |

## Known limitations

- **A stuck orange tab.** Pressing Esc at a permission dialog, or walking away
  from one, fires no hook at all, so the tab stays orange. It clears on the next
  prompt you type, the next `Stop` with no background work left, the owning agent
  finishing, or - once `CCTAB_TTL_WAITING` (15 min) has passed - at the next hook
  that fires. Outside tmux nothing repaints on time alone; inside tmux the cell
  decays after `CCTAB_TTL_WAITING`.
- **Ctrl+C fires no hook either.** The tab keeps reading blue until Claude Code's
  idle nudge repaints it about 60s later (`messageIdleNotifThresholdMs`) - white,
  or purple if background work is known - and your next prompt repaints it at once.
- **A prompt beginning with `<` does not clear orange.** Claude Code injects
  prompts like `<task-notification>...` itself, so such a prompt is not taken as
  proof that you dismissed a dialog.
- **Some dialogs are invisible to every hook** - the LSP recommendation, the
  plugin hint, the auto-mode upsell - so the tab keeps what it showed, usually
  white, while a modal waits for you.
- **`CLAUDE_CODE_DISABLE_PERMISSION_PROMPT_NOTIFY_HOOKS`**, if you set it, stops
  dialogs that are not tool calls (the managed-settings review, the sandbox
  network request) from turning the tab orange.
- **MCP requests without IDs cannot be matched to responses.** Such a request
  stays orange until your next prompt or `CCTAB_TTL_WAITING`. Direct MCP
  elicitation tracking relies on two hook events found in the Claude Code 2.1.274
  executable; earlier versions and live interactive delivery have not been
  validated. It also needs the state directory; without it a request may stay
  orange after you answer it.
- **Nothing repaints while Claude Code compacts the conversation.**
- **No notification when background work finishes**; purple just turns white.
- **Konsole repaints the tab on a ~2s tick**, so the dot trails the real state by
  up to about two seconds.
- **The Konsole restore puts back Konsole's stock formats** (`%d : %n` and
  `(%u) %H`), not a customised profile's. Restoration is best effort: it remembers
  the terminal type, not the original terminal destination. Keep the session's
  terminal and tmux routing stable. Detach/reattach within the same tmux session
  is supported, but a client detached when the last Claude exits cannot receive
  the restore. Failed or interrupted writes are not retried; a recorded restore
  obligation does not prove that the terminal applied the arming.
- **`KONSOLE_*` is inherited environment**: an xterm launched from a Konsole
  shell carries it, and there the switch sets the font instead. Set
  `CCTAB_TERMINAL` to anything other than `konsole` to turn it off.
- **`claude -p` typed straight at a terminal retitles that tab** for the run.
  Only redirected or piped runs (`| jq`, `> file`, a script; on Windows also
  `> NUL`) are skipped. A `-p` run killed before `SessionEnd` leaves the Konsole
  tab armed; see [Troubleshooting](#troubleshooting) for the manual `session-end` fix.
- **Windows Terminal**: very rarely, a few stray characters may print right after
  `/clear`, `/resume` or a fork. A terminal that is not reading output (frozen or
  suspended) keeps its previous title.
- **Location**: the branch is read from `HEAD`, not resolved, so it can name a
  branch with no commits yet, and `@` in a branch name is not escaped. A `.git`
  that is not a working repository (an empty `.git`, a dangling `gitdir:`) is
  skipped, as git does. An exported `GIT_DIR` wins over the working directory,
  as it does for git, so a shell that exports one (common with a bare dotfiles
  repo) shows that repository in every tab; unset it per session if that is not
  what you want. On Windows, a `.git`, `gitdir:` or `GIT_DIR` that points at a
  network share other than the one the session is on is not followed (touching
  it would send your NTLM credentials to that server), and the tab shows the
  plain path instead of `repo@branch`.
- **Known bugs:** `doctor` exits 1 without a report if `settings.json` is a
  directory. After an install that aborted half way, `uninstall` can remove a
  `CLAUDE_CODE_DISABLE_TERMINAL_TITLE` you set yourself afterwards; a
  `settings.json.cctab-preuninstall` backup is kept.

## For contributors

- [AGENTS.md](AGENTS.md) - how to build, test and work in this repository, and what is not built yet
- [docs/architecture.md](docs/architecture.md) - how it works and why, with the measurements behind the design
- [docs/state-contract.md](docs/state-contract.md) - the normative state and wait-ownership contract
- [docs/indicator-semantics.md](docs/indicator-semantics.md) - what each colour means, and the background-work policy
- [docs/history.md](docs/history.md) - how it was built, slice by slice
- [COMPARISON.md](COMPARISON.md) - how it compares with related projects

## Credits

The wait-ownership model is partly taken from
[Yannis-Adn/terminal-addons](https://github.com/Yannis-Adn/terminal-addons) (MIT).

## Licence

GPL-3.0-or-later. See [LICENSE](LICENSE).
