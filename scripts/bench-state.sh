#!/bin/sh
# What the wait-ownership record costs per edge, measured rather than remembered.
#
#   sh scripts/bench-state.sh                        bin/tabstatus against itself
#   sh scripts/bench-state.sh <baseline-binary>      ...and against a baseline
#   CCTAB_BENCH_EXECS=200 CCTAB_BENCH_ROUNDS=9 sh scripts/bench-state.sh
#
# WHY THIS EXISTS. src/state.rs used to carry remembered numbers - "paired deltas
# -8/+30/+1/+19us against a 420-460us baseline" - and they did not reproduce: the
# baseline drifted 3x between rounds under load and the sign of the delta changed
# twice. A per-edge cost of a few microseconds against a ~500us fork-and-exec floor
# is below the noise of a loaded desktop, so the only honest way to report it is a
# script anybody can re-run, with the arms INTERLEAVED and the spread shown.
#
# THE METHOD, and each choice is there to survive a machine that is also doing
# something else:
#   * ROUNDS passes, and within each pass every arm is timed once, in order. Load
#     that arrives mid-run hits all the arms rather than one of them.
#   * per round, the arm's time is the mean over EXECS invocations; the round's
#     figure is that mean. The arm's reported figure is the MINIMUM over rounds -
#     the round least disturbed - and the spread is min..max, so a reader can see
#     how much the machine moved underneath.
#   * the paired delta is computed per round against that round's own baseline arm
#     and reported as the MEDIAN of those, not as a difference of the two minima.
#
# WHAT IT DOES NOT MEASURE: the harness fork Claude Code pays to launch the hook,
# which is the larger half of the 767us a real edge costs. Every arm here pays one
# fork of its own and no more, so the DELTAS are the thing to read and the absolute
# figures are a floor, not a hook's cost.
#
# LAST RUN on this machine, two independent 21-round passes of 200 execs, no
# baseline argument, so the comparison is the layer against ITSELF switched off:
#
#   arm                  min us    max us    spread vs baseline
#   new-nostate             460       502        42          +0
#   new-read-nowait         475       620       145         +15
#   new-read-wait           453       515        62          -8
#   new-write               479       533        54         +22
#   new-agent-noop          436       484        48         -24
#   new-agent-clear         446       488        42         -14
#   new-idle                474       650       176         +16
#
# The two NEGATIVE arms are not noise and are worth understanding: an edge that
# owns no wait, or that holds one somebody else owns, returns `None` before the
# location walk and before the emit, so it is genuinely cheaper than a painting
# edge. The layer's own cost is the +15us on the hot `working` read and the +15 to
# +22us on a transition, both against a ~460us floor.
set -eu

here=$(cd -- "$(dirname -- "$0")/.." && pwd) || exit 1
cd -- "$here"

NEW=${CCTAB_BENCH_BIN:-$here/bin/tabstatus}
BASE=${1-}
EXECS=${CCTAB_BENCH_EXECS:-200}
ROUNDS=${CCTAB_BENCH_ROUNDS:-9}

[ -x "$NEW" ] || { printf 'error: %s is not executable\n' "$NEW" >&2; exit 1; }

W=$(mktemp -d) || exit 1
trap 'rm -rf "$W"' EXIT
trap 'rm -rf "$W"; exit 130' HUP INT TERM
mkdir -p "$W/work" "$W/sd"
SID=bench000-1111-2222-3333-444455556666
AG=aec99e1f4bda1972b

# The payloads, each with a 2 KB tool_response so the hot edge is not measured on
# an unrealistically small one.
blob=$(awk 'BEGIN{while(i++<2048)printf "z"}' </dev/null)
printf '{"session_id":"%s","hook_event_name":"PostToolUse","tool_name":"Read","tool_response":"%s"}\n' \
    "$SID" "$blob" >"$W/p.main"
printf '{"session_id":"%s","hook_event_name":"PostToolUse","agent_id":"%s","tool_response":"%s"}\n' \
    "$SID" "$AG" "$blob" >"$W/p.agent"
printf '{"session_id":"%s","hook_event_name":"PermissionRequest","agent_id":"%s"}\n' \
    "$SID" "$AG" >"$W/p.wait"
printf '{"session_id":"%s","hook_event_name":"Stop","last_assistant_message":"%s","background_tasks":[]}\n' \
    "$SID" "$blob" >"$W/p.stop"

# time_arm <bin> <edge> <payload> <statedir|-> [seed] -- mean microseconds per exec.
#
# ONE fork per iteration, which is the whole reason the environment is exported
# around the loop instead of being passed through `env` inside it: a subshell plus
# an `env` per exec added two more forks and put a 1100us floor under a 500us
# measurement, drowning the thing being measured.
#
# The record is re-seeded before the loop, never inside it, so the arm measures one
# steady state rather than an average over two.
time_arm() {
    _bin=$1; _edge=$2; _pay=$3; _sd=$4; _seed=${5-}
    unset XDG_RUNTIME_DIR CCTAB_STATE_DIR
    if [ "$_sd" != "-" ]; then
        rm -f "$_sd/$SID"
        [ -z "$_seed" ] || printf '%s' "$_seed" >"$_sd/$SID"
        CCTAB_STATE_DIR=$_sd
        export CCTAB_STATE_DIR
    fi
    HOME=$W CCTAB_DRY_RUN=1 CCTAB_NOW=1000000 CLAUDE_PID=
    export HOME CCTAB_DRY_RUN CCTAB_NOW CLAUDE_PID
    cd -- "$W/work" || exit 1
    _t0=$(date +%s%N)
    _i=0
    while [ "$_i" -lt "$EXECS" ]; do
        _i=$((_i + 1))
        "$_bin" "$_edge" <"$_pay" >/dev/null 2>&1
    done
    _t1=$(date +%s%N)
    cd -- "$here" || exit 1
    echo $(( (_t1 - _t0) / 1000 / EXECS ))
}

# The arms. `seed` is what the record holds before the loop, which is what decides
# whether the arm is a read or a read plus a write.
arms='new-nostate new-read-nowait new-read-wait new-write new-agent-noop new-agent-clear new-idle'
[ -z "$BASE" ] || arms="base-nostate $arms"

run_arm() {
    case $1 in
    base-nostate)     time_arm "$BASE" working "$W/p.main"  - ;;
    # No record at all: the layer is off, so this is the stateless program.
    new-nostate)      time_arm "$NEW"  working "$W/p.main"  - ;;
    # A record that already says `b w` with nothing waiting: pure read, no write.
    new-read-nowait)  time_arm "$NEW"  working "$W/p.main"  "$W/sd" "cts1
b w
" ;;
    # A read plus the UserPromptSubmit needle, which only runs when a wait is held.
    new-read-wait)    time_arm "$NEW"  working "$W/p.main"  "$W/sd" "cts1
b w
w $AG:1000000
" ;;
    # A transition: the base moves, so one temp-write plus one rename.
    new-write)        time_arm "$NEW"  working "$W/p.main"  "$W/sd" "cts1
b i
" ;;
    # A background subagent's tool call that owns no wait: read, no write, no paint.
    new-agent-noop)   time_arm "$NEW"  working "$W/p.agent" "$W/sd" "cts1
b w
" ;;
    # The un-painting edge this whole layer exists for: read, clear, write.
    new-agent-clear)  time_arm "$NEW"  working "$W/p.agent" "$W/sd" "cts1
b w
w $AG:1000000
" ;;
    # The edge whose payload WINDOW changed: idle now builds a tail for
    # background_tasks.
    new-idle)         time_arm "$NEW"  idle    "$W/p.stop"  "$W/sd" "cts1
b i
" ;;
    esac
}

printf 'bench: %s execs x %s interleaved rounds\n' "$EXECS" "$ROUNDS"
printf '  new      %s\n' "$NEW"
[ -z "$BASE" ] || printf '  baseline %s\n' "$BASE"
printf '\n'

: >"$W/samples"
r=0
while [ "$r" -lt "$ROUNDS" ]; do
    r=$((r + 1))
    for a in $arms; do
        printf '%s %s\n' "$a" "$(run_arm "$a")" >>"$W/samples"
    done
    printf '.' >&2
done
printf '\n\n' >&2

base_arm=new-nostate
[ -z "$BASE" ] || base_arm=base-nostate

awk -v arms="$arms" -v base="$base_arm" '
{ v[$1] = v[$1] " " $2 }
END {
    n = split(arms, a, " ")
    split(v[base], bs, " ")
    printf "%-17s %9s %9s %9s %11s\n", "arm", "min us", "max us", "spread", "vs baseline"
    for (i = 1; i <= n; i++) {
        split(v[a[i]], s, " ")
        mn = s[1]; mx = s[1]
        for (j in s) { if (s[j] + 0 < mn) mn = s[j]; if (s[j] + 0 > mx) mx = s[j] }
        # Paired per-round deltas, median of them.
        k = 0
        for (j = 1; j in s; j++) { if (j in bs) { d[++k] = s[j] - bs[j] } }
        for (x = 1; x <= k; x++) for (y = x + 1; y <= k; y++) if (d[y] < d[x]) { t = d[x]; d[x] = d[y]; d[y] = t }
        med = (k % 2) ? d[(k + 1) / 2] : int((d[k / 2] + d[k / 2 + 1]) / 2)
        printf "%-17s %9d %9d %9d %+11d\n", a[i], mn, mx, mx - mn, med
        delete d
    }
    printf "\nvs baseline is the MEDIAN of the per-round paired deltas against %s.\n", base
}' "$W/samples"
