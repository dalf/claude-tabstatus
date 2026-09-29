#!/bin/sh
# The hot-path budget, enforced against a baseline binary instead of remembered.
#
#   sh scripts/bench-hot.sh                        bin/tabstatus against ITSELF
#   sh scripts/bench-hot.sh <baseline-binary>      ...against a pre-refactor binary
#   CCTAB_BENCH=1 sh tests/run.sh                  the suite runs it, self against self
#   CCTAB_BENCH_EXECS=400 CCTAB_BENCH_ROUNDS=15 sh scripts/bench-hot.sh /tmp/old
#
# WHY THIS EXISTS. The backend design rejected a vtable, a capability registry, a
# runtime probe per paint and a zbus dependency, and every one of those rejections
# was argued on the hot path's budget - yet nothing in the tree CHECKED that budget
# after a refactor. A design whose central constraint is unmeasured is a design that
# regresses on the first commit nobody times, and eleven commits are coming.
#
# THE BUDGET, with the right numbers, because they have been misquoted. `0.37ms` in
# src/tmux.rs is the FORK FLOOR - what it costs this machine to start any process at
# all - not what this binary costs. A `working` edge self-times at ~460us (README's
# measured table) and a real hook costs 767us end to end, the difference being the
# fork Claude Code pays to launch us. So the figure a change is spent against is
# ~460us, the figure a user feels is 767us, and the budget is per EDGE: a fork on a
# per-tool-call edge is not a percentage, it is a doubling.
#
# THE METHOD IS scripts/bench-state.sh's, deliberately unchanged, because it was
# built to survive a desktop that is also doing something else: ROUNDS passes, every
# arm timed once per pass so load that arrives mid-run hits all of them, the arm's
# figure is the MINIMUM over rounds - the round least disturbed - the spread is
# shown so a reader can see how far the machine moved, and the paired delta is the
# MEDIAN of per-round deltas rather than a difference of two minima. Read that file
# for why each of those is there; this one adds only what a GATE needs.
#
# WHAT A GATE NEEDS is a noise band, and a band that is guessed is a band that
# passes everything. This one is measured by the same script: with no baseline
# argument both arms of every pair run the SAME binary, so every delta it prints is
# pure NOISE and nothing else. Twelve such null runs on this machine at the default
# 200 execs x 9 rounds - 48 paired edges - came out at:
#
#   worst positive  +15us (working)      44 of the 48 within +-10us
#   worst negative  -23us (idle)
#
# So the band is 50us, twice the worst null delta observed, which is the headroom a
# machine under more load than this one will need. That is 11% of the ~460us edge, so
# it cannot see an added indirection - and it is not meant to. What the design
# rejected mechanisms for is the SHAPE of their cost, and every one of those shapes is
# several times this band: a fork is 370us, the hand-rolled D-Bus Notify 255us, one
# tmux round trip 2.84ms. A regression small enough to hide here is a regression no
# rejection ever rested on.
#
# AND THE HALF THAT IS NOT A TIMING AT ALL. tests/run.sh's tmux section asserts that
# "the hot edges exec no tmux at all", by reading the server back and finding it
# unconfigured. That is the right idea aimed at one process: what the budget actually
# requires is that the hot edges fork NOTHING, tmux, git, hostname or otherwise, and
# a fork is observable directly. This runs every hot edge under a tracer and requires
# zero clone/clone3/vfork/execve beyond the binary's own exec, in two environments -
# bare, and tmux-shaped, which is where run.sh's check lives and where the risk is.
# The positive control matters more than the assertion: a tracer that sees nothing
# passes a binary that forks, so the tracer is first made to catch a fork it MUST
# catch, and a control that comes back clean fails this script. Measured here on this
# binary, same tracer, same tmux-shaped environment: `session-start` comes back with
# one hit and `working` with none, which is what makes the zero a measurement rather
# than a tautology.
set -eu

here=$(cd -- "$(dirname -- "$0")/.." && pwd) || exit 1
cd -- "$here"

NEW=${CCTAB_BENCH_BIN:-$here/bin/tabstatus}
BASE=${1-}
EXECS=${CCTAB_BENCH_EXECS:-200}
ROUNDS=${CCTAB_BENCH_ROUNDS:-9}
BAND=${CCTAB_BENCH_BAND:-50}

[ -x "$NEW" ] || { printf 'error: %s is not executable\n' "$NEW" >&2; exit 1; }
# Self against self is the null run, and it is also what the suite runs: it still
# gates the forks, and a band violation in it means the machine is too loaded to
# have measured anything, which is worth knowing before a real comparison.
null=
if [ -z "$BASE" ]; then
    BASE=$NEW
    null=yes
elif [ ! -x "$BASE" ]; then
    printf 'error: baseline %s is not executable\n' "$BASE" >&2
    exit 1
fi

W=$(mktemp -d) || exit 1
trap 'rm -rf "$W"' EXIT
trap 'rm -rf "$W"; exit 130' HUP INT TERM
mkdir -p "$W/work"
SID=bench000-1111-2222-3333-444455556666

# The payloads. The 2 KB tool_response is bench-state.sh's, for the same reason: a
# hot edge measured on a ten-byte payload is measured on a payload no turn sends.
blob=$(awk 'BEGIN{while(i++<2048)printf "z"}' </dev/null)
printf '{"session_id":"%s","hook_event_name":"PostToolUse","tool_name":"Read","tool_response":"%s"}\n' \
    "$SID" "$blob" >"$W/p.working"
printf '{"session_id":"%s","hook_event_name":"PermissionRequest","tool_name":"Bash"}\n' \
    "$SID" >"$W/p.waiting"
printf '{"session_id":"%s","hook_event_name":"Stop","last_assistant_message":"%s","background_tasks":[]}\n' \
    "$SID" "$blob" >"$W/p.idle"
printf '{"session_id":"%s","hook_event_name":"Notification","notification_type":"permission_prompt","message":"Claude needs your permission"}\n' \
    "$SID" >"$W/p.notify"

# time_arm <bin> <edge> -- mean microseconds per exec.
#
# ONE fork per iteration, which is why the environment is exported around the loop
# rather than passed through `env` inside it: bench-state.sh measured a subshell plus
# an `env` per exec at two extra forks and an 1100us floor under a 500us figure.
#
# No state directory, in either arm. That is the configuration the 312-case corpus
# and every run.sh case run under, and it is the only one a pre-refactor baseline and
# a candidate are guaranteed to agree about; what the state layer itself costs is
# bench-state.sh's question and it answers it per edge.
time_arm() {
    _bin=$1; _edge=$2
    unset XDG_RUNTIME_DIR CCTAB_STATE_DIR
    HOME=$W CCTAB_DRY_RUN=1 CCTAB_NOW=1000000 CLAUDE_PID=
    export HOME CCTAB_DRY_RUN CCTAB_NOW CLAUDE_PID
    cd -- "$W/work" || exit 1
    _t0=$(date +%s%N)
    _i=0
    while [ "$_i" -lt "$EXECS" ]; do
        _i=$((_i + 1))
        "$_bin" "$_edge" <"$W/p.$_edge" >/dev/null 2>&1
    done
    _t1=$(date +%s%N)
    cd -- "$here" || exit 1
    echo $(( (_t1 - _t0) / 1000 / EXECS ))
}

edges='working waiting idle notify'
arms=''
for e in $edges; do arms="$arms base-$e new-$e"; done

# ---- the tracer ------------------------------------------------------------
#
# strace is the tool the assertion is written for and it is not installed on the
# machine this was developed on, so a bare `command -v strace` would have made the
# whole check silently vacuous exactly where it was supposed to run. gdb's fork,
# vfork and exec catchpoints answer the same question, and they do catch what musl's
# `posix_spawn` actually issues - a `clone(CLONE_VM|CLONE_VFORK)`, which arrives as
# the vfork catchpoint and not the fork one. gdb stops at the FIRST hit, so it
# counts "at least one" rather than a total; for an assertion of ZERO that is the
# same statement. CCTAB_BENCH_TRACER forces one of strace|gdb|none, which is how the
# none path gets exercised rather than assumed.
tracer=${CCTAB_BENCH_TRACER:-auto}
if [ "$tracer" = auto ]; then
    if command -v strace >/dev/null 2>&1; then
        tracer=strace
    elif command -v gdb >/dev/null 2>&1; then
        tracer=gdb
    else
        tracer=none
    fi
fi

# forks_of <stdin-file> <program> [args...] -- how many times it forked or execed,
# not counting its own exec, or `?` when the tracer could not tell.
#
# `?` is the answer that matters. A tracer that never got the program running says
# "zero forks" just as loudly as a clean run does, and the first draft of this script
# passed a shell-script wrapper that forks on every invocation - gdb had refused it
# with "not in executable format" and the count came back 0. So every arm here proves
# the program RAN before it reports a zero, and `?` fails the caller.
forks_of() {
    _in=$1
    shift
    case $tracer in
    strace)
        strace -f -e trace=clone,clone3,vfork,execve -o "$W/trace" -- "$@" \
            <"$_in" >/dev/null 2>&1 || :
        # -f prefixes a pid once more than one process is traced, so the pid is
        # optional in the pattern. The one line that is ALLOWED is the program's own
        # execve, which strace logs before the program has run a byte - so its
        # absence is not a clean run, it is no run at all.
        _n=$(grep -cE '^([0-9]+[ \t]+)?(clone|clone3|vfork|execve)\(' "$W/trace" 2>/dev/null || :)
        if [ "${_n:-0}" -lt 1 ]; then echo '?'; else echo $(( _n - 1 )); fi
        ;;
    gdb)
        printf 'set debuginfod enabled off\nset confirm off\nset startup-with-shell off\ncatch fork\ncatch vfork\ncatch exec\nrun\n' >"$W/gdb.cmds"
        # -x, not -ex: gdb in batch mode reads commands from stdin too, and stdin
        # here belongs to the program being traced.
        gdb -batch -nx -x "$W/gdb.cmds" --args "$@" <"$_in" >"$W/trace" 2>&1 || :
        _n=$(grep -cE "Catchpoint [0-9]+ \((forked|vforked) process|Catchpoint [0-9]+ \(exec'd" \
            "$W/trace" 2>/dev/null || :)
        # The hit is read BEFORE the proof of running, and in that order on purpose:
        # a caught fork leaves gdb stopped at the catchpoint and batch mode then kills
        # the inferior, so there is no exit line to find. Only a count of zero has to
        # earn it.
        if [ "${_n:-0}" -gt 0 ]; then
            echo "$_n"
        elif grep -q '^\[Inferior 1 (process' "$W/trace"; then
            echo 0
        else
            echo '?'
        fi
        ;;
    *)
        echo '?'
        ;;
    esac
}

# ---- the report ------------------------------------------------------------
printf 'bench-hot: %s execs x %s interleaved rounds, band %sus\n' "$EXECS" "$ROUNDS" "$BAND"
printf '  candidate %s\n' "$NEW"
if [ -n "$null" ]; then
    printf '  baseline  the same binary - this is the NULL run that measures the band\n'
else
    printf '  baseline  %s\n' "$BASE"
fi
printf '\n'

: >"$W/samples"
r=0
while [ "$r" -lt "$ROUNDS" ]; do
    r=$((r + 1))
    for a in $arms; do
        case $a in
        base-*) printf '%s %s\n' "$a" "$(time_arm "$BASE" "${a#base-}")" >>"$W/samples" ;;
        new-*)  printf '%s %s\n' "$a" "$(time_arm "$NEW"  "${a#new-}")"  >>"$W/samples" ;;
        esac
    done
    printf '.' >&2
done
printf '\n\n' >&2

# min over rounds per arm, spread, median of the per-round paired deltas, and the
# gate, which is on the MINIMA: the least disturbed round of each arm is the closest
# either arm gets to the cost of the code, and the median delta is printed beside it
# as the second opinion rather than as the verdict.
gate=ok
if awk -v edges="$edges" -v band="$BAND" '
{ v[$1] = v[$1] " " $2 }
function stats(arm, out) {
    split(v[arm], s, " ")
    out["min"] = s[1]; out["max"] = s[1]
    for (j in s) {
        if (s[j] + 0 < out["min"]) out["min"] = s[j]
        if (s[j] + 0 > out["max"]) out["max"] = s[j]
    }
}
END {
    n = split(edges, e, " ")
    printf "%-10s %9s %9s %9s %9s %11s %9s\n", \
        "edge", "base us", "new us", "delta", "spread", "median pair", "verdict"
    bad = 0
    for (i = 1; i <= n; i++) {
        stats("base-" e[i], b); stats("new-" e[i], w)
        split(v["base-" e[i]], bs, " "); split(v["new-" e[i]], ws, " ")
        k = 0
        for (j = 1; j in ws; j++) { if (j in bs) d[++k] = ws[j] - bs[j] }
        for (x = 1; x <= k; x++) for (y = x + 1; y <= k; y++) if (d[y] < d[x]) { t = d[x]; d[x] = d[y]; d[y] = t }
        med = (k % 2) ? d[(k + 1) / 2] : int((d[k / 2] + d[k / 2 + 1]) / 2)
        delete d
        delta = w["min"] - b["min"]
        verd = (delta > band) ? "OVER" : "ok"
        if (delta > band) bad = 1
        printf "%-10s %9d %9d %+9d %9d %+11d %9s\n", \
            e[i], b["min"], w["min"], delta, w["max"] - w["min"], med, verd
    }
    printf "\ndelta is new min minus base min; median pair is the median of the\n"
    printf "per-round paired deltas. OVER means delta exceeds the %dus band.\n", band
    exit bad
}' "$W/samples"; then
    :
else
    gate=fail
fi

# ---- zero forks on the hot edges -------------------------------------------
printf '\n'
if [ "$tracer" = none ]; then
    printf 'SKIP  the zero-fork assertion: no strace and no gdb on PATH.\n'
    printf '      The timings above still hold; nothing checked the forks.\n'
else
    # The control. `sh -c` with three commands forks for the first two whatever the
    # shell is, and if the tracer cannot see THAT it cannot see anything, so a clean
    # control is a failure of this script and not a pass for the binary.
    ctl=$(forks_of /dev/null /bin/sh -c '/bin/true; /bin/true; :')
    # A string compare, not `-lt`: `?` is one of the answers and arithmetic on it
    # would be an error message where a verdict belongs.
    case $ctl in
    0 | '?' | '')
        printf 'FAIL  the %s control answered "%s" for a shell that forks twice;\n' \
            "$tracer" "$ctl"
        printf '      the zero-fork assertion below would have been vacuous.\n'
        gate=fail
        ;;
    *)
        printf 'tracer %s, control saw %s fork(s) in a shell that forks twice.\n' "$tracer" "$ctl"
        for e in $edges; do
            # Bare, then tmux-shaped. No CCTAB_DRY_RUN: the short-circuit returns
            # before the emit, and the emit is where a fork would be. The socket is
            # a path with no server, which is what a hot edge that reached for tmux
            # would fail on - after forking, which is the thing being counted.
            # A subshell per trace, not a `VAR=x forks_of`: assignments in front of
            # a FUNCTION are allowed to persist, so TMUX would leak into the next
            # arm. And not `env VAR=x`, which is itself an exec the tracer counts.
            bare=$(unset CCTAB_DRY_RUN TMUX TMUX_PANE
                   HOME=$W CLAUDE_PID=; export HOME CLAUDE_PID
                   forks_of "$W/p.$e" "$NEW" "$e")
            mux=$(unset CCTAB_DRY_RUN
                   HOME=$W CLAUDE_PID= TMUX="$W/no-such-server,1,0" TMUX_PANE=%0
                   export HOME CLAUDE_PID TMUX TMUX_PANE
                   forks_of "$W/p.$e" "$NEW" "$e")
            if [ "$bare" = 0 ] && [ "$mux" = 0 ]; then
                printf 'ok    %-8s forks nothing, bare or inside tmux\n' "$e"
            else
                printf 'FAIL  %-8s bare %s, inside tmux %s (? = the tracer could not\n' \
                    "$e" "$bare" "$mux"
                printf '               run it, which is not the same as a zero)\n'
                gate=fail
            fi
        done
        ;;
    esac
fi

printf '\n'
if [ "$gate" = fail ]; then
    printf 'RESULT: FAIL\n'
    exit 1
fi
printf 'RESULT: PASS\n'
exit 0
