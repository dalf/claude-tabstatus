# Working on claude-tabstatus

This file is for contributors and coding agents. Users read [README.md](README.md),
which covers installing, configuring and troubleshooting and nothing else; do not
put contributor material there.

claude-tabstatus is one Rust binary, `tabstatus`, that Claude Code runs as a hook
on thirteen events. Each run decides a state (working, waiting, background, idle)
and a location, and paints them into the terminal tab title. The same binary
carries the management verbs (`install`, `uninstall`, `doctor`, `version`,
`tmux-format`, `tmux-arm`, `print-embedded`, `help`).

## Where things are

| path | what it holds |
|---|---|
| `src/main.rs` | argv dispatch: the hook edges (`session-start`, `working`, `waiting`, `idle`, `notify`, `subagent-stop`, `session-end`, `elicitation`, `elicitation-result`) and the management verbs |
| `src/edge.rs` | the edge this run was invoked for, as a closed set, and what it resolves to |
| `src/payload.rs` | selective, top-level hook metadata parsing with Serde; unused values are skipped with `IgnoredAny` |
| `src/config.rs` | the environment, read once and turned into types |
| `src/state.rs` | wait ownership: the per-session record, its lock, expiry and reaping |
| `src/location.rs`, `src/git.rs` | the location: fork-free `.git` walk, `repo@branch` or the home-relative path, the ssh host |
| `src/text.rs` | the crossing from filesystem bytes to display text (`repair`) |
| `src/render.rs` | `compose`: length cap, ssh prefix, JSON-safety pass, glyph placement, in that order |
| `src/emit.rs` | delivery: `terminalSequence` JSON, raw OSC to a pane pty, or a console title |
| `src/support.rs` | one vocabulary for absence: `Support<T>` says available, unsupported, disabled by a knob of ours, unverifiable or failed, each with the reason a user can act on |
| `src/surface/` | the leaf terminal, chosen at runtime and dispatched statically: `probe.rs` the detection table and the `CCTAB_TERMINAL` override, `rows.rs` the fourteen capability rows as `const` data with their provenance, `compose.rs` the bytes a row spells |
| `src/mux/` | the multiplexer axis: `mod.rs` detects tmux or screen and `route`s who owns the title and which channel the leaf's bytes ride; `tmux.rs` the tmux integration: the record, `set-titles-string`, window-status formats, restore |
| `src/clock.rs` | the one clock (`CCTAB_NOW` pins it) and the one TTL grammar, shared by the state record's expiry and the tmux carrier |
| `src/armed.rs` | restore obligations and their provenance, read by `SessionEnd`: tmux's `@cctab_armed`, then the record's `s` line, then the old assumption under its own name |
| `src/manage.rs` | all management verbs: `install`, `uninstall`, `doctor`, `version`, `tmux-format`, `tmux-arm`, `print-embedded`, `help` |
| `src/tree.rs` | the generated plugin tree that `install` materialises and Claude Code loads |
| `src/embedded.rs` | `hooks/hooks.json` and `.claude-plugin/plugin.json`, compiled in with `include_str!` |
| `src/settings.rs`, `src/json.rs` | the one `settings.json` edit, done as a text splice over a span-preserving JSON reader |
| `src/sys/` | the platform seam: `mod.rs` re-exports exactly one of `unix.rs` or `windows.rs`; callers never write `#[cfg]` |
| `hooks/hooks.json` | which event runs which edge, with matchers and timeouts. **Source**, not a deployed file |
| `.claude-plugin/plugin.json` | the plugin manifest. **Source**, compiled in like `hooks.json` |
| `scripts/build.sh` | builds `bin/` and its source digests |
| `scripts/bench-state.sh` | measures what the state layer costs per edge |
| `scripts/bench-hot.sh` | optional stateless dry-run timing against an explicit baseline, or `--calibrate` for self-comparison |
| `tests/check_hot_subprocesses.py` | required Linux subprocess gate: traces actual protocol and private tmux delivery with syscall controls |
| `tests/` | integration suites, fixtures, the golden corpus and the shell oracle (see [Tests](#tests)) |
| `examples/tmux.conf` | an optional tmux configuration users can copy |
| `docs/` | design and contract documents (see [Documentation map](#documentation-map)) |
| `.github/workflows/` | `test.yml` (every push and PR) and `release.yml` (every tag) |
| `.cargo/config.toml` | `+crt-static` for the Windows MSVC target |

`hooks/hooks.json` invokes `${CLAUDE_PLUGIN_ROOT}/bin/tabstatus`, which is the
copy inside the generated tree, never the checkout's `bin/`. A `git checkout`
therefore cannot change what a running session executes. Why the plugin directory
is build output, and how the tree is written, is in
[docs/architecture.md](docs/architecture.md).

## Build

```sh
sh scripts/build.sh          # the host target; refreshes bin/ and its digests
sh scripts/build.sh --all    # every target this host can build
./bin/tabstatus install      # put the build you just made into the plugin tree
```

| Target | State |
|---|---|
| `x86_64-unknown-linux-musl` | **default on Linux**, static-pie |
| `x86_64-unknown-linux-gnu` | builds; dynamic, needs `GLIBC_2.34` |
| `x86_64-pc-windows-msvc` | **default on Windows**, static CRT, built on Windows only |
| `aarch64-apple-darwin`, `x86_64-apple-darwin` | native validation builds only; `build.sh` selects the Darwin host architecture; no release assets |

- **Each host lists only the targets it can build**, so `--all` is a success
  signal: on Linux it is the two Linux targets, on Windows the one MSVC target.
  Darwin lists only its native architecture. Windows is a native port behind
  `src/sys`, built and tested on Windows. `x86_64-pc-windows-gnu` is not a target.
  There is no aarch64 Linux build target. macOS arm64 native CI has passed;
  see [validation scope](docs/architecture.md#macos-validation).
- **musl is the Linux default** because it starts faster and carries no glibc
  requirement on a remote box; the measurements are in
  [docs/architecture.md](docs/architecture.md).
- **On Windows, run `build.sh` and `install` under Git Bash.**
  `bin/tabstatus.exe` is a *copy* of `bin/tabstatus-x86_64-pc-windows-msvc.exe`,
  not a symlink, because a symlink needs Developer Mode or elevation; Git Bash runs
  `./bin/tabstatus` as that `.exe`. `.cargo/config.toml` links the C runtime
  statically, so a local build is the binary CI tests and needs no Visual C++
  Redistributable. A `RUSTFLAGS` environment variable *replaces* that setting:
  leave it unset when building a binary to hand out. cargo also merges the
  `.cargo/config.toml` of every directory above the checkout (on Windows any local
  account may create `C:\.cargo`), so "the binary CI tests" holds only when none of
  those overrides it; `build.sh` warns about each one it finds, and builds anyway.
- `cargo` is taken from `PATH`, then from `~/.local/bin/mise`; `CARGO=...`
  overrides both. `CARGO_TARGET_DIR` is honoured.

### `bin/` is build output

`bin/` is gitignored, and a fresh clone has none until you build. It holds one
binary per triple (`bin/tabstatus-<triple>[.exe]`) plus `bin/tabstatus`, a
relative symlink to the host's binary (a copy on Windows). That is the binary you
*run*; `install` copies it into the generated plugin tree, and hooks run the copy.

`install` copies the binary that is running it into the tree, so it always has a
binary to deploy; the checkout's `bin/` matters only as the thing you run.
`install --force` does not mean "install anyway, I am about to build": it only
lets `install` (like `uninstall`) write a `settings.json` whose Windows ACL cannot
be kept.

### Embedded manifests and source digests

`hooks/hooks.json` and `.claude-plugin/plugin.json` are compiled into the binary
(`src/embedded.rs`), so editing one is a rustc rebuild input and reaches a running
session only after a rebuild and an `install`, exactly like editing
`src/main.rs`. Three layers keep the embedded copies honest:

| layer | catches | where it fires |
|---|---|---|
| rustc's rebuild dependency | any build from an edited manifest | `cargo build`; both files appear in `target/<triple>/release/tabstatus.d` |
| `bin/sources.sha256` (the host copy of the per-triple manifest) | a prebuilt binary gone stale against edited sources, with nobody rebuilding | `sh tests/run.sh` recomputes it |
| `tabstatus print-embedded <plugin\|hooks>` | the bytes themselves, in either direction | `sh tests/run.sh` diffs it; `doctor` reports it on a machine with no source tree |

`build.sh` writes a digest **per triple built** (`bin/sources.<triple>.sha256`, and
`.cksum` as the POSIX fallback) plus an unsuffixed copy for the host, because
`bin/tabstatus` is the host binary. A triple not built in this run keeps whatever
manifest it had, so a verify of it fails rather than inheriting another binary's
freshness. The digest covers `Cargo.toml`, `Cargo.lock`, both manifests,
`.cargo/config.toml` and every `.rs` under `src/` (including `src/sys/`), in the
order `tests/run.sh` recomputes; keep the two lists in step if you add an input.

`print-embedded` writes the embedded bytes verbatim with no trailing newline of its
own, so `tabstatus print-embedded hooks | diff - hooks/hooks.json` is empty exactly
when they agree.

### Cargo conventions

- **`Cargo.lock` is committed and every build uses `--locked`** (`build.sh`, CI,
  `cargo test --locked`).
- **Serde and serde_json parse hook metadata without `serde_derive`.** Do not
  enable the `derive` feature or add proc-macro dependencies. The lockfile can list
  optional derive packages that are never compiled; `cargo tree -e normal,build`
  shows the active graph.
- **Size first for our code, speed for the parser.** `[profile.release]` is
  `opt-level = "s"`, LTO, one codegen unit, `panic = "abort"`, stripped. The
  measured parser dependencies (`serde`, `serde_core`, `serde_json`, `memchr`) are
  overridden to `opt-level = 3`.
- **`rust-version` is 1.89**, for `std::fs::File::lock`. Do not lower it. The
  rationale is in `Cargo.toml` and [docs/architecture.md](docs/architecture.md).
- **No new dependencies without a measured reason.** The only Windows dependency is
  `windows-sys`, with the features listed and justified in `Cargo.toml`.
- **The first build needs crates.io access** (or a populated Cargo cache). After
  that, `cargo build --locked --offline` works without network.

## Tests

| suite | command | platforms |
|---|---|---|
| in-crate unit tests | `cargo test --locked` (CI: `--all-targets` on Windows, per target on Linux and macOS) | Linux, Windows, macOS |
| shell integration suite | `sh tests/run.sh` | Linux |
| payload policy | `python3 tests/test_payload.py` | Linux, Windows, macOS |
| semantic state traces | `python3 tests/test_state_contract.py -v` | Linux, Windows, macOS |
| background lifecycle traces | `python3 tests/test_background.py` | Linux, Windows, macOS |
| direct MCP elicitation | `python3 tests/test_elicitation.py` | Linux, Windows, macOS |
| persistence and concurrency | `python3 tests/test_state_guarantees.py` | Linux, macOS (`fcntl`; strace fault injection Linux-only) |
| direct Unix delivery | `python3 tests/test_unix_delivery.py -v` | Linux, macOS (disposable PTYs) |
| settings ACLs, ownership and backup metadata | `python3 tests/test_macos_acl.py` | native macOS (chmod/ls/stat, SDK fault interposition) |
| tmux window status | `python3 tests/test_tmux_status.py` | Linux (tmux) |
| corpus fixture helpers | `python3 -m unittest discover -s tests/corpus -p 'test_*.py' -v` | Linux |
| golden corpus | `sh tests/corpus/replay.sh bin/tabstatus` | Linux |
| ConPTY end to end | part of `cargo test` (`tests/conpty.rs`) | Windows |

The macOS entries have passed in native arm64 CI; see
[macOS validation](docs/architecture.md#macos-validation) for the recorded run.
They do not establish behaviour in a terminal application.

The in-crate unit tests are not replaced by the shell and Python harnesses: they
check argv and environment parsing, the location walk, the length cap and its
elision, structural hook parsing and the JSON writers one function at a time, so a
refactor can be checked a piece at a time instead of only end to end. Some of what
they assert cannot be seen from outside the binary at all, for example that
`repair` answers differently from `String::from_utf8_lossy` on a truncated
sequence.

Every runner tests `bin/tabstatus` by default, the binary `install` copies into
the tree, so a stale build fails here and not in somebody's tab.
`CCTAB_TEST_BIN=<path>` points the shell and Python suites at another binary, for
example `target/release/tabstatus` or one triple's `bin/tabstatus-<triple>`.

### The shell suite (`tests/run.sh`)

Dependency-free (no bats, no jq) and exits non-zero on any failure; 596
assertions, 81 of them in the tmux section. Sections cover glyph per edge, location, JSON-hostile and
non-UTF-8 names, glyph overrides and position, stdin draining, the
payload-discriminated edges, `hooks.json`, exit status, install and uninstall,
the embedded manifests and source digests, `CCTAB_TERMINAL`, the state layer and
tmux.

- **Nothing reaches a real terminal.** Every assertion uses `CCTAB_DRY_RUN=1`
  (prints the computed title and emits nothing) or runs with `CLAUDE_PID` unset;
  the tmux carrier checks name only a disposable private-server pane.

  ```sh
  CCTAB_DRY_RUN=1 bin/tabstatus working                 # -> 🔵 claude-tabstatus@main
  echo '{"notification_type":"idle_prompt"}'       | CCTAB_DRY_RUN=1 bin/tabstatus notify
  echo '{"notification_type":"agent_needs_input"}' | CCTAB_DRY_RUN=1 bin/tabstatus notify
  echo '{"agent_id":"a1","hook_event_name":"PostToolUse"}' | CCTAB_DRY_RUN=1 bin/tabstatus working
  echo '{"hook_event_name":"SessionStart","source":"compact"}' | CCTAB_DRY_RUN=1 bin/tabstatus session-start
  ```

  An edge that must paint *nothing* prints no bytes at all, not an empty title.
  Every no-op in the state table has its own assertion, because a no-op that
  quietly paints does not crash; it overwrites a correct state a minute later.
- **Terminal detection is neutralised** at the top (`KONSOLE_VERSION`,
  `KONSOLE_DBUS_SESSION`, `TMUX`, `STY`, `CCTAB_GLYPH_POS` unset), otherwise the
  glyph end flips with the terminal you run it from. The glyph-position section
  sets them per case.
- **`hooks/hooks.json` is asserted as a table**: one `event matcher edge timeout`
  row per hook, checked in both directions against the expected table, thirteen
  hooks exactly, one command per group, and no edge name the binary does not
  implement (an unknown edge falls back to idle). Eight deliberate mutations of
  `hooks.json` each fail at least one assertion; presence checks alone, and
  `claude plugin validate`, passed a tree with `Stop` and `UserPromptSubmit`
  swapped. If you change `hooks.json`, change the table in `tests/run.sh` too.
- **Install sections perform real installs** into throwaway directories under
  `mktemp -d`. One drops a bare copy of the binary with no tree above it (a binary
  scp'd to a VM) and drives a first install, `doctor`, every refusal, a refresh and
  `uninstall`. Another rebuilds the old checkout-symlink wiring and asserts the
  migration (see [docs/architecture.md](docs/architecture.md)). The load-bearing
  assertion is that no install path wrote one byte of a tracked file and nothing
  installed links to a directory holding a `.git`.
- **The tmux section** drives a private server, `tmux -L cctabprobe -f /dev/null`,
  killed afterwards, never attached; the user's own server is never listed,
  configured or killed. It is skipped, not failed, without a `tmux` binary.
- **The digest checks** recompute `bin/sources.sha256` (or `.cksum`) and compare
  the binary's `version` with `Cargo.toml`. Git does not preserve mtimes, so only a
  digest can tell after a clone whether `bin/` matches the sources.

### Environment rules for anything that installs or runs hooks

These apply to the test suites and to any script or agent session that exercises
the binary.

- **Redirect all four of `HOME`, `CLAUDE_CONFIG_DIR`, `XDG_DATA_HOME` and
  `CCTAB_STATE_DIR`** before running `install` or `uninstall`. Each covers
  something the others do not: `HOME` and `CLAUDE_CONFIG_DIR` are where the env
  key, the skills link and the install record go; `XDG_DATA_HOME` is where the
  ~680 KB plugin tree goes (falling back to `~/.local/share/claude-tabstatus`);
  `CCTAB_STATE_DIR` is derived from none of them, and `uninstall` purges the wait
  records it resolves from `XDG_RUNTIME_DIR` (`LOCALAPPDATA` on Windows) or
  `CCTAB_STATE_DIR` alone. `tests/run.sh` unsets `XDG_DATA_HOME`,
  `XDG_RUNTIME_DIR`, `CCTAB_STATE_DIR` and `LOCALAPPDATA` at the top and pins them
  per case.
- **The state layer is off unless a case turns it on.** With no
  `XDG_RUNTIME_DIR`/`LOCALAPPDATA` and no `CCTAB_STATE_DIR` there is no record,
  so every assertion outside the state section is the stateless answer and does
  not depend on what an earlier case left behind. The state section sets
  `CCTAB_STATE_DIR` per case under the suite's temporary directory.
- **Pin `CLAUDE_PID` per case** in anything that writes a record. This project is
  developed inside Claude Code, so the ambient `CLAUDE_PID` is a real session; the
  state layer stamps it into a record's origin, and inherited it made byte-exact
  cases fail in exactly the environment the suite runs in. It is not unset
  globally, because the headless-guard and pty sections need the ambient one; the
  tmux `session-start` cases run under `env -u CLAUDE_PID` so they never write
  into the developer's tab.
- **`CCTAB_NOW`** pins the epoch the tmux record and the state record carry. Test
  use only.
- **`CCTAB_DRY_RUN=1`** prints the computed title and emits nothing; it is also
  the quickest way to check a change by hand.

### Race harness

`state::tests::concurrent_hooks_of_one_session_lose_no_update` runs hooks of one
session as separate processes, released together, and must lose no update, on
Linux, Windows and the native macOS job. Its controls switch the lock off in the
test build only:

```sh
cargo test race_control -- --ignored --nocapture   # stops at the first lost update
cargo test race_400 -- --ignored --nocapture       # 400 rounds, with and without the lock
```

The results, and why the lock is a one-byte `LockFileEx` on Windows, are in
[docs/architecture.md](docs/architecture.md).

### ConPTY test (Windows)

`tests/conpty.rs` runs `session-start` and `session-end` end to end. It makes a
pseudo console (kernel32's, the inbox conhost; `CCTAB_TEST_CONPTY_DLL` names a
ConPTY package's `conpty.dll` instead, such as the one VS Code ships, which is the
modern ConPTY a Windows Terminal tab uses), starts a stand-in claude inside it,
and has that spawn the real binary the way Claude Code does: through Git Bash
(`CCTAB_TEST_GIT_BASH` overrides its path), `CREATE_NO_WINDOW`, stdio on pipes.
It asserts the exact titles reaching the pseudo console's output (the idle title,
identical to the `terminalSequence` one for the same directory, then an empty
one, and no OSC 50), and no title for a stand-in redirected to a file or `NUL`,
for a `CLAUDE_PID` on the same console that is not the hook's ancestor, or for a
relay claude that exits before its hook reads the payload (painted when it
stays). That last is Claude exiting before the hook looks; its pid being
reissued *between* the hook's checks cannot be forced, and is closed by the held
handle, whose creation time must be the one the walk read (a unit test forges a
different one). `CCTAB_TEST_CONPTY_SHOW=1` with `--nocapture` prints each stream.

The developer's own tab is never a target: the stand-in starts without the
ambient `CLAUDE_PID`, and no test calls `set_session_title` with an ancestor of
the test process. Keep it that way in any new test.

### The Python suites

- `test_payload.py`: subprocess hook-input policy, oversized and draining input,
  state-preservation regressions; isolated `HOME` and state directory, dry-run
  output.
- `test_state_contract.py`: replays authored semantic traces
  (`tests/fixtures/state-contract-v1.json`) and checks the expected state and
  output after every event. It decodes persistence and observes dry-run output; it
  does not reimplement the rules, and it does not see terminal delivery or tmux's
  own decay clock.
- `test_background.py`: six sanitised live lifecycle traces
  (`tests/fixtures/background-v1.json`) plus long duration, missing metadata,
  concurrent waits, bounded tracking and migration.
- `test_elicitation.py`: direct MCP elicitation sequences from a synthetic
  fixture (`tests/fixtures/elicitation-v1.json`); no MCP server needed.
- `test_state_guarantees.py`: persistence and concurrency guarantees beyond the
  traces.
- `test_tmux_status.py`: window-list rendering, split panes, background windows,
  TTL decay and exact format restoration, on private servers with a unique socket
  and isolated configuration each. It attaches a disposable PTY client and checks
  the displayed status text, excluding outer-title escapes so they cannot satisfy
  an assertion. `CCTAB_TEST_TMUX` selects another tmux build.

### The golden corpus

```sh
sh tests/corpus/replay.sh bin/tabstatus     # 312 passed, 0 failed
```

`tests/corpus/cases.jsonl` is the frozen, byte-level record of what the POSIX sh
implementation did (argv, cwd, environment, stdin, and the exact stdout, stderr and
pty bytes), replayed over a freshly allocated pty. The shell itself is kept as
`tests/oracle/tabstatus.sh`, byte-identical to the deleted
`scripts/tabstatus.sh`, so the corpus can be re-frozen from the real oracle;
`tests/oracle/run.sh` is the shell-era suite. Ten cases were deliberately
re-recorded after the port, each named in `tests/corpus/refreeze_fixed.py` for the
limitation it closes; the pre-fix freeze is kept as `cases.jsonl.before-fixes`.
`tests/corpus/USAGE.txt` documents the tools (`mkfixtures.sh`, `gencases.py`,
`refreeze_fixed.py`, `replay.py`, `runner.py`).

- **Fixtures live under `$TMPDIR`** (`$CCTAB_FIXTURES` overrides), never in the
  repo: the corpus `HOME` is the fixture root and the location walk goes *up*
  looking for `.git`, so a fixture tree inside a checkout makes every location
  case answer `repo@branch`.
- **Repository fixtures are hand-built** (a `.git` directory and a one-line
  `HEAD`), so no git binary is needed and HEAD bytes git will not write (no
  trailing newline, CRLF) can be asserted. A cross-check against real `git init`,
  `git worktree add` and `git checkout --detach` runs when git is present and is
  skipped otherwise.
- **The corpus configures no state directory**, so it pins the stateless
  behaviour; the state layer is pinned by `tests/run.sh`, the Python traces and the
  unit tests.

### What the suite does not assert

- The shell suite does not allocate PTYs. `tests/test_unix_delivery.py` checks
  exact Unix startup/exit delivery and headless guards using Python's standard
  library; the Linux golden corpus independently preserves its historical bytes.
  On Windows `tests/conpty.rs` covers delivery.
- tmux re-emitting the title to an attached client from `set-titles-string`: the
  shell suite asserts the whole server side, including that the same paint
  renders differently after a wait with no hook firing, but not the client
  stream. `test_tmux_status.py` checks the window status line on an attached
  client, not the outer title.
- `test_macos_acl.py`: isolated installer lifecycles, explicit and inherited ACLs,
  no ACL in an inheriting directory, symlink targets, restrictive umasks, backup
  metadata, owner/group preservation and native inspection/application/verification
  faults. Uses `chmod`, `ls` and `stat` plus a separate native text observer
  independently of production helpers. The group fixture differs from the parent
  directory, and passwordless `sudo` exercises foreign owners with and without
  privileges. CI sets `CCTAB_TEST_REQUIRE_SUDO=1` to require these fixtures; local
  runs skip the privileged cases if unavailable. Privileged installer runs receive
  only isolated HOME/config/data/state locations. Its SDK-built dyld interposer
  observes private, empty staging files through `fstatx_np`; a Rust test separately
  observes the intended staging ACL and ownership before writing bytes. Apple
  cross-checks compile that Rust test but cannot establish native preservation.
- macOS terminal applications and tmux, Intel macOS runtime behaviour and older
  macOS versions. Native arm64 CI covers the documented process, state and PTY scenarios.

## Benchmarking

```sh
sh scripts/bench-state.sh                      # bin/tabstatus, state layer against itself switched off
sh scripts/bench-state.sh <baseline-binary>    # ...and against another build
CCTAB_BENCH_EXECS=200 CCTAB_BENCH_ROUNDS=21 sh scripts/bench-state.sh
python3 tests/check_hot_subprocesses.py        # required by Linux CI; needs strace, tmux, cc
sh scripts/bench-hot.sh --calibrate           # self-comparison, no regression verdict
sh scripts/bench-hot.sh <baseline-binary>      # optional stateless dry-run comparison
CCTAB_BENCH=1 sh tests/run.sh                  # optional calibration; set CCTAB_BENCH_BASELINE for comparison
```

`CCTAB_BENCH_BIN` picks the binary under test (default `bin/tabstatus`). Arms are
interleaved within each round; the report gives each arm's minimum, its spread,
and the **median of per-round paired deltas** against the baseline arm. Read the
deltas, not the absolutes: every arm pays one fork, not Claude Code's own launch
cost. Do not quote remembered numbers in code or docs; re-run the script. The last
recorded results, comparable baseline-build procedure, enforced subprocess scope
and how to read the negative arms are in
[docs/architecture.md](docs/architecture.md).

## CI and releases

**Test** ([`.github/workflows/test.yml`](.github/workflows/test.yml)) runs on every
branch push, pull request, manual dispatch, and as a reusable workflow.

- *Linux* (`ubuntu-24.04`): installs `musl-tools`, `tmux` and `strace`; `cargo test --locked`
  for both Linux targets; `sh scripts/build.sh --all`; the corpus fixture unit
  tests and subprocess-checker tests; then, for **each** Linux binary, the required
  subprocess/delivery gate, all six existing Python suites, the Unix delivery suite,
  `tests/run.sh` and the golden corpus. Uploads both binaries and their `SHA256SUMS` as the
  `linux-binaries` artifact.
- *macOS* (`macos-15`, arm64 / `aarch64-apple-darwin`): links and executes
  `cargo test --locked --all-targets`, builds a native validation binary, and runs
  the payload, state-contract, background, elicitation, state-guarantees and Unix
  delivery suites, plus the native settings ACL suite. No artifacts are uploaded. Both Apple ABI cross-checks remain
  in the Linux job, including Intel. Native arm64 execution has passed;
  [evidence, scope and limits](docs/architecture.md#macos-validation).
- *Windows* (`windows-2025`, steps under Git Bash): `cargo test --locked
  --all-targets` (including the ConPTY, lock and junction tests);
  `sh scripts/build.sh --all`; a check of the `.exe`'s import table that fails on
  any Visual C++ runtime DLL (`vcruntime*`, `msvcp*`, `ucrtbase`,
  `api-ms-win-crt-*`); then `test_payload`, `test_state_contract`,
  `test_background` and `test_elicitation`. `tests/run.sh`, the corpus,
  `test_state_guarantees`, `test_unix_delivery` and `test_tmux_status` require Unix;
  the shell, corpus and tmux suites run only on Linux. Uploads the `.exe` and its
  `SHA256SUMS` (written with `--text`, so the line format matches Linux) as `windows-binaries`.

**Release** ([`.github/workflows/release.yml`](.github/workflows/release.yml)) runs
on every pushed tag. It calls the Test workflow on the tagged commit, downloads the
tested artifacts, checks each against the `SHA256SUMS` its own test job recorded,
joins those lines into one `SHA256SUMS` (never a recomputed digest), checks it once
more, and creates a GitHub release with generated notes and these assets:

- `tabstatus-x86_64-unknown-linux-musl`
- `tabstatus-x86_64-unknown-linux-gnu`
- `tabstatus-x86_64-pc-windows-msvc.exe` (not code-signed)
- `SHA256SUMS`

To cut a release:

1. Bump `version` in `Cargo.toml` and `.claude-plugin/plugin.json` (they must
   agree: a unit test in `src/embedded.rs` fails otherwise, and `tests/run.sh`
   checks the binary's `version` against `Cargo.toml`), rebuild
   so `Cargo.lock` and `bin/` follow, and commit.
2. `git tag v0.1.0 && git push origin v0.1.0` (with the new version).

Every tag name triggers a release, and a tag can be released only once; use a new
tag each time.

## Documentation map

| file | audience | holds |
|---|---|---|
| [README.md](README.md) | users only | what it does, platforms, install, update, uninstall, terminal setup, configuration, troubleshooting, known limitations |
| [AGENTS.md](AGENTS.md) | contributors and agents | this file: layout, build, tests, CI, release, roadmap |
| [CLAUDE.md](CLAUDE.md) | Claude Code | `@AGENTS.md` and nothing else; edit AGENTS.md instead |
| [docs/architecture.md](docs/architecture.md) | contributors | design and rationale: hooks and edges, delivery mechanisms, the generated tree, install ordering, tmux internals, platform notes, measured numbers, migration from the checkout-symlink wiring |
| [docs/state-contract.md](docs/state-contract.md) | contributors | the normative state record and transition rules |
| [docs/indicator-semantics.md](docs/indicator-semantics.md) | contributors, curious users | what each colour means and the precedence between them |
| [docs/history.md](docs/history.md) | anyone | slices 1-7 and the shell-to-Rust port, with its comparison table |
| [docs/backend-architecture.md](docs/backend-architecture.md) | contributors | the backend abstraction: how a terminal, a multiplexer and an operating system plug in without any of the three learning about the others; the migration table and what was dropped as superseded by #28 |
| [docs/backend-scouting.md](docs/backend-scouting.md) | contributors | findings, not design: what Windows, KDE and macOS would actually need, as a dated snapshot with markers where #28 answered a question |
| [docs/research/terminal-capability-matrix.md](docs/research/terminal-capability-matrix.md) | contributors | the cross-terminal capability matrix every row in `src/surface/rows.rs` cites, each claim marked verified or inferred |
| [docs/research/portability-census.md](docs/research/portability-census.md) | contributors | the measured portability census, kept as history with a re-measured header |
| [docs/research/dbus_notify.rs](docs/research/dbus_notify.rs) | contributors | a dependency-free, hand-rolled D-Bus `Notify` call: the measurement behind the D-Bus decision |
| [docs/research/attribution.md](docs/research/attribution.md) | contributors | provenance and licences of what the macOS session-terminal route took from `libc`, XNU and lsof, and what was deliberately not taken |
| [COMPARISON.md](COMPARISON.md) | anyone | how this project relates to similar ones |
| [tests/corpus/USAGE.txt](tests/corpus/USAGE.txt) | contributors | the golden corpus tools |

Rules:

- A rule stated normatively in `docs/state-contract.md` or
  `docs/indicator-semantics.md` is linked, not restated, elsewhere.
- Measured numbers and the reasoning behind a design go in
  `docs/architecture.md`, not the README.
- A user-visible behaviour change updates the README (configuration, limitations
  or troubleshooting) as well as the design docs.
- Links between files point at headings that exist in the target file; check
  anchors when you move a section.
- British spelling, plain and precise.

## Roadmap: not yet built

- **macOS** ([issue #1](https://github.com/dalf/claude-tabstatus/issues/1)):
  native source builds and arm64 automated validation have passed CI. Terminal applications, tmux and Intel runtime behaviour
  remain unvalidated. No release asset or product support claim is added; see
  [validation scope](docs/architecture.md#macos-validation).
- **aarch64 Linux**: add `aarch64-unknown-linux-musl` to `TARGETS` in
  `scripts/build.sh` and to the release assets. Until then the x86_64 binary
  fails at `exec` with *Exec format error*; `doctor` already compares the tree's
  marker triple with the running one.
- **A cached session title.** Transcript records carry `aiTitle`, readable with a
  bounded tail read, and it is the only field that tells apart several concurrent
  sessions that all render as the same `repo@branch`. The state record reserves an
  `n <epoch> <text>` line for it, last so its free-form text arrives whole; the cap
  and the elision would then belong to `render::compose`. It would be the first
  record read on the *painting* path rather than only on edges; budget about +13us
  for the read.
- **A compaction edge.** `SessionStart` is registered with
  `"matcher": "startup|resume|clear|fork"`, leaving out `compact`, and the binary
  refuses a `compact` payload as well, so nothing paints while a compaction runs.
  The place for it is a second `SessionStart` group with `"matcher": "compact"`,
  or the `PreCompact`/`PostCompact` events.
- **Konsole `TabColor`**, on the same OSC 50 property list as the arming, so the
  tab itself carries the colour. Whoever adds it must also add
  `TabColor=#000000` to the `SessionEnd` list, or the colour outlives the session.
- **OSC 9;4 progress.**
- **A per-session tmux policy.** Glyphs and TTLs are server-wide options;
  per-session ones would need the deadlines carried in the record.
- **Re-arming a tmux server that lost our options** other than by a restart.
  Nothing re-runs `SessionStart`, and a `tmux` call on the hot path to check is
  the cost this design refuses. The `client-attached` hook is installed in
  Konsole mode for Konsole arming only; re-checking the options from it is the
  remaining seam. `doctor` is the manual answer today.
- **An alert when tmux background work finishes**: purple simply turns white
  ([issue #19](https://github.com/dalf/claude-tabstatus/issues/19), not yet placed
  on the [roadmap issue](https://github.com/dalf/claude-tabstatus/issues/16); the
  state-contract decisions in
  [issue #10](https://github.com/dalf/claude-tabstatus/issues/10) are unchanged).

### Known defects, reproduced and deferred

- **`doctor` aborts when `settings.json` is a directory.** The read error in the
  env-key report propagates, so the env-key, settings, state, terminal, glyph, pty
  and title lines never print. Every other broken shape (empty, whitespace,
  unparseable, an array, missing, a dangling symlink) is reported and exits 0.
  Reproduce with `mkdir <config>/settings.json && tabstatus doctor`. Fix: report the
  I/O error as one more `env key: FAIL` line and let the rest of the report run.
- **`install` writes its record before it edits `settings.json`.** An install that
  aborts between the two (the concurrent-modification guard is one reachable way)
  leaves a record saying `env_had: false` with no key written. If the user then
  sets `CLAUDE_CODE_DISABLE_TERMINAL_TITLE` themselves, `uninstall` reads that
  orphan, concludes the key is ours and removes it; the refusal meant to prevent
  this fires only when there is no record at all. Recoverable from the
  `.cctab-preuninstall` copy, which is written first. Fix: write the record only
  after `settings.json` has actually changed, or cross-check the recorded
  `settings_path` before editing.

### Deliberately not built

- **GNU screen** (`$STY` gets nothing; tmux wins when both are set): see
  [docs/architecture.md](docs/architecture.md#screen-and-nested-tmux).
- **Nested tmux** (only the inner server is configured): see
  [docs/architecture.md](docs/architecture.md#screen-and-nested-tmux).

## Licence

GPL-3.0-or-later. See [LICENSE](LICENSE).
