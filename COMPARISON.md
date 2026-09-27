# Comparing claude-tabstatus with related projects

Research date: **27 September 2026**.

This document compares `claude-tabstatus` with five projects that address terminal titles, session status, or attention management:

- [Yannis-Adn/terminal-addons](https://github.com/Yannis-Adn/terminal-addons), specifically `wt-tab-status`.
- [JasperSui/claude-code-iterm2-tab-status](https://github.com/JasperSui/claude-code-iterm2-tab-status).
- [wasulajr/headsup](https://github.com/wasulajr/headsup).
- [j-mccarthy-veeam/iTerm2-fancy-claude-tabs](https://github.com/j-mccarthy-veeam/iTerm2-fancy-claude-tabs).
- [bluzername/claude-code-terminal-title](https://github.com/bluzername/claude-code-terminal-title).

The comparison is intended to guide this repository's design and priorities. It does not award an overall winner: the projects address different terminals and make different promises about what an indicator means.

## Scope and evidence

One research agent examined each external project. The review covered source code and configuration as well as READMEs: hook registration, state transitions, terminal delivery, installation, cleanup, dependencies, tests and CI. External revisions are pinned in the source register below so that later changes do not silently change the basis of the comparison.

The evidence has three levels:

- **Implementation:** behavior visible in the inspected source. This establishes what a code path does, but does not by itself establish that a particular terminal or Claude version exercises it correctly.
- **Documented:** an upstream claim, supported-platform statement or measurement. These are attributed rather than treated as independently reproduced results.
- **Assessment:** a consequence or recommendation inferred from the implementation. Important uncertainties are stated explicitly.

This was not a common-hardware benchmark or a live end-to-end trial of all five integrations. No external project's installer was run against the user's configuration. Isolated tests and probes, where performed, are reported separately. Stars, commit counts and README length are not used as measures of reliability.

For `claude-tabstatus`, the committed baseline was [`50bf014`](https://github.com/dalf/claude-tabstatus/tree/50bf014f727e441c080edf8393e3d555d8b1c6c7). Its runtime modules were inspected alongside the working tree. Another agent was changing installation to use only `install`; the deployment description below reflects that working-tree direction. It should not be read as a claim that the refactor was already committed or released. The removed `standalone` command is not evaluated as a competing installation option.

## Choosing by use case

| Project | Main question it answers | Primary fit | Cost or boundary to accept |
|---|---|---|---|
| [claude-tabstatus](#what-claude-tabstatus-actually-offers) | Which session needs input, and in which checkout/host? | Linux, Konsole, SSH and tmux pane aggregation | Hook-derived state, Linux lifecycle routing, known owner/parser issues |
| [wt-tab-status](#terminal-addons--wt-tab-status) | Is this Windows Terminal tab working, waiting, done or in error? | Windows Terminal on WSL | No tmux; one wait owner; Windows notification integration |
| [JasperSui's adapter](#jaspersuiclaude-code-iterm2-tab-status) | Which iTerm2 tab should attract my attention? | Native iTerm2 appearance and focus-aware alerts | Persistent Python adapter; focus acknowledgement can clear a live wait indication |
| [headsup](#headsup) | Which window should I return to, and how do I manage its workflow? | A broader macOS session-management setup | More components and configuration; completion and input-needed share orange |
| [iTerm2-fancy-claude-tabs](#iterm2-fancy-claude-tabs) | What is the named session's reported state? | Session-name tracking and background-shell indication | One watcher per session; dependence on session-file schema and freshness |
| [claude-code-terminal-title](#claude-code-terminal-title) | What task is this terminal about? | Simple task labels, optionally persistent in zsh | No lifecycle state; shared persistence and hook-delivery limitations |

For this repository's stated Linux/Konsole/SSH/tmux use case, none of the inspected alternatives is a direct replacement with the same deployment and state semantics. Conversely, richer native iTerm2 appearance, Windows taskbar alerts or task summaries are real capabilities elsewhere that the local project does not currently provide.

## What claude-tabstatus actually offers

### A narrow, useful product boundary

The central promise is to answer two questions across a row of terminal tabs: **which session needs input, and where is that session?** The displayed location is normally `repository@branch`, with path fallback and an SSH host prefix. It does not use a model to invent a task title, read transcripts for a summary, or add a desktop notification service.

Three visible states carry that promise: working, waiting and idle. Normal completion and API failure both become idle. A background subagent can continue running after the main loop becomes idle; the state layer preserves a wait if that agent is asking for input, but does not otherwise keep the whole session visibly working. That is a product choice about what “idle” means, and differs from some of the projects below. [Hook wiring][local-hooks], [edge decisions][local-edge], [state transitions][local-state].

### Delivery and deployment

Most events emit JSON containing `terminalSequence`, letting Claude perform the terminal write. Session start and end instead locate the parent Claude process's stdout through `/proc/$CLAUDE_PID/fd/1` and write to its PTY. Those direct writes are used for the initial title, clearing the title, and Konsole's title-format controls. This is a Linux-specific lifecycle dependency even though OSC title sequences themselves are widely understood. [Delivery implementation][local-emit].

The normal hook path has no resident process. The binary has no third-party Rust crates. The default build is static musl; the GNU build has a different libc dependency profile. Rust 1.89 or later is required to build, and the supplied build targets are x86-64 Linux musl and GNU. “Single binary” therefore describes deployment convenience, not native support for every operating system or architecture. tmux integration additionally needs the `tmux` executable. [Cargo configuration][local-cargo], [build script][local-build].

In the working-tree installer being developed during this review, `install` materializes the embedded manifests and executable into a generated plugin directory, then links that directory into the Claude configuration. Updating source requires rebuilding and installing again. This separates a development checkout from the files used by live hooks and also supports copying one executable to a remote Linux machine. The installer preserves unrelated settings text and records prior configuration for removal. These are useful lifecycle properties, but separate from the correctness of the status model. [Current installation documentation](README.md#install), [settings editor](src/settings.rs), [embedded assets](src/embedded.rs).

### Wait ownership is the strongest state-model feature

With a usable state directory and session ID, the runtime stores a base state plus a set of waiting owners. Each owner has its own timestamp. A normal tool completion from a different agent should not dismiss a named wait, and overlapping waits can be remembered together, up to the configured implementation bound of eight. Records are locked per session, replaced atomically, and normally written only when a transition changes their contents. Dead-session records are reaped on session start using process identity where available. [State implementation][local-state].

This is stronger than treating every tool completion as “working” or retaining only one wait owner. It is not a proof of complete correctness. The preceding review reproduced three issues now tracked separately:

- [#5: nested tool data can be treated as hook metadata](https://github.com/dalf/claude-tabstatus/issues/5).
- [#6: unrelated subagent activity can clear an unknown-owner wait](https://github.com/dalf/claude-tabstatus/issues/6).
- [#7: notification recognition depends on JSON whitespace](https://github.com/dalf/claude-tabstatus/issues/7).

The state layer also has an operational prerequisite: without `XDG_RUNTIME_DIR` or `CCTAB_STATE_DIR`, it falls back to stateless behavior. A deployment that starts successfully is not necessarily using wait ownership. This matters on remote machines and stripped-down execution environments; `doctor` should be part of validating such a deployment. [State directory selection][local-state].

### tmux adds both aggregation and expiry

Inside tmux, the pane title carries a location, state and timestamp. A tmux format aggregates all Claude panes in the attached session into a strip of indicators. Its timer can age a stale indicator to idle and eventually remove it, without spawning a watchdog. Default thresholds are 1,200 seconds for working, 900 for waiting and 3,600 for disappearance. [tmux implementation][local-tmux].

This mechanism needs tmux's status timer to be active. The glyphs, thresholds and title settings are shared across the server, so inconsistent configuration between sessions can produce surprising results. Original tmux title settings are saved for restoration at uninstall, rather than restored every time one Claude session ends. Konsole over SSH needs explicit terminal identification and uses attached-client PTYs for arming. These are substantial multiplexer features, not merely a claim that OSC 0 works inside a pane. [tmux implementation][local-tmux].

Expiry is a recovery heuristic. It does not establish that a long-running tool finished or that an unanswered dialog disappeared. Outside tmux, expiring a state record on a later hook does not itself schedule a terminal repaint. Similarly, direct Konsole cleanup restores stock title formats, not a snapshot of the user's customized formats. Those boundaries should remain visible in user-facing documentation. [State expiry][local-state], [Konsole restore][local-emit].

## terminal-addons / wt-tab-status

**Best fit:** Windows Terminal users running Claude under WSL who want tab colors, taskbar progress and notifications while retaining Claude's conversation title.

This is the closest conceptual relative and the local repository already credits its state-and-owner idea. Upstream declares Windows Terminal 1.15 or later, WSL 2 and Windows 10/11, with testing on Windows Terminal 1.24. Those are upstream claims; this review did not run Windows Terminal. The runtime requires `WT_SESSION` and explicitly exits when `TMUX` is set. [Requirements][wt-readme], [runtime guards][wt-script].

### States and attention policy

| Situation | wt-tab-status behavior | Difference from claude-tabstatus |
|---|---|---|
| Session starts/resumes or ends, excluding compact starts | Neutral/profile tab color | Local project paints idle initially, then clears on exit; compaction is also excluded |
| Work begins | Blue tab and indeterminate progress | Local project adds a working title glyph |
| Input needed | Red tab, paused progress, bell and toast | Local project uses an orange glyph without an OS alert |
| Main turn completes | Green/done unless background-agent detection keeps it active | Local project sets its idle base, preserving outstanding waits |
| API failure | Orange/error, error progress, bell and toast | Local project maps `StopFailure` to idle |

Notifications are emitted on state changes, reducing duplicate bells. Keeping the existing conversation title is a meaningful advantage when users identify sessions by topic. The tradeoff is that it does not add repository/branch/host location or aggregate tmux panes. Colors are not interchangeable across projects: orange means error here and waiting in `claude-tabstatus`. [Constants and emission][wt-script].

The registered events include direct `Elicitation` and `ElicitationResult`. Main-thread `PermissionRequest` becomes waiting immediately; requests carrying `agent_id` are ignored and rely on a later notification. Upstream explains this as avoiding prompts requested by background agents that are denied without display. The local project instead cites captures of real subagent dialogs and paints those permission requests immediately. These observations need versioned, reproducible traces to resolve; neither implementation alone establishes a universal rule about Claude's behavior. [Hook manifest][wt-hooks], [transition code][wt-script], [local wait rationale][local-state].

`Stop` preserves a wait or reports working when it detects background-agent types; background shells and monitors do not count. The implementation scans for matching `type` strings without checking a `status` value or restricting the match to the `background_tasks` array. This is less precise than the helper name `has_running_agents` suggests. `SubagentStop` is not registered. [Transition code][wt-script].

### Ownership, parsing and concurrency

Its record contains one state and one owner. Another agent cannot normally clear a named wait, but notification/elicitation waits get owner `any`. A second waiting event does not update the owner unless the visible state changes. Consequently, it does not represent overlapping independent waits. Direct elicitation hooks improve coverage, but `ElicitationResult` unconditionally sets working; there is no per-elicitation correlation. Copying those hooks alone would not solve local issue #6. [Owner checks and persistence][wt-script].

The parser accepts whitespace around colons, avoiding the local whitespace failure, but reads the entire payload into a Bash string and uses regular expressions without JSON-depth tracking. Escaped JSON in a string and a nested JSON object are different cases; handling the former does not establish safe top-level metadata extraction. [Parser and input path][wt-script].

All sessions share one directory-wide `flock`. Its two-second acquisition timeout is not checked before processing continues. An isolated probe in this review held the lock while invoking a hook: after approximately two seconds, the hook still updated its record and emitted bytes to a fake terminal. That is a reproduced lock-failure behavior, not a normal latency benchmark. The local project's per-session lock and atomic record replacement are preferable foundations for overlapping sessions. [Lock and update implementation][wt-script].

### Operations, data and verification

No daemon is required, and no Python, Node or jq is used on the runtime path. Bash still invokes system utilities, and notifications prepare a PowerShell script before starting PowerShell detached. The README's “few milliseconds” claim was not independently benchmarked. PTY discovery walks Linux ancestors to the first process named `claude` and requires its stdin to be a PTY. [Runtime and toast launcher][wt-script].

Installation and updates use the Claude plugin marketplace. Optional user changes configure Windows Terminal's bell, toast language and Claude's idle-notification delay. The latter is explicitly an undocumented setting upstream. State files live under the runtime directory or `/tmp`; session end removes them, but there is no independent expiry/reaper comparable to local tmux decay. Interrupt recovery depends on a later idle notification. [Lifecycle documentation][wt-readme].

Toasts can include commands, notification messages, answer excerpts and error details, which may remain in Windows notification history. This is a materially larger display/persistence surface than a location-only title, even without outbound telemetry. [Toast renderer][wt-toast].

The repository is MIT-licensed. CI runs shell linting, manifest checks and the Bash suite. The inspected suite exercises state sequences, ownership, concurrent hooks and PTY guards. The local research run had 50 passes and one environmental failure because the `script` utility was missing; PowerShell-dependent toast tests were skipped. This does not verify Windows rendering. [Tests][wt-tests], [CI][wt-ci], [license][wt-license].

**Useful lessons:** retain direct elicitation coverage, explicit error presentation and preservation of the conversation title as design options. Do not inherit the single-owner limitation, uncorrelated elicitation completion or unchecked lock timeout.

## JasperSui/claude-code-iterm2-tab-status

**Best fit:** macOS/iTerm2 users who want attention cues integrated with the terminal's native appearance and focus events.

This project uses Bash hooks to write JSON signals and a persistent Python adapter to apply them through iTerm2's API. It can show running, idle and attention prefixes, flash the tab, set badges, emit notifications and play sounds. It can alternatively write a subtitle variable, preserving the main title. Bootstrap explicitly targets macOS; there is no inspected remote-file transport or tmux aggregation backend. [README][jasper-readme], [bootstrap][jasper-bootstrap], [adapter][jasper-adapter].

### What the registered hooks cover

| Event | Registration and effect |
|---|---|
| `SessionStart` | Bootstrap the adapter; no initial status signal |
| `UserPromptSubmit` | Running |
| `Notification` | Only `idle_prompt` and `permission_prompt`; idle or attention |
| `PreToolUse` | Only `AskUserQuestion`; attention |
| `PostToolUse` | Only `AskUserQuestion`; running |
| `PostToolUseFailure` | Only `AskUserQuestion`; idle |
| `Stop` | Idle |

The inspected manifest has no `PermissionRequest`, direct elicitation, `StopFailure`, `SubagentStop` or `SessionEnd` hook. Permission visibility therefore relies on notifications, and an ordinary completed tool does not itself restore running after a permission notification. Its explicit question-failure branch is useful but should not be mistaken for comprehensive interruption recovery. [Manifest][jasper-hooks], [hook logic][jasper-hook].

### Acknowledgement and actual waiting are different

When the polling loop sees an attention session as focused, the adapter clears the signal and restores appearance. This includes attention arriving in an already-focused session. It does not check whether the user answered the dialog. This is coherent for an alert whose job is to bring the user to the tab; it is a weaker source for a persistent “this session still needs input” indicator. Running and idle prefixes are not dismissed on focus. [Focus handling][jasper-adapter].

There is one signal file per Claude session ID and no use of `agent_id` or tool IDs for ownership. Thus the implementation has no representation for multiple waits belonging to different agents. Concurrent events sharing a session can overwrite one another; distinct session signals targeting the same tab also share a presentation surface. Those are source-derived limitations, not a measured frequency of live failures. [Signal writer][jasper-hook], [session matching][jasper-adapter].

### Richer restoration, with lifecycle limits

The adapter snapshots the original color, coloring-enabled flag, session name, badge and tab title override. Its prefix can wrap a live title expression instead of freezing one captured title string. This preserves user-specific appearance more faithfully than restoring Konsole's stock formats. Configuration supports title, subtitle or both. [Snapshot and title handling][jasper-adapter].

Snapshots are held in memory, however. On adapter restart, cleanup can remove recognizable plugin overrides but cannot reconstruct all original settings from a lost snapshot. Signals normally expire after 1,800 seconds without a refresh. Since registered events do not include every tool call, a long quiet turn can age out. The recorded PID is typically a long-lived shell, so its continued existence alone does not prove Claude is still running. There is no session-end hook to remove the signal immediately. [Staleness and cleanup][jasper-adapter].

### Dependencies, delivery and configuration

The writer reads the whole payload, uses sed-based extraction and writes its signal directly, without a temporary-file rename or session transaction lock. The adapter skips unreadable JSON. A partial write can therefore be absent from a poll and temporarily look like a removed signal; rapid state changes can also be coalesced between polls. These are implementation-level possibilities, not reproduced live-terminal defects. The loop sleeps one second after processing, so that interval is not an end-to-end latency guarantee. [Writer][jasper-hook], [polling loop][jasper-adapter].

Bootstrap reuses a suitable Python runtime, or creates one and installs the `iterm2` package, then deploys an AutoLaunch script and records a version marker. Adapter updates require a restart or toggle. Config is hot-reloaded, with environment overrides, but a source discrepancy matters: setting the signal `dir` only in JSON changes the adapter's reader path without changing the shell writer's path. The uninstall instructions also still remove an older `/tmp` signal directory rather than the current runtime/cache default. These observations are specific to the pinned revision. [Bootstrap][jasper-bootstrap], [configuration][jasper-adapter], [uninstall][jasper-uninstall].

Signals contain project/location information, terminal identity, PID, timestamps and messages. Prompt snippets are opt-in; sanitization/redaction is not a guarantee that every sensitive phrase is removed. Default directory permissions are restricted. No runtime hosted telemetry mechanism was identified in the inspected scripts, but installing Python dependencies uses the network. [Writer][jasper-hook], [configuration defaults][jasper-adapter].

MIT licensing, Ruff, ShellCheck, mocked adapter tests, shell-hook tests and plugin-structure checks provide useful project infrastructure. These tests were inspected rather than run here. Mocked iTerm2 behavior does not establish a live terminal/Claude compatibility matrix. [CI][jasper-ci], [tests][jasper-tests], [license][jasper-license].

**Useful lessons:** preserve original appearance, offer a separate status subtitle where a terminal supports it, and make acknowledgement policy explicit. The Python adapter is a reasonable terminal-specific backend choice, but would change the local project's runtime footprint.

## headsup

**Best fit:** macOS users who want terminal attention cues as part of a broader session-management workflow.

headsup includes tab colors, labels, pane badges, a context/cost/usage status bar, notifications, diagnostic helpers, Finder integration and checkpoint/resume commands. `/sfl` writes a Markdown checkpoint; `/nil` opens fresh sessions with prompts referencing those checkpoints. That is a different capability from restoring a terminal title or resuming an original transcript. The project also provides a separate Codex integration, which was inspected as project code rather than validated against a running product. [README][headsup-readme].

### Orange deliberately combines completion and waiting

The Claude integration treats `Stop`, every `Notification`, and the start of `AskUserQuestion` as waiting/orange with attention. `SessionStart` is white; prompts and ordinary tool activity become blue. It therefore cannot communicate the local project's distinction between “the turn is finished” and “a dialog requires input” through color alone. This is an attention policy, not necessarily a defect: finished output may be exactly what a user wants to be summoned to read. [Event reducer][headsup-hook].

The installed Claude hooks are `SessionStart`, `Notification`, `Stop`, `UserPromptSubmit`, `PreToolUse` and `PostToolUse`. The inspected setup does not wire dedicated permission-request, tool-failure, API-failure, session-end or elicitation events. `AskUserQuestion` is recognized separately; all notification kinds otherwise share a branch. [Setup][headsup-setup], [classifier][headsup-hook].

The wait heuristic is a marker plus an in-flight-tool counter keyed to the terminal session/pane. A late `PostToolUse` can be suppressed when waiting with no tool in flight. There is no collection of agent-owned waits. State delivery uses atomic replacement, but counter updates are separate read/modify/write operations without an observed transaction lock. Concurrent hooks can therefore lose updates in principle; that consequence was not stress-tested here. [Counter and state writer][headsup-hook].

### Strong delivery recovery is not state reconstruction

The iTerm2 Python daemon polls state files at a nominal 30 ms interval, checks its connection, writes heartbeats and reapplies known desired states every seven seconds. A launchd watchdog runs every 30 seconds to restart unhealthy delivery. Hooks also have direct-OSC and fallback paths. These are useful protections against a lost connection or terminal appearance drift. [Daemon][headsup-daemon], [watchdog][headsup-watchdog].

Reconciliation replays hook-derived state; it does not independently determine what Claude is currently doing. A missed or incorrectly interpreted transition remains wrong when replayed successfully. This differs from both local tmux expiry and the session-file polling approach discussed below. A watchdog can make an indicator reliably visible without making its meaning accurate. [Reconciliation loop][headsup-daemon].

The README's roughly 50 ms healthy-path latency and roughly 440 ms one-shot cold start are upstream-reported figures. They are not directly comparable to local binary timings. The daemon awaits updates sequentially and its apply-twice helper includes a 150 ms delay, so a nominal 30 ms poll interval does not bound the response to a burst of changes across tabs. This review ran no common-hardware benchmark. [Timing implementation][headsup-daemon].

### Installation and platform breadth require careful reading

Upstream describes macOS support for iTerm2, WezTerm and AI Power Term. In the inspected snapshot, the primary Claude status hook detects iTerm2 and WezTerm, while AI Power Term paths appear elsewhere, including the separate Codex integration and status bar. The main setup also requires iTerm2. The broader advertised platform matrix cannot therefore be established from the Claude hook and installer alone. This is a source/documentation gap, not a reproduced failure in those applications. [README][headsup-readme], [terminal detection][headsup-hook], [prerequisites][headsup-setup].

Setup has a much wider footprint than deploying one status binary: Python and its iTerm2 package, jq, Swift tooling, hook and skill files, a notifier application, launchd configuration and a Finder Quick Action. It can update itself from GitHub, enable the iTerm2 API and modify permission rules. Its hook merge replaces arrays for matching event keys rather than appending entries. The `--no-permissions` switch skips one group of helper rules, not every permission-related setup path. A disable flag exists, but no complete uninstall script was found. These are concrete integration considerations for a machine that already has hooks. [Setup][headsup-setup].

WezTerm support uses OSC user variables and a Lua tab-title handler based on the active pane. That is not equivalent to the local project's aggregate strip across all panes. The supplied Lua file also contains terminal preferences beyond the status handler, so adopting it wholesale has a larger scope than adding an isolated integration. [WezTerm configuration][headsup-wezterm].

### Data, testing and lessons

The status bar reads account and local transcript/usage information, and checkpoints persist work summaries. Quota percentages are described upstream as estimates. Config files can contain sourced shell code, rather than being passive data only. The optional Codex adapter can call an existing external coordination helper and return a blocking Stop decision, which extends beyond passive status reporting. None of those paths were installed or exercised here. [Status bar][headsup-statusbar], [usage scanner][headsup-usage], [conditional coordination][headsup-codex].

The project is MIT-licensed and has release/changelog machinery and substantial diagnostics. A detailed manual QA document was found, but no conventional automated test suite or GitHub Actions workflow was visible in the inspected tree. That is a narrower automated-regression basis than the local Rust/integration/golden suites; it does not establish that the application is unreliable. [QA material][headsup-qa], [license][headsup-license].

**Useful lessons:** clear attention policies, friendly session labels and diagnostics that distinguish state computation from terminal delivery. A daemon and workflow suite are optional product directions, not requirements for a reliable Linux tab-title tool.

## iTerm2-fancy-claude-tabs

**Best fit:** users who want their tab to follow Claude's session name and reported aggregate state, and accept a persistent watcher and dependence on Claude's session-file schema.

This is the most useful architectural alternative to compare with hook-based state reconstruction. It registers only session start and end. Start launches a Bash watcher that finds the session under `~/.claude/sessions/*.json`, then polls `status`, `name` and `cwd`. A rename appears on the next poll; an unnamed session uses its directory basename. The title identifies intent through a session name rather than identifying the Git checkout. [README][fancy-readme], [watcher][fancy-watcher], [installer][fancy-install].

### State meaning comes from the session file

| Upstream status | Display | Meaning attributed by this project |
|---|---|---|
| `idle` | Green | Ready for the next prompt |
| `busy` | Yellow | Thinking or running tools |
| `waiting` | Blue | Permission, approval or other input needed |
| `shell` | Purple | Otherwise idle with a background shell task |
| Other | Grey | Unrecognized state |

The README explicitly excludes background subagents from the purple condition. Purple therefore does not mean “any outstanding background work.” The watcher source calls `shell` undocumented. These state meanings were not independently validated against a live Claude release. [Mapping][fancy-watcher], [background-shell explanation][fancy-readme].

Reading an aggregate state avoids implementing wait ownership locally. A later file update can correct a missed intermediate observation without reconstructing the whole event history. In exchange, correctness depends on the producer's schema, update cadence and definitions. There is no independent agent-owner model, elicitation correlation or validation that the reported state matches the screen. This is a different source of truth, not an automatically superior one.

Discovery searches for a compact `sessionId` fragment using grep; legal JSON whitespace can prevent discovery. Once a file is found, fields are parsed with jq, with Python as a fallback. On the normal jq path, a missing/null status becomes idle; malformed JSON is retried. The Python fallback does not normalize an explicit null in the same way. Although the README mentions `updatedAt`, the watcher does not use it to decide whether a record is stale. [Discovery and parsing][fancy-watcher].

### Polling and lifecycle costs

Normally there is one resident watcher per session. Each normal iteration invokes a JSON parser, field-extraction utilities, `sleep` and `stat`; changed-state detection avoids repeated terminal writes but does not eliminate polling work. The default interval is one second and is configurable. Short-lived transitions can occur entirely between polls. No CPU, memory or comparative latency benchmark was run. [Polling loop][fancy-watcher].

Output is raw OSC 6 color and OSC 0 title data written to a discovered TTY. macOS/iTerm2 is the primary target. Other terminals and tmux are listed as title-compatible upstream, but there is no dedicated multiplexer routing, configuration or pane aggregation in the source. Generic OSC support should not be equated with the local project's handling of attached tmux clients and multiple panes. [Output implementation][fancy-watcher], [compatibility claims][fancy-readme].

Normal session end kills the recorded watcher, removes its PID file, resets color and clears the title. It does not restore a captured prior appearance. If the session file disappears, the watcher exits and removes its PID file without resetting the display. If a crashed Claude leaves a file behind, there is no process-identity or freshness check to terminate that watcher. Repeated starts can create multiple watchers because startup does not guard against an already-running watcher for the same session ID. These are source-derived lifecycle risks; they were not reproduced against a live terminal. [End hook][fancy-end], [watcher lifecycle][fancy-watcher].

### Installation and project evidence

The installer requires jq, copies scripts under `~/.claude/bin`, backs up and rewrites settings, removes old integration artifacts and terminates recorded watchers on upgrade. Its hook preservation works for independently grouped hooks, but it can remove a whole group containing one of its commands alongside unrelated commands. Uninstall is documented as manual removal of hooks and scripts; the steps do not explicitly stop existing watchers. [Installer][fancy-install], [uninstall instructions][fancy-readme].

The runtime reads session metadata rather than transcripts and has no network calls in the inspected scripts. Names become visible in terminal UI and screenshots. There is no explicit sanitizer for terminal control characters in the title path. The complete pinned repository contains four tracked files and no tests, CI workflow or license file. That describes available evidence, not the author's private testing history. [Complete source tree][fancy-tree].

**Useful lessons:** automatic session-name tracking and a precisely defined background-shell state. Before adopting session-file polling locally, establish schema/version compatibility, freshness handling, process identity, duplicate-watcher prevention and explicit terminal routing.

## claude-code-terminal-title

**Best fit:** users who mainly want to distinguish tasks by name and do not need a working/waiting/idle state machine.

This project combines a folder name with a task label. It offers two separate ways to choose that label: a skill asks Claude to produce a category and task summary, while an optional `UserPromptSubmit` hook uses the first line of the prompt. The skill aims to update at significant task changes; the hook updates deterministically on ordinary prompts and skips empty input and slash commands. Both call the same shell emitter. Model invocation of the skill is a behavior requested through instructions, not a deterministic scheduling guarantee. [README][title-readme], [skill][title-skill], [hook][title-hook].

It does not track tool lifecycle, permission waits, completion, agent ownership or status expiry. Its smaller implementation is therefore not an alternative implementation of the same guarantees as `claude-tabstatus`. Task identity could complement the local location/state display, but running both title writers unchanged creates competing ownership of the same terminal title.

### Delivery and parsing

The prompt hook reads its input into a shell variable, then prefers Python's JSON parser, falls back to jq, and finally uses sed. The branches are not equivalent: the Python path checks that `prompt` is a string, while a regular-expression fallback cannot fully decode arbitrary escaped JSON or enforce top-level structure. Dependency availability can change the resulting title. There is no daemon. [Hook implementation][title-hook].

The emitter writes OSC 0 to `/dev/tty`; manual auto mode falls back to stdout. The hook explicitly selects TTY-only mode and suppresses output, so it does not return `terminalSequence` to Claude. Current Claude documentation says hooks run without a controlling terminal. This makes that hook path a compatibility risk on affected versions even if the same script works manually; no live reproduction was performed here. The saved-title file is updated before terminal emission, so silent delivery failure can still change persisted state. [Emitter][title-emitter], [official terminal-output contract][claude-hooks].

Upstream identifies macOS Terminal.app with zsh setup and iTerm2 as tested. Several Linux terminals and Windows Terminal through WSL are described as expected to work. Native Windows support is not in this revision. The emitter contains no dedicated tmux or screen routing logic, regardless of their appearance in compatibility descriptions. These platform claims were not independently exercised. [Compatibility table][title-readme].

### Persistence does not isolate sessions

Each nonempty title update attempts to atomically replace the same `~/.claude/terminal_title` file. Atomic replacement prevents a partially written title from being read, but does not assign that title to a session, TTY or project. Direct emission still targets the invoking terminal; the shared file does not instantly retitle every open tab. [Persistence][title-emitter].

The optional zsh integration makes cross-session mixing possible: it rereads the shared file at each prompt. New shells adopt a recent title, while a shell that has already set its claimed-title flag continues using the current shared file. If another session replaces that file, the first shell can pick up the second session's title. The freshness window does not establish ownership. For a multi-session tool, any borrowed task-label feature should instead live in the per-session record and feed one renderer. [zsh integration][title-zsh].

### Text handling, installation and tests

Task text and the optional prefix have basic control bytes removed, then are truncated using byte limits. Those limits can split UTF-8 despite being described as character caps. The folder basename is added afterward without equivalent sanitization. This is source-observed behavior, not a verified terminal exploit. The deterministic hook intentionally exposes a prompt excerpt in the title and on disk, which is a different privacy choice from location/status alone. [Emitter][title-emitter], [prompt extraction][title-hook].

Installation copies the skill and runs title smoke checks. Enabling the deterministic hook and disabling Claude's own title updates are manual configuration steps. Optional zsh setup asks for confirmation, backs up `.zshrc`, installs prompt logic and can change Terminal.app profile title settings. Removal offers to undo these changes, but restores three Terminal.app values to `true` rather than recovering recorded originals. Persisting the title after Claude exits is a feature here; there is no session-end restoration. [Installer][title-install], [zsh setup][title-zsh], [uninstaller][title-uninstall].

The MIT-licensed project has shell tests for composition, basic sanitization, length handling and prompt parsing. Ubuntu CI runs them with ShellCheck and verifies that the packaged skill matches its source. Those checks do not establish macOS UI behavior, per-session persistence or every parser fallback. The suite was inspected, not executed, during this comparison. [Tests][title-tests], [CI][title-ci], [license][title-license].

**Useful lessons:** offer task identity as an optional complement to location, distinguish deterministic prompt excerpts from model-generated summaries, and sanitize complete display text in one place. Avoid a shared last-title file when concurrent sessions are a core use case.

## Cross-project conclusions

### The meaning of a state matters more than the number of colors

| Question | Important distinctions |
|---|---|
| Does a finished turn need attention? | headsup says yes and uses its waiting color. claude-tabstatus and JasperSui distinguish idle from waiting. wt-tab-status uses a distinct done state. |
| Does background work keep the session busy? | wt-tab-status keeps detected agent work active. claude-tabstatus can show an idle main loop while subagents run, unless a wait remains. Fancy tabs reserve purple for upstream background-shell state, not subagents. |
| Does looking at the tab resolve attention? | JasperSui clears attention on focus. The local project aims to clear a wait through event/state evidence. Those are different contracts. |
| Does an expired indicator mean the task ended? | No. Local tmux expiry and JasperSui signal expiry are aging policies, not proof of completion. |
| Does the title identify work or location? | The local project emphasizes repository/branch/host; fancy tabs follow a session name; the title skill derives task text; wt-tab-status preserves Claude's topic title. |

A useful product specification should state these choices before adding more states. In particular, “idle” could mean the main conversation is free, every child task is finished, or the user has acknowledged the result. Treating those as synonyms creates false comparisons and confusing tests.

### Three architectures have different failure boundaries

| Architecture | Projects | Strength | Main failure boundary |
|---|---|---|---|
| Reconstruct state from hooks and write the terminal | claude-tabstatus, wt-tab-status | Prompt event-driven response; no dedicated watcher | Missing, late, misattributed or overlapping events can leave incorrect state |
| Hooks write desired state; a terminal adapter renders it | JasperSui, headsup | Native appearance APIs and focus handling; delivery recovery varies by implementation | Polling/writer races and adapter failure; where replay exists, it cannot repair wrong desired state |
| Poll producer-maintained aggregate state | Fancy tabs | Natural session renaming and correction on later snapshots | Producer schema/semantics, stale files, polling delay and lifecycle cleanup |

The task-title project is outside this state architecture comparison: it chooses a label and optionally reapplies it at shell prompts.

A future local reconciliation path could combine hook responsiveness with a periodically checked aggregate state, but that would introduce an additional compatibility contract and conflict-resolution policy. It should be justified by measured missed-event cases. Adding a watcher without deciding which source wins would merely create two competing answers.

### Multiplexer and remote support require more than OSC compatibility

“Supports tmux” can mean at least three different things: a title escape is accepted by a pane, a useful title reaches the outer terminal, or the outer title summarizes every relevant pane. Only the last describes the local aggregation feature. Neither generic OSC emission nor an active-pane user variable establishes equivalent behavior.

For SSH, a hook writing a remote signal file cannot automatically reach a desktop-local Python adapter. PTY-directed escape sequences can travel along the terminal connection, but terminal-specific controls still need correct routing and detection. The local project explicitly handles Konsole identification and tmux client routing, with Linux/PTY and timer constraints. Treating these as a separate backend capability is more informative than a single portability checkbox. [Local delivery][local-emit], [local multiplexer implementation][local-tmux].

### Latency figures need three separate measurements

1. **Event availability:** how long before Claude emits the event that identifies the change. A fast hook cannot compensate for waiting on a delayed notification instead of observing the initial permission request.
2. **Processing and transport:** payload reading, state update, hook startup, subprocesses, polling and terminal API calls.
3. **Visible rendering:** when the emulator actually repaints its tab. A microsecond-scale runtime improvement can be irrelevant beside a terminal repaint interval.

The local source reports sub-millisecond binary/hook measurements in particular environments. wt-tab-status claims a few milliseconds; headsup reports separate cold-start and daemon figures; JasperSui and fancy tabs use polling intervals around one second. Those are different workloads on different platforms. No “N times faster” claim follows from them. [Local benchmark harness][local-bench], [wt documentation][wt-readme], [headsup documentation][headsup-readme], [adapter loop][jasper-adapter], [watcher loop][fancy-watcher].

Even local benchmark labels need care: the state harness seeds a record before a repeated invocation loop, so a nominal transition arm mostly observes the steady state after its first transition. It is useful for comparing the overall path, but should not be presented as measuring a fresh write on every iteration. A comparative benchmark should replay defined event sequences, separate no-op/read/write cases, vary payload sizes and concurrent sessions, and report percentile/spread data. Visible terminal latency should be measured separately on each supported platform. [Harness implementation][local-bench].

### Installation and removal are part of the feature

The local move to a generated plugin directory makes deployment explicit and keeps source edits out of live hook paths. wt-tab-status delegates deployment to Claude's plugin marketplace. JasperSui adds a terminal-managed Python runtime and AutoLaunch adapter. headsup changes a wider set of desktop, permission and workflow integrations. Fancy tabs rewrite settings and manage watcher scripts. The task-title project optionally changes shell startup and Terminal.app profile settings.

These are different operational commitments, not just different command counts. A comparison should ask whether existing hooks survive, whether an update affects active sessions, whether removal stops resident processes, and whether previous appearance/configuration is restored exactly or reset to defaults. The per-project sections identify source-observed limitations; the absence of an installer here would not make a project automatically safer or easier to operate.

### Displaying more context changes what is exposed

Status plus location reveals repository/branch/host information. Task titles can reveal prompt or task text. Toasts can retain commands and answer excerpts in OS notification history. Workflow status bars and checkpoint files can expose account, usage and work-summary information. These are useful features with different data footprints, even when runtime processing stays local.

The local project's bounded payload reader and minimal state record avoid persisting tool bodies for its core status feature. That is a useful property to preserve if names, notifications or richer summaries are added. A single complete-title sanitization boundary and explicit opt-in rules for prompt-derived text would make those additions easier to reason about. The existing [security review](https://github.com/dalf/claude-tabstatus/issues/4) remains relevant; zero third-party crates does not make parsing or terminal output automatically correct.

## Verification and maintenance evidence

| Project | Repository evidence inspected | What this review actually established |
|---|---|---|
| claude-tabstatus | Rust tests, shell integration tests, golden corpus, Linux GNU/musl CI, tested-artifact release workflow | Earlier isolated working snapshot: 168 unit tests and 312 corpus cases passed. Integration run: 585 passed and 4 failed around the active installer work and temporary-copy setup; this is not a fully green release certification. |
| wt-tab-status | Shell tests, concurrency cases, optional PowerShell tests, ShellCheck and manifest CI | 50 passed, one environment-dependent PTY test failed because `script` was unavailable; PowerShell tests skipped. Lock-timeout behavior separately reproduced against a fake output target. |
| JasperSui | Mocked adapter tests, shell-hook and structure tests, linting and release automation | Source/CI inspection only; no live iTerm2 trial or test-suite execution. |
| headsup | Diagnostics, release tooling and detailed manual QA material | Source inspection only; no conventional automated test suite or Actions workflow found in the pinned tree. |
| Fancy tabs | Four-file implementation | Source inspection only; no tracked tests or CI found. |
| Task-title project | Shell tests, ShellCheck, packaged-skill consistency checks | Source/CI inspection only; no live terminal trial or test-suite execution. |

The local test figures are from the preceding repository review in this conversation, not a fresh run after every concurrent installer edit. Existing golden tests preserve behavior inherited from an older implementation; they are valuable regression evidence, but cannot independently prove that every inherited behavior is desirable. The reproduced nested-field, unknown-owner and whitespace cases demonstrate that distinction.

CI inspection establishes configured checks, not that the most recent remote run passed. Unit-test counts also do not measure comparative product maturity: a native terminal integration may depend heavily on UI behavior that mocks cannot cover. The strongest next step for all state-oriented approaches is a shared corpus of sanitized, versioned event/session traces with explicit expected user-visible states.

The inspected licenses are GPL-3.0-or-later for this repository and MIT for wt-tab-status, JasperSui, headsup and the task-title project. No license file was present in the pinned fancy-tabs tree. These are source metadata observations, not a legal assessment. [Local manifest][local-cargo], [wt license][wt-license], [JasperSui license][jasper-license], [headsup license][headsup-license], [task-title license][title-license], [fancy-tabs tree][fancy-tree].

## Priorities suggested by the comparison

1. **Make the existing state contract dependable.** Resolve top-level metadata extraction and whitespace together (#5/#7), then the unknown-owner retirement rules (#6). Test nested tool data, two live waits, out-of-order completions and unrelated subagent exits before expanding presentation features.
2. **Add direct elicitation lifecycle coverage with correlation.** wt-tab-status demonstrates the coverage benefit, but its unconditional completion branch is not a sufficient model for overlapping requests. Preserve request identity where available, and specify what happens when that identity is absent. [Official elicitation-result fields][claude-elicitation].
3. **Document idle, background work and attention explicitly.** Decide whether an idle main loop with running subagents is correctly white. Consider a separate background-work state only if users need that distinction; do not copy purple without its precise semantics.
4. **Preserve appearance where the backend can support it.** JasperSui demonstrates the value of retaining an original title/color policy. For Konsole, distinguish true restoration from stock defaults and retain a clear fallback when prior settings cannot be read reliably.
5. **Consider optional session names or task labels within the existing renderer.** Fancy tabs and the title skill address multiple tasks in one checkout better than a location alone. Keep labels per session, sanitize every component and avoid competing OSC writers or a global last-title file.
6. **Publish an evidence-based compatibility table and short entry-point guide.** Separate tested Linux targets from expected OSC compatibility, show the no-state-directory fallback, list the tmux timer prerequisites, and keep extensive measurements/design history in supporting documents.
7. **Treat additional attention channels as backend-specific options.** Windows taskbar progress, iTerm2 badges and desktop toasts can be useful, but add state, restoration and data-retention obligations. Their value should be evaluated independently from the core title/status path.

The most defensible direction for this repository is to strengthen its existing Linux/SSH/tmux behavior and state accuracy. Its combination of one executable, per-session wait ownership, location labels and multiplexer aggregation is distinctive among these five projects. The comparison supports borrowing specific capabilities while retaining that deployment model; it does not establish a need to turn the project into a desktop workflow suite.

## Source register

All external implementation links in this document refer to these fixed revisions. Recheck them before making decisions based on a newer release.

| Repository | Reviewed revision | Version evidence |
|---|---|---|
| dalf/claude-tabstatus | [`50bf014f727e441c080edf8393e3d555d8b1c6c7`](https://github.com/dalf/claude-tabstatus/tree/50bf014f727e441c080edf8393e3d555d8b1c6c7), plus explicitly identified installer working-tree changes | Cargo/plugin 0.1.0 |
| Yannis-Adn/terminal-addons | [`9e0f317237bad0759d558bc1d0c744cdd805a613`](https://github.com/Yannis-Adn/terminal-addons/tree/9e0f317237bad0759d558bc1d0c744cdd805a613) | wt-tab-status manifest 0.3.0 |
| JasperSui/claude-code-iterm2-tab-status | [`bd5ed6c7b2b5544a7bbf7d1454964ac66802767f`](https://github.com/JasperSui/claude-code-iterm2-tab-status/tree/bd5ed6c7b2b5544a7bbf7d1454964ac66802767f) | Plugin manifest 0.5.0; Python metadata still 0.2.0 |
| wasulajr/headsup | [`65397878a4740ff8eeeaae01a6b94255ab030665`](https://github.com/wasulajr/headsup/tree/65397878a4740ff8eeeaae01a6b94255ab030665) | VERSION 0.4.1 |
| j-mccarthy-veeam/iTerm2-fancy-claude-tabs | [`8efa6e387d15212ce8309e03b7112880ca72993c`](https://github.com/j-mccarthy-veeam/iTerm2-fancy-claude-tabs/tree/8efa6e387d15212ce8309e03b7112880ca72993c) | Compared by commit, not an assumed release |
| bluzername/claude-code-terminal-title | [`d3128cd797b4978c1f206481f256915abbda94cc`](https://github.com/bluzername/claude-code-terminal-title/tree/d3128cd797b4978c1f206481f256915abbda94cc) | Compared by commit, not an assumed release |

Claude's official hook documentation was consulted on the research date. Unlike the repository permalinks, those documentation URLs are moving references. Local relative links to installation files likewise follow this checkout; they identify the in-progress installation design rather than pretending it is part of the pinned runtime revision.

[local-hooks]: https://github.com/dalf/claude-tabstatus/blob/50bf014f727e441c080edf8393e3d555d8b1c6c7/hooks/hooks.json
[local-edge]: https://github.com/dalf/claude-tabstatus/blob/50bf014f727e441c080edf8393e3d555d8b1c6c7/src/edge.rs
[local-state]: https://github.com/dalf/claude-tabstatus/blob/50bf014f727e441c080edf8393e3d555d8b1c6c7/src/state.rs
[local-emit]: https://github.com/dalf/claude-tabstatus/blob/50bf014f727e441c080edf8393e3d555d8b1c6c7/src/emit.rs
[local-cargo]: https://github.com/dalf/claude-tabstatus/blob/50bf014f727e441c080edf8393e3d555d8b1c6c7/Cargo.toml
[local-build]: https://github.com/dalf/claude-tabstatus/blob/50bf014f727e441c080edf8393e3d555d8b1c6c7/scripts/build.sh
[local-tmux]: https://github.com/dalf/claude-tabstatus/blob/50bf014f727e441c080edf8393e3d555d8b1c6c7/src/tmux.rs
[local-bench]: https://github.com/dalf/claude-tabstatus/blob/50bf014f727e441c080edf8393e3d555d8b1c6c7/scripts/bench-state.sh
[wt-readme]: https://github.com/Yannis-Adn/terminal-addons/blob/9e0f317237bad0759d558bc1d0c744cdd805a613/README.md
[wt-script]: https://github.com/Yannis-Adn/terminal-addons/blob/9e0f317237bad0759d558bc1d0c744cdd805a613/plugins/wt-tab-status/scripts/tab-status.sh
[wt-hooks]: https://github.com/Yannis-Adn/terminal-addons/blob/9e0f317237bad0759d558bc1d0c744cdd805a613/plugins/wt-tab-status/hooks/hooks.json
[wt-toast]: https://github.com/Yannis-Adn/terminal-addons/blob/9e0f317237bad0759d558bc1d0c744cdd805a613/plugins/wt-tab-status/scripts/toast.ps1
[wt-tests]: https://github.com/Yannis-Adn/terminal-addons/blob/9e0f317237bad0759d558bc1d0c744cdd805a613/plugins/wt-tab-status/tests/run.sh
[wt-ci]: https://github.com/Yannis-Adn/terminal-addons/blob/9e0f317237bad0759d558bc1d0c744cdd805a613/.github/workflows/ci.yml
[wt-license]: https://github.com/Yannis-Adn/terminal-addons/blob/9e0f317237bad0759d558bc1d0c744cdd805a613/LICENSE
[jasper-readme]: https://github.com/JasperSui/claude-code-iterm2-tab-status/blob/bd5ed6c7b2b5544a7bbf7d1454964ac66802767f/README.md
[jasper-bootstrap]: https://github.com/JasperSui/claude-code-iterm2-tab-status/blob/bd5ed6c7b2b5544a7bbf7d1454964ac66802767f/scripts/bootstrap.sh
[jasper-adapter]: https://github.com/JasperSui/claude-code-iterm2-tab-status/blob/bd5ed6c7b2b5544a7bbf7d1454964ac66802767f/scripts/claude_tab_status.py
[jasper-hooks]: https://github.com/JasperSui/claude-code-iterm2-tab-status/blob/bd5ed6c7b2b5544a7bbf7d1454964ac66802767f/hooks/hooks.json
[jasper-hook]: https://github.com/JasperSui/claude-code-iterm2-tab-status/blob/bd5ed6c7b2b5544a7bbf7d1454964ac66802767f/scripts/hook.sh
[jasper-uninstall]: https://github.com/JasperSui/claude-code-iterm2-tab-status/blob/bd5ed6c7b2b5544a7bbf7d1454964ac66802767f/commands/uninstall.md
[jasper-ci]: https://github.com/JasperSui/claude-code-iterm2-tab-status/blob/bd5ed6c7b2b5544a7bbf7d1454964ac66802767f/.github/workflows/ci.yml
[jasper-license]: https://github.com/JasperSui/claude-code-iterm2-tab-status/blob/bd5ed6c7b2b5544a7bbf7d1454964ac66802767f/LICENSE
[jasper-tests]: https://github.com/JasperSui/claude-code-iterm2-tab-status/tree/bd5ed6c7b2b5544a7bbf7d1454964ac66802767f/tests
[headsup-readme]: https://github.com/wasulajr/headsup/blob/65397878a4740ff8eeeaae01a6b94255ab030665/README.md
[headsup-hook]: https://github.com/wasulajr/headsup/blob/65397878a4740ff8eeeaae01a6b94255ab030665/hooks/headsup-status.sh
[headsup-setup]: https://github.com/wasulajr/headsup/blob/65397878a4740ff8eeeaae01a6b94255ab030665/setup.sh
[headsup-daemon]: https://github.com/wasulajr/headsup/blob/65397878a4740ff8eeeaae01a6b94255ab030665/hooks/iterm2-daemon.py
[headsup-watchdog]: https://github.com/wasulajr/headsup/blob/65397878a4740ff8eeeaae01a6b94255ab030665/hooks/headsup-watchdog.sh
[headsup-wezterm]: https://github.com/wasulajr/headsup/blob/65397878a4740ff8eeeaae01a6b94255ab030665/wezterm/wezterm.lua
[headsup-statusbar]: https://github.com/wasulajr/headsup/blob/65397878a4740ff8eeeaae01a6b94255ab030665/hooks/headsup-context-bar.sh
[headsup-usage]: https://github.com/wasulajr/headsup/blob/65397878a4740ff8eeeaae01a6b94255ab030665/hooks/headsup-usage-windows.py
[headsup-codex]: https://github.com/wasulajr/headsup/blob/65397878a4740ff8eeeaae01a6b94255ab030665/hooks/headsup-codex-status.sh
[headsup-qa]: https://github.com/wasulajr/headsup/blob/65397878a4740ff8eeeaae01a6b94255ab030665/qa-tests/test-headsup45-codex-stop-timeout.md
[headsup-license]: https://github.com/wasulajr/headsup/blob/65397878a4740ff8eeeaae01a6b94255ab030665/LICENSE
[fancy-readme]: https://github.com/j-mccarthy-veeam/iTerm2-fancy-claude-tabs/blob/8efa6e387d15212ce8309e03b7112880ca72993c/README.md
[fancy-watcher]: https://github.com/j-mccarthy-veeam/iTerm2-fancy-claude-tabs/blob/8efa6e387d15212ce8309e03b7112880ca72993c/bin/claude-tab-updater.sh
[fancy-install]: https://github.com/j-mccarthy-veeam/iTerm2-fancy-claude-tabs/blob/8efa6e387d15212ce8309e03b7112880ca72993c/install.sh
[fancy-end]: https://github.com/j-mccarthy-veeam/iTerm2-fancy-claude-tabs/blob/8efa6e387d15212ce8309e03b7112880ca72993c/bin/claude-tab-end.sh
[fancy-tree]: https://github.com/j-mccarthy-veeam/iTerm2-fancy-claude-tabs/tree/8efa6e387d15212ce8309e03b7112880ca72993c
[title-readme]: https://github.com/bluzername/claude-code-terminal-title/blob/d3128cd797b4978c1f206481f256915abbda94cc/README.md
[title-skill]: https://github.com/bluzername/claude-code-terminal-title/blob/d3128cd797b4978c1f206481f256915abbda94cc/skill/terminal-title/SKILL.md
[title-hook]: https://github.com/bluzername/claude-code-terminal-title/blob/d3128cd797b4978c1f206481f256915abbda94cc/skill/terminal-title/hooks/set-title-hook.sh
[title-emitter]: https://github.com/bluzername/claude-code-terminal-title/blob/d3128cd797b4978c1f206481f256915abbda94cc/skill/terminal-title/scripts/set_title.sh
[title-zsh]: https://github.com/bluzername/claude-code-terminal-title/blob/d3128cd797b4978c1f206481f256915abbda94cc/setup-zsh.sh
[title-install]: https://github.com/bluzername/claude-code-terminal-title/blob/d3128cd797b4978c1f206481f256915abbda94cc/install-and-test.sh
[title-uninstall]: https://github.com/bluzername/claude-code-terminal-title/blob/d3128cd797b4978c1f206481f256915abbda94cc/uninstall.sh
[title-tests]: https://github.com/bluzername/claude-code-terminal-title/blob/d3128cd797b4978c1f206481f256915abbda94cc/tests/run.sh
[title-ci]: https://github.com/bluzername/claude-code-terminal-title/blob/d3128cd797b4978c1f206481f256915abbda94cc/.github/workflows/ci.yml
[title-license]: https://github.com/bluzername/claude-code-terminal-title/blob/d3128cd797b4978c1f206481f256915abbda94cc/skill/terminal-title/LICENSE
[claude-hooks]: https://code.claude.com/docs/en/hooks#emit-terminal-notifications
[claude-elicitation]: https://code.claude.com/docs/en/hooks#elicitationresult
