# Measured portability census (VERIFIED, run 2026-09-28)

Worktree: /home/alexandre/code/claude-tabstatus-backends @ 9bfd987 (+ dirty tree)
Toolchain: rustc 1.98.1, rust-version = "1.89" (Cargo.toml)

> **HISTORY, NOT THIS TREE.** These numbers describe `9bfd987`, before
> [#28](https://github.com/dalf/claude-tabstatus/pull/28) ported Windows. Re-measured
> on this branch's HEAD: `x86_64-pc-windows-gnu` **0 errors, 0 warnings**, bin only
> and `--all-targets`; `aarch64-apple-darwin` the same. Windows is no longer a
> type-check that fails - it is a port that is built and tested on a `windows-2025`
> runner in CI. The root-cause groups below are kept because they are still the
> honest shape of what a port of this kind has to move.
>
> **The "macOS compiles but is WRONG at runtime" section is NO LONGER true in
> substance**, and this paragraph used to say it was. Re-checked by grep in the
> current tree: every `"/proc` string in `src/` sits inside a
> `cfg(not(target_os = "macos"))` item, each with a macOS call beside it -
> `proc_pidinfo(PROC_PIDTBSDINFO)` for a start time; `kill(pid, 0)` for liveness,
> where `sys::liveness_from_kill` maps ESRCH to dead and EPERM to **alive**, which
> is exactly the wrong `Some(false)` this paragraph named; `TMPDIR` for the state
> directory; and `proc_pidfdinfo(PROC_PIDFDVNODEPATHINFO)` for the session's own
> tab, so `sys::HAS_SESSION_TTY` is `true` and session-start and session-end paint.
>
> What IS still true is the whole of what is left: **nothing has ever been RUN on a
> Mac.** No machine in this project can link a macOS binary and no CI runner is a
> Mac, so a green `cargo check` is still not evidence of support.
>
> What has moved is also WHERE: the `file:line` citations below are `9bfd987`'s, and
> the reads they name now live in `src/sys/unix.rs`.

## Raw commands / outputs
- check-windows.txt            cargo check --locked --offline --target x86_64-pc-windows-gnu
- check-macos.txt              cargo check --locked --offline --target aarch64-apple-darwin
- check-windows-alltargets.txt same + --all-targets
- check-macos-alltargets.txt   same + --all-targets

## Measured results
| target | scope | errors | warnings |
|---|---|---|---|
| x86_64-pc-windows-gnu | bin only      | **88** | 0 |
| x86_64-pc-windows-gnu | --all-targets | **102** (bin+test unit) | 1 ("build failed, waiting for other jobs") |
| aarch64-apple-darwin  | bin only      | **0**  | 0 |
| aarch64-apple-darwin  | --all-targets | **0**  | 0 |

macOS type-checks CLEAN, including the #[cfg(test)] unit tests. Every Windows
error is a *missing std extension trait*, not a logic error. No third-party crate
failed on either target (serde/serde_json/itoa/memchr/zmij all check fine).

## Windows root-cause groups (bin only, 88 = sum below)
45  OsStrExt byte access   (as_bytes on &OsStr / &OsString / OsString / OsStr::as_bytes as fn)
15  `use std::os::unix::...` import lines themselves (E0433)
 6  OsStrExt::from_bytes
 6  OsStringExt::from_vec
 4  MetadataExt::dev
 4  MetadataExt::ino
 2  FileTypeExt::is_char_device
 2  PermissionsExt::mode (read)
 2  PermissionsExt::from_mode
 1  OpenOptionsExt::mode
 1  DirBuilderExt::mode
Test-only delta (102-88 = 14): 7x from_mode, 3x os::unix::fs::symlink, 2x
from_bytes, 1x from_vec, 1x permissions().mode() -- all in src/tree.rs tests.

## macOS: compiles but WRONG at runtime
Every /proc read in src/ (VERIFIED by grep -rn '/proc' src/):
- src/emit.rs:181-184  readlink /proc/$CLAUDE_PID/fd/1  -> always None on macOS
  => direct-pty delivery mechanism (#2 of 3) silently disabled; Konsole arming
     and every raw-OSC paint become no-ops. Only the JSON stdout line survives.
- src/location.rs:208  /proc/sys/kernel/hostname -> falls through to $HOSTNAME
  then fork+exec `hostname` (location.rs:221). Correct output, but a ~370us fork
  on EVERY paint that resolves a host, doubling the binary's own runtime.
- src/state.rs:237     /proc/<pid>/stat field 22 -> start_time() returns None, so
  Origin liveness degrades to "pid exists?" with NO start-time disambiguation:
  a recycled pid is then read as the same session.
- src/manage.rs:875    Path::new("/proc/<pid>").exists() -> always false on macOS,
  so sweep_litter() deletes a LIVE concurrent install's temp file.
- src/state.rs:768-776 XDG_RUNTIME_DIR is unset on stock macOS => state layer is
  entirely off (stateless fallback) unless CCTAB_STATE_DIR is set.
Non-/proc macOS runtime divergences:
- src/emit.rs:150,188  b"/dev/pts/" prefix: macOS ptys are /dev/ttys###, which
  DOES match the b"/dev/tty" arm by accident. Windows has no path form at all.
- tmux/Konsole detection (src/config.rs) is unaffected; Konsole does not exist on
  macOS so KONSOLE_ARM/RESTORE (src/emit.rs:99-102) are dead there.

## Tests that cannot run off Linux
- tests/run.sh (625 `check` assertions, /bin/sh): 15 `ln -s`, 11 `readlink`,
  8 `chmod`, 3 `stat -c %a`, 65 tmux invocations, /proc hostname compare at
  run.sh:469-476 (already SKIP-guarded), starttime() at run.sh:2662 reading
  /proc/<pid>/stat field 22 (NOT guarded).
- tests/test_state_guarantees.py: `import fcntl`, flock at :197/:215, chmod at
  :129,:142,:143,:152 -> 2 of 6 tests (permission_failures, session_lock) are
  POSIX-only; the module fails at import on Windows.
- tests/test_tmux_status.py: `import fcntl, pty, termios`; pty.openpty +
  TIOCSWINSZ at :324-325 and :772-773; chmod at :249,:259 -> module fails at
  import on Windows; all 32 tests unavailable.
- tests/corpus/runner.py: fcntl/termios/os.openpty/O_NONBLOCK/SIGKILL at :22-133,
  and /proc/sys/kernel/hostname at :37 -> the whole frozen corpus replay is
  Linux-pty-shaped.
- tests/oracle/*.sh: readlink /proc/$$/exe at oracle/run.sh:36, /proc pty at
  oracle/tabstatus.sh:814 -> the POSIX-sh oracle is itself Linux-only.
