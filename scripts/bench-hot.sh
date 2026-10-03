#!/bin/sh
# Optional stateless dry-run timing, NOT the required subprocess check.
#   sh scripts/bench-hot.sh --calibrate
#   sh scripts/bench-hot.sh /absolute/path/to/baseline
# CCTAB_BENCH_BIN chooses the candidate. Use comparable release builds of one
# target/toolchain/profile; see docs/architecture.md#performance. The default
# 50us band is a local heuristic, not a portable CI latency guarantee.
# Required deterministic coverage: python3 tests/check_hot_subprocesses.py
set -eu

# Internal arm: a clean environment, timed only after entering the work directory.
if [ "${1-}" = --time-arm ]; then
    _bin=$2; _edge=$3; _work=$4; _execs=$5
    cd -- "$_work/work"
    _t0=$(date +%s%N)
    _i=0
    while [ "$_i" -lt "$_execs" ]; do
        _i=$((_i + 1))
        "$_bin" "$_edge" <"$_work/p.$_edge" >/dev/null
    done
    _t1=$(date +%s%N)
    echo $(( (_t1 - _t0) / 1000 / _execs ))
    exit
fi

here=$(cd -- "$(dirname -- "$0")/.." && pwd) || exit 1
cd -- "$here"

NEW=${CCTAB_BENCH_BIN:-$here/bin/tabstatus}
[ "$#" -eq 1 ] || { echo "usage: $0 <baseline-binary>|--calibrate" >&2; exit 2; }
BASE=$1
EXECS=${CCTAB_BENCH_EXECS:-200}
ROUNDS=${CCTAB_BENCH_ROUNDS:-9}
BAND=${CCTAB_BENCH_BAND:-50}

[ -x "$NEW" ] || { printf 'error: %s is not executable\n' "$NEW" >&2; exit 1; }
# An explicit calibration has no regression verdict. Identical files also count
# as calibration, even when supplied under different paths.
null=
if [ "$BASE" = --calibrate ]; then
    BASE=$NEW
    null=yes
elif [ ! -x "$BASE" ]; then
    printf 'error: baseline %s is not executable\n' "$BASE" >&2
    exit 1
elif cmp -s "$NEW" "$BASE"; then
    null=yes
fi
NEW=$(cd -- "$(dirname -- "$NEW")" && pwd)/$(basename -- "$NEW")
BASE=$(cd -- "$(dirname -- "$BASE")" && pwd)/$(basename -- "$BASE")
for value in "$EXECS" "$ROUNDS" "$BAND"; do
    case $value in ''|*[!0-9]*) echo 'counts and band must be positive integers' >&2; exit 2;; esac
    [ "$value" -gt 0 ] || exit 2
done

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

# The extra shell and env are outside the measured interval; each iteration
# still pays only the shell's launch of the binary. No ambient hook settings leak.
time_arm() {
    env -i PATH="$PATH" HOME="$W" CCTAB_DRY_RUN=1 CCTAB_NOW=1000000 \
        CCTAB_TERMINAL=other CCTAB_GLYPH_POS=prefix \
        /bin/sh "$here/scripts/bench-hot.sh" --time-arm "$1" "$2" "$W" "$EXECS"
}

edges='working waiting idle notify'
arms=''
for e in $edges; do arms="$arms base-$e new-$e"; done

# ---- the report ------------------------------------------------------------
printf 'bench-hot: %s execs x %s interleaved rounds, band %sus\n' "$EXECS" "$ROUNDS" "$BAND"
printf '  candidate %s\n' "$NEW"
if [ -n "$null" ]; then
    printf '  baseline  the same binary - CALIBRATION ONLY, identical binaries\n'
else
    printf '  baseline  %s\n' "$BASE"
fi
printf '  scope: stateless dry-run; no routing, carrier construction or delivery\n\n'

: >"$W/samples"
r=0
while [ "$r" -lt "$ROUNDS" ]; do
    r=$((r + 1))
    for a in $arms; do
        case $a in
        base-*) sample=$(time_arm "$BASE" "${a#base-}") || exit 1; printf '%s %s\n' "$a" "$sample" >>"$W/samples" ;;
        new-*) sample=$(time_arm "$NEW" "${a#new-}") || exit 1; printf '%s %s\n' "$a" "$sample" >>"$W/samples" ;;
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
if awk -v edges="$edges" -v band="$BAND" -v calibration="$null" '
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
        "edge", "base us", "new us", "delta", "spread", "median pair", "band"
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
    exit (bad && calibration != "yes")
}' "$W/samples"; then
    :
else
    gate=fail
fi

if [ "$gate" = fail ]; then
    printf '\nRESULT: FAIL (dry-run timing only)\n'
    exit 1
elif [ -n "$null" ]; then
    printf '\nRESULT: CALIBRATION (no regression verdict)\n'
else
    printf '\nRESULT: PASS (dry-run timing only)\n'
fi
