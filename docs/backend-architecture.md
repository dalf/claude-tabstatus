# The backend abstraction

How `claude-tabstatus` plugs in a terminal, a multiplexer and an operating system
without any of the three learning about the others.

Design for [#14](https://github.com/dalf/claude-tabstatus/issues/14), and the seam
[#1](https://github.com/dalf/claude-tabstatus/issues/1) (macOS),
[#3](https://github.com/dalf/claude-tabstatus/issues/3) (Windows) and
[#11](https://github.com/dalf/claude-tabstatus/issues/11) (appearance) are meant to
land against. The evidence every claim here rests on is in
[backend-scouting.md](backend-scouting.md); this file does not repeat it.

Base commit: **`a1e4153`**, the head of
[#28](https://github.com/dalf/claude-tabstatus/pull/28)'s `windows-port`. This
document was first written against `9bfd987` and has been re-checked line by line
against this base; where a claim was true then and is not true now it is corrected
here rather than carried. Two things in particular moved under it, and both are
corrected below: **#28 owns the platform axis outright**, so the `src/sys/` layout
sketched here is not what landed, and **Windows genuinely works** - it is not the
`Unsupported` stub an earlier draft of this file described.

`9bfd987` (PR #20, the #17 doctor-over-ssh remedy) is **not** in this base, so no
claim here rests on a string it introduced.

## The decision in one paragraph

There are **three axes, not two**, and they need different mechanisms. The
**platform** is chosen by `cfg(target_os)` — there is no machine where both
`/proc/<pid>/fd/1` and `AttachConsole` exist. The **surface** (the leaf terminal)
must be chosen at **runtime**, because one Linux binary faces Konsole, VTE, kitty,
WezTerm, Alacritty, foot, Ghostty and Windows Terminal through WSL — but it is
**dispatched statically**, through a closed enum and const capability tables, so
nothing indirect appears on the paint path. The **multiplexer** is neither: tmux is
a *retained-mode renderer* that owns a timer, a store, an attach event and a client
registry, and that simultaneously supplies the leaf with the channel its bytes
travel on. Runtime selection, static dispatch, three axes.

## Why three axes

| | platform | surface | multiplexer |
|---|---|---|---|
| chosen by | `cfg(target_os)` | runtime env probe + `CCTAB_TERMINAL` | runtime env probe |
| represented as | a module of free functions | a `Copy` enum + `&'static` caps | `Option<Mux>` |
| examples | linux, macos, windows | konsole, iterm2, windows-terminal, vte, unknown | tmux, screen, none |
| owns | pty/console, process identity, state dir, locking, paths | title bytes, arming bytes, tab colour, attention grammar | a timer, a store, a client registry, a channel |

Collapsing any pair breaks something concrete:

* **platform + surface**: WSL is a Linux platform driving a Windows Terminal
  surface, and the pinned `wt-tab-status` prior art does exactly that — its
  platform primitives are `/proc`, `flock` and `XDG_RUNTIME_DIR` while its surface
  is on another operating system. One axis cannot express it.
* **surface + multiplexer**: `Konsole` already means two different code paths
  (`emit::session_start` wrote the pane; `tmux::arm_konsole` wrote the attached
  clients' ptys, on the same `Konsole` verdict). Making them one enum variant makes
  the variant ambiguous — which is what `mux::route` now decides in one place.
* **platform + multiplexer**: tmux exists on macOS and not on native Windows, so
  the Windows platform module would carry tmux stubs.

**Immediate mode versus retained mode** is the distinction that makes the
multiplexer its own axis. A leaf terminal latches bytes: you write, it shows. tmux
*re-renders on a timer from a program you installed earlier*, which is exactly what
makes the project's TTL decay possible and what no leaf can do. A design that folds
the mux into delivery loses the timer.

## The layering rule, and the negative list

```
        ┌─────────────────────────────────────────────┐
  L3    │ policy: const capability tables, pure byte   │  cfg-INDEPENDENT
        │ composers, the resolution order             │  always compiled, always tested
        ├─────────────────────────────────────────────┤
  L2    │ driver: the call sequence, acquire/release   │  cfg-INDEPENDENT
        │ pairing, error → absence mapping            │  generic over L1, tested with a fake
        ├─────────────────────────────────────────────┤
  L1    │ raw: one-statement syscall wrappers          │  cfg-SELECTED
        │ no branches, no logic                       │  the only untestable code
        └─────────────────────────────────────────────┘
```

This exists to answer the hardest constraint in the project: **a
`cfg(target_os = "windows")` body never compiles into a Linux `cargo test`, so by
default every non-native backend ships with zero executed tests.** Splitting each
backend so that only L1 is cfg-selected keeps the decisions — which error code means
dead, how a `FILETIME` becomes a stamp, which bytes an OSC 9;4 is — compiled and
executed on a Linux developer's machine. The goal is that every `sys/*/raw.rs` is
small and branch-free; what cannot be tested should be too simple to be wrong.

> **SUPERSEDED BY [#28](https://github.com/dalf/claude-tabstatus/pull/28), and the
> constraint it answers was answered another way.** `src/sys/` in this tree is
> flat — `mod.rs`, `unix.rs`, `windows.rs`, std's own shape — with no L1/L2/L3 split
> and no `raw.rs`. The three-layer sketch above was designed for backends that
> could only ever be *type-checked* on Linux; #28's Windows backend is **built and
> tested on a real `windows-2025` runner in CI**, which is a stronger answer than
> a fake ever was. The layering rule is kept here because it still describes what
> L3 and L2 must not do — no `cfg` above the seam, no policy inside it — and
> `src/sys/mod.rs` states the same rule in its own words. It is **not** a
> description of a directory that exists.

### What is emphatically **not** a backend concern

A backend may supply **data** to these. It may never reorder or replace them.

- edge resolution and wait ownership ([state-contract.md](state-contract.md))
- the record format, its expiry and its reaping
- the order of the compose pipeline in `render.rs`
- the character-unit length cap and `CCTAB_MAX_LOCATION`
- the sanitizer's hostile set and its budget
- glyph selection and the meanings in [indicator-semantics.md](indicator-semantics.md)
- the dry-run short-circuit
- the exit-0 rule

Without this list written down, the first backend author moves elision policy into a
surface — it already looks like one — and the 312-case corpus becomes per-backend.

## One vocabulary for absence

Every capability query in the crate answers with the same type, and `doctor` prints
it. Today there are five unrelated tri-states and **none of them can say "you turned
it off"**.

```rust
pub enum Support<T = ()> {
    Available(T),
    Unsupported(&'static str),   // this build / this terminal cannot
    Disabled(&'static str),      // OUR knob, named so a user can grep for it
    Unverifiable(Option<T>, &'static str), // known value, if any; uncertainty reason
    Failed(io::Error),           // it was tried and it did not work
}
pub type Presence = Support<()>;
```

The type parameter is what keeps it **one** vocabulary rather than two: without it
there would be a capability enum for "can you?" plus a `Result` for "do it", which
is today's defect in a new place. With it, a `Support<PathBuf>` for the state
directory and a `bell: Presence` print identically.

**`Support` lives ABOVE `sys`, and that is a decision and not an accident.** #28's
platform functions answer in `Option`, `bool` and `HAS_*` — the right shape for a
caller that has to *branch* — and not one of their signatures changes to serve a
report. The lift into `Support` happens at the **reporting boundary**, in
`manage.rs`'s platform axis, where `HAS_SESSION_TTY` / `HAS_SESSION_CONSOLE` become
the `session terminal` row, `HAS_MODES` the `file modes` row, `HAS_RECORD_LOCK`,
`HAS_UNLINK_RUNNING`, `DIR_LINK`, `NO_STATE_DIR` and `ORIGIN_KEY` the rest, and
`process_start_time` the `process stamp` row. An earlier draft of this file had
`sys` itself answering in `Support`; that is not what this tree does.

`Unverifiable` is a fifth word where the survey's critique asked for four, and it is
forced by measured facts rather than taste: Windows Terminal gates OSC 777 behind
`compatibility.allowOSC777`, **default false**, with no env var, no version string
and no query that reveals it; `profiles.suppressApplicationTitle` silently discards
OSC 0/2 and is equally invisible. `Available` would lie to most users and
`Unsupported` to the rest. `Unverifiable(Some(value), reason)` retains the known
implementation while declining to promise effectiveness; `Unverifiable(None,
reason)` carries no implementation and permits no emission. Presence-only
capabilities with a known operation use `Some(())`.

`emittable()` borrows the value from `Available` or `Unverifiable(Some(_), _)`;
`should_emit()` is exactly whether that value exists. `gate(Some(knob))` replaces
both available and uncertain answers with `Disabled(knob)`, including report-only
uncertainty, so an explicit disable always wins over uncertainty. Other negative
answers keep their original reasons. `gate(None)` preserves the answer. These
helpers contain no terminal-specific policy.

`map` transforms available and uncertain values without losing the verdict or
reason, and leaves an absent value absent. `ok` and `is_available` remain
verified-only operations. `carry` now returns an unverified answer intact with
its original payload type; a type-changing conversion must use `map`, because
without a mapping it could only discard the uncertain value. Reporting retains
exactly five verdicts, with actionable reasons.

Liveness stays **outside** this vocabulary. It answers a question about a *foreign
process*, not about a capability of this build, and folding it in would make
`Unsupported` mean "that pid is not a thing". In this tree that is #28's
`sys::process_alive` and `sys::same_process`, both `Option<bool>`, where `None` is
"this user may not ask about that process" — and `state::Origin::alive` reads it as
"keep the record" for exactly that reason. It is **not** a `Liveness` enum; an
earlier draft said it was.

## Stacking: six invariants

Stacking is a **layer**, not a property of either backend. Only `mux::route()` and
`mux::resolve()` know these; **no backend ever tests `$TMUX`, `$STY` or
`$SSH_CONNECTION`**.

**I1 — Exactly one layer owns the title, and it is the outermost *renderer*.**
The test is `mux.caps().renders_title`, **never `mux.is_some()`**. GNU screen has
`renders_title: false`: `$STY` is a detection suppressor and nothing more. The
corpus pins this — `pty-session-start-konsole-in-screen` expects a plain
`ESC]0;⚪ plain@master BEL` on the pty, and a `mux.is_some()` test would send it a
tmux carrier instead.

**I2 — Leaf appearance bytes ride the outermost layer's channel, and the leaf never
learns it is stacked.** `SurfaceCaps::arming` is two byte strings with no opinion
about where they go. Today the routing decision is spread across two complementary
predicates in two files — `emit.rs` asks `Konsole && tmux.is_none()`,
`tmux::arm_konsole` asks `terminal != Konsole` and relies on a guard inside
`to_clients`. **Split those into two backend objects without a composition rule and
you get a double arm, or an arm with no restore** — the project's named recurring
defect. They collapse into one total condition in one place.

**I3 — Delivery is a property of the *message*, not of the stack.** What landed is
`Channel::carries_raw()`: `Channel::Protocol` (the hook's `terminalSequence`
allowlist) carries no raw bytes ever, `Channel::Clients` always does, and
`Channel::Direct` carries them exactly where the session's own tab is a pty —
`sys::HAS_SESSION_TTY`. `mux::route` filters the appearance channel through it, so a
Raw-delivery capability like Konsole's OSC 50 is routed to the pty, or to the mux's
clients, or **nowhere**. There is no `Delivery` enum on the capability rows; an
earlier draft of this file proposed one, and the total `match` in `route` turned out
to be the smaller shape.

> This is the invariant that lets a Windows **surface** backend ship independently
> of the Windows **platform** backend. On Windows, title, progress, toast and BEL
> all travel the allowlist and need zero platform code; only tab colour needs a
> console. A single stack-level channel would gate all four on a console the session
> may not even have.
>
> **It is also where #28 and this branch meet without either special-casing the
> other.** On native Windows `sys::HAS_SESSION_TTY` is `false`, so
> `Channel::Direct.carries_raw()` is `false`, so a Konsole arming outside a
> multiplexer has no channel there and `route` returns `None` for it. That falls out
> of the const; there is no `#[cfg(windows)]` anywhere in `mux` or `surface`, and
> `mux::tests::a_channel_that_cannot_carry_raw_bytes_carries_no_arming` asserts the
> equality rather than the platform.

**I4 — Leaf layout flows up into the outer layer's compiled program.** `Elide` is
derived from the leaf and handed to `GlyphPos::parse`, which still owns the decision
and still lets `CCTAB_GLYPH_POS` win, and thence into tmux's `set-titles-string`.
The mux never asks a leaf where its glyph goes.

**I5 — The outer layer may be the only source of leaf identity, so it is asked
first.** Resolution order:

```
1. probe the multiplexer from the environment        (no exec)
2. ask it for leaf evidence                          (may exec)
3. resolve the leaf: override ▸ mux's word ▸ env probes ▸ Unknown
4. derive elide and the capability row                (exactly once)
```

This is precisely the shape [#18](https://github.com/dalf/claude-tabstatus/issues/18)
needs: a Konsole verdict that only `tmux list-clients` can produce arrives *before*
anything derived from the leaf exists, so a late verdict cannot leave the glyph
position stale. Step 2 costs nothing on the hot path because the hot path passes a
zero-sized `NoOracle` whose answer is a constant — **the zero-fork property that
`tests/run.sh`'s "the hot edges exec no tmux at all" pins becomes a type fact
rather than a convention**. The [required Linux subprocess check](architecture.md#performance)
also rejects process creation and execution attempts in the documented Linux
configurations, with controls for each syscall name.

**I6 — Environment evidence is per-signal, and a multiplexer vetoes the signals that
leak.** Each probe carries a typed `NonEmpty` or `Exact` matching rule,
`survives_mux` eligibility and `locale_forwarding` metadata. `KONSOLE_VERSION`,
`KONSOLE_DBUS_SESSION`, `ITERM_SESSION_ID`, `LC_TERMINAL` and `TERM_PROGRAM` are
`survives_mux: false`. Locale forwarding marks `LC_TERMINAL` as a candidate for
configured SSH forwarding; it guarantees no hop and is not a veto. The mux flag
*is* today's `!flag("TMUX") && !flag("STY")`
conjunction, lifted out of each terminal's detector — and it fixes the same bug on
macOS before it ships, where a stale `ITERM_SESSION_ID` inside tmux would otherwise
reproduce #18's defect on the other platform.

The bounded vendor values and precedence are recorded in the
[automatic probe policy](research/terminal-capability-matrix.md#automatic-probe-policy).
Detection and doctor share one matching walk; doctor prints matched values and
retains the mux oracle's hint so it outranks lower environment evidence even when
both name the same family. The current `NoOracle` supplies no hint.

**ssh is evidence, not a layer.** It interposes no renderer. It may lose the
environment evidence or carry hints according to client and server configuration.
Without an eligible hint the family is `Surface::Unknown`, whose `arming` is
`None`. `CCTAB_TERMINAL` can supply a missing family hint, including a terminal on
another operating system, so the override matches every variant's name.

## The armed record

Arming sources select **which surface's restore bytes to attempt**. They do not
record confirmed writes, and even a completed write does not prove that a terminal
applied the bytes. `Armed::resolve` in `src/armed.rs` chooses the first available
answer in this order:

| rung | source | meaning |
|---|---|---|
| 1 | tmux session option `@cctab_armed` | retained shared arming policy; a surface name carries the restore obligation, `-` says no retained policy |
| 2 | session record `s <surface>` | conservative restore obligation for the selected appearance route; no negative value |
| 3 | `ArmSource::Assumed` | legacy assumption from the current hook's environment when neither store answers |

The record grammar is specified in
[time and persistence](state-contract.md#time-and-persistence). Unknown surface
names read as absent, so a later version cannot make this version select arbitrary
bytes. Missing or unreadable stores fall through to the next rung. Rung 3 preserves
the historical behaviour exercised by the stateless golden corpus; it cannot
recover a startup surface that the end hook's environment no longer names.

**Ordering and partial writes.** `SessionStart` resolves state and routing first.
Inside tmux it then attempts to install the shared policy and reattachment hook,
and attempts client arming. Next it writes the optional session `s` line, before
the direct startup write. That direct write combines arming (when routed directly)
and title bytes in one ordered buffer and one guarded terminal acquisition;
`write_all` can issue several writes and is not atomic. An error may follow a
complete arm and a partial title. Startup therefore keeps the restore obligation
after both skipped delivery and errors. A headless start can leave an `s` line
without sending any arming bytes. This conservative behaviour and the existing
record bytes are preserved; the line is not a delivery receipt.

**Detached tmux and shared ownership.** Tmux records policy before any client
writes, including starts with no attached clients. Its `client-attached[1971]`
hook applies that policy to later attaching clients. A non-arming start uses
`set -o` to initialise `-` only if no shared record exists; it cannot erase an
outstanding obligation or reattachment hook. The last Claude pane attempts the
restore, even if another pane selected the arming surface. Teardown removes the
policy and hook after the attempt, including when detached or when delivery
fails, so later attaches do not arm after the last owner has left.

**Delivery outcomes.** Direct and client writes return `io::Result<bool>`:
`Ok(false)` means delivery was skipped, `Ok(true)` means the write or console API
call completed, and `Err` means an operation failed, possibly after a partial
write. For a client list, `true` means at least one completed write; a refused or
unresolvable destination remains a skip, and an empty list is a detached skip.
Every listed client is attempted even after a failure, and the first error is
returned. A client-listing error is also returned. Startup and teardown attempt
both client and direct delivery before returning an error to the silent hook
boundary. The `tmux-arm` management verb likewise remains silent and exits zero
for delivery outcomes. No receipts, retry state or terminal acknowledgements are
persisted. Tmux configuration commands and state persistence remain best effort;
a recorded policy does not prove that every configuration command succeeded.

**Stable topology and best-effort restoration.** Only the surface is remembered.
`mux::route` selects the delivery channel from the **current** stack; direct
delivery resolves the **current** `CLAUDE_PID` destination, and tmux writes to its
**currently attached** clients. Changing `CCTAB_TERMINAL` alone no longer loses the
remembered surface when a store answers, but changing `TMUX`, `TMUX_PANE`,
`CCTAB_NO_TMUX`, the session process's terminal, or the surrounding multiplexer
can redirect or prevent restoration. No original terminal identity is retained.
Detach/reattach within the same tmux session is supported through retained policy;
a client that detaches before teardown cannot receive its restore. Mixed terminal
types attached to one tmux session share the selected policy.

Persistence failure, hook interruption, guard refusal, missing destinations and
write failures can all prevent restoration. `SessionEnd` reads the session record
before state resolution removes it, and tmux retires its policy after the last
owner's attempt. Neither cleanup waits for acknowledgement or retries later. The
restore uses Konsole's stock formats, not a saved custom profile.

**Focused checks.** `emit` unit tests distinguish skipped and completed writes and
inject failures before, during and after the arm in a combined startup buffer.
The client-writer unit test proves that an error does not stop later clients.
`tests/test_state_guarantees.py` checks headless obligations, real disposable-PTY
startup/restore bytes, and a syscall-injected startup write failure whose record
still drives restoration. `tests/test_tmux_status.py` covers attached and detached
shared ownership, reattachment and final cleanup on private servers. These tests
observe bytes and policy, not terminal application.

## Attention (#14)

The measured facts that shape it are in
[backend-scouting.md §2 and §4](backend-scouting.md); the consequences:

- **Delivery splits three ways** and the capability says which: through the hook's
  `terminalSequence` allowlist, through a direct pty write, or fully out of band.
- **OSC 9;4 taskbar progress is the recommended first channel.** It is the only
  candidate that is sanctioned by the hook protocol, portable to all three operating
  systems, free of any dependency and free of a pty write — so it works on macOS and
  Windows *before* either port lands.
- **Fan-out differs by message class.** Appearance goes to **every** attached client
  (each tab needs arming); attention goes to **at most one**, or two Konsole windows
  on one tmux session produce two popups per transition — a storm that per-edge
  coalescing cannot see, because it coalesces per *edge*, not per *delivery*.
- **Coalescing uses the same three rungs as the armed record**, because the
  `replaces_id` the D-Bus protocol would use arrives in a reply that costs 14.8 ms to
  read. With no store there is no coalescing, and `doctor` says so.
- **Focus acknowledgement is answered "yes, by not alerting"** where the grammar has
  it — OSC 99's `o=unfocused`. *Inbound* acknowledgement stays
  `Unsupported("a one-shot hook has no reader")`: fd 0 is `/dev/null`, `/dev/tty` is
  unusable, and nothing in the crate is long-lived. That is a printed line, not a
  silent gap.
- **A backend that needs a daemon must say so** as a first-class property, and stay
  opt-in. The pinned iTerm2 prior art is the worked example of what forces one.

`Resolved` carries an optional paint and
`Transition { Entered, Remained, Left, Unknown }` independently. The
[logical attention contract](state-contract.md#logical-attention-transitions)
defines the comparison, including expiry, silent events, reset and teardown.
`main` retains the decision before filtering out absent paints; a future attention
consumer belongs before that filter. Transitions are **consumed by nothing**
today. They do not guarantee persistence or delivery and cannot by themselves
provide exactly-once alerts. Direct Rust assertions exercise this dormant API;
the golden corpus separately checks that title output has not changed.

## Dependencies

The gate, and the verdicts, are in
[backend-scouting.md §5](backend-scouting.md). Net effect: **two target-gated
first-party binding crates, neither on any hot path, and no change to the default
Linux build's dependency tree.** Notifications are hand-rolled on Linux — one
`org.freedesktop.Notifications.Notify` call over a `UnixStream`, measured at 255 µs
— or a subprocess elsewhere. zbus was rejected at 49 required transitive crates and
an async runtime, for one method call.

> **BOTH ARE TAKEN NOW, and this is no longer a projection.** `Cargo.toml` carries
> `windows-sys` behind `cfg(windows)` and `libc = "0.2"` behind
> `cfg(target_os = "macos")`. `libc` has zero transitive dependencies; a Linux build
> resolves neither crate, so the sentence about the default Linux build's dependency
> tree is now a measured fact rather than a design intention. Licences, and why no
> `libproc` wrapper crate was taken in place of `libc`, are recorded in
> [research/attribution.md](research/attribution.md).

## Migration

### What has landed, on this branch

This work was first written against `9bfd987` as thirteen commits on
`feat/14-backend-abstraction` ([#21](https://github.com/dalf/claude-tabstatus/pull/21)).
It is **re-landed here as a curated subset** on top of
[#28](https://github.com/dalf/claude-tabstatus/pull/28)'s Windows port, not rebased:
#28 owns the platform axis, and what is re-landed is only what #28 does not have.

| sha | commit | what it is |
|---|---|---|
| `c5472c3` | port to native Windows behind a `sys` platform seam | **#28.** The platform axis, for real |
| `8c878bb` | the state layer on Windows | **#28** |
| `b0c2432` | session-start and session-end on native Windows | **#28** |
| `a1e4153` | build, test and release the Windows binary in CI | **#28**, and the base of this branch |
| `ff63464` | `Support<T>` — one vocabulary for absence | re-landed |
| `aaba975` | `src/surface/` — the **surface axis**, fourteen rows wide | re-landed |
| `f251c22` | `src/mux/` — the **multiplexer axis**; the two arming predicates collapse into one `route()` | re-landed |
| `c3e3d03` | `Transition`, carried and consumed by nothing | re-landed |
| `6efda27` | the armed record — restore driven by a record, not re-derived | re-landed |
| `4de5107` | `doctor` prints the three axes, from ONE resolved `Stack` | re-landed |
| *(this commit)* | these two documents, the research files, and `scripts/bench-hot.sh` | re-landed, and **corrected against this base** |

**Dropped as superseded by #28, and deliberately not ported:**

| from the old branch | why it is not here |
|---|---|
| `src/rawpath.rs` (`e7096d5`) | #28 answers the same question with `sys::os_str_from_bytes` and `sys::os_string_from_vec`, which return `Cow<'_, OsStr>` — correct, because reconstructing an `OsStr` from arbitrary bytes on Windows can allocate, and a borrowed-only newtype cannot say so. Everything else reads bytes through `OsStr::as_encoded_bytes`. |
| `src/sys/{mod,linux,macos,posix,windows,fileid,stamp}.rs` (`2c2e6e0`, `be83e6a`) | #28's `src/sys/{mod,unix,windows}.rs` is the platform axis, built and tested on a real Windows runner. Nothing in this branch adds a line to `src/sys/`. |
| `sys::STAMP_KIND` / `StampKind::describe` | part of the superseded `src/sys/stamp.rs`. doctor's platform line names #28's `sys::ORIGIN_KEY` instead — the same fact one layer out, and the one a reader of a state directory shared between WSL and native Windows actually needs. |
| `scripts/manifest-sources.sh` (`ea473d2`) | #28 reached the same fix independently; see below. |
| the `[lib]` target (`ea473d2`) | this tree has no `src/lib.rs`; the binding the old branch made in `lib.rs::paint` is made in `src/main.rs::paint`. |

**Measured here, on this branch's HEAD.** `cargo test --locked --offline --target
x86_64-unknown-linux-musl` 244 passed 0 failed 4 ignored; `sh tests/run.sh` 806
passed 0 failed; `sh tests/corpus/replay.sh` 312 passed 0 failed 0 diverged, with
`tests/corpus/cases.jsonl` unchanged through all four commits; corpus unittest 2 OK;
all six Python suites OK. `cargo check --locked --offline --target
x86_64-pc-windows-gnu` **0 errors, 0 warnings**; `--target aarch64-apple-darwin` 0
errors, 0 warnings; host `--all-targets` warning-free.
The original check was `sh scripts/bench-hot.sh <the windows-port baseline>`.
Its dry-run timing and tmux-shaped tracing did not cover delivery, and its default
self-comparison could not detect a baseline regression. The [current checks](architecture.md#performance)
separate required Linux subprocess/delivery assertions from optional baseline timing.

**Historical dry-run timings, not an enforced CI latency bound.** One run here
measured working +14 µs, waiting −1 µs, idle −6 µs, notify +4 µs; an independent run
against the same baseline binary measured +1, +17, −4, −5. Both pass, and the
disagreement between them is the point: each is one machine-moment on a desktop that
is also doing something else, which is why `bench-state.sh`'s method reports a spread
and why the optional comparison uses a band. Re-run the script; do not read any
single figure here as a spec.

### Windows: what the counts mean now

**The whole crate compiles for Windows — and unlike the earlier draft of this file,
it WORKS there.** #28 is a real port: a `windows-sys` backend, a directory junction
for the plugin link, a preflight before `settings.json` is touched, the record lock
on `LockFileEx`, and CI that builds `x86_64-pc-windows-msvc` on a `windows-2025`
runner, runs `cargo test --locked --all-targets` there, runs four of the Python
suites on native Windows Python, and publishes the binary. Windows is **not**
`Unsupported`, and any sentence in this repository that says it is has been
overtaken.

The old branch tracked "Windows errors" per commit because on that base the number
was falling from 88 to 0 and a green type-check was the whole of what it could
claim. On this base the number is 0 before the first of these commits and 0 after
the last, so it is not a per-commit column any more; it is a baseline every
commit here re-measured and did not move, and every commit message says so.

**macOS is still unported**, and is still the case worth understanding: it compiles,
because it takes the `unix` backend, and then five `/proc` reads mean nothing there
at runtime. #28's `src/sys/unix.rs` documents that per function — `process_start_time`
says "or on a Unix with no `/proc`", `session_tty` resolves through `/proc` — so the
degradation is written down rather than silent, but it is a degradation.
[#1](https://github.com/dalf/claude-tabstatus/issues/1) is the port.

> **OVERTAKEN IN ITS FACTS, NOT IN ITS WARNING.** macOS has a backend: `libc` is a
> `cfg(target_os = "macos")` dependency with zero transitive dependencies, and the
> five reads are answered rather than degraded. Re-checked by grep in this tree:
> **every `"/proc` string in `src/` is inside a `cfg(not(target_os = "macos"))`
> item**, each with a macOS answer next to it — `proc_pidinfo(PROC_PIDTBSDINFO)`
> for a start time, `kill(pid, 0)` for liveness, `TMPDIR` for the state directory,
> `None` for the host-name file so `$HOSTNAME` is reached one failed `open`
> earlier, and `proc_pidfdinfo(PROC_PIDFDVNODEPATHINFO)` for the session's own tab.
> `sys::HAS_SESSION_TTY` is `true` there, so **session-start arms the tab and
> session-end clears it**, through the same prefix/char-device/writable guard Linux
> uses. Only the lookups are `cfg`-selected; every decision downstream of them is a
> pure function the Linux `cargo test` runs.
>
> Native arm64 CI now links and exercises process identity, reaping, locks and
> disposable PTY delivery. **Native arm64 execution passed at `078c547`**; the
> recorded run establishes the tested OS calls and scenarios. Intel remains cross-checked only;
> terminal applications and macOS tmux remain unvalidated. See
> [macOS validation](architecture.md#macos-validation) for the evidence boundary.
> [#1](https://github.com/dalf/claude-tabstatus/issues/1) remains open.

### What `doctor` prints

The axes were the whole refactor and nothing printed them. `doctor` now resolves ONE
`Stack` at the top of the report — it built `Config::from_env()` three separate
times, and the moment `MuxOracle::leaf_hint` execs (#18) that is three forks whose
answers can disagree — and ends in a fixed-column capability table:

```text
platform    linux                        a record carries its session's origin as `p <pid> <start>`
  session terminal      ok    CLAUDE_PID=4242 - session-start and session-end write it directly
  process stamp         ok    112867300, this process (pid 2745932)
  record lock           ok    a record is written under an exclusive lock proven to hold that same file
  state dir             n/a   no CCTAB_STATE_DIR and no XDG_RUNTIME_DIR
  file modes            ok    the record directory is created 0700
  replace while running ok    one rename, atomic, over the very binary the hooks exec
  plugin link           ok    a symlink points Claude Code's config at the generated tree
  hostname              ok    fedora.home
surface     konsole                      Konsole, measured on a running terminal
  evidence              ok    $KONSOLE_VERSION
  elide                 left  the tab label is cut from the left, so a glyph goes last
  title (OSC 0)         ok    icon name and window title together
  ...
  arm / restore         ok    ESC]50;LocalTabTitleFormat=%w;RemoteTabTitleFormat=%w BEL
                              back to ESC]50;LocalTabTitleFormat=%d : %n;RemoteTabTitleFormat=(%u) %H BEL
                              which is the terminal's COMPILED-IN default, not your profile
multiplexer none                         n/a: neither $TMUX nor $STY is set
```

Every row of the platform block is a `HAS_*` or an `Option` of #28's that the paint
path already reads, lifted into `Support` **here and nowhere else**. That is what
the `DIR_LINK`, `ORIGIN_KEY`, `NO_STATE_DIR` and `RUNTIME_DIR_VAR` constants are for
from a reader's side, and it is why the same report is worth reading on a Windows
box: `session terminal` becomes the console route, `file modes` becomes `n/a` with
the ACL sentence, `plugin link` says `junction`, and the platform line says `q`.

Four decisions in that block are load-bearing rather than cosmetic:

* **`Support::label` and `Support::reason` are the whole formatter.** doctor cannot
  print a sixth word, and cannot print an absence without the reason the row carries.
* **The columns are FIXED**, so a report from Linux and one from Windows diff
  cleanly — which is the only way to compare a row that was measured with one that
  was read out of vendor source.
* **Every surface line carries its `CapSource`**, because **thirteen of the fourteen
  rows have never had a byte delivered to them** - exactly one, Konsole, is
  `Measured`; eleven are `VendorSource` and two are `Inferred` - and a reader who
  sees `ok` is owed the difference.
* **No escape byte is ever written.** doctor is read in the terminal whose tab is
  misbehaving; a report that echoed the arming would arm it while describing it.
  `tests/run.sh` pins zero escape bytes in the whole report.

`doctor --surface <name>` prints that block alone, for any of the fourteen names —
including a terminal this machine could never run. Every input is `&'static` data, so
it needs no terminal, no session and no config directory, and it is how the Windows
column gets read from a Linux box.

The const surface table is a protocol catalogue. Versioned entries keep their
minimum beside the grammar in `rows.rs`, and `Protocol::reported` resolves the
reporting verdict from separate `VersionEvidence`. Ordinary doctor uses that
answer; `--surface` always displays the catalogue requirement as `?`, even if the
local environment names a recent terminal. See
[protocol catalogue and running-version evidence](architecture.md#protocol-catalogue-and-running-version-evidence)
for the parsing and mux evidence rules. Neither path changes title emission or
arming. Version-uncertain reporting answers carry no implementation of their own;
the catalogue remains the source of the typed grammar. `reported()` is a reporting
view, not an emission plan. Foreign-setting uncertainty in the catalogue retains
the known grammar (Windows Terminal's OSC 777 and VS Code's OSC 99). Conhost
progress remains uncertain with no known implementation; no syntax is invented
for it.

The platform axis reports the session's terminal from `$CLAUDE_PID` and deliberately
does **not** open it to prove the capability: on Windows the console route attaches a
console, and releasing one invalidates the process's stdout handles — so a report
that took the capability would truncate itself on exactly the platform it exists to
explain.

**One doctor string changed** in the re-land, and only one: the `pty:` line became
the platform axis's `session terminal` row. All four of its sentences are unchanged,
verified by diffing the whole report against the previous commit's binary. No
existing `tests/run.sh` assertion changed; the 35 added are purely additive.

### What remains

| # | why it is not here |
|---|---|
| macOS | #28 ported Windows, not macOS. [#1](https://github.com/dalf/claude-tabstatus/issues/1). **Largely here since, at the `sys` layer**: `libc` is a `cfg(target_os = "macos")` dependency with zero transitive deps, and every `/proc` read in `src/` is now inside `cfg(not(target_os = "macos"))` with a macOS answer beside it — start time (`proc_pidinfo`), liveness (`kill(pid, 0)`), state directory (`TMPDIR`), host name (no file, straight to `$HOSTNAME`), origin key (`r`), and the session terminal (`proc_pidfdinfo`), so `sys::HAS_SESSION_TTY` is `true` and session-start and session-end paint. Native builds and arm64 CI **passed at `078c547`**. Intel retains cross-checks only, the corpus remains a Linux specification, and the terminal rows remain source-derived rather than tested in macOS applications. See [validation scope](architecture.md#macos-validation). |
| the tier-3 restore rule | a **behaviour change**, so it needs its own commit with newly recorded corpus cases, and an owner's decision this branch did not have |
| an attention channel | this branch builds the seam [#14](https://github.com/dalf/claude-tabstatus/issues/14) needs and stops there. The recommended first channel and its policy table are in [backend-scouting.md §4](backend-scouting.md) |
| the Windows state layer's file identity | #28 landed it (`8c878bb`); nothing here touches it |

**Sequencing against `feat/18`.** `aaba975` and `f251c22` rewrite the exact functions
[#18](https://github.com/dalf/claude-tabstatus/issues/18) rewrites — the leaf
detector that was `Terminal::detect` in `config.rs`, and `arm_konsole` in `tmux.rs` —
so expect a real conflict there, and resolve it in favour of invariant I5: when #18
merges, its probe becomes the body of `MuxOracle::leaf_hint` rather than a second
`list-clients` call. That is not a prediction about a branch this document can see;
it is a statement about where the seam was put.

### The manifest fix, and a bug it uncovered — **#28 got there first**

**This was already fixed on this base, independently, by
[#28](https://github.com/dalf/claude-tabstatus/pull/28), and the credit is theirs.**
`scripts/build.sh:135` and `tests/run.sh:1420` both spell
`find src -type f -name '*.rs' | LC_ALL=C sort` on this branch, landed in `a1e4153`
— the same two halves of the fix the old branch reached, and reached for the same
reason: `src/sys/` gave `src/` a subdirectory. Nothing in these four commits
changes either line.

It is recorded here anyway, because the *why* is not obvious from the diff and
because a future reader who sees `LC_ALL=C` in a sort has to be able to find out
that it is load-bearing.

The one thing the old branch did that this base does not is spell the list **once**,
in a `scripts/manifest-sources.sh` both callers source. Here it is spelled twice and
kept in step by hand, which is the older arrangement with the newer command in it —
worth noting as a hazard, not as a defect, since a drift between the two is exactly
what the staleness guard would report.

What broke, and would break again:

1. **`ls src/*.rs` does not descend**, so twenty new files would be invisible to the
   staleness guard — the precise failure it exists to catch.
2. **`sort` is locale-dependent once paths contain `/`.** glibc's UTF-8 collations
   ignore punctuation at the primary level. Reproduced here:

   ```console
   $ printf 'src/sys/mod.rs\nsrc/sys.rs\nsrc/system.rs\n' > ord.txt
   $ LC_ALL=C sort ord.txt        $ LC_ALL=fr_FR.UTF-8 sort ord.txt
     src/sys.rs                     src/sys/mod.rs
     src/sys/mod.rs                 src/sys.rs
     src/system.rs                  src/system.rs
   ```

   `build.sh` *writes* the manifest and `run.sh` *recomputes and diffs* it. With
   subdirectories, a developer in `fr_FR` who builds and a CI runner in `C` who
   checks produce different orderings of identical files, and the suite fails with
   "the committed binaries are not stale" — a spurious failure with a maximally
   misleading message. **`LC_ALL=C` is load-bearing, not hygiene.**

Both halves — `find` instead of a non-descending glob, and `LC_ALL=C` on the sort —
are in this tree already, in `a1e4153`.

## Known weaknesses

Recorded because a design whose weaknesses are unwritten gets them discovered by a
user instead.

1. **The armed record's fix does not reach the configuration the corpus tests.**
   See above. With neither tmux nor a state directory there is nowhere to write a
   record, so rung 3 is all there is — and rung 3 is the defect, now wearing its
   own name in `doctor`. Rung 2 additionally has no negative value, so a
   `CCTAB_TERMINAL` that names Konsole only at the END can still select a restore
   for a tab outside tmux that may never have been armed. A conservative `s` line
   can do the same after skipped startup delivery. Those bytes select Konsole's
   stock formats and may replace custom formats; the record does not establish
   what the terminal applied. See the [arming contract](#the-armed-record) for
   the stable-topology and best-effort limits.
2. **Six of the capability rows were written from vendor source, not from a running
   terminal** (Windows Terminal, conhost, iTerm2, Terminal.app, Ghostty, VS Code).
   A test can prove a row is well-*formed*; it cannot prove it is *true*. The row
   records its own provenance and `doctor` prints it, but a wrong row still ships —
   on exactly the surfaces where nobody has ever delivered a byte.
3. **The surface commit is a large diff across three files with no split that leaves
   both halves compiling**, because the moment `Terminal` becomes `Surface` every
   call site must move. That is `aaba975` here, and it landed that way. The 312
   replay says whether a byte changed; bisecting inside it is not pleasant.
4. **Fake backends and cross-checks cannot validate OS calls.** Windows has a
   native runner. macOS now has an arm64 job that links and executes tests,
   including real processes and disposable PTYs, and **passed at `078c547`**.
   The Intel target has only cross-checks. Runtime evidence remains limited to
   the executed architecture, OS image and scenarios; see
   [macOS validation](architecture.md#macos-validation).
5. **Generic code with zero instantiations is type-checked but never
   monomorphised.** A bound only codegen would reject is not caught on Linux. The
   safety net is the fake staying a *complete* impl; if it drifts, the net thins
   silently.
6. **Hand-declared `extern` blocks are the one thing neither a test nor
   `cargo check` can validate** — `cargo check` does not link, and a wrong
   `#[repr(C)]` layout is accepted silently on Linux and fails at runtime on the
   target. Taking no dependency is what makes an offline migration landable; this is
   its price.

   > **THE LAYOUT HALF OF THIS IS FALSE, and was falsified deliberately.**
   > `src/sys/unix.rs`'s two hand-written macOS structs carry seven
   > `const _: () = assert!(…)` items pinning `size_of` and `offset_of`, and
   > `cargo check --target aarch64-apple-darwin` / `x86_64-apple-darwin` — run in
   > CI, not only locally — evaluates every one of them. A wrong `#[repr(C)]` is
   > therefore **not** accepted silently: it fails the build for the target it
   > would have broken. Negative controls re-run against this tree: a spurious
   > `u32` fails three of the seven, an `off_t` written as `i32` fails one.
   >
   > **Two parts survive, and they are what a future backend should copy the
   > caution from.** First, cross-checks do not link: the successful native arm64
   > run supplies `proc_pidfdinfo` linking and execution evidence for that target
   > only; Intel remains cross-checked. Second, a
   > size assert cannot see two same-width fields transposed — measured, not
   > argued: `fi_type` and `fi_guardflags` swapped compiles clean on both Apple
   > targets with all seven asserts green. That is survivable in this one case only
   > because nothing is read out of that struct. It is not survivable in general:
   > `darwin-libproc-sys` 0.2.0 declares `vnode_info` with `vi_fsid` and `vi_pad`
   > in the other order from the header — same size, different offset — which is
   > precisely the error class the asserts are blind to, and one reason libc's own
   > declarations carry everything nested here.
7. ~~**`RawPath`'s soundness rests on a std *doc* guarantee**~~ — **superseded by
   [#28](https://github.com/dalf/claude-tabstatus/pull/28) and not ported.** There is
   no `RawPath` in this tree. Bytes come out of an `OsStr` through
   `OsStr::as_encoded_bytes` and go back in through `sys::os_str_from_bytes` /
   `sys::os_string_from_vec`, which return `Cow<'_, OsStr>` because reconstructing
   from arbitrary bytes on Windows can allocate. The underlying hazard — cutting a
   WTF-8 sequence anywhere but an ASCII byte — belongs to whoever slices, and is
   documented where they do it; it is no longer concentrated in one newtype whose
   soundness argument a Linux test could not observe.
