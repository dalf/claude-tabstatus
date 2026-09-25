#!/bin/sh
# claude-tabstatus test suite.
#
#   sh   tests/run.sh
#   dash tests/run.sh
#   CCTAB_TEST_SH=/bin/sh busybox sh tests/run.sh    # multi-call shells
#
# Dependency-free: no bats, no jq. Exits non-zero if anything fails.
#
# Every assertion runs the script under test through CCTAB_DRY_RUN=1, or with
# CLAUDE_PID unset, so nothing here can ever write an escape sequence to a real
# terminal.

here=$(cd -- "$(dirname -- "$0")" && pwd) || exit 1
script=$here/../scripts/tabstatus.sh

tmp=$(mktemp -d) || exit 1
cleanup() { rm -rf "$tmp"; }
# Split: a signal trap does not abort a non-interactive shell, so a combined
# `trap cleanup EXIT HUP INT TERM` would clean up and then keep running.
trap cleanup EXIT
trap 'cleanup; exit 130' HUP INT TERM

# Run the script under test with the SAME interpreter that is running this
# file, so `dash tests/run.sh` really exercises dash. CCTAB_TEST_SH overrides.
#
# /proc/$$/exe alone is not enough. On a multi-call binary - busybox ash, which
# is /bin/sh on Alpine, or toybox - it resolves to .../bin/busybox, and
# `busybox /path/to/tabstatus.sh` is read as an applet name, so every assertion
# would fail for a reason that has nothing to do with the code under test.
# Some shells also exec the last command of a `-c` string, which makes the
# probe resolve to whatever that was. So: require a shell-shaped name, and
# require it to actually run a script file.
sh_under_test=${CCTAB_TEST_SH-}
if [ -z "$sh_under_test" ]; then
    _probe=$(readlink "/proc/$$/exe" 2>/dev/null) || _probe=
    case ${_probe##*/} in
    sh | dash | ash | bash | ksh | ksh93 | mksh | zsh | yash | posh) ;;
    *) _probe= ;;
    esac
    if [ -n "$_probe" ] && [ -x "$_probe" ]; then
        printf ':\n' >"$tmp/probe.sh"
        "$_probe" "$tmp/probe.sh" >/dev/null 2>&1 || _probe=
        rm -f "$tmp/probe.sh"
    else
        _probe=
    fi
    sh_under_test=${_probe:-sh}
fi
# Resolve to an absolute path, so that an assertion can empty PATH and still
# have an interpreter to run.
case $sh_under_test in
/*) ;;
*)
    _abs=$(command -v "$sh_under_test" 2>/dev/null) || _abs=
    [ -z "$_abs" ] || sh_under_test=$_abs
    ;;
esac

pass=0
fail=0

# check <label> <expected> <actual>
check() {
    if [ "$2" = "$3" ]; then
        pass=$((pass + 1))
        printf 'PASS  %s\n' "$1"
    else
        fail=$((fail + 1))
        printf 'FAIL  %s\n      expected [%s]\n      actual   [%s]\n' "$1" "$2" "$3"
    fi
}

# dry <edge> [cwd] -- rendered title on stdout
dry() {
    _edge=$1
    _dir=${2-$here}
    (cd -- "$_dir" 2>/dev/null && CCTAB_DRY_RUN=1 "$sh_under_test" "$script" "$_edge" </dev/null)
}

printf 'interpreter under test: %s\n' "$sh_under_test"
printf 'script under test:      %s\n\n' "$script"

# --- glyph per edge --------------------------------------------------------
mkdir -p "$tmp/plaindir"
check 'edge session-start -> idle glyph' '⚪ plaindir' "$(dry session-start "$tmp/plaindir")"
check 'edge working'                     '🔵 plaindir' "$(dry working "$tmp/plaindir")"
check 'edge waiting'                     '🟠 plaindir' "$(dry waiting "$tmp/plaindir")"
check 'edge idle'                        '⚪ plaindir' "$(dry idle "$tmp/plaindir")"
check 'edge session-end -> empty title'  ''            "$(dry session-end "$tmp/plaindir")"
check 'unknown edge falls back to idle'  '⚪ plaindir' "$(dry no-such-edge "$tmp/plaindir")"
check 'missing edge argument -> idle'    '⚪ plaindir' \
    "$(cd -- "$tmp/plaindir" && CCTAB_DRY_RUN=1 "$sh_under_test" "$script" </dev/null)"

# --- location -------------------------------------------------------------
check 'plain directory basename' '⚪ plaindir' "$(dry idle "$tmp/plaindir")"

fakehome=$tmp/fakehome
mkdir -p "$fakehome"
check '$PWD == $HOME renders as ~' '⚪ ~' \
    "$(cd -- "$fakehome" && HOME=$fakehome CCTAB_DRY_RUN=1 "$sh_under_test" "$script" idle </dev/null)"
check 'a dir inside $HOME is not ~' '⚪ plaindir' \
    "$(cd -- "$tmp/plaindir" && HOME=$tmp CCTAB_DRY_RUN=1 "$sh_under_test" "$script" idle </dev/null)"
check 'root renders as /' '⚪ /' \
    "$(cd -- / && HOME=/nonexistent-home CCTAB_DRY_RUN=1 "$sh_under_test" "$script" idle </dev/null)"
check 'a HOME with a trailing slash still renders as ~' '⚪ ~' \
    "$(cd -- "$fakehome" && HOME=$fakehome/ CCTAB_DRY_RUN=1 "$sh_under_test" "$script" idle </dev/null)"

# --- JSON-hostile directory names -----------------------------------------
mkdir -p "$tmp/we\"ird"
check 'double quote is stripped from the location' '⚪ weird' "$(dry idle "$tmp/we\"ird")"
mkdir -p "$tmp/back\\slash"
check 'backslash is stripped from the location' '⚪ backslash' "$(dry idle "$tmp/back\\slash")"
mkdir -p "$tmp/both\"x\\y"
check 'quote and backslash together are stripped' '⚪ bothxy' "$(dry idle "$tmp/both\"x\\y")"
nlname=$(printf 'new\nline')
mkdir -p "$tmp/$nlname"
check 'a newline in the location is stripped' '⚪ newline' "$(dry idle "$tmp/$nlname")"
mkdir -p "$tmp/\"\\"
check 'a location stripped to nothing becomes ?' '⚪ ?' "$(dry idle "$tmp/\"\\")"
# The sanitizer is a pure-shell loop, so an empty PATH must not degrade it.
check 'sanitizing needs no external command' '⚪ weird' \
    "$(cd -- "$tmp/we\"ird" && PATH= CCTAB_DRY_RUN=1 "$sh_under_test" "$script" idle </dev/null)"
# High bytes must NOT go through the sanitizer at all: an accented or emoji
# directory name is perfectly good in a JSON string and stays byte for byte.
mkdir -p "$tmp/café-déjà"
check 'a non-ASCII location is left alone' '⚪ café-déjà' "$(dry idle "$tmp/café-déjà")"
# printf format-string injection: $title is always an argument, never a format.
mkdir -p "$tmp/pct-100%s%d-x"
check 'a % in the location is not a printf format' '⚪ pct-100%s%d-x' \
    "$(dry idle "$tmp/pct-100%s%d-x")"

# --- glyph overrides ------------------------------------------------------
check 'CCTAB_GLYPH_WORKING override' '> plaindir' \
    "$(cd -- "$tmp/plaindir" && CCTAB_GLYPH_WORKING='>' CCTAB_DRY_RUN=1 "$sh_under_test" "$script" working </dev/null)"
check 'CCTAB_GLYPH_WAITING override' '? plaindir' \
    "$(cd -- "$tmp/plaindir" && CCTAB_GLYPH_WAITING='?' CCTAB_DRY_RUN=1 "$sh_under_test" "$script" waiting </dev/null)"
check 'CCTAB_GLYPH_IDLE override' '. plaindir' \
    "$(cd -- "$tmp/plaindir" && CCTAB_GLYPH_IDLE='.' CCTAB_DRY_RUN=1 "$sh_under_test" "$script" idle </dev/null)"
check 'an empty glyph override leaves no leading space' 'plaindir' \
    "$(cd -- "$tmp/plaindir" && CCTAB_GLYPH_IDLE= CCTAB_DRY_RUN=1 "$sh_under_test" "$script" idle </dev/null)"

# --- stdin drain ----------------------------------------------------------
payload='{"session_id":"abc","transcript_path":"/tmp/t.jsonl","cwd":"/x","hook_event_name":"Stop"}'
check 'a one-line payload on stdin is drained, not echoed' '⚪ plaindir' \
    "$(cd -- "$tmp/plaindir" && printf '%s\n' "$payload" | CCTAB_DRY_RUN=1 "$sh_under_test" "$script" idle)"
check 'a payload with no trailing newline is drained' '⚪ plaindir' \
    "$(cd -- "$tmp/plaindir" && printf '%s' "$payload" | CCTAB_DRY_RUN=1 "$sh_under_test" "$script" idle)"
check 'a multi-line payload is drained' '⚪ plaindir' \
    "$(cd -- "$tmp/plaindir" && printf '%s\n%s\n%s\n' "$payload" "$payload" "$payload" | CCTAB_DRY_RUN=1 "$sh_under_test" "$script" idle)"

# --- exit status ----------------------------------------------------------
for e in session-start working waiting idle session-end no-such-edge; do
    (cd -- "$tmp/plaindir" && CCTAB_DRY_RUN=1 "$sh_under_test" "$script" "$e" </dev/null >/dev/null 2>&1)
    check "exit 0 on dry-run edge $e" '0' "$?"
done

# --- real emission paths (CLAUDE_PID unset: no pty is ever touched) -------
# The terminalSequence line is asserted byte for byte: the \u001b / \u0007
# escapes are what keep it valid JSON, and a raw control byte here would be the
# subtle bug this test exists to catch.
expect_json='{"terminalSequence":"\u001b]0;🔵 plaindir\u0007","suppressOutput":true}'
check 'working emits the terminalSequence JSON line' "$expect_json" \
    "$(cd -- "$tmp/plaindir" && unset CLAUDE_PID; "$sh_under_test" "$script" working </dev/null)"
check 'idle emits the terminalSequence JSON line' \
    '{"terminalSequence":"\u001b]0;⚪ plaindir\u0007","suppressOutput":true}' \
    "$(cd -- "$tmp/plaindir" && unset CLAUDE_PID; "$sh_under_test" "$script" idle </dev/null)"
check 'the emitted JSON line has no raw ESC byte' '0' \
    "$(cd -- "$tmp/plaindir" && unset CLAUDE_PID; "$sh_under_test" "$script" working </dev/null | tr -dc '\033' | wc -c | tr -d ' ')"
check 'the emitted JSON is exactly one line' '1' \
    "$(cd -- "$tmp/plaindir" && unset CLAUDE_PID; "$sh_under_test" "$script" working </dev/null | wc -l | tr -d ' ')"

# Headless guard: no CLAUDE_PID means the direct-write edges do nothing at all.
check 'session-start with no CLAUDE_PID emits nothing' '' \
    "$(cd -- "$tmp/plaindir" && unset CLAUDE_PID; "$sh_under_test" "$script" session-start </dev/null 2>&1)"
check 'session-end with no CLAUDE_PID emits nothing' '' \
    "$(cd -- "$tmp/plaindir" && unset CLAUDE_PID; "$sh_under_test" "$script" session-end </dev/null 2>&1)"
(cd -- "$tmp/plaindir" && unset CLAUDE_PID; "$sh_under_test" "$script" session-start </dev/null >/dev/null 2>&1)
check 'session-start with no CLAUDE_PID still exits 0' '0' "$?"

# A CLAUDE_PID that resolves to something that is not a tty must also be inert.
check 'a CLAUDE_PID whose fd 1 is not a tty emits nothing' '' \
    "$(cd -- "$tmp/plaindir" && CLAUDE_PID=1 "$sh_under_test" "$script" session-start </dev/null 2>&1)"
check 'a nonsense CLAUDE_PID emits nothing' '' \
    "$(cd -- "$tmp/plaindir" && CLAUDE_PID=not-a-pid "$sh_under_test" "$script" session-start </dev/null 2>&1)"

# --- summary --------------------------------------------------------------
printf '\n----------------------------------------\n'
printf '%d passed, %d failed\n' "$pass" "$fail"
if [ "$fail" -ne 0 ]; then
    printf 'RESULT: FAIL\n'
    exit 1
fi
printf 'RESULT: PASS\n'
exit 0
