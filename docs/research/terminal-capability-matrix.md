# Cross-terminal capability matrix

Evidence base for the `claude-tabstatus` backend abstraction (issues #1 #3 #11 #14).
Compiled 2026-09-28. Every row is labelled **V** (verified: I read the vendor source or
the vendor doc in this session) or **I** (inferred: reasoned, not read).

Native macOS arm64 PTY and tmux acceptance subsequently passed at `67a0823`.
Those transport observations do not promote a source-derived row to measured
terminal-application behaviour; see [validation scope](../architecture.md#macos-validation).

Raw sources fetched during this survey live in `./src/` next to this file
(konsole_vt102.cpp, vte_seq.cc, kitty_vtparser.c, kitty_window.py, tmux_input.c,
tmux_opts.c, tmux_feat.c, tmux_tty.c, screen_ansi.c, xterm_charproc.c, xterm_ptyx.h,
vte_ansi.rs, wez_osc.rs, wez_csi.rs, wt_osme.cpp, wt_adapt.cpp, xtermjs_input.ts,
gh_osc.zig, gt_client.cc, konsole_session.cpp).

---

## Runtime version reporting

The matrix describes terminal families and the surveyed implementations, not a
proof that every installed release implements every listed protocol. The executable
requirements live beside their grammars in `src/surface/rows.rs`, the authoritative
source for both runtime reporting and the offline catalogue:

| Konsole protocol | Minimum release |
|---|---|
| OSC 777 notification | 23.04.0 |
| OSC 34 tab colour | 24.12.0 |
| OSC 9;4 progress | 26.04.0 |

These are the existing Konsole floors carried by the surface row, now enforced in
reporting; no new protocol survey or emission is implied. Ordinary doctor resolves
these against usable `KONSOLE_VERSION` evidence. Older versions report `n/a`;
missing, invalid or mux-inherited evidence reports `?`. `doctor --surface konsole`
shows the grammars and requirements as an offline catalogue, without claiming a
running version. The full evidence policy is in
[the architecture](../architecture.md#protocol-catalogue-and-running-version-evidence).

## 0. Legend

| mark | meaning |
|---|---|
| Y | implemented and has an effect |
| **N/A** | parsed then deliberately discarded — the terminal accepts the bytes and does nothing |
| N | not recognised (logged/ignored) |
| cfg | gated on a setting that is OFF by default |
| — | not applicable |

`N/A` is the important column: it is indistinguishable from `Y` from the writer's side.

---

## 1. Title: OSC 0 / 1 / 2

| terminal | OSC 0 | OSC 1 (icon) | OSC 2 | notes | ev |
|---|---|---|---|---|---|
| Konsole | Y | Y | Y | `Session::SessionAttributes{IconNameAndWindowTitle=0,IconName=1,WindowTitle=2}`, Session.h:466-479; dispatch Vt102Emulation.cpp:1134 + Session.cpp:651-670. OSC 30 = SessionName (tab title), OSC 32 = SessionIcon, OSC 34 = SessionColor, OSC 50 = ProfileChange | V |
| GNOME Terminal / VTE | Y | Y | Y | `VTE_OSC_XTERM_SET_WINDOW_TITLE` etc., vteseq.cc:7926. VTE refuses to *report* titles (CVE-2003-0070), vteseq.cc:10602-10624 | V |
| xterm | Y | Y | Y | reference implementation; `allowTitleOps` default True (main.h:114-116) | V |
| kitty | Y | Y | Y | vt-parser.c OSC table | V |
| WezTerm | Y | Y (used as tab title) | Y | wezterm.org/escape-sequences.html: "OSC 1 … used as the Tab title when it is non-empty" | V |
| Alacritty | Y | **N** | Y | vte crate 0.15 `osc_dispatch` matches `b"0" | b"2"` only — OSC 1 falls to `unhandled` (alacritty/vte src/ansi.rs:1350, 1523) | V |
| foot | Y | Y | Y | foot-ctlseqs.7.scd | V |
| Ghostty | Y | Y | Y | ghostty.org/docs/vt/reference | V |
| iTerm2 | Y | Y | Y | VT100Token XTERMCC_* | V |
| Terminal.app | Y | I | Y | `nsterm` terminfo on this box: `tsl=\E]2;` `fsl=^G` `dsl=\E]2;^G` — OSC 2 is the documented route; OSC 0 accepted | V(terminfo)/I |
| Windows Terminal | Y | Y | Y | `OscActionCodes{SetIconAndWindowTitle=0,SetWindowIcon=1,SetWindowTitle=2,DECSWT_SetWindowTitle=21}`, OutputStateMachineEngine.hpp:207-233 | V |
| ConEmu | Y | Y | Y | conemu.github.io: `ESC ] 0..2 ; "txt" ST`. Plus `OSC 9;3;"txt"` = set ConEmu **tab** text | V |
| VS Code (xterm.js) | Y | Y | Y | InputHandler.ts:286-290 registers OSC 0/1/2 | V |
| tmux | Y (pane) | **N** | Y (pane) | input.c:2720-2729 handles `case 0: case 2:` only, gated on `allow-set-title` (default **on**, options-table.c:1288-1294). **OSC 1 is not in tmux's OSC table at all** → dropped, logged "unknown". Sets `pane_title`; does NOT reach the outer terminal | V |
| GNU screen | Y (window) | Y | Y | ansi.c:1266 `typ == 0|1|2|11|20|39|49`; 0 and 2 additionally fall through into the hardstatus path (ansi.c:1289-1295) | V |

**Multiplexer outward channel.** tmux does not forward OSC 0/2; it re-emits a title of its own
from `set-titles-string` using the **terminfo** capabilities `tsl`/`fsl` (`tty_set_title`,
tty.c:740-749). `set-titles` defaults to **off** (options-table.c:981-986). Whether `tsl`/`fsl`
exist is decided by tmux's `title` feature, whose capabilities are hardcoded
`tsl=\E]0;` / `fsl=\a` (tty-features.c:51-60). A client gets that feature from
(a) `terminal-features` default `"xterm*:clipboard:ccolour:cstyle:focus:title,screen*:title,rxvt*:ignorefkeys"`
(options-table.c:550-560), or (b) `tty_default_features()` matched against the **XTVERSION /
DA2 reply name** — and the table contains only mintty, tmux, rxvt-unicode, iTerm2, foot,
WezTerm, ghostty, Rio, XTerm (tty-features.c:580-680). **Konsole, kitty, Alacritty and
GNOME Terminal are not in that table**, so under those terminals the `title` feature comes
only from `TERM` matching `xterm*`. Konsole *does* answer XTVERSION
(`\033P>|Konsole <ver>\033\\`, Vt102Emulation.cpp:2737-2740) — tmux just has no entry for it.
This matches what the repo already documented at README.md:1335 and src/tmux.rs:1240.

---

## 2. Title stack — XTWINOPS `CSI 22 t` / `CSI 23 t`

Grammar (xterm ctlseqs, invisible-island.net): `CSI 22 ; Ps t` pushes, `CSI 23 ; Ps t` pops,
`Ps` = 0 icon+window, 1 icon, 2 window; an optional **third** parameter 1..10 addresses a
stack slot directly without pushing/popping.

| terminal | 22t/23t | depth | detail | ev |
|---|---|---|---|---|
| xterm | **Y** | unbounded list | `ewPushTitle = 22`, `ewPopTitle = 23` (ptyx.h:1507-1508); handler charproc.c:9260-9300 under `AllowWindowOps()`. `DEF_ALLOW_WINDOW = False` (main.h:118-119) **but** `AllowWindowOps(w,name) = (allowWindowOps || !disallow_win_ops[name])` (ptyx.h:3998) and the default `disallowedWindowOps` is only `"GetIconTitle,GetWinTitle"(+rectops/paste64/setxprop)` (main.h:150-173) → **push/pop title work out of the box; only the *query* ops 20t/21t are blocked** | V |
| VTE / GNOME Terminal | **Y (window title only)** | `VTE_WINDOW_TITLE_STACK_MAX_DEPTH`, bottom dropped on overflow | vteseq.cc:10626-10676. `Ps` = -1/0/2 accepted; **`Ps=1` (icon title) is an explicit no-op** | V |
| kitty | **Y** | `deque(maxlen=10)` (window.py:794) | vt-parser.c:1357-1361 → `screen_manipulate_title_stack`; window.py:1929-1938. Tolerates the spurious third `0` emitted by weechat/ncurses | V |
| Alacritty | **Y** | `TITLE_STACK_MAX_DEPTH` = 4096 (term/mod.rs:3273) | vte crate ansi.rs:1739-1744 `('t',[]) … 22 => push_title(), 23 => pop_title()`; term/mod.rs:2235-2256 | V |
| foot | **Y** | ? | foot-ctlseqs.7.scd: `22 - Push window title+icon`, `23 - Pop window title+icon` | V |
| Ghostty | **Y** | ? | stream.zig:2433-2446 `inline 22, 23 … .title_push / .title_pop`, and it reads the optional index parameter | V |
| iTerm2 | **Y** | NSMutableArray | `XTERMCC_PUSH_TITLE` in VT100Token.{h,m}/VT100CSIParser.m; `pushWindowTitle/popWindowTitle/pushIconTitle/popIconTitle` in iTermSessionNameController.m; docs/names-and-titles.md says "Window and icon titles support push/pop stacks (standard xterm behavior)" | V |
| tmux | **Y (per pane)** | screen title stack | input.c:2185-2210: `case 22:`/`case 23:` accept sub-param 0 or 2 → `screen_push_title()` / `screen_pop_title()`, and the pop fires `input_fire_pane_title_changed` + status redraw. **Sub-param 1 (icon) is a no-op.** This operates on the *pane* title, not on the outer terminal's title | V |
| **Konsole** | **N/A — explicitly discarded** | — | Vt102Emulation.cpp:2054-2055: `case token_csi_ps('t', 22) : /* IGNORED: Save icon and window title on stack */ break;` and the same for 23. The bytes are consumed, no error is reported, nothing happens | V |
| **WezTerm** | **N/A — parsed then dropped** | — | wezterm-escape-parser/src/csi.rs:2635-2645 parses `Window::PushIconAndWindowTitle` / `PopIconAndWindowTitle`, but term/src/terminalstate/mod.rs:2212-2217 matches all six push/pop title variants to `{}` — an empty arm placed *above* the "unhandled" logging arm, so it is not even logged | V |
| **Windows Terminal / conhost** | **N** | — | `enum WindowManipulationType { Invalid=0, DeIconifyWindow=1, IconifyWindow=2, RefreshWindow=7, ResizeWindowInCharacters=8, ReportTextSizeInPixels=14, ReportCharacterCellSize=16, ReportTextSizeInCharacters=18 }` — DispatchTypes.hpp:584-594. No 22/23 | V |
| **VS Code / xterm.js** | **cfg, default off** | 10 | InputHandler.ts:77-78 `case 22: return !!opts.pushTitle; case 23: return !!opts.popTitle;` — gated on `ITerminalOptions.windowOptions`, whose default is `windowOptions: {}` (OptionsService.ts:53). VS Code does not set it | V |
| **GNU screen** | **N** | — | ansi.c:961 `case 't':` handles only `7` (refresh) and `8` (resize) | V |
| ConEmu | N | — | not in conemu.github.io's documented set | I |
| Terminal.app | ? | — | no source, not documented; must be measured on a Mac | I |

**Verdict for issue #11.** The title stack is *not* a portable capture/restore primitive.
It works in 9 of 15, is a silent no-op in Konsole and WezTerm (the project's own primary
target is one of them), is absent in Windows Terminal and screen, and is off by default in
VS Code. Worse, `Ps=1` (icon title) is a no-op in VTE and tmux even where the window title
works. Anything that *depends* on a 23t restore actually happening needs a fallback that
re-asserts a known-good title, and it must never be the only restore path.

---

## 3. Tab / session colour by escape sequence

| terminal | mechanism | ev |
|---|---|---|
| Konsole | **`OSC 34 ; <color> BEL`** — `SessionColor = 34` (Session.h:475) → `Session::setSessionAttribute` case `SessionColor` → `QColor::fromString` → `setColor()` → `_tabColor` + `tabColorSetByUser(true)` (Session.cpp:704-711, 2407-2415). Also `OSC 50 ; TabColor=#RRGGBB BEL` via `ProfileChange=50` → `profileChangeCommandReceived` → `SessionManager::sessionProfileCommandReceived` builds a runtime profile; `Profile::TabColor` exists (Profile.h:362, 659-662). The repo already uses OSC 50 for the tab-title format (src/emit.rs:99) | V |
| iTerm2 | `OSC 1337 ; SetColors=tab=RRGGBB ST`, and `=default` to clear | V |
| ConEmu | no colour, but `OSC 9 ; 3 ; "txt" ST` changes the **tab text** independently of the window title | V |
| WezTerm | no direct escape. `OSC 1337 ; SetUserVar=<name>=<base64> ST` (wez_osc.rs:1269-1280) sets a pane user var that a lua `format-tab-title` handler reads. i.e. **colour requires user config on the other side** | V |
| kitty | no escape. `kitty @ set-tab-color` / `set-tab-title` over remote control, which needs `allow_remote_control` and `KITTY_LISTEN_ON`; that is a socket round trip, not an OSC | V (env/API), I (exact rc verb) |
| GNOME Terminal / VTE | settings-file only. VTE exposes *termprops* (progress, shell precmd/preexec, containers) but nothing that colours a tab | V |
| Windows Terminal | settings-file only (`tabColor` in the profile). `OSC 9001` (`WTAction`) exists but is used for focus mode etc. | V (enum), I (no colour verb) |
| VS Code | settings/extension API only (`vscode.window.createTerminal({color})`) | I |
| xterm, Alacritty, foot, Ghostty, Terminal.app, tmux, screen | no per-tab colour escape (tmux/screen colour their own status line from format strings, which is what this project already exploits) | V for tmux/screen, I otherwise |

---

## 4. Desktop notification

Three competing grammars:
* **OSC 9** (iTerm2): `ESC ] 9 ; <text> ST` — body only, no title.
* **OSC 777** (urxvt): `ESC ] 777 ; notify ; <title> ; <body> ST`.
* **OSC 99** (kitty): `ESC ] 99 ; <k=v:k=v> ; <payload> ST`, keys
  `i`(id) `d`(done) `p`(title|body|close|icon|?|alive|buttons) `e`(base64) `o`(always|unfocused|invisible)
  `u`(urgency 0/1/2) `w`(auto-close ms) `a`(report,focus) `c` `f` `g` `n` `s` `t`
  (sw.kovidgoyal.net/kitty/desktop-notifications/).

| terminal | OSC 9 | OSC 777 | OSC 99 | ev |
|---|---|---|---|---|
| kitty | Y (title = whole payload) | Y (title;body) | Y (full protocol) | notifications.py:1089-1106 | V |
| foot | Y | Y | Y | foot-ctlseqs.7.scd | V |
| Ghostty | Y | Y | Y | osc.zig:953-961 (`kitty_desktop_notification`, `rxvt_extension`, `iterm2`) | V |
| Konsole | **N** (OSC 9 is routed to the ConEmu progress handler, Vt102Emulation.cpp:1861) | Y — `Notification = 777` → `osc777Received` signal + `KNotification::event` with `ProcessNotification` / `ProcessNotificationHidden` depending on focus, and a "Show session" default action carrying an `xdgActivationToken` (Vt102Emulation.cpp:1261-1290) | Y — `KittyNotification = 99` with `i/c/d/e/o/u/p/n/f` parsing (Vt102Emulation.cpp:1324-1580) | V |
| VTE / GNOME Terminal | routed to the ConEmu handler (progress only) | **cfg** — `urxvt_extension()` returns immediately unless `enable_legacy_osc777()`; and even then only `notify;Command completed` is honoured, as a `SHELL_POSTEXEC` termprop. **Not a desktop notification.** (vteseq.cc:2034-2095) | N | V |
| iTerm2 | Y | N | N | iterm2.com/documentation-escape-codes.html | V |
| WezTerm | Y ("toast") | Y (notify extension only) | N | wezterm.org/escape-sequences.html | V |
| Windows Terminal | see progress | **cfg** — `DoUrxvtAction` handles `notify;title;body` but returns early unless `OptionalFeature::DesktopNotification` is set from settings (adaptDispatch.cpp:3880-3903; wired in TerminalCore/Terminal.cpp). conhost never sets it | V |
| VS Code | N | N | **Y** — `xterm.raw.parser.registerOscHandler(99, …)` in terminalContrib/notification/browser/terminal.notification.contribution.ts, itself gated on `TerminalOscNotificationsSettingId.EnableNotifications` | V |
| xterm | N | N | N | not in ctlseqs | V |
| Alacritty | N | N | N | vte crate OSC table has 0/2,4,8,10/11/12,22,50,52,104,110/111/112 only | V |
| ConEmu | `9;2` = GUI MessageBox (modal, not a toast) | N | N | conemu.github.io | V |
| Terminal.app | N | N | N | — | I |
| **tmux** | **intercepted** — `input_osc_9` requires the payload to start `4` and otherwise returns; a plain `OSC 9;text` is silently discarded (input.c:3028-3040) | **N** — 777 is not in tmux's OSC switch (input.c:2720-2775), logged "unknown" and dropped | **N** — same | V |
| **GNU screen** | N | N | N | ansi.c:1266 whitelist is `0,1,2,11,20,39,49` (+83) | V |

**The single most consequential row in this table:** inside tmux, *every* notification
grammar dies. OSC 9 is eaten by tmux's progress parser, 777 and 99 are dropped. The only
way a notification reaches the outer terminal from inside tmux is the passthrough wrapper
(§7) — and Claude Code's `terminalSequence` allowlist (OSC 0,1,2,9,99,777 + BEL, per
src/emit.rs:4-7) is exactly the set tmux destroys.

---

## 5. Taskbar / progress — `OSC 9 ; 4 ; <state> ; <progress>`

ConEmu grammar (conemu.github.io/en/AnsiEscapeCodes.html, and restated at
learn.microsoft.com/windows/terminal/tutorials/progress-bar-sequences):
`ESC ] 9 ; 4 ; st ; pr BEL`, st = 0 hide / 1 normal / 2 error / 3 indeterminate / 4 warning-paused,
pr = 0..100.

| terminal | support | detail | ev |
|---|---|---|---|
| ConEmu | Y (origin) | Windows 7 taskbar progress | V |
| Windows Terminal | Y | tab-header progress ring + Windows taskbar; needs WT ≥ 1.6, and taskbar animation needs "Show animations in Windows" | V |
| Konsole | Y (partial) | Vt102Emulation.cpp:1861-1894: state 0 → `progressHidden()`, 1 → `progressChanged(pr)`, **3 → `progressHidden()`**, and 2 (error) and 4 (paused) are `// TODO` no-ops | V |
| VTE / GNOME Terminal | Y | `conemu_extension()` vteseq.cc:2148-2215 → `VTE_PROPERTY_ID_PROGRESS_HINT` / `_VALUE` termprops. **Refuses BEL termination — ST only** (`if (seq.is_st_bel()) return;`, vteseq.cc:2160-2162) | V |
| kitty | Y (full) | progress.py `ProgressState{unset,set,error,indeterminate,paused}`; window.py:1380-1397 routes `OSC 9` whose payload starts `4;` to progress and everything else to the notifier; auto-clears after 60 s (5 s once at 100 %) | V |
| Ghostty | Y | `conemu_progress_report` in osc.zig:125,197,439; listed in ghostty.org/docs/vt/reference as "OSC 9;4 Report progress state" | V |
| WezTerm | Y (parsed) | wez_osc.rs:320-351, `ConEmuProgress(Progress::{None,SetPercentage,SetError,SetIndeterminate,Paused})` | V |
| **tmux** | **Y — and it re-emits outward** | `input_osc_9` (input.c:3028+) → `input_set_progress_bar`; outward via `tty_set_progress_bar` → terminfo cap `Spb` (tty.c:3053-3056), defined by the `progressbar` feature as `Spb=\E]9;4;%p1%d;%p2%d\E\\` (tty-features.c:373-381). Granted by `tty_default_features` to tmux, iTerm2, ghostty, Rio only. **New in tmux 3.6+ — version-dependent** | V |
| iTerm2 | Y | documented `OSC 9 ; 4 ; [st] ; [pr] ST` | V |
| foot | N | not in foot-ctlseqs | V |
| Alacritty, xterm, VS Code, GNU screen, Terminal.app | N | — | V for the first four (source/OSC tables), I for Terminal.app |

**Trap:** OSC 9 is overloaded. In kitty/Ghostty/WezTerm/iTerm2/tmux the `4;` prefix
disambiguates; in Konsole a payload that is *not* `4;…` falls through to
`_pendingSessionAttributesUpdates[9] = value` and does nothing visible; in VTE a
BEL-terminated `9;4` is dropped on purpose. A backend that emits progress must use **ST**,
not BEL, if it wants VTE.

---

## 6. BEL and window-manager urgency

| terminal | behaviour | ev |
|---|---|---|
| xterm | `bellIsUrgent` resource, **default False** (charproc.c:427). Runtime-settable by the application: DEC private mode **1042** (`srm_BELL_IS_URGENT = 1042`, ptyx.h:1269) — so `CSI ? 1042 h` turns the X11 URGENT WM hint on for subsequent bells. `popOnBell` is the neighbouring mode | V |
| VTE / GNOME Terminal | BEL sets `m_bell_pending` (vteseq.cc:2469, 2585) → `bell` signal; gnome-terminal turns that into an urgency hint / needs-attention | V (signal), I (WM step) |
| Konsole | bell modes are profile settings ("Bell in session" notification / visual bell); OSC 777 and OSC 99 carry an explicit urgency instead (`u=0/1/2`) | V |
| kitty | OSC 99 `u=` maps to the notification urgency; `window_alert_on_bell`, `bell_on_tab` are config | V(protocol)/I(config names) |
| Windows Terminal | BEL → taskbar flash, configurable via `bellStyle` | I |
| tmux | BEL becomes a tmux *alert* (`monitor-bell`, `bell-action`, `visual-bell`); whether it is forwarded to the outer terminal depends on `bell-action`. The `#{session_alerts}` format is already in tmux's default `set-titles-string` | I |
| GNU screen | `vbell`, `bell_msg`; BEL becomes a window-activity flag | I |
| Wayland | there is no per-surface urgency hint; the equivalent is `xdg-activation-v1` request-activate, which is what Konsole's notification `xdgActivationToken` path uses (Vt102Emulation.cpp:1285-1289). **A terminal cannot raise urgency on Wayland without a desktop-portal/notification round trip** | V (Konsole code), I (protocol generality) |

---

## 7. Multiplexer passthrough

### tmux
* Wrapper: `DCS tmux ; <payload with every ESC doubled> ST`, i.e. `\ePtmux;\e\e]…\a\e\\`.
  input.c:2620-2682: the DCS payload must start with the literal `"tmux;"`, and the remainder
  is handed to `screen_write_rawstring`.
* Gate: `allow-passthrough`, **default `off` (0)**, scope window|pane, choices off/on/all
  (options-table.c:1269-1278). `on` = only while the pane is visible; `all` = even when
  invisible (the `allow_passthrough == 2` argument to `screen_write_rawstring`).
* Consequence: passthrough writes straight to the attached client's terminal. It bypasses
  tmux's own model entirely — **it does not touch `pane_title`**, which is precisely the
  behaviour src/emit.rs:10-12 records for Claude Code ≥ 2.1.274.
* `update-environment` default is `"DISPLAY KRB5CCNAME MSYSTEM SSH_ASKPASS SSH_AUTH_SOCK
  SSH_AGENT_PID SSH_CONNECTION WAYLAND_DISPLAY WINDOWID XAUTHORITY XDG_CURRENT_DESKTOP
  XDG_SESSION_DESKTOP XDG_SESSION_TYPE"` (options-table.c:1207-1217) — **no terminal
  identity variable is on it**, so whatever the tmux *server* was started with is what every
  pane sees forever.

### GNU screen
* Wrapper: plain `DCS <payload> ST`. ansi.c:1317-1318, `case DCS: LAY_DISPLAYS(&win->w_layer, AddStr(win->w_string));`
  — **unconditional, no option to enable, no `screen;` prefix**. Different wrapper, different
  gate semantics, same intent.

### What survives each layer

| bytes | bare terminal | inside tmux | inside screen |
|---|---|---|---|
| OSC 0 / OSC 2 | reaches terminal | captured as `pane_title`; outer title only if `set-titles` on **and** the `title` feature granted | captured as window title, and re-emitted to the display if that window is in the foreground (`SetXtermOSC`) |
| OSC 1 | reaches terminal (except Alacritty) | **dropped** | forwarded |
| OSC 9 notification | reaches terminal | **eaten by the progress parser** | dropped |
| OSC 9;4 progress | reaches terminal | consumed, re-emitted outward via `Spb` if the feature is granted | dropped |
| OSC 777 / OSC 99 | reaches terminal | **dropped** | dropped |
| OSC 34 / OSC 50 (Konsole) | reaches terminal | **dropped** | dropped |
| CSI 22t/23t | see §2 | applies to the **pane** title, never the outer one | dropped |
| anything, wrapped | — | reaches the outer terminal iff `allow-passthrough` ≠ off | reaches the display always |

---

## 8. Detection table — env var → terminal, and what it survives

| signal | identifies | leaks into children | survives ssh | survives tmux | ev |
|---|---|---|---|---|---|
| `KONSOLE_VERSION`, `KONSOLE_DBUS_SESSION`, `KONSOLE_DBUS_SERVICE`, `KONSOLE_DBUS_WINDOW` | Konsole | **yes, into everything** | no (not in `SendEnv`) | **yes, and worse: the tmux *server* inherits them from its first client and hands them to every pane of every session forever** (`update-environment` has no `KONSOLE_*`) | V |
| `VTE_VERSION` | VTE ≥ 0.34 (GNOME Terminal, Tilix, Terminator, Ptyxis, …) — **not** a specific app | yes | no | yes (frozen at server start) | V |
| `TERM_PROGRAM` / `TERM_PROGRAM_VERSION` | iTerm2 / Apple_Terminal / vscode / WezTerm / ghostty / tmux(!) — a *shared* namespace with no registry | yes | **sometimes yes**: Ghostty's `ssh --forward-env` explicitly requests `SendEnv` of `COLORTERM,TERM_PROGRAM,TERM_PROGRAM_VERSION` (ghostty src/cli/ssh.zig) → **a remote host can see a local terminal's identity and then be wrong about everything else** | yes, frozen | V |
| `ITERM_SESSION_ID` | iTerm2, non-empty dedicated session hint | yes | only if configured | yes, inherited | V ([iTerm2 source](https://github.com/gnachman/iTerm2/blob/v3.5.0/sources/PTYSession.m#L2471)) |
| `LC_TERMINAL=iTerm2` | iTerm2, exact value; other values do not identify iTerm2 | yes | only if configured on both ends; `LC_*` forwarding is not guaranteed | yes, inherited | V ([iTerm2 source](https://github.com/gnachman/iTerm2/blob/v3.5.0/sources/PTYSession.m#L2451)) |
| `KITTY_PID`, `KITTY_WINDOW_ID`, `KITTY_LISTEN_ON`, `KITTY_INSTALLATION_DIR`, `KITTY_PUBLIC_KEY` | kitty | yes | no | yes, frozen — and `KITTY_LISTEN_ON` will point at a socket that is no longer the right window | V |
| `WEZTERM_PANE`, `WEZTERM_UNIX_SOCKET`, `WEZTERM_EXECUTABLE` | WezTerm | yes | no | yes, and `WEZTERM_PANE` becomes **wrong**, not merely stale | I (env names), V (prefix list below) |
| `ALACRITTY_WINDOW_ID`, `ALACRITTY_SOCKET`, `ALACRITTY_LOG` | Alacritty | yes (`builder.env("ALACRITTY_WINDOW_ID", …)`, alacritty_terminal/src/tty/unix.rs) | no | yes, stale | V |
| `GHOSTTY_RESOURCES_DIR`, `GHOSTTY_BIN_DIR` | Ghostty | yes | no | yes | V |
| `WT_SESSION`, `WT_PROFILE_ID` | Windows Terminal | yes | n/a | yes | V (name), I (WSL propagation) |
| `ConEmuPID`, `ConEmuANSI`, `ConEmuBuild` | ConEmu | yes | n/a | yes | I |
| `TERM=xterm-kitty` / `foot` / `xterm-ghostty` / `wezterm` / `alacritty` | that terminal, **only when terminfo is installed and no wrapper rewrote TERM** | yes | yes (`TERM` is the one variable ssh always sends) — **and this is the trap: `TERM=xterm-kitty` on a box with no kitty terminfo is a broken session, so people override it** | **no** — tmux/screen replace it with `screen*`/`tmux*` | V |
| `TMUX`, `TMUX_PANE` | inside tmux | yes | no | — | V |
| `STY`, `WINDOW` | inside GNU screen | yes | no | — | V |
| `SSH_TTY`, `SSH_CONNECTION`, `SSH_CLIENT` | this shell came in over ssh | yes | — | **refreshed on reattach** (`SSH_CONNECTION` *is* on `update-environment`) but only for processes started after the reattach | V |
| XTVERSION reply `DCS > | <name> <version> ST` | the actual terminal, authoritatively | — | — | — | V |

### Automatic probe policy

The implemented probe tables are narrower than the survey above. Linux retains
non-empty `KONSOLE_VERSION` then `KONSOLE_DBUS_SESSION`; Windows retains non-empty
`WT_SESSION`. macOS uses this ordered table:

| Variable | Rule | Family | Primary evidence |
|---|---|---|---|
| `ITERM_SESSION_ID` | non-empty | iTerm2 | [iTerm2 v3.5.0 session environment](https://github.com/gnachman/iTerm2/blob/v3.5.0/sources/PTYSession.m#L2471) |
| `LC_TERMINAL` | exactly `iTerm2` | iTerm2 | [iTerm2 v3.5.0 environment assignment](https://github.com/gnachman/iTerm2/blob/v3.5.0/sources/PTYSession.m#L2451) |
| `TERM_PROGRAM` | exactly `iTerm.app` | iTerm2 | [iTerm2 v3.5.0 environment assignment](https://github.com/gnachman/iTerm2/blob/v3.5.0/sources/PTYSession.m#L2474) |
| `TERM_PROGRAM` | exactly `Apple_Terminal` | Terminal.app | [Apple-distributed ncurses terminal description](https://github.com/apple-oss-distributions/ncurses/blob/main/ncurses/misc/terminfo.src#L978) documents the exported value |

Value matching is case-sensitive byte equality against these bounded literals,
not the project's surface names. No trimming, substring matching, Unicode case
folding or string repair is performed. Other `LC_TERMINAL` values, including
`WezTerm`, are not mapped here; no verified vendor assignment is recorded for
them. Other `TERM_PROGRAM` values, including `tmux`, are ineligible in this table.
An ineligible value does not prevent a later eligible probe from answering.
Dedicated session ID evidence keeps its existing precedence when hints disagree;
`LC_TERMINAL` keeps its earlier priority ahead of the new `TERM_PROGRAM` fallback.

The resolution ladder is explicit non-empty override, mux hint, eligible probes,
then Unknown. Override names alone use ASCII case-insensitive matching; unknown
non-empty overrides stop the ladder and empty overrides do not. Non-empty `TMUX`
or `STY` veto all inherited macOS and Linux probes, including malformed or disabled
tmux. Windows' existing `WT_SESSION` eligibility is unchanged.

Doctor uses the same matching walk and ladder. Presence evidence keeps its
`$VARIABLE` spelling; value evidence includes the matched literal, for example
`$LC_TERMINAL=iTerm2`. These are family hints, not running-version evidence or
confirmation that a sequence was applied. Capabilities and version verdicts are
unchanged. SSH is not a detection veto: forwarding requires appropriate client
[SendEnv](https://man.openbsd.org/ssh_config#SendEnv) and server
[AcceptEnv](https://man.openbsd.org/sshd_config#AcceptEnv) configuration; a missing
variable does not identify the local terminal.

**The asymmetry that makes env detection unsound.** Only one terminal cleans up after the
others. GNOME Terminal scrubs, before spawning, the exact prefixes
`ALACRITTY_ CONTOUR_ FOOT_ GHOSTTY_ ITERM2_ KITTY_ KONSOLE_ MC_ MINTTY_ MOSH_ PUTTY_ RXVT_
TERM_ TERMINAL_ URXVT_ WEZTERM_ XTERM_ ZUTTY_ GNOME_TERMINAL_` plus the names
`STY TMUX TMUX_PANE VTE_VERSION WT_SESSION WT_PROFILE WINDOWID TERM TERMCAP COLORTERM`
(gnome-terminal src/terminal-client-utils.cc:211-267, 269-298). Every other terminal
inherits whatever was already there. So a stale `KONSOLE_VERSION` is cleared by launching
GNOME Terminal and by nothing else. **Presence of a variable is weak evidence; absence is
none at all.**

Two more traps worth naming:
* `TERM_PROGRAM` is set to `tmux` by tmux itself in recent versions, i.e. the variable that
  is supposed to name the terminal names the multiplexer instead.
* `VTE_VERSION` identifies a *library*, not a product. Any code that maps it to
  "GNOME Terminal" is wrong for Tilix, Terminator, Ptyxis, Guake, Black Box and Xfce Terminal,
  which differ on tab colour, notification and menu behaviour.

---

## 9. Terminal queries — can capability detection be dynamic?

The candidate queries and their replies:

| query | bytes out | reply | ev |
|---|---|---|---|
| DA1 | `CSI c` | `CSI ? 62 ; Ps… c` | V |
| DA2 | `CSI > c` | `CSI > Pp ; Pv ; Pc c` | V |
| XTVERSION | `CSI > q` | `DCS > | <name> <version> ST` — e.g. Konsole answers `\033P>|Konsole 25.x\033\\` (Vt102Emulation.cpp:2737-2740) | V |
| XTGETTCAP | `DCS + q <hex names> ST` | `DCS 1 + r <hex> ST` (valid) or `DCS 0 + r ST` | V |
| DECRQSS | `DCS $ q <Pt> ST` | `DCS 1 $ r <Pt> ST` or `DCS 0 $ r ST` | V |
| OSC 4 colour | `OSC 4 ; n ; ? ST` | `OSC 4 ; n ; rgb:…` | V |

**Verdict: not viable for this hook. Hard no.**

1. **There is nowhere to read the reply.** The reply is typed *into the pty as terminal
   input*. The hook's stdin is `/dev/null`; the pty's input side is owned by Claude Code's
   TUI, which is in raw mode and reading it. To see the answer the hook would have to steal
   bytes from the application's own input stream. Even if it succeeded it would have
   corrupted a keystroke; if it failed the reply is injected as text into the user's session.
2. **There is no bounded wait.** Every one of these queries is answered only by terminals
   that implement it. Terminals that do not, answer nothing — the exact case detection cares
   about. So the only possible timeout is a wall-clock one, and the budget is 370 µs total
   (one `tmux set-option` round trip already costs 2.84 ms, i.e. ~7.7× the whole program).
   A query that must tolerate a slow local terminal needs tens of milliseconds.
3. **Inside tmux, queries answer about tmux, not the terminal.** tmux is a full emulator; it
   answers DA1/DA2/XTVERSION itself and only asks the outer terminal at client-attach time
   (`tty_puts(tty, "\033[>q")`, tty.c:416). A query from a pane can never learn the outer
   terminal's identity.
4. **Over ssh a query costs a network RTT**, and it fails open in exactly the way that
   matters — a lost reply looks like "not supported".
5. **Reply-eating cascades.** tmux, screen and Claude Code all parse the pty; a query whose
   reply is swallowed leaves the hook waiting and, on timeout, having emitted stray bytes.

**What is viable, and where it belongs.** tmux already solves this problem properly: a
*long-lived* process asks XTVERSION once at attach and caches a feature set (tty-keys.c:1654+,
tty-features.c:573-686). That is the correct shape — query at a session boundary in something
that owns the pty, cache, and let short-lived tools read the cache. For this project, the
SessionStart edge is the only place that could do it, and it would have to persist the answer
to the state directory for the hot edges to read. That is a *possible* future feature, not a
detection strategy the hot path can rely on. **Capability detection must be static:
compile-time `cfg`, plus an env/config-derived backend choice, plus an explicit user
override (`CCTAB_TERMINAL`, which the repo already has at src/config.rs:73-94).**

---

## 10. Where the boolean-flag model and the named-backend model disagree

These are the concrete rows that break each model.

1. **Konsole and WezTerm accept `CSI 23 t` and do nothing.** A boolean `supports_title_stack`
   is unknowable from the wire; a named backend simply knows. → favours named backends.
2. **VTE takes `OSC 9;4` only with ST, never BEL**, and kitty/Konsole take either. The
   difference is in the *encoding of a capability both have*, not in whether they have it.
   A boolean cannot express it. → favours named backends.
3. **tmux has `OSC 9;4` inward and `Spb` outward**, and the outward half depends on a feature
   granted to the *outer* terminal. One process therefore faces two capability sets
   simultaneously. Neither a flat flag set nor a single named backend covers that. → favours
   a **stack** of surfaces, not one.
4. **xterm's title stack is on by default but its title *query* is off by default**
   (`disallowedWindowOps = "GetIconTitle,GetWinTitle"`), and `bellIsUrgent` is off by default
   but can be turned on by the application with `CSI ? 1042 h`. Capabilities are not static
   properties of a named terminal; some are *modes the program can change*. → neither model
   alone.
5. **VS Code's `pushTitle`/`popTitle` are real code behind an option that defaults to
   `windowOptions: {}`.** The name "VS Code" does not determine the answer; a setting does.
   → a named backend must still carry per-capability confidence, not a single boolean.
6. **`VTE_VERSION` names a library shared by ~8 products** with different tab/notification
   behaviour. "Named backend" is therefore not the same as "detected name".
7. **Notification grammars are 3-way and non-overlapping** (kitty: 9+777+99; iTerm2: 9 only;
   Konsole: 777+99 but *not* 9; VS Code: 99 only; VTE: none). A single boolean
   `can_notify` loses the information needed to actually emit anything.
