# Backend scouting: what Windows, KDE and macOS would actually need

Evidence for [#14](https://github.com/dalf/claude-tabstatus/issues/14), and for the
ports it must not silently expand into ([#1](https://github.com/dalf/claude-tabstatus/issues/1)
macOS, [#3](https://github.com/dalf/claude-tabstatus/issues/3) Windows) and the
appearance work it must coordinate with
([#11](https://github.com/dalf/claude-tabstatus/issues/11)).

This file is **findings, not design**. The architecture they support is in
[backend-architecture.md](backend-architecture.md); every claim it makes about a
platform should be traceable to a row here. Kept separate on purpose: a design can
be revised without re-deriving the evidence, and evidence that is wrong should
falsify the design rather than hide inside it.

Base commit for every code citation: **`9bfd987`**, on 2026-09-28. Claims about
`adopt_konsole`/`adopt_terminal` belong to the uncommitted
[#18](https://github.com/dalf/claude-tabstatus/issues/18) work and are **not** in
this tree; where that matters it is said so explicitly.

**Citation drift, stated once rather than chased into every line.** This is a dated
snapshot and the branch carrying it has since moved the code it cites — *this branch
is what moved it*. Six citations now name a path or line that reads differently:
`src/tmux.rs` is `src/mux/tmux.rs`; the `exec 3>/dev/tty` prose once at
`emit.rs:167-172` is at `surface/rows.rs:42`; the `TabColor` seam note is at
`surface/rows.rs:86`; `emit.rs:106` and `tmux.rs:596` are both inside `mux::route()`
now; `Command::new` is **18** sites rather than 14, across more files; and
`location.rs:194`'s "the ONLY fork in this binary" comment sits at line 220 — still
there, and still false. Every *finding* below survives its citation moving. Follow
the prose, not the line number.

> ## What this branch's base has overtaken
>
> This file is a **dated snapshot**, and it is carried unchanged except for the
> markers below, because evidence that is quietly rewritten to agree with what
> happened is no longer evidence. Its base is `9bfd987`; this branch's base is
> `a1e4153`, the head of
> [#28](https://github.com/dalf/claude-tabstatus/pull/28)'s Windows port. Five
> things it says are no longer true of the tree, and each is marked where it
> appears:
>
> * **§1's Windows census is history.** 88 errors became **0 errors, 0 warnings**,
>   re-measured on this branch's HEAD for `x86_64-pc-windows-gnu` and
>   `aarch64-apple-darwin`, plain and `--all-targets`. More than that: #28 is a real
>   port, built and tested on a `windows-2025` runner in CI. Windows **works**.
> * **§8's `ls src/*.rs` row was acted on by #28, not by this work.** `a1e4153`
>   spells `find src -type f -name '*.rs' | LC_ALL=C sort` in both `scripts/build.sh`
>   and `tests/run.sh`. The credit for that fix is #28's.
> * **Two of the Open questions were answered by shipping.** Numbers 1 and 6 are
>   marked below with what #28 did about them.
> * **macOS has a backend now, and "no backend on macOS" is the reading to drop.**
>   `libc` is a `cfg(target_os = "macos")` dependency of this tree with zero
>   transitive dependencies, `sys::session_tty` resolves fd 1 there, and
>   `sys::HAS_SESSION_TTY` is `true`, so session-start and session-end paint. §1's
>   "silently deliver nothing", §5's `libc` row and §6's macOS session-terminal row
>   are each marked. **Nothing in it has ever been run on a Mac**, which is why the
>   marks say what is asserted at compile time and what is not.
> * **Open question 4 is answered too**, by a scouting pass this file did not
>   originally contain: §4 gained a subsection on what `replaces_id` is worth
>   without the reply, and the question below points at it.
>
> Everything else here — the hook-protocol ceiling, the capability matrix, the
> dependency verdicts, the prior art, the §9 reasoning — was re-read against this
> tree and still holds.

## Method, and what this is not

Fourteen agents surveyed the tree and the backends on 2026-09-28. Two checks were
run here, one dependency-free D-Bus client was written and executed here, and the
shipped `claude` binary was read here. Everything else is a vendor source or
vendor document, cited inline.

| mark | meaning |
|---|---|
| **V** | verified in this session: a command was run, a source file was read, or a vendor doc was read |
| **I** | inferred: reasoned from adjacent evidence, not observed |

**No macOS or Windows machine was available.** Nothing below is a native
observation on either, and the `cargo check --target` results are type-checks, not
runs. **Cargo had no network**, so no proposed dependency was compiled; transitive
counts come from crates.io/lib.rs metadata, not from a resolved lockfile. Claims
that need real hardware are collected in [Open questions](#open-questions).

## 1. The measured portability census

Run in this worktree with rustc 1.98.1 (`rust-version = "1.89"`). **V**

| target | `cargo check` | `--all-targets` |
|---|---|---|
| `x86_64-pc-windows-gnu` | **88 errors, 0 warnings** | 102 errors, 1 warning |
| `aarch64-apple-darwin` | **0 errors, 0 warnings** | 0 errors, 0 warnings |

> **HISTORY. Re-measured on this branch's HEAD**, which sits on
> [#28](https://github.com/dalf/claude-tabstatus/pull/28):
> `x86_64-pc-windows-gnu` **0 errors, 0 warnings**, plain and `--all-targets`;
> `aarch64-apple-darwin` the same. The 88 below are what the pre-port tree cost, and
> the table under them is still the right shape for anyone reading how a port of
> this kind decomposes — but it is not a measurement of this tree.

Both numbers already in the tree are stale: the README says 53 errors across six
files, [#3](https://github.com/dalf/claude-tabstatus/issues/3) says 80 across nine.
Any future count must also say which scope it measured — the extra 14 live in
`src/tree.rs`'s `#[cfg(test)]` module, which plain `cargo check` does not compile.

**Every Windows error is a missing std extension trait. None is a logic error**,
and no dependency failed on either target.

| root cause | count | where |
|---|---|---|
| `std::os::unix::ffi::{OsStrExt, OsStringExt}` — byte-oriented `OsStr` | **65 (74%)** | config, edge, emit, git, location, tmux, manage, tree |
| `std::os::unix::fs::PermissionsExt` / `OpenOptionsExt` mode bits | 8 | manage, tree |
| `std::os::unix::fs::MetadataExt::{dev, ino}` file identity | 5 | state, location |
| `std::os::unix::fs::symlink` | 4 | manage, tree |
| `std::os::unix::fs::FileTypeExt::is_char_device` | 3 | emit |
| remainder (`DirBuilderExt::mode`, misc) | 3 | state |

**macOS is the harder case precisely because it compiles** — and it is the one that
is *still* true here, because #28 ported Windows and not macOS. macOS takes
`src/sys/unix.rs`, so five `/proc` reads and the `XDG_RUNTIME_DIR` state directory
type-check and then mean the wrong thing at runtime. What changed is that the seam
now documents each one where it is written (`process_start_time` says "or on a Unix
with no `/proc`"), so the degradation is recorded rather than invisible. As of
`9bfd987`: `emit::session_tty` always returns `None` so session-start, session-end and
every tmux pane write silently deliver nothing; `state::start_time` yields no
process identity so the reaper falls back to a 24-hour file age; `manage::sweep_litter`
reads a missing `/proc/<pid>` as a dead process and can delete a live concurrent
installer's temporary file. A green type-check is not evidence of support, and the
build script is right to refuse to list a target it has not run.

> **ALL THREE OF THESE READS ARE ANSWERED NOW, not just documented.** Re-checked by
> grep in this tree rather than recalled: **every `"/proc` string in `src/` sits
> inside a `cfg(not(target_os = "macos"))` item**, each with a macOS answer beside
> it.
>
> * `session_tty` no longer returns `None`: it resolves fd 1 through
>   `proc_pidfdinfo(PROC_PIDFDVNODEPATHINFO)` and `sys::HAS_SESSION_TTY` is `true`,
>   so **session-start arms the tab and session-end clears it**. The tmux pane
>   writes were never affected in the first place — `sys::write_tty` takes a path
>   tmux names, not fd 1.
> * `state::start_time` goes through `sys::process_start_time`, which on macOS is
>   `proc_pidinfo(PROC_PIDTBSDINFO)` with `pbi_start_tvsec`/`_tvusec`, under its own
>   `sys::ORIGIN_KEY` of `r` so no other platform's number is ever compared to it.
> * `manage::sweep_litter` never reads `/proc` directly; it calls
>   `sys::process_alive`, which on macOS is `kill(pid, 0)` with the ESRCH/EPERM
>   mapping, and unknown liveness counts as alive so a concurrent installer's temp
>   file is not removed.
>
> The `XDG_RUNTIME_DIR` clause is answered too: `sys::RUNTIME_DIR_VAR` is `TMPDIR`
> on macOS.
>
> **What does NOT change is the sentence that closes this paragraph.** A green
> type-check is still not evidence of support, and nothing in the macOS route has
> ever been RUN. What is new is that the one hand-written layout is no longer a
> type-check's silence but a compile-time assertion that fails the build — see Open
> question 2 — which narrows what "compiles and is silently wrong" can mean here
> without eliminating it.

**The test suite is more Linux-welded than the Rust is.** `tests/run.sh` is 625
POSIX-sh assertions using symlinks, octal mode bits and tmux; three Python modules
(`fcntl`, `pty`, `termios`) fail at *import* on Windows. Porting the code does not
port the specification.

> **Still true, and #28 answered it by splitting the suite rather than porting it.**
> Its CI runs `cargo test --locked --all-targets` on a `windows-2025` runner — the
> in-crate tests, including Windows-only ones for ConPTY and the record lock — plus
> four of the six Python suites on native Windows Python. `tests/run.sh`, the golden
> corpus replay, `test_state_guarantees` and `test_tmux_status` stay Linux-only, and
> `tests/run.sh` is 806 assertions on this branch's HEAD.

## 2. The hook-protocol ceiling

Read from the shipped `/usr/bin/claude` 2.1.274 on this machine. **V** This bounds
every backend, so it is recorded before any capability table.

```js
var Keo = new Set([0,1,2,9,99,777]), Veo = 4096;
```

* **Allowlist**: OSC 0, 1, 2, 9, 99, 777, plus a bare BEL. **4096-byte cap** on the
  whole field. A bare `ESC` anywhere else rejects the entire field, not just the token.
* **The bytes are re-serialised, not passed through.** The terminator is forced to
  BEL, except under kitty where it is ST. So a backend cannot choose its own
  terminator through this path.
* **tmux/screen DCS passthrough wrapping is confirmed.** Claude wraps the sequence in
  `ESC P tmux; … ESC \` when `$TMUX` is set. This repo's claim in `src/emit.rs` is
  **correct**: `terminalSequence` can therefore never update `pane_title`, and with
  tmux's `allow-passthrough` off by default it is dropped entirely. `$ZELLIJ` falls
  through unwrapped, which is a third case nothing in this repo handles yet.
* **OSC 9;4 taskbar progress passes the validator**: the payload grammar
  `4;<0-4>[;<0-100>]` is explicitly permitted. This is the single most consequential
  finding for [#14](https://github.com/dalf/claude-tabstatus/issues/14) — see §4.
* **OSC 9 free text** is permitted only when the payload does not *start* with a
  digit, which is how the validator keeps OSC 9 progress and OSC 9 notification apart.
* **SessionStart cannot use it.** The TUI registers the writer as an effect; before
  that, `write()` finds no writer and **silently drops**. This confirms why
  session-start writes the pty directly.

Hook subprocess environment, also read from the binary (**V**, and *not* on the
public docs page): `CLAUDECODE=1`, `CLAUDE_CODE_SESSION_ID`,
`CLAUDE_CODE_CHILD_SESSION=1`, **`CLAUDE_CODE_SESSION_ATTENDED`** (`"0"` for
background/daemon session kinds, `"1"` for interactive), `CLAUDE_PID` =
`String(process.pid)`, plus `CLAUDE_EFFORT`, `AI_AGENT`, `TRACEPARENT`.

`CLAUDE_CODE_SESSION_ATTENDED` is a better headless signal than anything this repo
currently reads, and an attention channel has an obvious reason to consult it.

**Timeouts, and the fact that decides attention delivery**: the default command hook
budget is 600 s; `SessionEnd` gets ~1500 ms by default
(`CLAUDE_CODE_SESSIONEND_HOOKS_TIMEOUT_MS`). **Hooks run in parallel and the event
awaits them, so a slow hook blocks the user's turn.** Any channel that can hang is
therefore a correctness problem, not a latency problem.

## 3. Terminal capabilities

Full matrix with per-row source citations:
[`terminal-capability-matrix.md`](research/terminal-capability-matrix.md) —
15 terminals × 8 capability families, each row marked V or I.

Four results change what the existing code should do.

**`N/A` is invisible from the writer's side.** Several terminals parse a sequence,
deliberately discard it, and report nothing. That is indistinguishable from success
to anything that only writes. **No capability can ever be confirmed by emitting it**,
which is the strongest single argument against probing and for a capability table.

**The title stack is not a portable capture/restore primitive** — relevant to
[#11](https://github.com/dalf/claude-tabstatus/issues/11), which needs to save and
restore appearance. `CSI 22 t` / `CSI 23 t` works in 9 of 15 terminals, but Konsole
*explicitly discards it* (`Vt102Emulation.cpp:2054-2055`, an empty `case` with the
comment `/* IGNORED: Save icon and window title on stack */`), WezTerm parses and
drops it, Windows Terminal and GNU screen do not implement it, and VS Code gates it
off by default. **The project's own primary target is one of the terminals where it
silently does nothing.** Anything depending on a `23t` restore needs a fallback that
re-asserts a known-good value, and must never be the only restore path. **V**

**Konsole has a better tab-colour mechanism than the one the code comment
anticipates.** `src/emit.rs` notes `TabColor=#RRGGBB` riding in the OSC 50 property
list; that is real (`ProfileChange=50` → `Profile::TabColor`). But Konsole also has
**`OSC 34 ; <color> BEL`** (`SessionColor`, `Session.h:475` →
`Session::setSessionAttribute` → `setColor()` → `_tabColor`, `Session.cpp:704-711`,
`2407-2415`), which sets the tab colour directly rather than by synthesising a
runtime profile. **V**

**OSC 9 does not mean the same thing to everyone.** Konsole reads OSC 9 as ConEmu
*progress*, not as an iTerm2 *notification*. A backend that emits an iTerm2-grammar
OSC 9 notification to Konsole is emitting a malformed progress command. **V**

### Detection is one-directional evidence

| trap | consequence | ev |
|---|---|---|
| `KONSOLE_*` leaks into every child, and a tmux **server** inherits it from its first client and hands it to every pane of every session thereafter | the existing `!TMUX && !STY` guard is necessary, and still insufficient — this is exactly what [#18](https://github.com/dalf/claude-tabstatus/issues/18) is fixing | V |
| `TERM_PROGRAM` is set to `tmux` by tmux itself | the variable that names the terminal names the multiplexer | V |
| `VTE_VERSION` identifies a *library*, not a product | mapping it to "GNOME Terminal" is wrong for Tilix, Terminator, Ptyxis, Guake, Black Box, Xfce Terminal — which differ on tab colour and notification | V |
| Ghostty's `ssh --forward-env` sends `TERM_PROGRAM` over ssh | a remote host can see a *local* terminal's identity and be wrong about everything else it implies | V |
| GNOME Terminal is the **only** terminal that scrubs the others' variables before spawning | a stale `KONSOLE_VERSION` is cleared by launching GNOME Terminal and by nothing else | V |
| `TERM` is the one variable ssh always sends, and tmux/screen replace it | `TERM` is simultaneously the most portable and the least trustworthy signal | V |

> **Presence of a variable is weak evidence; absence is none at all.**

That single sentence justifies keeping `CCTAB_TERMINAL` as an override that
short-circuits detection entirely, and it means detection must be allowed to answer
"unknown" without that being a failure.

### Querying the terminal is not available to us

A hook subprocess is detached with fd 0 on `/dev/null`, and `exec 3>/dev/tty` fails
(`src/emit.rs:167-172`). There is no reader, so DA1/DA2, XTVERSION, XTGETTCAP and
DECRQSS are all unusable — **capability detection can never be dynamic in this
process model.** A round trip would also blow the budget: one `tmux set-option`
round trip is 2.84 ms against a working edge that measures ~460 µs (§8).

## 4. Attention channels

Cost and reach of each candidate. Delivery column: **hook** = fits the
`terminalSequence` allowlist; **pty** = needs the direct pty write (Linux-only
today); **oob** = out of band entirely.

> **"Linux-only today" is stale for the pty column.** The direct pty write is every
> Unix now: `sys::session_tty` resolves fd 1 on macOS as well, so a **pty** row
> reaches macOS too. It is still not Windows, where the session's tab is a console
> and `sys::HAS_SESSION_TTY` is `false`; a **pty** row there means
> `sys::set_session_title`, which carries a title and not arbitrary bytes.

| channel | delivery | reach | cost | ev |
|---|---|---|---|---|
| **OSC 9;4 taskbar progress** | **hook** | Windows Terminal, ConEmu, kitty, Ghostty, WezTerm | free — one line of stdout | V |
| BEL | **hook** | ~all, but many map it to nothing or to audio only | free | V |
| OSC 777 notify | **hook** | urxvt, some VTE builds; **not** Konsole | free | V |
| OSC 99 (kitty) | **hook** | kitty, Konsole ≥ 24.12 | free | V |
| OSC 9 free text | **hook** | iTerm2, WT, kitty — **but Konsole reads it as progress** | free | V |
| Konsole `OSC 34` tab colour | **pty** | Konsole only | free | V |
| freedesktop D-Bus notification | **oob** | any Linux desktop with a notification daemon | **255 µs to write, 14.8 ms to read the reply** | V |
| Windows toast | **oob** | Windows | `powershell.exe` spawn — tens of ms | V |
| iTerm2 `RequestAttention` | **pty** | iTerm2 | free | V |

**OSC 9;4 is the standout.** It is the only attention-shaped channel that is
simultaneously sanctioned by the hook protocol, portable across three operating
systems, free of any dependency, and free of a direct pty write — which means it is
the one channel that would work on macOS and Windows *before* either port lands.
It is also the channel the pinned Windows prior art uses.

### The D-Bus measurement, and what it costs the design

A complete dependency-free D-Bus client for
`org.freedesktop.Notifications.Notify` was written and **run on this machine**
(`docs/research/dbus_notify.rs`, ~250 lines, std only — `UnixStream`, `AUTH EXTERNAL`,
`Hello`, one marshalled `(susssasa{sv}i)` method call):

```
connect+auth+hello+notify written: 255.019µs
wait for reply:                     14.80897ms
notification id = 110
```

**Writing is affordable; waiting is not.** 14.8 ms against a working edge of ~460 µs
(§8) is a 32× overrun, and hooks block the turn (§2). So the rule is *fire and
forget*.

That collides with the obvious coalescing design: `replaces_id` normally comes back
in the reply you must not wait for. Options, in preference order, none yet verified:
pass a self-chosen non-zero `replaces_id` and never learn the server's answer (**I** —
the spec says the server reuses a non-zero `replaces_id`, but this was not tested);
or persist the id from a rare, deliberately slow path; or accept no replacement and
coalesce purely on our own side by not sending. This is a real open question, not a
detail.

### What `replaces_id` is worth without the reply

> **THIS SUBSECTION IS NEWER THAN THE REST OF THE FILE.** The paragraph above is the
> snapshot's text and is left standing; this answers its last sentence. Open
> question 4 is closed by it.

The spec sentence above turned out to describe **no single behaviour**. A separate
scouting pass read each daemon's own source and found four different ones. That pass
is the provenance of this table; **it was not re-run in this tree, and no
notification daemon was exercised here** — the only D-Bus code this repository has
ever executed is `docs/research/dbus_notify.rs`, which measured the timings above.

| daemon | what it does with a client-chosen non-zero `replaces_id` | consequence for fire-and-forget |
|---|---|---|
| **dunst** | honours it | replacement works |
| **KDE Plasma** (`plasma-workspace`) | honours it | replacement works |
| **xfce4-notifyd** | honours it | replacement works |
| **mako** | **zeroes it** | no replacement; each call is a new notification |
| **swaync** | honours it **only if it is ≤ its own counter** | a *low* id would replace **an unrelated application's** notification |
| **GNOME Shell** | **discards it and allocates a fresh id** | the client never learns the real id, so every call stacks another banner |

Two of those are the design constraints, and they point opposite ways. swaync makes
a **low** id actively dangerous — it is the one row where guessing wrong touches
somebody else's notification rather than our own. GNOME Shell makes *any* id
useless, so it cannot be fixed on the wire at all.

**The workaround, which is what the backend should do:**

* a **large, stable, non-zero id derived from the session id** — large so it is
  above swaync's counter and cannot collide with another application's, stable so
  repeated calls in one session agree, non-zero so the three honouring daemons and
  swaync use it;
* plus the hints **`x-dunst-stack-tag`** and **`x-canonical-private-synchronous`**
  carrying that same key, which give dunst and the GNOME/Canonical lineage a
  second, id-independent way to coalesce;
* plus, for **GNOME Shell**, the only thing that works client-side: **send at most
  one notification per waiting episode**, and let the absence of a second send be
  the coalescing.

Five of six daemons covered, no reply read, and the 14.8 ms wait stays unpaid. What
is *not* claimed: none of this was observed running, here or anywhere in this
repository.

## 5. Dependencies: the gate, and the verdicts

The project had six crates when this was written (`serde`, `serde_core`, `serde_json`, `itoa`,
`memchr`, plus `zmij`) and no `[features]` section. Proposed gate — a dependency is
right only when **all four** hold:

1. it is **not** on the hot path, or costs nothing there;
2. it is a de-facto std extension (raw declarations + link directives), **or** the
   API surface we need is too large or too subtle to hand-roll correctly;
3. it drags in **no** async runtime and **no** proc-macro;
4. its licence is compatible with GPL-3.0-or-later, and it is confined to
   `[target.'cfg(…)'.dependencies]` so no other platform's build sees it.

| crate | transitive | verdict | reason |
|---|---|---|---|
| `windows-sys` 0.61 | 1 | **depend**, `cfg(windows)` only — **and #28 since did exactly this**, target-gated with seven features plus a dev-dependency, so this row is settled rather than proposed | raw declarations plus link directives; hand-rolling `raw-dylib` externs is the same code without the maintenance |
| `libc` 0.2 | 0 | **depend**, `cfg(target_os="macos")` only — **and this tree since did exactly that**, so the row is settled rather than proposed | zero transitive deps; the caveat came true both ways — it **does** declare `proc_pidinfo`, `proc_pidfdinfo`, `vinfo_stat`, `vnode_info` and `vnode_info_path`, and it does **not** declare `proc_fileinfo`, `vnode_fdinfowithpath` or `PROC_PIDFDVNODEPATHINFO`, which are hand-declared with asserted layouts. See the note under this table |
| D-Bus (`zbus` 5.19) | **49 required** | **reject** | async runtime, MSRV 1.87, 49 crates for one method call that measured 255 µs hand-rolled |
| `notify-rust` 4.18 | 31 | **reject** | wraps zbus on Linux; same cost, less control |
| `dbus` 0.9 | 4 | **reject** | needs system `libdbus`; a link-time dependency on a hook binary that must run on a box we do not control |
| `crossterm` 0.29 | 16 | **reject** | solves interactive terminal I/O; we write bytes and exit |
| `termwiz` 0.23 | **71** | **reject** | the right *ideas* (capability modelling, `ProbeHints`) at an impossible weight; borrow the idea, not the crate |
| `terminfo` 0.9 | 6 | **reject** | terminfo cannot answer any question in §3 — it has no entry for tab colour, notification or progress |
| `rustix` / `nix` | 6 / 3 | **reject** | std already covers what is needed; `File::lock` landed in std 1.89, which is the MSRV floor |
| `dirs` / `directories` | several | **reject** | the whole need is three `getenv`s and a fallback |
| `winapi` 0.3 | 2 | **reject** | superseded by `windows-sys` |
| `is-terminal`, `supports-color`, `sysinfo`, `cfg-if` | 4 / 1 / 1 / 0 | **reject** | each replaces a handful of lines this crate already has |
| `windows` 0.62 (full WinRT) | 8, 9.1 MB | **reject** | the COM/WinRT projection for what is a handful of Win32 calls |

> **`libc` IS NOW A DEPENDENCY OF THIS TREE**, and the "macOS has no backend"
> reading of this table is stale. `Cargo.toml` carries
> `[target.'cfg(target_os = "macos")'.dependencies] libc = "0.2"`, zero transitive
> dependencies, and no Linux or Windows build resolves it — the same shape
> `windows-sys` has behind `cfg(windows)`. It supplies three answers `/proc` gives
> on Linux: `proc_pidinfo(PROC_PIDTBSDINFO)` for a start time, `kill(pid, 0)` for
> liveness, and `proc_pidfdinfo(PROC_PIDFDVNODEPATHINFO)` for the session's own tab.
> Licence `MIT OR Apache-2.0`, one-way compatible into GPL-3.0-or-later; the
> provenance record is in [research/attribution.md](research/attribution.md), which
> also says why no `libproc` wrapper crate was taken.

**Net effect: two target-gated crates, both first-party platform bindings, neither
on any hot path, and no change to the default Linux build's dependency tree.**
Notifications are hand-rolled (Linux) or a subprocess (macOS `osascript`, Windows
`powershell.exe`), matching what the prior art does and what §4 shows is affordable.

## 6. Platform primitives, per OS

What each backend must supply, and with what. Linux is the reference column.

| primitive | Linux (today) | macOS | Windows |
|---|---|---|---|
| session terminal | readlink `/proc/$CLAUDE_PID/fd/1` + `/dev/pts/`\|`/dev/tty` prefix + char-device | `proc_pidfdinfo(pid, 1, PROC_PIDFDVNODEPATHINFO)` → `vnode_fdinfowithpath.vip_path` — **SHIPPED**, sharing the prefix/char-device/writable guard with Linux; layout asserted at compile time, permission gate `CHECK_SAME_USER` (both **V**); never run on a Mac (**I**) | `AttachConsole(pid)` + `CONOUT$` — **process-global, needs a paired release** — **SHIPPED by #28** |
| process identity | `/proc/<pid>/stat` field 22, clock ticks since boot | `proc_pidinfo(PROC_PIDTBSDINFO)` → `pbi_start_tvsec`/`_tvusec`, **seconds since epoch** | `GetProcessTimes` → **100 ns FILETIME** |
| liveness | `/proc/<pid>` exists | `kill(pid,0)`: `ESRCH` dead, `EPERM` **alive** | `OpenProcess`: must not read access-denied as dead |
| state dir | `$XDG_RUNTIME_DIR` — per-user, 0700, tmpfs, **cleared at logout** | `$TMPDIR` (per-user under `/var/folders/<hash>/T`) | `%LOCALAPPDATA%` — **persistent**, unlike the other two |
| file identity | `MetadataExt::dev`/`ino` | same | `GetFileInformationByHandle` / `FILE_ID_INFO` |
| private dir | `DirBuilderExt::mode(0o700)` | same | a DACL, not a flag |
| atomic link swap | `symlink` + `rename(2)` | same | directory junction |
| hostname | `/proc/sys/kernel/hostname`, then `$HOSTNAME`, then **fork `hostname`** | `gethostname(3)` — fork-free | `GetComputerNameExW` — fork-free |

Three of these are *encoding* differences that persist to disk, not just API
differences. The state record stores `start` as a bare number; Linux writes ticks
since boot, macOS would write seconds since epoch, Windows 100 ns units. Records are
per-user and a WSL binary and a native Windows binary can share a home directory, so
**a bare number compared for equality across two encodings makes every live session
read as dead** — background indicators would never clear. Any persisted
platform-derived value needs a backend tag, and an unrecognised tag must mean
*absent*, not *different*.

The state-directory row hides a second trap: `REAP_AFTER = 24h` is justified by the
OS emptying `XDG_RUNTIME_DIR` at logout. `%LOCALAPPDATA%` is not emptied, so on
Windows the reaper becomes the only garbage collector. Volatility is a property the
platform layer must *report*, not a constant the reaper may assume.

> **CONFIRMED BY [#28](https://github.com/dalf/claude-tabstatus/pull/28), including
> the encoding trap.** Read back off this branch's base: `sys::RUNTIME_DIR_VAR` is
> `LOCALAPPDATA`, file identity is `FILE_ID_INFO`, the plugin link is a junction
> (`sys::DIR_LINK`), and `process_start_time` is `GetProcessTimes`' FILETIME. The
> backend tag this section asked for exists and is `sys::ORIGIN_KEY` - `"p"` on
> Unix, `"q"` on Windows - and each platform reads the other's origin line as an
> unknown key, which is exactly "an unrecognised tag must mean *absent*, not
> *different*". Volatility is the one row not reported: `doctor`'s `state dir` row
> prints the path and, when there is none, `sys::NO_STATE_DIR`, but nothing yet says
> whether the OS empties it.

## 7. What prior art proves about backend shape

| project | surface | what it proves |
|---|---|---|
| [wt-tab-status](https://github.com/Yannis-Adn/terminal-addons/blob/9e0f317/plugins/wt-tab-status/scripts/tab-status.sh) | Windows Terminal, driven from **Linux/WSL** | The axes are genuinely independent: its platform primitives are Linux (`/proc` parent walk, `flock`, `XDG_RUNTIME_DIR`) while its surface is Windows. **The surface can be on a different operating system than the platform.** |
| [its `toast.ps1`](https://github.com/Yannis-Adn/terminal-addons/blob/9e0f317/plugins/wt-tab-status/scripts/toast.ps1) | WinRT toast | An out-of-band channel is a *spawn*, and it detaches (`setsid -f`) precisely because it must not block. |
| [JasperSui's iTerm2 adapter](https://github.com/JasperSui/claude-code-iterm2-tab-status/blob/bd5ed6c/scripts/claude_tab_status.py) | iTerm2 | What forces a **persistent adapter**: capture/restore of colour, badge and name needs an API session that outlives a one-shot hook. "This backend needs a daemon" must be a first-class, opt-in property — which is exactly what [#14](https://github.com/dalf/claude-tabstatus/issues/14) requires. |
| [headsup](https://github.com/wasulajr/headsup/blob/6539787/hooks/headsup-status.sh) | generic | A different product policy (normal completion = attention), and a reminder that the semantics in [indicator-semantics.md](indicator-semantics.md) are a choice, not a default. |

WSL is the case that settles the architecture: a **Linux platform** paired with a
**Windows Terminal surface**, with `WT_SESSION` crossing through `WSLENV` (**I**).
No single-axis model can express it.

## 8. Findings that contradict the tree

Recorded here because the next reader will otherwise trust the comment.

| claim in the tree | status |
|---|---|
| `src/location.rs:194` — the `hostname` fork is "the ONLY fork in this binary" | **false.** `Command::new` appears 14 times across `tmux.rs`, `tree.rs`, `location.rs`. `tmux::session_start` and `tmux::arm_konsole` fork on `Paint::SessionStart`. The budget is per **edge**, not per binary: the working edge forks zero times, and that is the claim worth making. |
| README: Windows fails with 53 errors across six files | **stale** at `9bfd987`: 88 across nine (bin scope). **Overtaken entirely** on this branch's base — #28 ported Windows, and the count is 0. |
| [#3](https://github.com/dalf/claude-tabstatus/issues/3): 80 errors across nine files | same: stale then, moot now. #3 is done, by #28. |
| `src/emit.rs` — Claude Code wraps `terminalSequence` in tmux passthrough | **confirmed** by reading the shipped binary. |
| `src/emit.rs` — `TabColor` rides in the OSC 50 property list | **confirmed**, and `OSC 34` is the more direct mechanism. |
| `scripts/build.sh:108` and `tests/run.sh:1407` glob `src/*.rs` | **true at `9bfd987`, and FIXED BY #28, not by this work.** `a1e4153` spells `find src -type f -name '*.rs' \| LC_ALL=C sort` in both files. Both halves of the fix — descending, and a locale-independent sort — were reached independently there; see [backend-architecture.md](backend-architecture.md#the-manifest-fix-and-a-bug-it-uncovered-28-got-there-first). |
| "370 µs" as the binary's runtime | **conflated.** `src/tmux.rs:37` measures **0.37 ms as the *fork floor***, not as the work. The README's own measured table (lines 495-507) puts a `working` edge at **~460 µs** self-timed and **767 µs** for a real hook including the harness fork. Every budget argument should be stated against 460 µs / 767 µs, and per **edge**, not against 370 µs. |

The last row matters more than it looks: 370 µs was the number used to reject
vtables, registries, probes and a D-Bus dependency. The real figures are larger, so
those rejections rest on the *shape* of the cost — a fork, a round trip, a blocking
read — rather than on a tight absolute budget. They still hold, for that reason.
A measurement gate, not an argument, is what should defend the budget from here on.
**Superseded performance check.** The original `scripts/bench-hot.sh` combined
optional dry-run timing with tracing that could skip and did not exercise native
session-terminal discovery. It did not enforce the full claims above. The
[current performance checks](architecture.md#performance) require Linux subprocess
and delivery assertions in CI, with separate opt-in dry-run baseline measurements.

## 9. Static or runtime?

The owner's decision was compile-time first, with a standing instruction to record
what the investigation found. All six scouts reached the same split independently.

**Platform axis — compile-time, unanimously.** There is no machine where both
`/proc/<pid>/fd/1` and `AttachConsole` exist; all 88 Windows errors are of the form
"this symbol does not exist on this target", which is what `cfg` is for. WSL is not
a counterexample: a WSL binary *is* a Linux binary, and `cfg(target_os="linux")` is
already right for it.

**Surface axis — runtime selection is required, and the evidence is concrete.** One
`x86_64-unknown-linux-gnu` binary faces Konsole, VTE, kitty, WezTerm, Alacritty,
foot, Ghostty and Windows Terminal via WSL, and they disagree on every row of §3.
Konsole *alone* is four capability tiers across versions (OSC 777 at 23.04, OSC 99 +
OSC 34 at 24.12, OSC 9;4 and D-Bus tab-colour at 26.04). `cfg` cannot see any of it.

**But runtime selection is not runtime dispatch.** The set of surfaces is closed and
known at compile time; only *which one is present* is not. So the shape is the one
`src/config.rs` already has — an enum resolved once from the environment plus
`CCTAB_TERMINAL`, dispatched by `match`, with capabilities as const data. A
trait-object registry buys only the ability to add a surface without recompiling,
which this project does not need. **Runtime selection, static dispatch.**

**A third axis is needed.** tmux is not another terminal: it is a *retained-mode*
renderer with four powers a leaf terminal has none of — a timer (which is what makes
decay possible), a key-value store, an attach event, and a client registry — and it
simultaneously *supplies the leaf with its channel*. A leaf terminal is
immediate-mode: write bytes, they latch. Folding tmux into the platform axis puts
tmux stubs in the Windows module; folding it into the surface axis makes
`Konsole` mean two different things (`emit.rs:106` vs `tmux.rs:596` are already
different code paths); folding it into delivery loses the timer. It is its own axis.

## Open questions

Needing hardware, or the owner:

1. ~~**Is `CLAUDE_PID` exported to hooks on native Windows Claude Code?**~~
   **ANSWERED BY SHIPPING.** #28's `sys::set_session_title(claude_pid, title)` takes
   it and guards on it — a 64-bit process, an ancestor of the hook, whose stdout is
   that console — and `b0c2432` paints session-start and clears at session-end on
   native Windows on that basis. The seam does not assume a session pid is knowable:
   `Ok(false)` is the refused guard, and `doctor`'s `session terminal` row prints
   which of the four answers applied.
2. ~~**Does `proc_pidfdinfo` on another process's fd 1 succeed for an ordinary user
   on current macOS?**~~
   **ANSWERED AS FAR AS A TYPE-CHECK AND A KERNEL SOURCE CAN ANSWER IT — and the
   remaining part is stated plainly, because it is the part that matters.**
   `src/sys/unix.rs` now calls
   `proc_pidfdinfo(pid, 1, PROC_PIDFDVNODEPATHINFO, …)` and reads `vip_path`. Three
   separate questions were inside this one, and two of them are settled:
   * **Permission.** XNU's `bsd/kern/proc_info.c` gates `proc_pidfdinfo` with
     `proc_security_policy(p, PROC_INFO_CALL_PIDFDINFO, flavor, CHECK_SAME_USER)` —
     read here, not taken on trust. Same uid, therefore no entitlement and nothing
     SIP withholds. `$CLAUDE_PID` is this user's own `claude`. **V**, against the
     source.
   * **Layout.** The two structs libc does not declare are hand-written and their
     sizes and offsets are `const _: () = assert!(…)` items that fail the build:
     seven of them, evaluated by `cargo check` for **both** Apple ABIs, in CI as
     well as locally. Negative controls confirm they bite — a spurious `u32` fails
     three, an `off_t` written as `i32` fails one. A wrong size is not corruption
     either: the kernel returns `ENOMEM` for a `buffersize` below the flavour's own
     before copying a byte. **V**, at compile time.
   * **What is left, and it is the whole of what is left: NOTHING HERE HAS EVER RUN
     ON A MAC.** No machine in this project can link a macOS binary, only
     type-check one. That the call returns a path at runtime, that a macOS pty is
     spelled `/dev/ttysNNN` and so passes the existing `/dev/tty` prefix, and that
     `proc_pidfdinfo` links at all are unobserved. **I.** A green `cargo check` is
     not evidence of support, and this row does not claim otherwise.

   The headless guard is the part that makes the unproven part survivable: the same
   file shows XNU serving this flavour only behind
   `fp_get_ftype(p, fd, DTYPE_VNODE, EBADF, &fp)`, so a pipe or socket on fd 1 is
   `EBADF` and never a path, and a redirected fd 1 yields a *file* path that
   `is_tty_path` rejects. A mistake in the unproven step is an error return, not a
   wrong terminal.
3. **Does `AttachConsole` work from a detached hook child, and what is the correct
   release discipline?** It is process-global, which is why the terminal writer must
   be an opaque type with a `Drop` rather than an `Option<File>`.
4. ~~**Does a self-chosen non-zero `replaces_id` work** without reading the reply
   (§4)?~~
   **ANSWERED, AND THE ANSWER IS A SPLIT — five daemons of six can be covered
   without reading a reply, and the sixth has to be handled by not sending.** The
   verdict, the daemon-by-daemon breakdown and the hint-based workaround are
   recorded in §4 under *What `replaces_id` is worth without the reply*. It does
   **not** decide D-Bus coalescing against us: fire-and-forget coalescing is
   affordable, at the cost of one hint pair and one client-side rule.
5. **Does `WT_SESSION` really cross into WSL via `WSLENV`**, and does Windows
   Terminal clear it for non-WT children? Marked **I** throughout.
6. ~~**Does `std::fs::File::lock` behave equivalently on Windows?**~~
   **ANSWERED BY SHIPPING, and the answer was no — it is worse, in a way that had to
   be designed around.** #28's `sys::lock_exclusive` is `LockFileEx`, which is
   **mandatory**: it refuses every other handle's *read* of the bytes it covers. So
   it covers one byte far past any record, and no reader ever meets it. The
   in-crate test `the_record_lock_excludes_another_locker_and_no_reader` is the half
   Windows has to be made to keep, and it runs on the Windows runner in CI.
7. **What does the 312-case golden corpus become with three targets?** It pins exact
   stdout bytes from a Linux binary and there is no wine/qemu runner. Most likely
   answer — it stays the Linux specification and each backend gets its own — but it
   is a decision, and it bounds what "byte-identical" means for every future port.
