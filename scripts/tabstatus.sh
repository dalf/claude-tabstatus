#!/bin/sh
# claude-tabstatus - put the Claude Code session state into the terminal tab
# title, in front of the location.
#
#   sh "${CLAUDE_PLUGIN_ROOT}/scripts/tabstatus.sh" <edge>
#
# Slice 1 edges:  session-start | working | idle | session-end
# `waiting` is implemented and testable but no slice-1 hook emits it yet.
#
# POSIX sh only. No [[ ]], no arrays, no ${v//x/y}, no ${v^^}, no process
# substitution, no `local`. Exercised under bash-as-sh, dash and busybox ash.
#
# Two delivery mechanisms, because neither one covers every edge:
#
#   * Most edges print ONE line of JSON carrying `terminalSequence` and Claude
#     Code emits the bytes to its own terminal. Only notification/title OSCs
#     (0, 1, 2, 9, 99, 777) and BEL are permitted there; anything else is
#     silently dropped.
#   * session-start and session-end write the pty directly, because
#     terminalSequence cannot carry them: SessionStart is too early (the TUI
#     writer is not mounted yet, so the sequence is dropped) and the Konsole
#     arming sequence is OSC 50, which is not on the allowlist above. Those
#     two edges resolve the pty through /proc, so they are Linux-only; see
#     "Known limitations" in the README.
#
# This script always exits 0. A non-zero PreToolUse hook can block a tool, and
# a non-zero hook anywhere is noise.

# ---------------------------------------------------------------------------
# 0. Drain the hook payload from stdin.
#
# Slice 1 does not parse it; it only has to be consumed so the writer never
# sees EPIPE. One `cat` fork, deliberately, rather than a shell `read` loop:
# POSIX `read` may not consume past the line it returns, so on a pipe every
# shell reads a byte per syscall. Measured end to end under /bin/sh, payload
# piped in (30 runs, us per invocation):
#
#            0KB    16KB    64KB   256KB
#   loop    1970    5447   13697   46617
#   cat     2672    3619    2576    2914
#
# so one fork buys a flat cost, and the crossover is under 16KB. PostToolUse -
# the hot edge a later slice adds - carries the whole tool_response, which is
# precisely the case the loop pessimized.
#
# `[ -t 0 ]` is there so that running this by hand from a terminal does not
# block on a stdin that never reaches EOF. A real hook always gets a pipe.
# ---------------------------------------------------------------------------
[ -t 0 ] || cat >/dev/null 2>&1

edge=${1-}

# ---------------------------------------------------------------------------
# 1. LOCATION
#
# ===== SEAM (slice 2) ======================================================
# Slice 2 replaces the body of this block with, in precedence order:
#     1. git repo@branch      -> "streaming-browser@master"
#     2. ssh hostname prefix  -> "srv:streaming-browser@master"
#     3. the basename fallback below
# Keep it fork-free: walk up for a .git, read .git/HEAD with the `read`
# builtin and strip "ref: refs/heads/" with parameter expansion; take the repo
# name from the toplevel path. Do not shell out to `git`.
# Everything downstream of this block only consumes $place, so nothing else
# has to change.
# ---------------------------------------------------------------------------
[ -n "${PWD-}" ] || PWD=$(pwd 2>/dev/null)
place=${PWD##*/}
# ${HOME%/} because a HOME carrying a trailing slash would otherwise miss this
# comparison and render the home directory as its basename instead of ~.
_home=${HOME-}
_home=${_home%/}
if [ -n "$_home" ] && [ "$PWD" = "$_home" ]; then
    place='~'
elif [ -z "$place" ]; then
    place='/'
fi
# ===== end SEAM ============================================================

# ---------------------------------------------------------------------------
# 2. Sanitize $place for a JSON string literal.
#
# `"` and `\` would break the literal. Control characters are illegal inside
# one, and a newline would also break the one-line-per-hook contract. Deleting
# them is enough - nothing downstream needs the original bytes. The `case`
# guard means this runs only for a pathological directory name, never on the
# common path.
#
# The body is a pure-shell character loop rather than a `tr` pipeline: no
# fork, no dependency on how a given `tr` parses a character class embedded in
# a larger set, and a merely quote-bearing name degrades to `weird` instead of
# to `?` when PATH is broken.
#
# NOT handled: a directory name that is not valid UTF-8 - legal on Linux, and
# still illegal inside a JSON string. Documented under "Known limitations"
# rather than fixed, because the only cheap detector would also fork for every
# perfectly good accented or emoji directory name.
# ---------------------------------------------------------------------------
case $place in
*[\"\\]* | *[[:cntrl:]]*)
    _rest=$place
    place=
    while [ -n "$_rest" ]; do
        _ch=${_rest%"${_rest#?}"}
        _rest=${_rest#?}
        case $_ch in
        '"' | \\ | [[:cntrl:]]) ;;
        *) place=$place$_ch ;;
        esac
    done
    [ -n "$place" ] || place='?'
    ;;
esac

# ---------------------------------------------------------------------------
# 3. Edge -> glyph. The CCTAB_GLYPH_* overrides let an ASCII fallback (or no
# glyph at all, by setting one to the empty string) need no code change.
# ---------------------------------------------------------------------------
case $edge in
working)
    glyph=${CCTAB_GLYPH_WORKING-🔵}
    ;;
waiting)
    glyph=${CCTAB_GLYPH_WAITING-🟠}
    ;;
session-end)
    glyph=
    ;;
*)
    # idle, session-start, and any edge a future hooks.json adds that this
    # version of the script does not know about yet.
    glyph=${CCTAB_GLYPH_IDLE-⚪}
    ;;
esac

if [ "$edge" = session-end ]; then
    title=
elif [ -n "$glyph" ]; then
    title="$glyph $place"
else
    title=$place
fi

# ---------------------------------------------------------------------------
# 4. CCTAB_DRY_RUN=1 prints the computed title and emits nothing at all.
# This is what makes the edge table testable with no Claude session running.
# ---------------------------------------------------------------------------
if [ "${CCTAB_DRY_RUN-}" = 1 ]; then
    printf '%s\n' "$title"
    exit 0
fi

# ---------------------------------------------------------------------------
# 5. Emission
# ---------------------------------------------------------------------------
case $edge in
session-start | session-end)
    # Resolve the session's pty from the environment. Hook subprocesses are
    # detached - fd 0 is /dev/null and `exec 3>/dev/tty` fails - so /dev/tty
    # is not usable here. CLAUDE_PID is exported into every hook subprocess.
    #
    # Guard: if CLAUDE_PID is unset, or fd 1 does not resolve to a writable
    # character device under /dev/pts or /dev/tty, do nothing rather than
    # retitle an unrelated terminal. That covers a redirected or piped
    # `claude -p`, and every platform without /proc. It does NOT cover a
    # `claude -p` typed straight at a terminal, whose fd 1 really is that
    # tab's pty - see "Known limitations".
    tty=
    if [ -n "${CLAUDE_PID-}" ]; then
        tty=$(readlink "/proc/$CLAUDE_PID/fd/1" 2>/dev/null)
        case $tty in
        /dev/pts/* | /dev/tty*) ;;
        *) tty= ;;
        esac
        if [ -n "$tty" ]; then
            [ -c "$tty" ] && [ -w "$tty" ] || tty=
        fi
    fi
    [ -n "$tty" ] || exit 0

    # GATE: OSC 50 means "set font" in xterm and is unknown in most other
    # terminals, so it goes out only when the terminal really is Konsole.
    # Konsole applies profile properties per tab, at runtime, in memory, and
    # never inherits them into new tabs or writes them to disk, so every other
    # tab keeps Konsole's default tab title by construction.
    #
    # KONSOLE_* is inherited environment, which is a weak signal: it survives
    # into any child terminal launched from a Konsole shell, and into every
    # pane of a tmux server that was first started under Konsole. $TMUX and
    # $STY at least take the multiplexer case out, where the arming would
    # reach the multiplexer rather than the tab and be dropped there.
    konsole=
    if [ -z "${TMUX-}" ] && [ -z "${STY-}" ]; then
        if [ -n "${KONSOLE_VERSION-}" ] || [ -n "${KONSOLE_DBUS_SESSION-}" ]; then
            konsole=1
        fi
    fi

    # 2>/dev/null comes first so a failing redirection is silent too.
    if [ "$edge" = session-start ]; then
        if [ -n "$konsole" ]; then
            # Arm this tab: %w makes the OSC 0 payload the entire tab text.
            # Under Konsole's stock formats an OSC 0 title is invisible in the
            # tab, which is why Claude's own title never shows up there.
            # SEAM: TabColor=#RRGGBB rides in this same OSC 50 property list -
            # and whoever adds it must also add TabColor=#000000 to the
            # session-end list below, or the colour outlives the session.
            printf '\033]50;LocalTabTitleFormat=%%w;RemoteTabTitleFormat=%%w\007\033]0;%s\007' \
                "$title" 2>/dev/null >"$tty"
        else
            # Windows Terminal and friends need no arming.
            printf '\033]0;%s\007' "$title" 2>/dev/null >"$tty"
        fi
    else
        # We own restore: with the built-in terminal title disabled (which
        # install.sh does, otherwise it repaints over ours every 960ms) Claude
        # Code no longer clears the title on exit either. The two formats
        # below are Konsole's compiled-in defaults - not whatever a customized
        # profile had, see "Known limitations".
        if [ -n "$konsole" ]; then
            printf '\033]50;LocalTabTitleFormat=%%d : %%n;RemoteTabTitleFormat=(%%u) %%H\007\033]0;\007' \
                2>/dev/null >"$tty"
        else
            printf '\033]0;\007' 2>/dev/null >"$tty"
        fi
    fi
    ;;
*)
    # A raw control byte inside a JSON string is invalid JSON, so ESC and BEL
    # travel as  and . The doubled backslashes below survive
    # printf as single ones.
    printf '{"terminalSequence":"\\u001b]0;%s\\u0007","suppressOutput":true}\n' "$title"
    ;;
esac

exit 0
