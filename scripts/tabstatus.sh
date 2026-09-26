#!/bin/sh
# claude-tabstatus - put the Claude Code session state into the terminal tab
# title, in front of the location.
#
#   sh "${CLAUDE_PLUGIN_ROOT}/scripts/tabstatus.sh" <edge>
#
# Edges:  session-start | working | waiting | idle | notify | session-end
#
# The first five paint; `notify` decides between idle, waiting and painting
# nothing at all by looking at the notification kind, and session-end unpaints.
# Which hook event maps to which edge is hooks.json's business, not this
# script's - see the state table in the README. The script is STATELESS: what it
# paints is a pure function of the edge argument plus, for two edges, a handful
# of `case` globs on the raw payload. Nothing is written, so nothing is stale
# after a SIGKILL and there is nothing to prune.
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

edge=${1-}

# ---------------------------------------------------------------------------
# 0. Take the hook payload off stdin.
#
# Two ways in, because two edges have to SEE the payload and the hot ones must
# never touch it.
#
#   * `notify` and `session-start` read it into a variable with the `read`
#     builtin, which costs no fork. Both payloads are small and single-line -
#     measured 845 bytes for a Notification and 769 for a SessionStart - and at
#     that size the builtin is in fact FASTER than the `cat` below, in every
#     shell this is tested under.
#   * Every other edge only has to CONSUME stdin, so the writer never sees
#     EPIPE. One `cat` fork, deliberately, rather than a shell `read` loop:
#     POSIX `read` may not consume past the line it returns, so on a pipe every
#     shell reads a byte per syscall, which makes the read branch LINEAR in the
#     payload while `cat` is flat.
#
# How linear depends on the shell, and the figures below are the reason this
# comment does not simply say "small". us per invocation, 40 reps, one payload
# with the discriminator as its last field, `read` (notify) against `cat`
# (working), all three shells this script is exercised under:
#
#   read         845B    2KB     8KB    64KB   256KB     1MB
#     bash       2072   2214    2215    3303    6584   19584
#     dash       1298   1559    3039   12561   40978  159785
#     busybox    2358   2854    5021   20799   76117  299807
#   cat is flat everywhere: 1.4-3.1ms at every size in that row, and at 1MB
#   4.6ms under bash, 1.8 under dash, 3.0 under busybox ash.
#
# So the crossover is NOT one number: about 32KB under bash, but only about 2KB
# under dash and busybox ash. `read` wins at the sizes these two edges actually
# see, and nowhere else. THE RULE for whoever wires the next edge: an edge may
# take the `read` branch only if its payload schema has no unbounded field.
# Notification is {message, title?, notification_type} and SessionStart is
# {source - an enum of five words - agent_type?, model?, session_title?, a few
# numbers}, both on top of a base of {session_id, transcript_path, cwd,
# prompt_id?, permission_mode?, agent_id?}: bounded strings throughout, which is
# what bounds the read. The unbounded fields in the hook schema are `tool_input`
# and `tool_response`, both arbitrary JSON, and every event carrying one is on
# the `cat` branch below.
#
# PostToolUse is the reason that split exists. It is the hot edge - one per tool
# call - and its payload carries the whole tool_response, hundreds of KB on a
# large Read, which is precisely the case the read loop pessimizes. It also has
# nothing to decide: it paints `working` unconditionally, which stays correct
# even for a subagent's tool call, because the main session really is working
# then. So it takes the `cat` branch and never touches the bytes.
#
# PostToolUseFailure takes it too, and that one costs something: its payload
# carries `is_interrupt`, the only discriminator in the whole table that could
# tell an ABORTED tool call (idle) from a failed one (working). Reading it means
# reading a payload that also carries the tool_input of whatever was aborted - a
# Write's entire file content - at the prices above. Not worth it while the flag
# cannot be true: the hook dispatch is handed the turn's own abort signal, so an
# interrupt skips the spawn altogether, which is exactly why a measured Ctrl+C
# produced no hook at all. Every PostToolUseFailure observed so far carried
# is_interrupt:false, a tool that REPORTED an error, and a turn that continued -
# for which `working` is the right paint. If that ever changes, the flag is in
# the payload and this is the tradeoff to reopen.
#
# `[ -t 0 ]` is there so that running this by hand from a terminal does not
# block on a stdin that never reaches EOF. A real hook always gets a pipe. It
# leaves $payload empty, which every glob in 0b treats as "no discriminator
# present", i.e. paint the edge as asked.
# ---------------------------------------------------------------------------
payload=
if [ ! -t 0 ]; then
    case $edge in
    notify | session-start)
        # 2>/dev/null because a closed rather than empty stdin makes `read`
        # complain in some shells, and this script is never allowed to be noise.
        IFS= read -r payload 2>/dev/null
        # Then drain whatever follows, in case a future payload is not one line.
        # Builtins only: a second `cat` here would put a second fork on an edge
        # that currently has none.
        while IFS= read -r _junk 2>/dev/null; do :; done
        ;;
    *) cat >/dev/null 2>&1 ;;
    esac
fi

# ---------------------------------------------------------------------------
# 0b. The two edges whose state the payload decides.
#
# Both tests are `case` globs on the raw line. Never jq: a fork per
# notification, to parse 845 bytes that can be matched literally, plus a
# dependency the rest of the plugin does not have.
#
# What the globs can see, exactly. They carry the compact spelling Claude Code
# actually writes - `"key":"value"`, JSON.stringify output, no space after `:` or
# `,` - and the session-start one also tolerates one space after the colon.
# They do NOT see past the first line, because section 0 keeps one line and
# drains the rest. So a pretty-printed payload matches nothing, and the two
# edges then fail in OPPOSITE directions:
#
#   * notify falls through to silence, which is the safe direction: an unpainted
#     tab keeps the state it already showed.
#   * session-start falls through to PAINTING AND ARMING - the exact pair of
#     failures the compact case below exists to prevent. Measured: a
#     pretty-printed `{"source": "compact"}` on three lines paints idle in all
#     three shells. So hooks.json's `"matcher": "startup|resume|clear|fork"` is
#     the load-bearing guard, and this glob is only a belt to it. A belt that
#     fails open is still worth having against a hand-edited config; it must
#     just not be mistaken for the guard.
#
# The globs also trust the producer to emit conforming JSON, in two specific
# ways: a raw unescaped `"` inside a string value satisfies a glob's leading
# quote, and a literal NUL inside the discriminator is dropped by `read`, which
# splices `"permi<NUL>ssion_prompt"` into a real match. A conforming encoder
# writes `\"` and `\u0000`, so neither is reachable from Claude Code - measured,
# an escaped injection in `message` or `cwd` is correctly defeated - but a
# producer bug here would be amplified rather than absorbed.
#
# NOTIFY. `notification_type` is a plain string in the event schema (required,
# but not a closed enum), so this is a three-way and the third branch is silence:
#
#   * idle_prompt is the quiet-turn nudge, fired messageIdleNotifThresholdMs
#     (default 60000, user-configurable) after a turn ends. It is the one kind
#     that MUST NOT paint waiting - it arrives after EVERY quiet turn end, so
#     mapping it to waiting would turn every idle tab orange a minute later and
#     collapse two of the three states into one. Matched first for that reason.
#     It is also the only recovery this design has from an INTERRUPTED turn -
#     and it recovers nothing after a dialog the user walks away from, because
#     the keystroke that dismisses one also cancels this notification for good.
#     See the Ctrl+C note in the README.
#   * The waiting kinds are a BACKSTOP, not the fast path. permission_prompt is
#     scheduled 6000ms after the dialog goes up (measured +6.004 to +6.021s over
#     four sessions), fires at most once per dialog, and is suppressed outright
#     by CLAUDE_CODE_DISABLE_PERMISSION_PROMPT_NOTIFY_HOOKS. PermissionRequest,
#     23ms after PreToolUse, is the real-time signal.
#
#     The product cancels the notification if the dialog resolves or the user
#     types first, but that cancellation is BEST-EFFORT and has been measured
#     losing: in one session this hook fired in the same millisecond as the
#     answering keystroke, so its `waiting` and PostToolUse's `working` were
#     emitted concurrently and landed 38ms apart. The order came out right;
#     nothing guarantees it, and if the backstop ever writes last the tab sticks
#     orange until the next edge.
#
#     permission_prompt is kept anyway, even though PermissionRequest covers
#     every tool-call dialog in real time, because it is the ONLY signal for the
#     dialogs that are not tool calls: the managed-settings review and the
#     sandbox network request both notify with this kind. The other four kinds
#     are what PermissionRequest does not cover either - a worker/teammate
#     prompt, an agent asking for input, and the two MCP elicitation dialogs.
#   * Everything else is deliberately silent - agent_completed,
#     elicitation_complete, elicitation_response, computer_use_exit,
#     push_notification, auth_success, and whatever kind a later Claude Code
#     invents. None of them is a state change, and the elicitation pair in
#     particular is the RESPONSE side of an elicitation, not a wait.
#
# Why the three-way lives HERE and not in hooks.json, since the hook runner can
# match a Notification registration on notification_type - it is the field it
# extracts for that event, exactly as it extracts `source` for SessionStart.
# Two matcher-scoped blocks (idle_prompt -> idle, the five waiting kinds ->
# waiting) would delete this read and these globs outright. They would also
# RACE: the runner skips matcher filtering entirely when the extracted value is
# falsy, so a Notification whose notification_type is absent or EMPTY fires both
# blocks and the tab keeps whichever painted last, where today it keeps what it
# had. Today that needs a producer bug - the field is required - but the failure
# it trades for is nondeterministic colour, and this plugin's whole claim is that
# the colour is not a guess. One block listing the six kinds acted on here has no
# race, and buys the spawn for the eight kinds that are already no-ops: a handful
# of 2.5ms processes per session, in exchange for making the backstop's existence
# depend on a field name inside a binary this plugin does not control, and on a
# matcher that no test here can exercise end to end. Rejected on that trade. If
# the read ever becomes the bottleneck, this is the door.
#
# What no glob here can see: a dialog that blocks the session and notifies
# NOTHING. The LSP recommendation, the plugin hint and the auto-mode-default
# upsell are dialogs with no notification and no tool call, so no hook of any
# kind fires and the tab keeps whatever it last showed. The product's own 60s
# nudge cannot correct it either, because that notifier is gated on no dialog
# being on screen. That is the boundary of what hooks can see; it is in the
# README under "Known limitations" so the next reader does not call it a bug.
#
# SESSION-START. hooks.json's matcher already excludes `compact`; this is the
# belt to that brace, because the failure it prevents is the worst one in the
# table. An auto-compaction re-fires SessionStart MID-TURN, which would repaint
# the idle dot while Claude is still working AND arm the tab a second time with
# no matching unarm. Two globs, once per session, on a 769-byte payload - the
# only thing standing between that failure and a config line somebody edits,
# within the limit stated above: it catches the spelling Claude Code emits and
# the same line with a space after the colon, not a reformatted payload.
# ---------------------------------------------------------------------------
case $edge in
notify)
    case $payload in
    *'"notification_type":"idle_prompt"'*)
        edge=idle
        ;;
    *'"notification_type":"permission_prompt"'* \
        | *'"notification_type":"worker_permission_prompt"'* \
        | *'"notification_type":"agent_needs_input"'* \
        | *'"notification_type":"elicitation_dialog"'* \
        | *'"notification_type":"elicitation_url_dialog"'*)
        edge=waiting
        ;;
    *)
        # Not a state change. Emit nothing at all - not an empty title, which
        # would blank the tab - and do not spend the location walk on it.
        exit 0
        ;;
    esac
    ;;
session-start)
    case $payload in
    *'"source":"compact"'* | *'"source": "compact"'*) exit 0 ;;
    esac
    ;;
esac

# ---------------------------------------------------------------------------
# 1. LOCATION
#
#     streaming-browser@master       inside a git repo
#     srv:streaming-browser@master   the same session over ssh
#     ~/code/bug_fedora              not a repo: the home-relative path
#
# The asymmetry is deliberate. Inside a repo the branch is the thing that
# changes under you and the subdirectory is noise, so the subdirectory is not
# shown at all; outside a repo there is no branch, so the path itself has to
# carry the information. A LOCAL session gets no host prefix - the absence of
# one is itself the signal that this is the machine you are sitting at.
#
# Fork-free throughout: the repo is found by walking up with `[ -e ]`, HEAD is
# read with the `read` builtin, and everything else is parameter expansion.
# `git rev-parse` would be shorter and completely correct and would cost 15-40ms
# per call; this whole script costs ~2.5ms, and it runs on every PostToolUse,
# one per tool call. So: no `git`, at the price of reimplementing the two
# parts of its repository discovery that actually show up in a tab.
#
# Every `read` below puts `2>/dev/null` BEFORE the input redirection.
# Redirections are applied left to right, so that is the only order in which a
# missing or unreadable file stays silent.
# ---------------------------------------------------------------------------
[ -n "${PWD-}" ] || PWD=$(pwd 2>/dev/null)
# A relative or empty PWD would make every path operation below meaningless.
case ${PWD-} in
/*) ;;
*) PWD=/ ;;
esac
# Two paths, deliberately:
#
#   * $_phys, with every symlink resolved, is what the repository walk uses.
#     $PWD is the LOGICAL path, and a cwd reached through a symlink has no .git
#     on its logical ancestors, so walking that loses the repo and the branch
#     entirely - the whole payload of this feature - where git reports them
#     fine. `cd -P .` is the fork-free way to get the physical path: POSIX
#     makes it rewrite $PWD to a pathname with no symlink components, and it
#     does so in bash-as-sh, dash and busybox ash alike. CDPATH is cleared so
#     that a CDPATH from the environment cannot make `cd` print anything, and a
#     failure (an unreadable or deleted cwd) simply leaves the logical path.
#   * $PWD, still logical, is what the ~-abbreviation in 1c uses, because a
#     distro whose /home is a symlink to /var/home would otherwise lose the ~.
_logical=$PWD
CDPATH= cd -P . 2>/dev/null
case ${PWD-} in
/*) ;;
*) PWD=$_logical ;;
esac
_phys=$PWD
PWD=$_logical
# ${HOME%/} because a HOME carrying a trailing slash would otherwise miss the
# comparisons in 1c and render the home directory as its full path instead of ~.
_home=${HOME-}
_home=${_home%/}

# --- 1a. Read one candidate .git ------------------------------------------
# Sets $_branch from $1, and returns true only if that candidate really is a
# repository. A function, not inline code, because 1b has two callers; a shell
# function costs no fork. No `local`, so the names stay _-prefixed and global.
#
#   * .git may be a FILE holding "gitdir: <path>" - that is what a linked
#     worktree (git worktree add) and a submodule get - and that path may be
#     relative to the directory holding the .git file. The joined path is left
#     unnormalized (it can contain ..); the kernel resolves it.
#   * "ref: refs/heads/feature/tab-title" keeps its slashes: the branch is
#     everything after refs/heads/, not the last component.
#   * A raw object id means a detached HEAD, and renders as a 7-character short
#     sha.
#   * A missing trailing newline is fine, because `read` still assigns the
#     partial line, and a CRLF line ending is trimmed - a repo can be checked
#     out on a Windows share.
#
# "Is a repository" here means "HEAD parses". git also insists on objects/ and
# refs/; one readable file is cheaper and rules out what actually turns up in
# the wild, which is an empty .git directory left behind by some other tool.
# Getting this wrong is not academic: an empty /tmp/.git would otherwise make
# every path under /tmp render as `tmp`.
_cctab_head() {
    _gd=$1
    _branch=
    if [ -f "$_gd" ]; then
        _line=
        IFS= read -r _line 2>/dev/null < "$_gd"
        # A gitdir: line holds a path, so PATH_MAX bounds anything legitimate.
        # Longer means this is not a gitfile but some other file that happens to
        # be called .git, and the pattern work below is not free on a huge
        # string: bound it before touching it.
        [ "${#_line}" -le 4096 ] || return 1
        case $_line in *[[:cntrl:]]) _line=${_line%?} ;; esac
        case $_line in
        'gitdir: '?*)
            _gd=${_line#gitdir: }
            case $_gd in
            /*) ;;
            *) _gd=${1%/*}/$_gd ;;
            esac
            ;;
        *) return 1 ;;
        esac
    fi
    [ -f "${_gd%/}/HEAD" ] || return 1
    _head=
    IFS= read -r _head 2>/dev/null < "${_gd%/}/HEAD"
    # A real HEAD's first line is a 41-byte object id or a ref name; git's own
    # limit on a ref is well inside 255 bytes. Anything longer is not a HEAD,
    # and it must be rejected BEFORE the cuts below: `${_head%"${_head#???????}"}`
    # is quadratic in the length of $_head, so a 200KB first line stalls the
    # whole hook for seconds (measured: 100KB = 6.5s). Rejecting it here just
    # continues the walk, which is the same degradation as an unreadable HEAD.
    [ "${#_head}" -le 255 ] || return 1
    # Trim the trailing line noise: a CR from a Windows checkout, and the spaces
    # or tabs that git itself ignores after a ref name. Bounded by the guard
    # above, and only ever a handful of iterations in practice.
    while :; do
        case $_head in
        *[[:cntrl:][:blank:]]) _head=${_head%?} ;;
        *) break ;;
        esac
    done
    case $_head in
    'ref: '?*)
        _branch=${_head#ref: }
        # A symref normally points into refs/heads/. When it does not, keep the
        # namespace but drop the uninformative `refs/` prefix, so a HEAD left on
        # refs/remotes/origin/main reads `origin/main` and refs/tags/v1.0 reads
        # `tags/v1.0` instead of spending the whole tab budget on `refs/`.
        case $_branch in
        refs/heads/*) _branch=${_branch#refs/heads/} ;;
        refs/remotes/*) _branch=${_branch#refs/remotes/} ;;
        refs/*) _branch=${_branch#refs/} ;;
        esac
        ;;
    ???????*)
        # At least 7 long and all hex: an object id, so HEAD is detached.
        case $_head in
        *[!0-9a-fA-F]*) ;;
        *) _branch=${_head%"${_head#???????}"} ;;
        esac
        ;;
    esac
    [ -n "$_branch" ]
}

# --- 1b. Find the repository ----------------------------------------------
# On the way out, $_top is the working tree whose basename names the repo, and
# $_branch is its branch. An empty $_top means "not a repo".
_top=
_branch=
if [ -n "${GIT_DIR-}" ]; then
    # An explicit GIT_DIR wins over the walk, as it does for git, and a GIT_DIR
    # that is not a repository is not second-guessed by walking anyway.
    _abs=$GIT_DIR
    case $_abs in
    /*) ;;
    *) _abs=$PWD/$_abs ;;
    esac
    if _cctab_head "$_abs"; then
        # Name the repo after GIT_DIR's own location rather than after $PWD:
        # /w/repo/.git -> repo, and a bare /srv/repo.git -> repo.
        _top=${_abs%/}
        case $_top in
        */.git) _top=${_top%/.git} ;;
        *) _top=${_top%.git} ;;
        esac
    fi
else
    # Walk up looking for a .git that checks out. Bounded, so that no
    # pathological path can spin here: 64 components is far past any real tree,
    # and each step costs one stat and no fork. ${_dir%/} keeps the probe at /
    # from becoming "//.git", whose meaning POSIX leaves to the implementation.
    # $_phys, not $PWD: see the note above on symlinked working directories.
    _dir=$_phys
    _n=0
    while [ "$_n" -lt 64 ]; do
        _n=$((_n + 1))
        if [ -e "${_dir%/}/.git" ] && _cctab_head "${_dir%/}/.git"; then
            _top=$_dir
            break
        fi
        [ "$_dir" != / ] || break
        _dir=${_dir%/*}
        [ -n "$_dir" ] || _dir=/
    done
fi

# --- 1c. Compose ----------------------------------------------------------
if [ -n "$_top" ]; then
    place=${_top%/}
    place=${place##*/}
    # A GIT_DIR carrying dot components leaves one as the basename - `GIT_DIR=.`
    # inside a bare repo is a real idiom, and `.git/.` or `a/../.git` are easy to
    # type - and a tab labelled `.` or `..` says nothing. Name it after the
    # working directory instead, minus a `.git` suffix so that a bare repo still
    # reads the same as it does when GIT_DIR names it absolutely.
    case $place in
    '' | . | ..)
        place=${PWD%/}
        place=${place##*/}
        place=${place%.git}
        ;;
    esac
    # A repo checked out at / has no basename to show.
    [ -n "$place" ] || place='/'
    place=$place@$_branch
else
    # Not a repo: the home-relative path. ~ for HOME itself, ~/x/y beneath it,
    # and anything outside HOME stays absolute.
    place=$PWD
    if [ -n "$_home" ]; then
        if [ "$PWD" = "$_home" ]; then
            place='~'
        else
            case $PWD in
            "$_home"/*) place='~'${PWD#"$_home"} ;;
            esac
        fi
    fi
fi

# --- 1d. Length policy ----------------------------------------------------
# A tab is narrow. Konsole gives one roughly 49-60 columns and then elides from
# the LEFT; Windows Terminal truncates from the RIGHT. Neither default keeps
# the half you want, so cap it here and cut the uninformative end ourselves:
#
#   * a path is cut at the FRONT, on a component boundary, because the last
#     components are the ones that say where you are:
#         ~/code/one/two/three/four   ->  …/two/three/four
#   * a repo@branch is cut at the BACK, because the repo name is what
#     identifies the tab:
#         repo@some-very-long-branch  ->  repo@some-very-lo…
#
# Both of those cuts count by `?` and by ${#var}, so they are only ever applied
# to text that is pure printable ASCII, where the unit is unambiguous and every
# shell agrees. Elsewhere it is not: the unit is BYTES unless the shell has
# multibyte support AND the locale is UTF-8. Measured on `é`: bash-as-sh and
# busybox ash 1.37 count one character in a UTF-8 locale and two bytes in C;
# dash 0.5.12 counts two bytes in every locale, having no multibyte support at
# all. So the shell that elides a non-ASCII location soonest is dash, not ash,
# and any shell in the C locale does the same. Cutting `ééé…` by count where the
# unit is a byte would slice a UTF-8 sequence in half and put an invalid byte
# inside the JSON string literal - inventing exactly the corruption the
# sanitizer documents that it cannot fix. So a
# location carrying any non-ASCII byte is left at full length instead, and the
# terminal's own elision deals with it: Konsole cuts from the left, which keeps
# the same informative tail this policy wants. `*[!\ -~]*` is the test, and it
# agrees across bash, dash and ash in both the C and a UTF-8 locale, which
# `[[:print:]]` does not.
#
# The component peeling is safe for any bytes, because it only ever cuts at a
# `/`, so an over-long non-ASCII path still loses its leading components.
#
# The default cap is 32, which leaves room for the glyph, a host prefix and the
# terminal's own padding inside even the smallest of those budgets.
# CCTAB_MAX_LOCATION overrides it; 0 turns the cap off entirely, and a value
# below 8 is raised to 8 because less than that leaves nothing readable. The
# arithmetic budgets ONE column for the marker, which is what `…` costs to
# display; a CCTAB_ELLIPSIS of `...` therefore overshoots the cap by two. The
# unit is whatever ${#var} counts in the running shell, per the note above. The
# host prefix is added afterwards and deliberately not counted against this cap;
# it carries a cap of its own (CCTAB_MAX_HOST in 1e) instead, because it is the
# one part of the title that Windows Terminal - which truncates from the RIGHT -
# would otherwise keep while dropping everything that matters.
# Leading zeros are rejected rather than accepted: $((08)) is an illegal octal
# constant, which in dash aborts the script outright and would have rendered an
# empty title - the one outcome this script must never produce.
_max=${CCTAB_MAX_LOCATION-32}
case $_max in
0) _max= ;;
[1-9] | [1-9][0-9] | [1-9][0-9][0-9]) ;;
*) _max=32 ;;
esac
if [ -n "$_max" ] && [ "$_max" -lt 8 ]; then
    _max=8
fi
if [ -n "$_max" ] && [ "${#place}" -gt "$_max" ]; then
    _ell=${CCTAB_ELLIPSIS-…}
    if [ -n "$_top" ]; then
        # Keep the first $_max - 1 characters and spend the last one on the
        # marker. The nested expansion is the fork-free way to take a prefix:
        # build a pattern of N `?`, strip it to get the tail, then strip that
        # tail off the end.
        case $place in
        *[!\ -~]*) ;;
        *)
            _pat=
            _n=$((_max - 1))
            while [ "$_n" -gt 0 ]; do
                _pat=$_pat?
                _n=$((_n - 1))
            done
            place=${place%"${place#$_pat}"}$_ell
            ;;
        esac
    else
        # Drop whole leading components while that helps. The marker costs two
        # columns here, because a cut on a boundary reads as "…/", so the
        # budget the loop has to hit is $_max - 2.
        _sep=/
        while [ $((${#place} + 2)) -gt "$_max" ]; do
            _rest=${place#*/}
            if [ "$_rest" = "$place" ] || [ -z "$_rest" ]; then
                break
            fi
            place=$_rest
        done
        case $place in
        *[!\ -~]*) ;;
        *)
            if [ $((${#place} + 2)) -gt "$_max" ]; then
                # One component left and it still does not fit, so cut inside
                # it and drop the "/" from the marker: there is no boundary
                # left to mark, and that buys back the column it was using.
                # $_n can come out as 0, when only the "/" needed to go; the
                # loop then builds an empty pattern and the cut is a no-op,
                # which is correct.
                _pat=
                _n=$((${#place} + 1 - _max))
                while [ "$_n" -gt 0 ]; do
                    _pat=$_pat?
                    _n=$((_n - 1))
                done
                place=${place#$_pat}
                _sep=
            fi
            ;;
        esac
        place=$_ell$_sep$place
    fi
fi

# --- 1e. The ssh prefix ---------------------------------------------------
# Only when this really is an ssh session, because the absence of a prefix is
# how a local session is recognized. Fork-free first: /proc carries the
# hostname as a plain file. CCTAB_HOST overrides the detection outright (a
# short label beats a long FQDN in a tab); $HOSTNAME comes next, which bash
# sets and dash and ash do not; `hostname` is the last resort and the only
# forking branch in this whole block, reached only where /proc is absent.
#
# The domain goes in every case, so a.b.c renders as a - unless the name is all
# digits and dots, because chopping 192.168.1.5 to `192` would name nothing.
#
# Then a cap of its own. `${_h%%.*}` only removes a DOMAIN, and the hostnames one
# actually ssh into on cloud and k8s hosts are single labels with no dot at all,
# up to the kernel's 64 bytes: a 40-character host renders a 68-column title
# against a 49-60 column budget, and Windows Terminal truncates from the RIGHT,
# so the tab would show the host and lose the location entirely. Same cut and
# same ASCII guard as 1d. CCTAB_MAX_HOST overrides it and 0 turns it off.
#
# If the session says ssh but no name resolves, the prefix becomes a literal
# `ssh` rather than nothing: an empty prefix would render byte for byte like a
# local session and invert the one signal this design rests on.
if [ -n "${SSH_CONNECTION-}" ] || [ -n "${SSH_TTY-}" ]; then
    _h=${CCTAB_HOST-}
    if [ -z "$_h" ] && [ -r /proc/sys/kernel/hostname ]; then
        IFS= read -r _h 2>/dev/null < /proc/sys/kernel/hostname
    fi
    [ -n "$_h" ] || _h=${HOSTNAME-}
    if [ -z "$_h" ]; then
        _h=$(hostname 2>/dev/null) || _h=
    fi
    case $_h in
    *[!0-9.]*) _h=${_h%%.*} ;;
    esac
    _hmax=${CCTAB_MAX_HOST-16}
    case $_hmax in
    0) _hmax= ;;
    [1-9] | [1-9][0-9] | [1-9][0-9][0-9]) ;;
    *) _hmax=16 ;;
    esac
    if [ -n "$_hmax" ] && [ "$_hmax" -lt 4 ]; then
        _hmax=4
    fi
    if [ -n "$_hmax" ] && [ "${#_h}" -gt "$_hmax" ]; then
        case $_h in
        *[!\ -~]*) ;;
        *)
            _pat=
            _n=$((_hmax - 1))
            while [ "$_n" -gt 0 ]; do
                _pat=$_pat?
                _n=$((_n - 1))
            done
            _h=${_h%"${_h#$_pat}"}${CCTAB_ELLIPSIS-…}
            ;;
        esac
    fi
    [ -n "$_h" ] || _h=ssh
    place=$_h:$place
fi

# ---------------------------------------------------------------------------
# 2. Sanitize $place for a JSON string literal.
#
# `"` and `\` would break the literal. Control characters are illegal inside
# one, and a newline would also break the one-line-per-hook contract. Deleting
# them is enough - nothing downstream needs the original bytes. The `case`
# guard means this runs only for a pathological directory, branch or host name,
# never on the common path.
#
# The body is a pure-shell character loop rather than a `tr` pipeline: no
# fork, no dependency on how a given `tr` parses a character class embedded in
# a larger set, and a merely quote-bearing name degrades to `weird` instead of
# to `?` when PATH is broken.
#
# It is also QUADRATIC in the length of $place, because `${_rest#?}` copies the
# whole remainder on every iteration: measured 11ms at 256 characters, 293ms at
# 1024 and 2.2s at 2048 under bash-as-sh, and worse under busybox ash. The 1d
# cap does not bound the input, because 1d exempts a non-ASCII location from
# cutting by count - so a long accented name carrying one quote used to arrive
# here at full length and stall the hook for seconds. Hence the hard iteration
# bound below: 256 is far past anything a tab can show, and a location longer
# than that was pathological by construction.
#
# The `?` fallback below is now unreachable by construction: every location
# section 1 can produce carries a `/`, a `~` or an `@`, and none of those is
# stripped here. It stays as the belt to the braces, for whatever location form
# a later slice invents.
#
# NOT handled: a directory, branch or host name that is not valid UTF-8 - legal
# on Linux, and still illegal inside a JSON string. Documented under "Known
# limitations" rather than fixed, because the only cheap detector would also fork
# for every perfectly good accented or emoji name.
# ---------------------------------------------------------------------------
case $place in
*[\"\\]* | *[[:cntrl:]]*)
    _rest=$place
    place=
    _n=256
    while [ -n "$_rest" ]; do
        if [ "$_n" -le 0 ]; then
            # Bound reached. Drop any trailing non-ASCII unit before marking the
            # cut: where `?` counts bytes (dash, or any shell in the C locale)
            # the stop can land inside a multibyte sequence, and half a sequence
            # is precisely the invalid byte this section exists to keep out of
            # the JSON. At most a handful of bytes, so the loop is cheap.
            _n=8
            while [ "$_n" -gt 0 ]; do
                _n=$((_n - 1))
                case $place in
                *[!\ -~]) place=${place%?} ;;
                *) break ;;
                esac
            done
            place=$place${CCTAB_ELLIPSIS-…}
            break
        fi
        _n=$((_n - 1))
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
# 2b. Which terminal is rendering the tab?
#
# Hoisted here because the answer decides WHERE the glyph goes, not just
# whether the Konsole arming is sent later.
#
# $TMUX / $STY take the multiplexer case out: KONSOLE_* leaks into any child
# launched from a Konsole shell, and into every pane of a tmux server that was
# first started under Konsole, so inside a multiplexer those variables say
# nothing about the terminal actually drawing the tab.
# ---------------------------------------------------------------------------
konsole=
if [ -z "${TMUX-}" ] && [ -z "${STY-}" ]; then
    if [ -n "${KONSOLE_VERSION-}" ] || [ -n "${KONSOLE_DBUS_SESSION-}" ]; then
        konsole=1
    fi
fi

# ---------------------------------------------------------------------------
# 2c. Which END of the title does the glyph go on?
#
# Konsole's tab bar elides from the LEFT. That is
# QTabBar::setElideMode(Qt::ElideLeft) at a single hardcoded call site in
# libkonsoleprivate - no config key reads it, and elideMode is not something a
# Qt stylesheet can set - so a LEADING glyph is the first thing cut. Measured
# in the wild: a 23-cell title in a 19-cell tab rendered "...de-tabstatus@main"
# with the dot gone. The END of the string always survives, so under Konsole
# the glyph goes last.
#
# Windows Terminal truncates from the RIGHT, so there the glyph goes first.
# Prefix is also the safe default for any terminal we cannot identify, which
# includes every session reached over ssh: the local terminal's variables do
# not travel. Set CCTAB_GLYPH_POS in the remote shell to override, or use
# `both` to be immune to either direction for the price of two columns.
#
# An unrecognised value falls through to prefix rather than failing.
# ---------------------------------------------------------------------------
if [ -n "${CCTAB_GLYPH_POS-}" ]; then
    glyph_pos=$CCTAB_GLYPH_POS
elif [ -n "$konsole" ]; then
    glyph_pos=suffix
else
    glyph_pos=prefix
fi

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
    # version of the script does not know about yet. `notify` never reaches
    # here: 0b has already turned it into idle, into waiting, or into silence.
    glyph=${CCTAB_GLYPH_IDLE-⚪}
    ;;
esac

if [ "$edge" = session-end ]; then
    title=
elif [ -z "$glyph" ]; then
    title=$place
else
    case $glyph_pos in
    suffix) title="$place $glyph" ;;
    both) title="$glyph $place $glyph" ;;
    *) title="$glyph $place" ;;
    esac
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
    # $konsole was computed in section 2b, before the title was composed.

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
