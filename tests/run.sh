#!/bin/sh
# claude-tabstatus test suite.
#
#   sh tests/run.sh
#   CCTAB_TEST_BIN=/path/to/tabstatus sh tests/run.sh
#
# Dependency-free: no bats, no jq. Exits non-zero if anything fails.
#
# What is under test is bin/tabstatus, the SAME binary hooks/hooks.json invokes -
# not a build in target/, so a stale committed binary fails here rather than in
# somebody's tab. The suite used to run scripts/tabstatus.sh under three shells
# in four locales, because the implementation's answer depended on both; a binary
# has no interpreter and, since the length cap became locale-independent, no
# locale dependence either, so that whole axis is gone.
#
# Every assertion runs the binary through CCTAB_DRY_RUN=1, or with CLAUDE_PID
# unset, so nothing here can ever write an escape sequence to a real terminal.
#
# CLAUDE_PID is NOT unset globally - the headless-guard and pty sections need the
# ambient one, and removing it costs 34 assertions. The state section, which is the
# only other part that reads it, pins it per case instead: see the comment there.

here=$(cd -- "$(dirname -- "$0")" && pwd) || exit 1
repo=$(cd -- "$here/.." && pwd) || exit 1
bin=${CCTAB_TEST_BIN:-$repo/bin/tabstatus}
if [ ! -x "$bin" ]; then
    printf 'error: %s is missing or not executable.\n' "$bin" >&2
    printf '       Build it: sh scripts/build.sh\n' >&2
    exit 1
fi

tmp=$(mktemp -d) || exit 1
cleanup() { rm -rf "$tmp"; }
# Split: a signal trap does not abort a non-interactive shell, so a combined
# `trap cleanup EXIT HUP INT TERM` would clean up and then keep running.
trap cleanup EXIT
trap 'cleanup; exit 130' HUP INT TERM

# Section 2c picks which END of the title the glyph goes on, from the terminal.
# Left unpinned, this suite would render differently depending on where it is
# run - a Konsole tab elides from the left, so the glyph goes last there, and
# every assertion below would flip. Neutralise the detection here; the
# glyph-position section sets these explicitly, per case.
unset KONSOLE_VERSION KONSOLE_DBUS_SESSION TMUX STY CCTAB_GLYPH_POS
# And the state layer, for two reasons. XDG_RUNTIME_DIR is set in any real login
# session, so leaving it here would (a) write records into the user's own
# /run/user/<uid> from a test run and (b) make every assertion below whose stdin
# carries a `session_id` depend on what an earlier assertion left behind. The
# state section near the end sets CCTAB_STATE_DIR per case, under $tmp; every
# other assertion in this file is therefore the STATELESS answer, which is what
# makes it the byte-for-byte guard it was before the record existed.
unset XDG_RUNTIME_DIR CCTAB_STATE_DIR

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
#
# HOME is pinned to $tmp so that every fixture beneath it renders as a stable
# ~/... path. The expectations in this file must not depend on where mktemp
# happened to put us.
dry() {
    _edge=$1
    _dir=${2-$here}
    (cd -- "$_dir" 2>/dev/null && HOME=$tmp CCTAB_DRY_RUN=1 "$bin" "$_edge" </dev/null)
}

# mkrepo <dir> <HEAD contents> -- a hand-built repository
#
# `git` is never called: the product reads .git/HEAD and nothing else, so a
# directory plus a one-line file reproduce exactly what it sees - on any
# machine, with no git installed, and with the HEAD byte for byte under test.
# A cross-check against a real git runs at the end of this file when a git
# binary happens to be around.
mkrepo() {
    mkdir -p "$1/.git" || return 1
    printf '%s' "$2" >"$1/.git/HEAD"
}

# nest <base> <n> -- create <base>/a/a/... n deep, and print the deepest path
nest() {
    _p=$1
    _i=0
    while [ "$_i" -lt "$2" ]; do
        _p=$_p/a
        _i=$((_i + 1))
    done
    mkdir -p "$_p" || return 1
    printf '%s' "$_p"
}

printf 'binary under test: %s\n' "$bin"
printf 'version:           %s\n\n' "$("$bin" version)"

# Every path-form expectation below assumes $tmp is not itself inside a
# repository. It normally is not, because mktemp puts us under /tmp, but if
# TMPDIR pointed into a checkout the script would correctly report that repo
# and a dozen assertions would fail for a reason that is not a bug. Say it
# once, loudly, rather than let it look like a dozen bugs.
_anc=$tmp
_anc_repo=
while :; do
    # Validated the way the product validates, so that a stray EMPTY .git
    # directory - /tmp/.git exists on more machines than you would think - does
    # not report a repo that is not one.
    if [ -f "${_anc%/}/.git/HEAD" ] || [ -f "${_anc%/}/.git" ]; then
        _anc_repo=$_anc
        break
    fi
    [ "$_anc" != / ] || break
    _anc=${_anc%/*}
    [ -n "$_anc" ] || _anc=/
done
check 'the test tmpdir is not inside a git repo' '' "$_anc_repo"

# --- glyph per edge --------------------------------------------------------
mkdir -p "$tmp/plaindir"
check 'edge session-start -> idle glyph' '⚪ ~/plaindir' "$(dry session-start "$tmp/plaindir")"
check 'edge working'                     '🔵 ~/plaindir' "$(dry working "$tmp/plaindir")"
check 'edge waiting'                     '🟠 ~/plaindir' "$(dry waiting "$tmp/plaindir")"
check 'edge idle'                        '⚪ ~/plaindir' "$(dry idle "$tmp/plaindir")"
check 'edge session-end -> empty title'  ''             "$(dry session-end "$tmp/plaindir")"
check 'unknown edge falls back to idle'  '⚪ ~/plaindir' "$(dry no-such-edge "$tmp/plaindir")"
check 'missing edge argument -> idle'    '⚪ ~/plaindir' \
    "$(cd -- "$tmp/plaindir" && HOME=$tmp CCTAB_DRY_RUN=1 "$bin" </dev/null)"

# --- location: not a repo, so the path -------------------------------------
# The asymmetry with the repo form below is the design: no branch to show means
# the path has to carry the information, so it is the whole home-relative path
# and not just the basename.
mkdir -p "$tmp/code/bug_fedora"
check 'a non-repo renders the home-relative path' '⚪ ~/code/bug_fedora' \
    "$(dry idle "$tmp/code/bug_fedora")"
check '$PWD == $HOME renders as ~' '⚪ ~' "$(dry idle "$tmp")"
check 'a HOME with a trailing slash still renders as ~' '⚪ ~' \
    "$(cd -- "$tmp" && HOME=$tmp/ CCTAB_DRY_RUN=1 "$bin" idle </dev/null)"
check 'a path outside HOME stays absolute' "⚪ $tmp/plaindir" \
    "$(cd -- "$tmp/plaindir" && HOME=/nonexistent-home CCTAB_MAX_LOCATION=0 CCTAB_DRY_RUN=1 "$bin" idle </dev/null)"
check 'root renders as /' '⚪ /' \
    "$(cd -- / && HOME=/nonexistent-home CCTAB_DRY_RUN=1 "$bin" idle </dev/null)"
check 'an empty HOME does not produce a stray ~' "⚪ $tmp/plaindir" \
    "$(cd -- "$tmp/plaindir" && HOME= CCTAB_MAX_LOCATION=0 CCTAB_DRY_RUN=1 "$bin" idle </dev/null)"
# A HOME *prefix* is not a HOME component: /tmp/x/homeless is not under /tmp/x/home.
mkdir -p "$tmp/homeprefix" "$tmp/homeprefixed"
check 'a sibling whose name only starts with HOME is not abbreviated' \
    "⚪ $tmp/homeprefixed" \
    "$(cd -- "$tmp/homeprefixed" && HOME=$tmp/homeprefix CCTAB_MAX_LOCATION=0 CCTAB_DRY_RUN=1 "$bin" idle </dev/null)"

# --- location: inside a repo ----------------------------------------------
mkrepo "$tmp/repos/plain" 'ref: refs/heads/master
'
check 'a repo renders repo@branch' '⚪ plain@master' "$(dry idle "$tmp/repos/plain")"
mkdir -p "$tmp/repos/plain/deep/er/still"
check 'a subdirectory of a repo shows the repo, not the subdirectory' '⚪ plain@master' \
    "$(dry idle "$tmp/repos/plain/deep/er/still")"
mkrepo "$tmp/repos/slashy" 'ref: refs/heads/feature/tab-title
'
check 'a branch name keeps its slashes' '⚪ slashy@feature/tab-title' \
    "$(dry idle "$tmp/repos/slashy")"
mkrepo "$tmp/repos/nonl" 'ref: refs/heads/no-newline'
check 'a HEAD with no trailing newline still parses' '⚪ nonl@no-newline' \
    "$(dry idle "$tmp/repos/nonl")"
mkrepo "$tmp/repos/crlf" "$(printf 'ref: refs/heads/crlf-branch\r')"
check 'a CRLF HEAD loses the CR' '⚪ crlf@crlf-branch' "$(dry idle "$tmp/repos/crlf")"
mkrepo "$tmp/repos/detached" '0123456789abcdef0123456789abcdef01234567
'
check 'a detached HEAD renders a 7-char short sha' '⚪ detached@0123456' \
    "$(dry idle "$tmp/repos/detached")"
# git's sha256 repositories have 64-hex object ids; the same rule applies.
mkrepo "$tmp/repos/sha256" '0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef
'
check 'a 64-hex detached HEAD also renders 7 chars' '⚪ sha256@0123456' \
    "$(dry idle "$tmp/repos/sha256")"
# An uppercase object id is still an object id.
mkrepo "$tmp/repos/upper" 'ABCDEF0123456789ABCDEF0123456789ABCDEF01
'
check 'an uppercase detached HEAD parses' '⚪ upper@ABCDEF0' "$(dry idle "$tmp/repos/upper")"
# A ref that is not under refs/heads (a bisect or a rebase leaves these) keeps
# its namespace rather than being silently mistaken for a branch name, but loses
# the `refs/` prefix, which is pure overhead in a tab: `refs/remotes/origin/main`
# would otherwise spend the budget before reaching the informative part and then
# be elided mid-word.
mkrepo "$tmp/repos/otherref" 'ref: refs/bisect/bad
'
check 'a ref outside refs/heads keeps its namespace' '⚪ otherref@bisect/bad' \
    "$(dry idle "$tmp/repos/otherref")"
mkrepo "$tmp/repos/remoteref" 'ref: refs/remotes/origin/main
'
check 'a remote-tracking HEAD drops refs/remotes/' '⚪ remoteref@origin/main' \
    "$(dry idle "$tmp/repos/remoteref")"
mkrepo "$tmp/repos/tagref" 'ref: refs/tags/v1.0
'
check 'a tag HEAD drops refs/ and keeps tags/' '⚪ tagref@tags/v1.0' \
    "$(dry idle "$tmp/repos/tagref")"
# git ignores blanks after the ref name; so must the tab, or a trailing space
# rides into the title and into the JSON literal.
mkrepo "$tmp/repos/trailws" 'ref: refs/heads/master
'
check 'trailing blanks after a ref name are trimmed' '⚪ trailws@master' \
    "$(dry idle "$tmp/repos/trailws")"
# A HEAD whose first line is absurdly long is not a HEAD. It must be rejected
# before the short-sha cut, which is quadratic in the length of the line: a
# 200KB line used to stall the hook past 15s. Rejecting it continues the walk,
# so this renders as a path.
mkrepo "$tmp/repos/hugehead" ''
_huge=aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa
_i=0
while [ "$_i" -lt 6 ]; do
    _huge=$_huge$_huge
    _i=$((_i + 1))
done
printf '%s\n' "$_huge" >"$tmp/repos/hugehead/.git/HEAD"
check 'an over-long HEAD line degrades to the path' '⚪ ~/repos/hugehead' \
    "$(dry idle "$tmp/repos/hugehead")"
# The nearest repo wins: a repo checked out inside another one is where you are.
mkrepo "$tmp/repos/plain/inner" 'ref: refs/heads/inner-branch
'
check 'the innermost repo wins' '⚪ inner@inner-branch' \
    "$(dry idle "$tmp/repos/plain/inner")"

# --- location: .git that is not a directory -------------------------------
# A linked worktree (git worktree add) writes an ABSOLUTE gitdir; a submodule
# writes a RELATIVE one. Both shapes are what real git produces - verified
# against a real git in the cross-check at the end of this file.
mkdir -p "$tmp/wtstore/worktrees/wt-linked"
printf 'ref: refs/heads/feature/tab-title\n' >"$tmp/wtstore/worktrees/wt-linked/HEAD"
mkdir -p "$tmp/wt-linked"
printf 'gitdir: %s\n' "$tmp/wtstore/worktrees/wt-linked" >"$tmp/wt-linked/.git"
check 'a worktree .git file with an absolute gitdir' '⚪ wt-linked@feature/tab-title' \
    "$(dry idle "$tmp/wt-linked")"
mkdir -p "$tmp/super/.git/modules/mysub" "$tmp/super/mysub"
printf 'ref: refs/heads/master\n' >"$tmp/super/.git/HEAD"
printf 'ref: refs/heads/sub-branch\n' >"$tmp/super/.git/modules/mysub/HEAD"
printf 'gitdir: ../.git/modules/mysub\n' >"$tmp/super/mysub/.git"
check 'a submodule .git file with a relative gitdir' '⚪ mysub@sub-branch' \
    "$(dry idle "$tmp/super/mysub")"
check 'the superproject of that submodule is unaffected' '⚪ super@master' \
    "$(dry idle "$tmp/super")"
# A gitdir pointer with no trailing newline, and one with a CRLF.
mkdir -p "$tmp/wt-nonl"
printf 'gitdir: %s' "$tmp/wtstore/worktrees/wt-linked" >"$tmp/wt-nonl/.git"
check 'a gitdir pointer with no trailing newline' '⚪ wt-nonl@feature/tab-title' \
    "$(dry idle "$tmp/wt-nonl")"
mkdir -p "$tmp/wt-crlf"
printf 'gitdir: %s\r\n' "$tmp/wtstore/worktrees/wt-linked" >"$tmp/wt-crlf/.git"
check 'a gitdir pointer with a CRLF line ending' '⚪ wt-crlf@feature/tab-title' \
    "$(dry idle "$tmp/wt-crlf")"
# A .git symlink to a real git directory.
mkdir -p "$tmp/symhost/.git"
printf 'ref: refs/heads/sym-branch\n' >"$tmp/symhost/.git/HEAD"
mkdir -p "$tmp/symrepo"
ln -s "$tmp/symhost/.git" "$tmp/symrepo/.git" 2>/dev/null
check 'a .git symlink is followed' '⚪ symrepo@sym-branch' "$(dry idle "$tmp/symrepo")"

# --- location: a cwd reached through a symlink -----------------------------
# $PWD is the LOGICAL path, so walking it would leave the repo entirely when the
# symlink points at a SUBDIRECTORY of one - the repo and the branch, which are
# the whole payload, replaced by a path. The walk therefore runs on the physical
# path (`cd -P .`, fork-free), and names the repo after the real toplevel rather
# than after the symlink.
mkdir -p "$tmp/repos/plain/sub/deeper"
ln -s "$tmp/repos/plain/sub" "$tmp/link-into-repo" 2>/dev/null
check 'a symlink into a repo subdirectory still finds the repo' '⚪ plain@master' \
    "$(dry idle "$tmp/link-into-repo")"
ln -s "$tmp/repos/plain" "$tmp/link-to-top" 2>/dev/null
check 'a symlink to a repo toplevel names the real repo' '⚪ plain@master' \
    "$(dry idle "$tmp/link-to-top")"
# ...while the ~ abbreviation keeps using the LOGICAL path, so a distro whose
# /home is a symlink to /var/home does not lose its ~.
ln -s "$tmp/plaindir" "$tmp/link-to-plain" 2>/dev/null
check 'the path form renders the logical path, not the resolved one' '⚪ ~/link-to-plain' \
    "$(dry idle "$tmp/link-to-plain")"

# --- location: a .git that is not a repository ----------------------------
# git validates a candidate and keeps walking if it fails, and so must this:
# an empty .git directory left behind by some other tool (an empty /tmp/.git
# really does exist in the wild) must not turn every path under it into a repo.
mkdir -p "$tmp/junk/emptygit/.git"
check 'an empty .git directory is not a repo' "⚪ ~/junk/emptygit" \
    "$(dry idle "$tmp/junk/emptygit")"
mkdir -p "$tmp/junk/garbagegit"
printf 'this is not a gitdir pointer\n' >"$tmp/junk/garbagegit/.git"
check 'a .git file that is not a gitdir pointer is not a repo' "⚪ ~/junk/garbagegit" \
    "$(dry idle "$tmp/junk/garbagegit")"
mkdir -p "$tmp/junk/stalegit"
printf 'gitdir: %s/junk/no-such-gitdir\n' "$tmp" >"$tmp/junk/stalegit/.git"
check 'a stale gitdir pointer is not a repo' "⚪ ~/junk/stalegit" \
    "$(dry idle "$tmp/junk/stalegit")"
mkdir -p "$tmp/junk/emptyhead/.git"
: >"$tmp/junk/emptyhead/.git/HEAD"
check 'an empty HEAD is not a repo' "⚪ ~/junk/emptyhead" \
    "$(dry idle "$tmp/junk/emptyhead")"
mkdir -p "$tmp/junk/dirhead/.git/HEAD"
check 'a HEAD that is a directory is not a repo' "⚪ ~/junk/dirhead" \
    "$(dry idle "$tmp/junk/dirhead")"
mkdir -p "$tmp/junk/shorthead/.git"
printf 'abc123\n' >"$tmp/junk/shorthead/.git/HEAD"
check 'a HEAD too short to be an object id is not a repo' "⚪ ~/junk/shorthead" \
    "$(dry idle "$tmp/junk/shorthead")"
# ...and a bad .git does not hide a good one further up.
mkdir -p "$tmp/repos/plain/hasjunk/.git"
check 'a junk .git inside a repo falls through to the repo' '⚪ plain@master' \
    "$(dry idle "$tmp/repos/plain/hasjunk")"

# --- location: the upward walk is bounded ---------------------------------
# The bound is 64 probes, the first of which is $PWD itself, so a repo 63
# levels up is the last one that can be found.
_r=$tmp/bound
mkrepo "$_r" 'ref: refs/heads/at-the-top
'
_deep=$(nest "$_r" 63)
check 'a repo 63 levels up is still found' '⚪ bound@at-the-top' "$(dry idle "$_deep")"
_deep=$(nest "$_r" 64)
check 'a repo 64 levels up is past the bound' '⚪ …/a/a/a/a/a/a/a/a/a/a/a/a/a/a/a' \
    "$(dry idle "$_deep")"

# --- location: GIT_DIR overrides the walk ---------------------------------
check 'GIT_DIR (absolute) overrides the walk' '⚪ plain@master' \
    "$(cd -- "$tmp/plaindir" && GIT_DIR=$tmp/repos/plain/.git HOME=$tmp CCTAB_DRY_RUN=1 "$bin" idle </dev/null)"
check 'GIT_DIR (relative) is resolved against $PWD' '⚪ plain@master' \
    "$(cd -- "$tmp/repos/plain" && GIT_DIR=.git HOME=$tmp CCTAB_DRY_RUN=1 "$bin" idle </dev/null)"
mkdir -p "$tmp/bare/proj.git"
printf 'ref: refs/heads/bare-branch\n' >"$tmp/bare/proj.git/HEAD"
check 'GIT_DIR on a bare repo drops the .git suffix' '⚪ proj@bare-branch' \
    "$(cd -- "$tmp/plaindir" && GIT_DIR=$tmp/bare/proj.git HOME=$tmp CCTAB_DRY_RUN=1 "$bin" idle </dev/null)"
check 'a GIT_DIR that is not a repo falls back to the path' '⚪ ~/plaindir' \
    "$(cd -- "$tmp/plaindir" && GIT_DIR=$tmp/junk/no-such-gitdir HOME=$tmp CCTAB_DRY_RUN=1 "$bin" idle </dev/null)"
check 'an empty GIT_DIR is ignored, not honoured' '⚪ plain@master' \
    "$(cd -- "$tmp/repos/plain" && GIT_DIR= HOME=$tmp CCTAB_DRY_RUN=1 "$bin" idle </dev/null)"
# A GIT_DIR carrying dot components would otherwise put a bare `.` or `..` in the
# tab, and `GIT_DIR=.` inside a bare repo is a real idiom. Name it after the
# working directory in that case, minus a `.git` suffix so that a bare repo reads
# the same as it does when GIT_DIR names it absolutely.
check 'GIT_DIR=. in a bare repo does not render as a dot' '⚪ proj@bare-branch' \
    "$(cd -- "$tmp/bare/proj.git" && GIT_DIR=. HOME=$tmp CCTAB_DRY_RUN=1 "$bin" idle </dev/null)"
check 'a GIT_DIR with a /./ component names the repo' '⚪ plain@master' \
    "$(cd -- "$tmp/repos/plain" && GIT_DIR=$tmp/repos/plain/./.git HOME=$tmp CCTAB_DRY_RUN=1 "$bin" idle </dev/null)"
check 'a GIT_DIR with a /../ component names the repo' '⚪ plain@master' \
    "$(cd -- "$tmp/repos/plain" && GIT_DIR=$tmp/repos/plain/sub/../.git HOME=$tmp CCTAB_DRY_RUN=1 "$bin" idle </dev/null)"
check 'a GIT_DIR of .git/. names the repo' '⚪ plain@master' \
    "$(cd -- "$tmp/repos/plain" && GIT_DIR=.git/. HOME=$tmp CCTAB_DRY_RUN=1 "$bin" idle </dev/null)"

# --- location: $PWD comes from the environment ----------------------------
# A hook subprocess inherits its environment, so $PWD can arrive stale, absent
# or nonsense. Every shell this runs under verifies $PWD against the real cwd
# at startup and replaces it when it does not match, which is what makes the
# whole block able to trust it - assert that rather than assume it.
check 'a stale PWD naming a real directory is not trusted' '⚪ plain@master' \
    "$(cd -- "$tmp/repos/plain" && PWD=/etc HOME=$tmp CCTAB_DRY_RUN=1 "$bin" idle </dev/null)"
check 'a relative PWD is not trusted' '⚪ plain@master' \
    "$(cd -- "$tmp/repos/plain" && PWD=relative HOME=$tmp CCTAB_DRY_RUN=1 "$bin" idle </dev/null)"
check 'a PWD naming a directory that does not exist is not trusted' '⚪ plain@master' \
    "$(cd -- "$tmp/repos/plain" && PWD=/no/such/dir HOME=$tmp CCTAB_DRY_RUN=1 "$bin" idle </dev/null)"
check 'an unset PWD still resolves' '⚪ plain@master' \
    "$(cd -- "$tmp/repos/plain" && unset PWD; HOME=$tmp CCTAB_DRY_RUN=1 "$bin" idle </dev/null)"

# --- location: the length cap ---------------------------------------------
# A path is cut at the FRONT on a component boundary, because the trailing
# components say where you are, and both Konsole (which elides from the left)
# and Windows Terminal (which truncates from the right) then show the same
# informative end.
mkdir -p "$tmp/one/two/three/four/five/six/seven/eight"
check 'a long path is elided from the left, on a boundary' '⚪ …/four/five/six/seven/eight' \
    "$(dry idle "$tmp/one/two/three/four/five/six/seven/eight")"
check 'CCTAB_MAX_LOCATION=0 turns the cap off' "⚪ ~/one/two/three/four/five/six/seven/eight" \
    "$(cd -- "$tmp/one/two/three/four/five/six/seven/eight" && CCTAB_MAX_LOCATION=0 HOME=$tmp CCTAB_DRY_RUN=1 "$bin" idle </dev/null)"
check 'CCTAB_MAX_LOCATION widens the cap' "⚪ …/two/three/four/five/six/seven/eight" \
    "$(cd -- "$tmp/one/two/three/four/five/six/seven/eight" && CCTAB_MAX_LOCATION=40 HOME=$tmp CCTAB_DRY_RUN=1 "$bin" idle </dev/null)"
check 'CCTAB_MAX_LOCATION narrows the cap' '⚪ …/seven/eight' \
    "$(cd -- "$tmp/one/two/three/four/five/six/seven/eight" && CCTAB_MAX_LOCATION=16 HOME=$tmp CCTAB_DRY_RUN=1 "$bin" idle </dev/null)"
check 'a garbage CCTAB_MAX_LOCATION falls back to the default' '⚪ …/four/five/six/seven/eight' \
    "$(cd -- "$tmp/one/two/three/four/five/six/seven/eight" && CCTAB_MAX_LOCATION=lots HOME=$tmp CCTAB_DRY_RUN=1 "$bin" idle </dev/null)"
# A leading zero is an illegal octal constant to the shell's arithmetic, which
# would abort the script and emit an empty title; it has to be rejected, not
# clamped. A three-digit 032 is legal arithmetic but still not what anyone
# meant, so it gets the default too.
check 'a leading-zero cap is rejected, not evaluated' '⚪ …/four/five/six/seven/eight' \
    "$(cd -- "$tmp/one/two/three/four/five/six/seven/eight" && CCTAB_MAX_LOCATION=08 HOME=$tmp CCTAB_DRY_RUN=1 "$bin" idle </dev/null)"
check 'a leading-zero cap writes nothing to stderr' '' \
    "$(cd -- "$tmp/one/two/three/four/five/six/seven/eight" && CCTAB_MAX_LOCATION=08 HOME=$tmp CCTAB_DRY_RUN=1 "$bin" idle </dev/null 2>&1 >/dev/null)"
(cd -- "$tmp/plaindir" && CCTAB_MAX_LOCATION=08 HOME=$tmp CCTAB_DRY_RUN=1 "$bin" idle </dev/null >/dev/null 2>&1)
check 'a leading-zero cap still exits 0' '0' "$?"
check 'a padded cap like 032 is rejected too' '⚪ …/four/five/six/seven/eight' \
    "$(cd -- "$tmp/one/two/three/four/five/six/seven/eight" && CCTAB_MAX_LOCATION=032 HOME=$tmp CCTAB_DRY_RUN=1 "$bin" idle </dev/null)"
check 'a cap below 8 is raised to 8' '⚪ …/eight' \
    "$(cd -- "$tmp/one/two/three/four/five/six/seven/eight" && CCTAB_MAX_LOCATION=2 HOME=$tmp CCTAB_DRY_RUN=1 "$bin" idle </dev/null)"
# One component that does not fit has no boundary left to cut on, so it is cut
# inside, and the marker loses its slash to say so.
mkdir -p "$tmp/aaaaaaaaaabbbbbbbbbbccccccccccdddddddddd"
check 'one over-long component is cut inside itself' '⚪ …abbbbbbbbbbccccccccccdddddddddd' \
    "$(dry idle "$tmp/aaaaaaaaaabbbbbbbbbbccccccccccdddddddddd")"
# A count-based cut must never land inside a UTF-8 sequence. The unit `?` and
# A non-ASCII location is cut like any other. The shell could not do this: its
# ${#var} counted BYTES unless the shell had multibyte support and the locale was
# UTF-8 - bash and busybox ash counted characters in a UTF-8 locale, dash counted
# bytes always, every shell counted bytes in C - so cutting by count risked
# leaving half a UTF-8 sequence in the JSON string, and the implementation
# exempted any location carrying a non-ASCII byte from the cap entirely. That was
# the case a narrow tab needed most. The binary decodes UTF-8 itself, so the unit
# is a character in every locale and the exemption is gone (README limitations 3
# and 4). The cut is on a character boundary by construction.
mkdir -p "$tmp/ééééééééééééééééééééééééééééééééééééé"
check 'a long non-ASCII component is cut like an ASCII one' '⚪ …ééééééééééééééééééééééééééééééé' \
    "$(dry idle "$tmp/ééééééééééééééééééééééééééééééééééééé")"
check 'the cut lands on a character boundary, so it is 32 characters' '32' \
    "$(dry idle "$tmp/ééééééééééééééééééééééééééééééééééééé" | sed 's/^⚪ //' | LC_ALL=C.UTF-8 awk '{print length($0)}')"
mkrepo "$tmp/repos/accentlong" 'ref: refs/heads/ééééééééééééééééééééééééééééééééééééé
'
check 'a long non-ASCII branch is cut at the back like an ASCII one' '⚪ accentlong@éééééééééééééééééééé…' \
    "$(dry idle "$tmp/repos/accentlong")"
# The same answer in the C locale, which is the half of the fix the corpus cases
# maxloc-nonascii-*-c-locale pin: LANG cannot change a title any more.
check 'LC_ALL=C gives the same answer as C.UTF-8' '⚪ …ééééééééééééééééééééééééééééééé' \
    "$(cd -- "$tmp/ééééééééééééééééééééééééééééééééééééé" && LC_ALL=C HOME=$tmp CCTAB_DRY_RUN=1 "$bin" idle </dev/null)"
check 'and so does an uninstalled UTF-8 locale name' '⚪ …ééééééééééééééééééééééééééééééé' \
    "$(cd -- "$tmp/ééééééééééééééééééééééééééééééééééééé" && LC_ALL=xx_YY.UTF-8 HOME=$tmp CCTAB_DRY_RUN=1 "$bin" idle </dev/null)"

check 'CCTAB_ELLIPSIS overrides the marker' '⚪ .../four/five/six/seven/eight' \
    "$(cd -- "$tmp/one/two/three/four/five/six/seven/eight" && CCTAB_ELLIPSIS=... HOME=$tmp CCTAB_DRY_RUN=1 "$bin" idle </dev/null)"
# A repo is cut at the BACK instead: the repo name is what identifies the tab,
# so the branch is what gives way.
mkrepo "$tmp/repos/longbranch" 'ref: refs/heads/some-very-long-branch-name-indeed
'
check 'a long repo@branch is cut at the back' '⚪ longbranch@some-very-long-branc…' \
    "$(dry idle "$tmp/repos/longbranch")"
check 'a repo@branch under the cap is untouched' '⚪ plain@master' "$(dry idle "$tmp/repos/plain")"

# --- location: the ssh prefix --------------------------------------------
# A prefix means "not this machine". A local session gets none, which is the
# whole signal, so the default must stay bare.
check 'a local session gets no host prefix' '⚪ plain@master' "$(dry idle "$tmp/repos/plain")"
check 'SSH_TTY adds the host prefix' '⚪ srv:plain@master' \
    "$(cd -- "$tmp/repos/plain" && SSH_TTY=/dev/pts/9 CCTAB_HOST=srv HOME=$tmp CCTAB_DRY_RUN=1 "$bin" idle </dev/null)"
check 'SSH_CONNECTION alone also adds it' '⚪ srv:plain@master' \
    "$(cd -- "$tmp/repos/plain" && SSH_CONNECTION='10.0.0.1 22 10.0.0.2 22' CCTAB_HOST=srv HOME=$tmp CCTAB_DRY_RUN=1 "$bin" idle </dev/null)"
check 'an empty SSH_TTY is not an ssh session' '⚪ plain@master' \
    "$(cd -- "$tmp/repos/plain" && SSH_TTY= SSH_CONNECTION= CCTAB_HOST=srv HOME=$tmp CCTAB_DRY_RUN=1 "$bin" idle </dev/null)"
check 'the domain is stripped from the hostname' '⚪ srv:plain@master' \
    "$(cd -- "$tmp/repos/plain" && SSH_TTY=x CCTAB_HOST=srv.example.com HOME=$tmp CCTAB_DRY_RUN=1 "$bin" idle </dev/null)"
check 'the ssh prefix also applies to a path location' '⚪ srv:~/plaindir' \
    "$(cd -- "$tmp/plaindir" && SSH_TTY=x CCTAB_HOST=srv HOME=$tmp CCTAB_DRY_RUN=1 "$bin" idle </dev/null)"
# The prefix is added after the cap and deliberately not counted by it: the
# host must not be the thing that gets eaten.
check 'the host prefix is not eaten by the cap' '⚪ srv:…/four/five/six/seven/eight' \
    "$(cd -- "$tmp/one/two/three/four/five/six/seven/eight" && SSH_TTY=x CCTAB_HOST=srv HOME=$tmp CCTAB_DRY_RUN=1 "$bin" idle </dev/null)"
# With no CCTAB_HOST, Linux answers from /proc, with no fork. Compare against
# what this machine says rather than hard-coding a hostname.
if [ -r /proc/sys/kernel/hostname ]; then
    IFS= read -r _realhost 2>/dev/null </proc/sys/kernel/hostname
    check 'the hostname comes from /proc when CCTAB_HOST is unset' "⚪ ${_realhost%%.*}:plain@master" \
        "$(cd -- "$tmp/repos/plain" && SSH_TTY=x HOME=$tmp CCTAB_DRY_RUN=1 "$bin" idle </dev/null)"
    check 'reading the hostname needs no external command' "⚪ ${_realhost%%.*}:plain@master" \
        "$(cd -- "$tmp/repos/plain" && SSH_TTY=x HOME=$tmp PATH= CCTAB_DRY_RUN=1 "$bin" idle </dev/null)"
else
    printf 'SKIP  /proc hostname assertions (no /proc/sys/kernel/hostname)\n'
fi
# `%%.*` only removes a DOMAIN, and the hosts one ssh into on cloud and k8s
# machines are single labels up to 64 bytes long. Uncapped, the prefix eats the
# whole tab and Windows Terminal - which truncates from the right - would show the
# host and nothing else. So the host has a cap of its own.
_k8s=my-cluster-worker-pool-a-7f9d8c6b5-x2kqz
check 'a long single-label host is capped' '⚪ my-cluster-work…:plain@master' \
    "$(cd -- "$tmp/repos/plain" && SSH_TTY=x CCTAB_HOST=$_k8s HOME=$tmp CCTAB_DRY_RUN=1 "$bin" idle </dev/null)"
check 'CCTAB_MAX_HOST narrows the host cap' '⚪ my-clu…:plain@master' \
    "$(cd -- "$tmp/repos/plain" && SSH_TTY=x CCTAB_MAX_HOST=7 CCTAB_HOST=$_k8s HOME=$tmp CCTAB_DRY_RUN=1 "$bin" idle </dev/null)"
check 'CCTAB_MAX_HOST=0 turns the host cap off' "⚪ $_k8s:plain@master" \
    "$(cd -- "$tmp/repos/plain" && SSH_TTY=x CCTAB_MAX_HOST=0 CCTAB_HOST=$_k8s HOME=$tmp CCTAB_DRY_RUN=1 "$bin" idle </dev/null)"
check 'a garbage CCTAB_MAX_HOST falls back to the default' '⚪ my-cluster-work…:plain@master' \
    "$(cd -- "$tmp/repos/plain" && SSH_TTY=x CCTAB_MAX_HOST=08 CCTAB_HOST=$_k8s HOME=$tmp CCTAB_DRY_RUN=1 "$bin" idle </dev/null)"
check 'a host under the cap is untouched' '⚪ build-runner-eu:plain@master' \
    "$(cd -- "$tmp/repos/plain" && SSH_TTY=x CCTAB_HOST=build-runner-eu HOME=$tmp CCTAB_DRY_RUN=1 "$bin" idle </dev/null)"
# A dotted quad is not host.domain: chopping 192.168.1.5 to `192` names nothing.
check 'an all-digits-and-dots host keeps its dots' '⚪ 192.168.1.5:plain@master' \
    "$(cd -- "$tmp/repos/plain" && SSH_TTY=x CCTAB_MAX_HOST=0 CCTAB_HOST=192.168.1.5 HOME=$tmp CCTAB_DRY_RUN=1 "$bin" idle </dev/null)"
# An ssh session whose hostname resolves to nothing must not render byte for byte
# like a local one: that would invert the one signal the whole design rests on.
check 'an ssh session with no resolvable host says ssh' '⚪ ssh:plain@master' \
    "$(cd -- "$tmp/repos/plain" && SSH_TTY=x CCTAB_HOST=.example.com HOME=$tmp CCTAB_DRY_RUN=1 "$bin" idle </dev/null)"

# --- location: no forks on the hot path ----------------------------------
# Slice 3 runs this script on every PostToolUse, so the location must cost no
# process. An empty PATH is the cheap proof: `git`, `hostname` and `basename`
# would all be unreachable, and the answer must still be right.
check 'a repo location needs no external command' '⚪ plain@master' \
    "$(cd -- "$tmp/repos/plain" && PATH= HOME=$tmp CCTAB_DRY_RUN=1 "$bin" idle </dev/null)"
check 'a path location needs no external command' '⚪ ~/code/bug_fedora' \
    "$(cd -- "$tmp/code/bug_fedora" && PATH= HOME=$tmp CCTAB_DRY_RUN=1 "$bin" idle </dev/null)"
check 'an elided path needs no external command' '⚪ …/four/five/six/seven/eight' \
    "$(cd -- "$tmp/one/two/three/four/five/six/seven/eight" && PATH= HOME=$tmp CCTAB_DRY_RUN=1 "$bin" idle </dev/null)"

# --- JSON-hostile names ---------------------------------------------------
# The sanitizer is slice 1's, but slice 2 gave it three new ways to be fed: a
# branch name and a hostname can carry hostile bytes too, and a hand-written
# HEAD is not bound by git's own ref-name rules.
mkdir -p "$tmp/we\"ird"
check 'double quote is stripped from a path location' '⚪ ~/weird' "$(dry idle "$tmp/we\"ird")"
mkdir -p "$tmp/back\\slash"
check 'backslash is stripped from a path location' '⚪ ~/backslash' "$(dry idle "$tmp/back\\slash")"
mkdir -p "$tmp/both\"x\\y"
check 'quote and backslash together are stripped' '⚪ ~/bothxy' "$(dry idle "$tmp/both\"x\\y")"
nlname=$(printf 'new\nline')
mkdir -p "$tmp/$nlname"
check 'a newline in a path location is stripped' '⚪ ~/newline' "$(dry idle "$tmp/$nlname")"
mkdir -p "$tmp/\"\\"
check 'a name made only of hostile bytes leaves the path skeleton' '⚪ ~/' \
    "$(dry idle "$tmp/\"\\")"
mkrepo "$tmp/repos/hostile" 'ref: refs/heads/we"ird\branch
'
check 'a hostile branch name is sanitized too' '⚪ hostile@weirdbranch' \
    "$(dry idle "$tmp/repos/hostile")"
mkrepo "$tmp/repos/ctrlbranch" "$(printf 'ref: refs/heads/a\001b')"
check 'a control byte inside a branch name is stripped' '⚪ ctrlbranch@ab' \
    "$(dry idle "$tmp/repos/ctrlbranch")"
check 'a hostile hostname is sanitized too' '⚪ srv:plain@master' \
    "$(cd -- "$tmp/repos/plain" && SSH_TTY=x CCTAB_HOST='s"r\v' HOME=$tmp CCTAB_DRY_RUN=1 "$bin" idle </dev/null)"
# The sanitizer is a pure-shell loop, so an empty PATH must not degrade it.
check 'sanitizing needs no external command' '⚪ ~/weird' \
    "$(cd -- "$tmp/we\"ird" && PATH= HOME=$tmp CCTAB_DRY_RUN=1 "$bin" idle </dev/null)"
# That loop is QUADRATIC, and the 1d cap does not bound its input: a non-ASCII
# location is exempt from cutting by count, and CCTAB_MAX_LOCATION=0 turns the cap
# off outright. Unbounded, a few hundred hostile characters cost seconds and a few
# thousand never finish - which breaks both "always exit 0" and "never an empty
# title", on a script slice 3 runs per tool call. So the loop stops at 256.
_c200=aaaaaaaaaabbbbbbbbbbccccccccccddddddddddeeeeeeeeeeffffffffffgggggggggghhhhhhhhhhiiiiiiiiiijjjjjjjjjjkkkkkkkkkkllllllllllmmmmmmmmmmnnnnnnnnnnoooooooooopppppppppp
_deepq=$tmp/sanbound/q\"$_c200/$_c200
mkdir -p "$_deepq"
_out=$(cd -- "$_deepq" && CCTAB_MAX_LOCATION=0 HOME=$tmp CCTAB_DRY_RUN=1 "$bin" idle </dev/null)
check 'a pathological quoted location still exits 0' '0' "$?"
check 'the quote is still gone from a bounded location' 'clean' \
    "$(case $_out in *'"'*) printf quoted ;; *) printf clean ;; esac)"
check 'a bounded location is marked with the ellipsis' 'marked' \
    "$(case $_out in *…) printf marked ;; *) printf unmarked ;; esac)"
check 'a bounded location stops near 256, not at 380' 'bounded' \
    "$([ "${#_out}" -le 270 ] && printf bounded || printf 'runaway:%s' "${#_out}")"
# High bytes must NOT go through the sanitizer at all: an accented or emoji
# name is perfectly good in a JSON string and stays byte for byte.
mkdir -p "$tmp/café-déjà"
check 'a non-ASCII location is left alone' '⚪ ~/café-déjà' "$(dry idle "$tmp/café-déjà")"
mkrepo "$tmp/repos/accent" 'ref: refs/heads/branché
'
check 'a non-ASCII branch name is left alone' '⚪ accent@branché' "$(dry idle "$tmp/repos/accent")"
# printf format-string injection: $title is always an argument, never a format.
mkdir -p "$tmp/pct-100%s%d-x"
check 'a % in a path location is not a printf format' '⚪ ~/pct-100%s%d-x' \
    "$(dry idle "$tmp/pct-100%s%d-x")"
mkrepo "$tmp/repos/pct" 'ref: refs/heads/100%s%d
'
check 'a % in a branch name is not a printf format' '⚪ pct@100%s%d' \
    "$(dry idle "$tmp/repos/pct")"

# --- glyph overrides ------------------------------------------------------
check 'CCTAB_GLYPH_WORKING override' '> plain@master' \
    "$(cd -- "$tmp/repos/plain" && CCTAB_GLYPH_WORKING='>' CCTAB_DRY_RUN=1 "$bin" working </dev/null)"
check 'CCTAB_GLYPH_WAITING override' '? plain@master' \
    "$(cd -- "$tmp/repos/plain" && CCTAB_GLYPH_WAITING='?' CCTAB_DRY_RUN=1 "$bin" waiting </dev/null)"
check 'CCTAB_GLYPH_IDLE override' '. plain@master' \
    "$(cd -- "$tmp/repos/plain" && CCTAB_GLYPH_IDLE='.' CCTAB_DRY_RUN=1 "$bin" idle </dev/null)"
check 'an empty glyph override leaves no leading space' 'plain@master' \
    "$(cd -- "$tmp/repos/plain" && CCTAB_GLYPH_IDLE= CCTAB_DRY_RUN=1 "$bin" idle </dev/null)"

# --- glyph position ---------------------------------------------------------
# Konsole elides the tab label from the LEFT, so a leading glyph is the first
# thing cut. The glyph therefore goes last under Konsole and first everywhere
# else, with CCTAB_GLYPH_POS overriding both.
_gp() { # _gp <env assignments...> -- runs `idle` in the plain repo
    ( cd -- "$tmp/repos/plain" && env "$@" CCTAB_DRY_RUN=1 \
        "$bin" idle </dev/null )
}
check 'default position is prefix when the terminal is unknown' '⚪ plain@master' \
    "$(_gp -u KONSOLE_VERSION -u KONSOLE_DBUS_SESSION -u CCTAB_GLYPH_POS)"
check 'CCTAB_GLYPH_POS=suffix puts the glyph last' 'plain@master ⚪' \
    "$(_gp CCTAB_GLYPH_POS=suffix)"
check 'CCTAB_GLYPH_POS=both puts a glyph at each end' '⚪ plain@master ⚪' \
    "$(_gp CCTAB_GLYPH_POS=both)"
check 'an unrecognised CCTAB_GLYPH_POS falls back to prefix' '⚪ plain@master' \
    "$(_gp CCTAB_GLYPH_POS=sideways)"
check 'Konsole selects suffix with no configuration' 'plain@master ⚪' \
    "$(_gp -u CCTAB_GLYPH_POS -u TMUX -u STY KONSOLE_VERSION=260801)"
check 'KONSOLE_DBUS_SESSION alone also selects suffix' 'plain@master ⚪' \
    "$(_gp -u CCTAB_GLYPH_POS -u TMUX -u STY KONSOLE_DBUS_SESSION=/Sessions/6)"
check 'inside tmux the Konsole variables no longer select suffix' '⚪ plain@master' \
    "$(_gp -u CCTAB_GLYPH_POS -u STY KONSOLE_VERSION=260801 TMUX=/tmp/x,1,0)"
check 'inside screen the Konsole variables no longer select suffix' '⚪ plain@master' \
    "$(_gp -u CCTAB_GLYPH_POS -u TMUX KONSOLE_VERSION=260801 STY=1.pts-0)"
check 'CCTAB_GLYPH_POS wins over Konsole detection' '⚪ plain@master' \
    "$(_gp -u TMUX -u STY KONSOLE_VERSION=260801 CCTAB_GLYPH_POS=prefix)"
check 'an empty glyph leaves no trailing space in suffix position' 'plain@master' \
    "$(_gp CCTAB_GLYPH_POS=suffix CCTAB_GLYPH_IDLE=)"
check 'the ssh host prefix stays at the front in suffix position' 'srv:plain@master ⚪' \
    "$(_gp CCTAB_GLYPH_POS=suffix SSH_CONNECTION='1 2 3 4' CCTAB_HOST=srv)"

# --- stdin drain ----------------------------------------------------------
payload='{"session_id":"abc","transcript_path":"/tmp/t.jsonl","cwd":"/x","hook_event_name":"Stop"}'
check 'a one-line payload on stdin is drained, not echoed' '⚪ plain@master' \
    "$(cd -- "$tmp/repos/plain" && printf '%s\n' "$payload" | CCTAB_DRY_RUN=1 "$bin" idle)"
check 'a payload with no trailing newline is drained' '⚪ plain@master' \
    "$(cd -- "$tmp/repos/plain" && printf '%s' "$payload" | CCTAB_DRY_RUN=1 "$bin" idle)"
check 'a multi-line payload is drained' '⚪ plain@master' \
    "$(cd -- "$tmp/repos/plain" && printf '%s\n%s\n%s\n' "$payload" "$payload" "$payload" | CCTAB_DRY_RUN=1 "$bin" idle)"

# --- the payload-discriminated edges --------------------------------------
# Two edges look at the payload, and both do it with `case` globs on the raw
# line. The fixtures below are shaped exactly as Claude Code writes them:
# compact JSON, no space after `:` or `,`, one trailing newline. A fixture with
# spaces in it would assert a contract the product does not have.
#
# dryp is dry() with the payload arriving on OUR stdin instead of /dev/null.
dryp() {
    _edge=$1
    _dir=${2-$here}
    (cd -- "$_dir" 2>/dev/null && HOME=$tmp CCTAB_DRY_RUN=1 "$bin" "$_edge")
}
# notif <notification_type> [message]
notif() {
    printf '{"session_id":"s1","transcript_path":"%s/t.jsonl","cwd":"%s","scratchpad_dir":"%s/sp","hook_event_name":"Notification","message":"%s","notification_type":"%s"}\n' \
        "$tmp" "$tmp/repos/plain" "$tmp" "${2-Claude needs your permission}" "$1"
}
# sstart <source>
sstart() {
    printf '{"session_id":"s1","transcript_path":"%s/t.jsonl","cwd":"%s","scratchpad_dir":"%s/sp","hook_event_name":"SessionStart","source":"%s","model":"claude-haiku-4-5"}\n' \
        "$tmp" "$tmp/repos/plain" "$tmp" "$1"
}

# THE one that must not regress. idle_prompt is the quiet-turn nudge and it
# fires after EVERY quiet turn end, so painting it waiting would turn every idle
# tab orange a minute later and collapse two of the three states into one.
check 'notify: idle_prompt paints idle, never waiting' '⚪ plain@master' \
    "$(notif idle_prompt 'Claude is waiting for your input' | dryp notify "$tmp/repos/plain")"
# The waiting kinds. permission_prompt is the 6-second backstop behind
# PermissionRequest; the other three are the cases PermissionRequest never
# covers - a worker/teammate prompt, an agent asking for input, and an MCP
# elicitation dialog.
for k in permission_prompt worker_permission_prompt agent_needs_input \
    elicitation_dialog elicitation_url_dialog; do
    check "notify: $k paints waiting" '🟠 plain@master' \
        "$(notif "$k" | dryp notify "$tmp/repos/plain")"
done
# The no-ops. Each of these is a real notification kind that is NOT a state
# change, and the failure mode they guard against is the quiet one: a kind that
# silently paints would overwrite a correct state with a wrong one. So assert
# ZERO BYTES, not an empty title - an empty title blanks the tab, which is what
# session-end does on purpose and what these must never do.
for k in agent_completed elicitation_complete elicitation_response \
    computer_use_exit push_notification auth_success; do
    check "notify: $k paints nothing at all" '0' \
        "$(notif "$k" | dryp notify "$tmp/repos/plain" | wc -c | tr -d ' ')"
done
# notification_type is a plain string in the event schema, not a closed enum, so
# a kind this version has never heard of is a certainty, not a hypothetical. It
# must be silent too, in both directions: no paint, and no stderr.
check 'notify: an unknown future kind paints nothing' '0' \
    "$(notif some_kind_invented_later | dryp notify "$tmp/repos/plain" | wc -c | tr -d ' ')"
check 'notify: an unknown kind is silent on stderr too' '' \
    "$(notif some_kind_invented_later | dryp notify "$tmp/repos/plain" 2>&1 >/dev/null)"
check 'notify: an empty payload paints nothing' '0' \
    "$(dryp notify "$tmp/repos/plain" </dev/null | wc -c | tr -d ' ')"
# idle_prompt is tested FIRST in the script, so a payload carrying both spellings
# resolves to idle. What must not happen is the reverse: a waiting notification
# whose free-text message merely mentions the other kind must stay waiting.
check 'notify: idle_prompt inside the message does not steal the paint' '🟠 plain@master' \
    "$(notif permission_prompt 'the idle_prompt kind is not this one' | dryp notify "$tmp/repos/plain")"
# The value is matched whole: worker_permission_prompt is its own kind and must
# not be mistaken for permission_prompt by a sloppy glob, and a kind that merely
# ENDS in a known one is not that kind.
check 'notify: a kind that merely contains a known one is not it' '0' \
    "$(notif not_really_idle_prompt_either | dryp notify "$tmp/repos/plain" | wc -c | tr -d ' ')"
# A payload that is not one line, and one with no trailing newline: the first
# line is what carries the kind, and `read` assigns a partial last line.
check 'notify: a payload with no trailing newline still parses' '⚪ plain@master' \
    "$(cd -- "$tmp/repos/plain" && printf '{"notification_type":"idle_prompt"}' | HOME=$tmp CCTAB_DRY_RUN=1 "$bin" notify)"
check 'notify: a multi-line payload is read and drained' '🟠 plain@master' \
    "$(cd -- "$tmp/repos/plain" && printf '{"notification_type":"permission_prompt"}\ntrailing\ntrailing\n' | HOME=$tmp CCTAB_DRY_RUN=1 "$bin" notify)"
(notif idle_prompt | dryp notify "$tmp/repos/plain" >/dev/null 2>&1)
check 'notify: a painting kind exits 0' '0' "$?"
(notif agent_completed | dryp notify "$tmp/repos/plain" >/dev/null 2>&1)
check 'notify: a no-op kind exits 0' '0' "$?"

# SessionStart's `compact` source. hooks.json's matcher already excludes it;
# this is the belt to that brace, and the failure it prevents is the worst one
# in the table - an auto-compaction re-fires SessionStart MID-TURN, which would
# repaint the idle dot while Claude is still working and arm the tab a second
# time with no matching unarm.
check 'session-start: source compact paints nothing at all' '0' \
    "$(sstart compact | dryp session-start "$tmp/repos/plain" | wc -c | tr -d ' ')"
(sstart compact | dryp session-start "$tmp/repos/plain" >/dev/null 2>&1)
check 'session-start: source compact exits 0' '0' "$?"
for s in startup resume clear fork; do
    check "session-start: source $s still paints idle" '⚪ plain@master' \
        "$(sstart "$s" | dryp session-start "$tmp/repos/plain")"
done
# The word alone is not the discriminator: a session started in a directory
# called `compact`, or resumed from a transcript named after one, is a normal
# start. The glob carries the whole `"source":"compact"` spelling for that
# reason.
check 'session-start: the word compact elsewhere is not the source' '⚪ plain@master' \
    "$(cd -- "$tmp/repos/plain" && printf '{"cwd":"%s/compact","hook_event_name":"SessionStart","source":"startup"}\n' "$tmp" | HOME=$tmp CCTAB_DRY_RUN=1 "$bin" session-start)"
# ...and no payload at all still paints, which is what keeps the by-hand
# recovery recipe in the README working from a shell.
check 'session-start: no payload still paints idle' '⚪ plain@master' \
    "$(dry session-start "$tmp/repos/plain")"
# The belt's exact reach, pinned in both directions, because the comment in
# section 0b now claims it. A space after the colon IS caught - that is the one
# whitespace variant a hand-rolled payload is most likely to have - and a
# pretty-printed multi-line payload is NOT, because section 0 keeps only the
# first line. The second case fails OPEN (it paints and, on a real Konsole,
# arms), which is why hooks.json's matcher is the load-bearing guard and this is
# only the belt.
check 'session-start: compact with a space after the colon is still caught' '0' \
    "$(cd -- "$tmp/repos/plain" && printf '{"hook_event_name":"SessionStart","source": "compact"}\n' | HOME=$tmp CCTAB_DRY_RUN=1 "$bin" session-start | wc -c | tr -d ' ')"
check 'session-start: a pretty-printed compact payload escapes the belt' '⚪ plain@master' \
    "$(cd -- "$tmp/repos/plain" && printf '{\n  "hook_event_name": "SessionStart",\n  "source": "compact"\n}\n' | HOME=$tmp CCTAB_DRY_RUN=1 "$bin" session-start)"
# Same whitespace variant on the notify side falls through to silence, which is
# the safe direction: an unpainted tab keeps the state it already showed.
check 'notify: a spaced-out kind is silent, not misread' '0' \
    "$(cd -- "$tmp/repos/plain" && printf '{"hook_event_name":"Notification","notification_type": "permission_prompt"}\n' | HOME=$tmp CCTAB_DRY_RUN=1 "$bin" notify | wc -c | tr -d ' ')"

# --- the edges that must NOT read the payload ------------------------------
# PostToolUse is the hot edge - one per tool call - and its payload carries the
# whole tool_response, hundreds of KB on a large Read. It paints `working`
# unconditionally and never looks at the bytes, which is both why it can afford
# to run on every call and why a tool_response containing the text of some other
# event cannot mislead it. Build a 256KB payload whose response body ends in a
# verbatim idle_prompt notification - a transcript, a log or this very test file
# is enough to produce one - and assert the title is still `working`.
_pad=aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa
_i=0
while [ "$_i" -lt 13 ]; do
    _pad=$_pad$_pad
    _i=$((_i + 1))
done
_bigpl=$tmp/bigpayload.json
printf '{"session_id":"s1","cwd":"%s","hook_event_name":"PostToolUse","tool_name":"Read","tool_use_id":"tu1","duration_ms":23,"tool_response":"%s{\\"notification_type\\":\\"idle_prompt\\"}"}\n' \
    "$tmp/repos/plain" "$_pad" >"$_bigpl"
check 'working: a 256KB payload is drained, not read' '🔵 plain@master' \
    "$(cd -- "$tmp/repos/plain" && HOME=$tmp CCTAB_DRY_RUN=1 "$bin" working <"$_bigpl")"
(cd -- "$tmp/repos/plain" && HOME=$tmp CCTAB_DRY_RUN=1 "$bin" working <"$_bigpl" >/dev/null 2>&1)
check 'working: a 256KB payload still exits 0' '0' "$?"
check 'working: a tool_response mentioning idle_prompt is still working' '🔵 plain@master' \
    "$(cd -- "$tmp/repos/plain" && printf '{"hook_event_name":"PostToolUse","tool_response":"{\\"notification_type\\":\\"idle_prompt\\"}"}\n' | HOME=$tmp CCTAB_DRY_RUN=1 "$bin" working)"
# A background subagent's PostToolUse paints NOTHING. This is the defect the
# binary exists to fix: PostToolUse is registered unmatched, so a subagent's tool
# calls fire it in the main session and used to repaint `working` over an open
# main-thread permission dialog, N times for N tool calls, with only a one-shot
# notification to restore the orange. `agent_id` is present only inside a
# subagent call, so its presence is the discriminator - and reading it needed the
# bounded payload read, which is why the shell could not have this fix.
check 'working: a subagent PostToolUse paints nothing' '' \
    "$(cd -- "$tmp/repos/plain" && printf '{"agent_id":"aec99e1f4bda1972b","agent_type":"general-purpose","hook_event_name":"PostToolUse","tool_name":"Read"}\n' | HOME=$tmp CCTAB_DRY_RUN=1 "$bin" working)"
(cd -- "$tmp/repos/plain" && printf '{"agent_id":"a1","hook_event_name":"PostToolUse"}\n' | HOME=$tmp CCTAB_DRY_RUN=1 "$bin" working >/dev/null 2>&1)
check 'working: a suppressed subagent edge still exits 0' '0' "$?"
# ABSENT, not empty, is what marks the main thread. A future payload with an
# empty agent_id must not silence the edge for a whole session.
check 'working: a main-thread PostToolUse with no agent_id paints working' '🔵 plain@master' \
    "$(cd -- "$tmp/repos/plain" && printf '{"hook_event_name":"PostToolUse","tool_name":"Read","tool_response":"ok"}\n' | HOME=$tmp CCTAB_DRY_RUN=1 "$bin" working)"
check 'working: an EMPTY agent_id is not a subagent' '🔵 plain@master' \
    "$(cd -- "$tmp/repos/plain" && printf '{"agent_id":"","hook_event_name":"PostToolUse","tool_name":"Read"}\n' | HOME=$tmp CCTAB_DRY_RUN=1 "$bin" working)"
check 'working: a null agent_id is not a subagent either' '🔵 plain@master' \
    "$(cd -- "$tmp/repos/plain" && printf '{"agent_id":null,"hook_event_name":"PostToolUse","tool_name":"Read"}\n' | HOME=$tmp CCTAB_DRY_RUN=1 "$bin" working)"
check 'working: agent_id quoted inside a tool_response is not a subagent' '🔵 plain@master' \
    "$(cd -- "$tmp/repos/plain" && printf '{"hook_event_name":"PostToolUse","tool_response":"{\\"agent_id\\":\\"a1\\"}"}\n' | HOME=$tmp CCTAB_DRY_RUN=1 "$bin" working)"
check 'working: one space after the colon is still a subagent' '' \
    "$(cd -- "$tmp/repos/plain" && printf '{"agent_id": "a1","hook_event_name":"PostToolUse"}\n' | HOME=$tmp CCTAB_DRY_RUN=1 "$bin" working)"
# The other edges never look at agent_id: UserPromptSubmit also maps to working
# and carries no payload discriminator, and idle must not be suppressible.
check 'idle: agent_id in the payload does not suppress idle' '⚪ plain@master' \
    "$(cd -- "$tmp/repos/plain" && printf '{"agent_id":"a1","hook_event_name":"Stop"}\n' | HOME=$tmp CCTAB_DRY_RUN=1 "$bin" idle)"

# The deliberate reversal of the intended table: a PermissionRequest carrying
# agent_id was to be a NO-OP. Measured, that paints the wrong state - an async
# subagent's dialog arrives AFTER the main session's Stop, so the tab would read
# idle while a prompt only the human can answer is on screen, which is the exact
# failure this whole state is for. The edge paints waiting either way and the
# script never looks at agent_id.
check 'waiting: a subagent PermissionRequest still paints waiting' '🟠 plain@master' \
    "$(cd -- "$tmp/repos/plain" && printf '{"agent_id":"aec99e1f4bda1972b","agent_type":"general-purpose","hook_event_name":"PermissionRequest","tool_name":"Write"}\n' | HOME=$tmp CCTAB_DRY_RUN=1 "$bin" waiting)"
check 'waiting: a main-thread PermissionRequest paints waiting' '🟠 plain@master' \
    "$(cd -- "$tmp/repos/plain" && printf '{"hook_event_name":"PermissionRequest","tool_name":"Write","permission_suggestions":[]}\n' | HOME=$tmp CCTAB_DRY_RUN=1 "$bin" waiting)"
# The invariant behind section 0's split, asserted from the outside: ONLY notify
# and session-start read the payload, so nothing a payload says can talk another
# edge out of its state. One line carrying BOTH discriminators, in the compact
# spelling, at the front of the payload where a glob would certainly see it.
_both='{"hook_event_name":"PostToolUse","notification_type":"idle_prompt","source":"compact","tool_response":"x"}'
for _c in 'working:🔵' 'waiting:🟠' 'idle:⚪'; do
    _e=${_c%%:*}
    _g=${_c#*:}
    check "the $_e edge cannot be redirected by a payload" "$_g plain@master" \
        "$(cd -- "$tmp/repos/plain" && printf '%s\n' "$_both" | HOME=$tmp CCTAB_DRY_RUN=1 "$bin" "$_e")"
done

# --- hooks.json: the wiring is part of the contract -----------------------
# Every assertion above tests the script. These test the CONFIG, because two of
# slice 3's failure modes live there and nothing else would catch them:
#
#   * an edge name typo. The script maps any unknown edge to `idle`, so
#     `notifiy` in hooks.json would quietly paint idle on every notification -
#     including the permission ones, which is the state this slice exists to
#     show.
#   * a dropped matcher. PreToolUse -> waiting is registered for exactly the two
#     tools that always block on the user; without the matcher it would paint
#     waiting on EVERY tool call.
#
# Read with the `read` builtin and parameter expansion rather than with jq or
# sed, which keeps the suite's dependency list exactly where it already was.
_hooks=$here/../hooks/hooks.json
# Parse hooks.json into one `event matcher edge timeout` row per registered
# hook, with `-` for an absent matcher, and assert the WHOLE table. Presence
# checks - "PostToolUse appears somewhere", "the word waiting appears
# somewhere" - pass just as happily on a transposed file that would paint idle
# when you submit a prompt; measured, a copy of this tree with Stop -> working
# and UserPromptSubmit -> idle passed the earlier presence-only assertions and
# `claude plugin validate` as well. The pairing is the part of slice 3 that
# lives ONLY in config, so it is the part that has to be pinned here.
#
# Line-based, with the `read` builtin and parameter expansion rather than jq or
# sed, which keeps the suite's dependency list exactly where it already was.
# Indentation is the discriminator: an event key sits at 4 spaces, a hook GROUP
# opens at 6 and closes at 6, `"matcher"` and the inner `"hooks"` sit at 8, and
# a hook entry's fields sit at 12. A row is emitted when the GROUP closes, not
# when its `"timeout"` line goes by, because JSON keys have no required order -
# `"matcher"` written after `"hooks"` is the same file, and a parser keyed on
# field order read it as unmatched. Measured: that spelling is exactly what
# json.dump produces when a matcher is added programmatically, and it slipped
# past the first version of this loop.
#
# This assumes the file's 2-space pretty format. Reformatting it to one line
# would produce no rows at all and fail every assertion below, which is the safe
# direction: loud, not silent.
_ev=
_matcher=-
_edge=
_t=
_ncmd=0
_rows=
_unknown=
_badgroup=
_known=' session-start working waiting idle notify subagent-stop session-end '
while IFS= read -r _l; do
    case $_l in
    '    "'*'": ['*)
        _ev=${_l#*\"}
        _ev=${_ev%%\"*}
        ;;
    '      {'*)
        _matcher=-
        _edge=
        _t=
        _ncmd=0
        ;;
    '        "matcher": "'*)
        _matcher=${_l#*: \"}
        _matcher=${_matcher%\"*}
        ;;
    *'bin/tabstatus\" '*)
        _edge=${_l#*'bin/tabstatus\" '}
        _edge=${_edge%%\"*}
        _ncmd=$((_ncmd + 1))
        case $_known in
        *" $_edge "*) ;;
        *) _unknown="$_unknown $_edge" ;;
        esac
        ;;
    '            "timeout": '*)
        _t=${_l#*: }
        _t=${_t%,}
        ;;
    '      }'*)
        _rows="$_rows$_ev $_matcher $_edge $_t
"
        [ "$_ncmd" = 1 ] || _badgroup="$_badgroup[$_ev:$_ncmd commands]"
        ;;
    esac
done <"$_hooks"

# The table, as the README states it. Every row asserted, both directions.
_want='SessionStart startup|resume|clear|fork session-start 5
UserPromptSubmit - working 5
PreToolUse AskUserQuestion|ExitPlanMode waiting 5
PermissionRequest - waiting 5
PostToolUse - working 5
PostToolUseFailure - working 5
Notification - notify 5
Stop - idle 5
StopFailure - idle 5
SubagentStop - subagent-stop 5
SessionEnd - session-end 1'
# An edge name the script does not implement would be painted `idle` by the
# fallback in section 3, so a typo there is silent in production.
check 'hooks.json uses no edge the script does not implement' '' "$_unknown"
# Each wanted row is present, with its matcher (or its deliberate absence) and
# its timeout. `- ` in the row is what pins PermissionRequest, PostToolUse,
# PostToolUseFailure, Notification, Stop and StopFailure as UNMATCHED: a matcher
# on any of them would change the row and fail here.
printf '%s\n' "$_want" >"$tmp/want.rows"
printf '%s' "$_rows" >"$tmp/got.rows"
while IFS= read -r _w; do
    [ -n "$_w" ] || continue
    check "hooks.json wires [$_w]" 'yes' \
        "$(case "
$_rows" in *"
$_w
"*) printf yes ;; *) printf no ;; esac)"
done <"$tmp/want.rows"
# ...and nothing else is registered. This is what catches an ADDED event -
# SubagentStop, a second SessionStart group - rather than a changed one.
_extra=
while IFS= read -r _g; do
    [ -n "$_g" ] || continue
    case "
$_want
" in
    *"
$_g
"*) ;;
    *) _extra="$_extra[$_g]" ;;
    esac
done <"$tmp/got.rows"
check 'hooks.json registers nothing the table does not list' '' "$_extra"
check 'hooks.json registers exactly eleven hooks' '11' \
    "$(printf '%s' "$_rows" | wc -l | tr -d ' ')"
# One command per group, so that a second hook smuggled into an existing group -
# which the row above would not show - fails here instead.
check 'every hooks.json group holds exactly one command' '' "$_badgroup"
# SubagentStop used to be deliberately ABSENT, because a subagent finishing must
# not read as the session going idle and because it also fires for the internal
# compaction summarizer (agent_type empty, which no matcher could filter) right
# before a SessionStart/compact. Both reasons survive; what changed is that they
# are now enforced by the OWNER test rather than by not registering the hook. The
# edge paints nothing of its own - with no state directory it is a complete no-op,
# pinned below - and with one it clears only the wait whose agent_id matches. That
# is the only signal for a dialog you DECLINED: no hook fires for a denial, so the
# tool never runs and no PostToolUse ever arrives.
for _e in subagent-stop; do
    check "$_e paints nothing at all without a state directory" '' \
        "$(cd -- "$tmp/repos/plain" \
           && printf '{"session_id":"s1","agent_id":"a1","hook_event_name":"SubagentStop"}\n' \
              | HOME=$tmp CCTAB_DRY_RUN=1 "$bin" "$_e")"
done

# --- exit status ----------------------------------------------------------
for e in session-start working waiting idle notify subagent-stop session-end no-such-edge; do
    (cd -- "$tmp/repos/plain" && CCTAB_DRY_RUN=1 "$bin" "$e" </dev/null >/dev/null 2>&1)
    check "exit 0 on dry-run edge $e" '0' "$?"
done

# --- real emission paths (CLAUDE_PID unset: no pty is ever touched) -------
# The terminalSequence line is asserted byte for byte: the \u001b / \u0007
# escapes are what keep it valid JSON, and a raw control byte here would be the
# subtle bug this test exists to catch.
expect_json='{"terminalSequence":"\u001b]0;🔵 plain@master\u0007","suppressOutput":true}'
check 'working emits the terminalSequence JSON line' "$expect_json" \
    "$(cd -- "$tmp/repos/plain" && unset CLAUDE_PID; "$bin" working </dev/null)"
check 'idle emits the terminalSequence JSON line' \
    '{"terminalSequence":"\u001b]0;⚪ plain@master\u0007","suppressOutput":true}' \
    "$(cd -- "$tmp/repos/plain" && unset CLAUDE_PID; "$bin" idle </dev/null)"
check 'the emitted JSON line has no raw ESC byte' '0' \
    "$(cd -- "$tmp/repos/plain" && unset CLAUDE_PID; "$bin" working </dev/null | tr -dc '\033' | wc -c | tr -d ' ')"
check 'the emitted JSON is exactly one line' '1' \
    "$(cd -- "$tmp/repos/plain" && unset CLAUDE_PID; "$bin" working </dev/null | wc -l | tr -d ' ')"
# The notify edge on the real path, where a silently-painting no-op would
# actually reach a tab: an ignored kind must put NOTHING on stdout, not a
# terminalSequence carrying an empty title.
check 'notify idle_prompt emits the idle terminalSequence' \
    '{"terminalSequence":"\u001b]0;⚪ plain@master\u0007","suppressOutput":true}' \
    "$(cd -- "$tmp/repos/plain" && unset CLAUDE_PID; notif idle_prompt | "$bin" notify)"
check 'notify permission_prompt emits the waiting terminalSequence' \
    '{"terminalSequence":"\u001b]0;🟠 plain@master\u0007","suppressOutput":true}' \
    "$(cd -- "$tmp/repos/plain" && unset CLAUDE_PID; notif permission_prompt | "$bin" notify)"
check 'notify: an ignored kind emits no JSON line at all' '0' \
    "$(cd -- "$tmp/repos/plain" && unset CLAUDE_PID; notif agent_completed | "$bin" notify | wc -c | tr -d ' ')"
# There is deliberately NO `source compact emits nothing on the real path`
# assertion here. It used to exist and it was vacuous: it ran with CLAUDE_PID=1,
# `readlink /proc/1/fd/1` fails for an unprivileged user, so the tty guard in
# section 5 empties $tty and the script exits before emitting anything -
# measured, 0 bytes for source=compact AND 0 bytes for source=startup, so the
# assertion would have passed with the compact belt deleted. The belt itself is
# asserted in dry run above; the tty guard is asserted below. Covering the
# emitting path properly needs a real pty, which this suite cannot allocate
# without a dependency - done by hand instead, over an allocated pty: 0 bytes
# for compact, 82 bytes (the OSC 50 arming pair plus the idle title) for startup.

# Headless guard: no CLAUDE_PID means the direct-write edges do nothing at all.
check 'session-start with no CLAUDE_PID emits nothing' '' \
    "$(cd -- "$tmp/repos/plain" && unset CLAUDE_PID; "$bin" session-start </dev/null 2>&1)"
check 'session-end with no CLAUDE_PID emits nothing' '' \
    "$(cd -- "$tmp/repos/plain" && unset CLAUDE_PID; "$bin" session-end </dev/null 2>&1)"
(cd -- "$tmp/repos/plain" && unset CLAUDE_PID; "$bin" session-start </dev/null >/dev/null 2>&1)
check 'session-start with no CLAUDE_PID still exits 0' '0' "$?"

# A CLAUDE_PID that resolves to something that is not a tty must also be inert.
check 'a CLAUDE_PID whose fd 1 is not a tty emits nothing' '' \
    "$(cd -- "$tmp/repos/plain" && CLAUDE_PID=1 "$bin" session-start </dev/null 2>&1)"
check 'a nonsense CLAUDE_PID emits nothing' '' \
    "$(cd -- "$tmp/repos/plain" && CLAUDE_PID=not-a-pid "$bin" session-start </dev/null 2>&1)"

# --- the bounded payload read ---------------------------------------------
# Only two 8 KiB WINDOWS of the first line are ever searched - its front and its
# back - and everything between them is drained without being looked at. That
# bound is what makes the hot edge affordable; in the shell, reading the line and
# running `case` globs over it cost 165ms under dash and 20ms under bash-as-sh on
# a 1 MB Notification, measured on this machine, against a 5s hook timeout.
#
# There has to be a back window because a Notification serializes
# `notification_type` LAST, after the unbounded `message`: with a front window
# alone, an MCP elicitation carrying a ~7.4 KB message silently lost the
# discriminator and the notify edge painted nothing at all.
#
# The bound is asserted through its OBSERVABLE consequences, which is the honest
# way to test it without a clock in the suite: a last-member discriminator is
# seen at any message size, and one that is neither near the front nor near the
# back is not seen.
_pad8k=$(awk 'BEGIN{while(i++<9000)printf "a"}')
check 'notify: idle_prompt inside the front window is seen' '⚪ plain@master' \
    "$(cd -- "$tmp/repos/plain" && printf '{"notification_type":"idle_prompt","message":"%s"}\n' "$_pad8k" | HOME=$tmp CCTAB_DRY_RUN=1 "$bin" notify)"
check 'notify: the same kind as the LAST member is seen too' '⚪ plain@master' \
    "$(cd -- "$tmp/repos/plain" && printf '{"message":"%s","notification_type":"idle_prompt"}\n' "$_pad8k" | HOME=$tmp CCTAB_DRY_RUN=1 "$bin" notify)"
# The real shape, at a size no prefix window could have reached.
_pad200k=$(awk 'BEGIN{while(i++<200000)printf "a"}')
check 'notify: permission_prompt last after a 200KB message is seen' '🟠 plain@master' \
    "$(cd -- "$tmp/repos/plain" && printf '{"message":"%s","notification_type":"permission_prompt"}\n' "$_pad200k" | HOME=$tmp CCTAB_DRY_RUN=1 "$bin" notify)"
check 'notify: a kind in neither window is not seen' '' \
    "$(cd -- "$tmp/repos/plain" && printf '{"a":"%s","notification_type":"idle_prompt","b":"%s"}\n' "$_pad8k" "$_pad8k" | HOME=$tmp CCTAB_DRY_RUN=1 "$bin" notify)"
# agent_id is read from the FRONT window only, on purpose: a false positive there
# silences every `working` repaint for the rest of the session, and the back of a
# PostToolUse payload is `tool_response`, which can be an object whose keys are
# not escaped. Real captures put agent_id at byte 760 of 1360.
check 'working: agent_id only in the back window does not silence' '🔵 plain@master' \
    "$(cd -- "$tmp/repos/plain" && printf '{"tool_response":"%s","agent_id":"a1"}\n' "$_pad8k" | HOME=$tmp CCTAB_DRY_RUN=1 "$bin" working)"
# A payload far past any real one is still drained and still answers.
_bigger=$tmp/4mb.json
awk 'BEGIN{printf "{\"hook_event_name\":\"PostToolUse\",\"tool_response\":\""; while(i++<4194304)printf "a"; printf "\"}\n"}' >"$_bigger"
check 'working: a 4MB payload is drained and paints' '🔵 plain@master' \
    "$(cd -- "$tmp/repos/plain" && HOME=$tmp CCTAB_DRY_RUN=1 "$bin" working <"$_bigger")"
(cd -- "$tmp/repos/plain" && HOME=$tmp CCTAB_DRY_RUN=1 "$bin" working <"$_bigger" >/dev/null 2>&1)
check 'working: a 4MB payload still exits 0' '0' "$?"
# The drain is not optional: stdin has to reach EOF, or Claude Code's writer sees
# EPIPE on a pipe it is still filling. What is asserted here is that a payload
# that size costs nothing in ANSWERS; the cost in time is in the README.

# --- a name that is not valid UTF-8 --------------------------------------
# JSON text must be UTF-8, so an invalid byte in a directory, branch or hostname
# used to produce a hook line Claude Code could not parse - and therefore no
# title at all (README limitation 2). The byte is replaced by U+FFFD at the
# boundary where a path stops being a path and becomes text; the expectations
# below are the bytes, so this needs no JSON parser.
_badname=$(printf 'bad\377utf8')
mkdir -p "$tmp/$_badname"
check 'an invalid byte in a path becomes U+FFFD' '⚪ ~/bad�utf8' \
    "$(dry idle "$tmp/$_badname")"
check 'and the emitted line is valid JSON' \
    '{"terminalSequence":"\u001b]0;⚪ ~/bad�utf8\u0007","suppressOutput":true}' \
    "$(cd -- "$tmp/$_badname" && HOME=$tmp "$bin" idle </dev/null)"
mkrepo "$tmp/repos/badbranch" "$(printf 'ref: refs/heads/b\377r\n')"
check 'an invalid byte in a branch becomes U+FFFD' '⚪ badbranch@b�r' \
    "$(dry idle "$tmp/repos/badbranch")"
check 'an invalid byte in the ssh host becomes U+FFFD' '⚪ s�v:plain@master' \
    "$(cd -- "$tmp/repos/plain" && SSH_TTY=x CCTAB_HOST="$(printf 's\377v')" HOME=$tmp CCTAB_DRY_RUN=1 "$bin" idle </dev/null)"
# The other way a broken line could be emitted: the glyph and the ellipsis are
# attached AFTER the sanitizer, so the writer escapes whatever it is given.
check 'a hostile glyph cannot break the JSON line' \
    '{"terminalSequence":"\u001b]0;q\"x ~/plaindir\u0007","suppressOutput":true}' \
    "$(cd -- "$tmp/plaindir" && CCTAB_GLYPH_POS=prefix CCTAB_GLYPH_IDLE='q"x' HOME=$tmp "$bin" idle </dev/null)"
# The length cap's own ellipsis runs THROUGH the sanitizer, because section 2
# comes after section 1d, so its quote is deleted rather than escaped.
check "the cap's ellipsis is sanitized, not escaped" \
    '{"terminalSequence":"\u001b]0;⚪ qx/four/five/six/seven/eight\u0007","suppressOutput":true}' \
    "$(cd -- "$tmp/one/two/three/four/five/six/seven/eight" && CCTAB_ELLIPSIS='q"x' CCTAB_GLYPH_POS=prefix HOME=$tmp "$bin" idle </dev/null)"
# The sanitizer's OWN 256-character bound appends its ellipsis after the loop,
# so that copy reaches the writer unsanitized - the one place a hostile
# CCTAB_ELLIPSIS could still have broken the line. It needs a quote in the path
# to enter the loop at all, and the cap off to get past 256 characters.
_long=$(awk 'BEGIN{while(i++<100)printf "a"}')
mkdir -p "$tmp/q\"$_long/$_long/$_long"
check 'the 256-character bound ellipsis is escaped by the writer' 'yes' \
    "$(cd -- "$tmp/q\"$_long/$_long/$_long" && CCTAB_MAX_LOCATION=0 CCTAB_ELLIPSIS='q"x' HOME=$tmp "$bin" idle </dev/null | grep -q 'aq\\"x\\u0007' && printf yes)"

# --- install / uninstall --------------------------------------------------
# Against a throwaway config dir under $tmp, never the real one:
# CLAUDE_CONFIG_DIR is what the installer reads, and $tmp is removed on exit.
_cfg=$tmp/config
_ins() { ( cd -- "$repo" && HOME=$tmp CLAUDE_CONFIG_DIR=$_cfg "$bin" "$@" </dev/null 2>&1 ); }
_ins install >/dev/null
check 'install: created settings.json with just the one key' \
    '{
  "env": {
    "CLAUDE_CODE_DISABLE_TERMINAL_TITLE": "1"
  }
}' "$(cat "$_cfg/settings.json")"
check 'install: mode 600, because that file holds env values' '600' \
    "$(stat -c %a "$_cfg/settings.json" 2>/dev/null || printf 600)"
check 'install: linked the plugin at the repo' "$repo" \
    "$(readlink "$_cfg/skills/claude-tabstatus")"
check 'install: recorded the prior state' 'yes' \
    "$([ -f "$_cfg/claude-tabstatus.state" ] && printf yes)"
check 'install: is idempotent' 'yes' \
    "$(_ins install | grep -q 'already "1" - unchanged' && printf yes)"
check 'doctor: reports a healthy install' 'yes' \
    "$(_ins doctor | grep -q 'env key:   OK' && _ins doctor | grep -q 'plugin:    OK' && printf yes)"
_ins uninstall >/dev/null
check 'uninstall: removed the key and the empty env with it' '{}' \
    "$(cat "$_cfg/settings.json")"
check 'uninstall: removed the link' '' "$(ls -d "$_cfg/skills/claude-tabstatus" 2>/dev/null)"
check 'uninstall: removed the state record' '' \
    "$(ls "$_cfg/claude-tabstatus.state" 2>/dev/null)"
# Every other byte of an existing file survives, which is the point of splicing
# the member in instead of reprinting the document.
rm -rf "$_cfg"
mkdir -p "$_cfg"
printf '{\n  "permissions": {"allow": []},\n\n  "env": {\n    "FOO": "bar"\n  },\n  "model": "opus"\n}\n' >"$_cfg/settings.json"
cp "$_cfg/settings.json" "$tmp/settings.orig"
_ins install >/dev/null
check 'install: adds exactly one line to an existing file' '4a5
>     "CLAUDE_CODE_DISABLE_TERMINAL_TITLE": "1",' \
    "$(diff "$tmp/settings.orig" "$_cfg/settings.json")"
_ins uninstall >/dev/null
check 'uninstall: puts the file back byte for byte' '' \
    "$(diff "$tmp/settings.orig" "$_cfg/settings.json")"
# An empty "env" the USER already had must survive an uninstall: the state file
# records whether the env object was there before, because without that the
# uninstaller cannot tell one it created from an empty one it found. A
# differential fuzz of 680 documents found exactly this.
rm -rf "$_cfg"
mkdir -p "$_cfg"
printf '{\n  "env": {},\n  "model": "opus"\n}\n' >"$_cfg/settings.json"
cp "$_cfg/settings.json" "$tmp/settings.emptyenv"
_ins install >/dev/null
_ins uninstall >/dev/null
check 'uninstall: an empty env the user already had is kept' '' \
    "$(diff "$tmp/settings.emptyenv" "$_cfg/settings.json")"
# ...and one the installer created is not left behind.
rm -rf "$_cfg"
mkdir -p "$_cfg"
printf '{\n  "model": "opus"\n}\n' >"$_cfg/settings.json"
cp "$_cfg/settings.json" "$tmp/settings.noenv"
_ins install >/dev/null
_ins uninstall >/dev/null
check 'uninstall: an env the installer created is removed' '' \
    "$(diff "$tmp/settings.noenv" "$_cfg/settings.json")"
# A state file written by the SHELL installer (state_version 1) still restores a
# value the user had set themselves: it recorded the value under a different name,
# and its source text is what gets spliced back.
rm -rf "$_cfg"
mkdir -p "$_cfg"
printf '{\n  "env": {\n    "CLAUDE_CODE_DISABLE_TERMINAL_TITLE": "1",\n    "X": "y"\n  }\n}\n' >"$_cfg/settings.json"
printf '{"state_version":1,"written_by":"claude-tabstatus install.sh","env_key":"CLAUDE_CODE_DISABLE_TERMINAL_TITLE","env_key_before":{"had":true,"value":"0"},"symlink_before":{"had":false,"target":null}}\n' >"$_cfg/claude-tabstatus.state"
_ins uninstall >/dev/null
check 'uninstall: a state_version 1 record still restores the old value' \
    '{
  "env": {
    "CLAUDE_CODE_DISABLE_TERMINAL_TITLE": "0",
    "X": "y"
  }
}' "$(cat "$_cfg/settings.json")"
# A document this tool cannot round-trip is refused, whole.
rm -rf "$_cfg"
mkdir -p "$_cfg"
printf '[1, 2]' >"$_cfg/settings.json"
_out=$(_ins install)
check 'install: refuses a settings.json that is not an object' 'yes' \
    "$(printf '%s' "$_out" | grep -q 'not the expected shape' && printf yes)"
check 'install: and changes nothing when it refuses' '[1, 2]' "$(cat "$_cfg/settings.json")"
check 'install: not even the symlink' '' "$(ls -d "$_cfg/skills/claude-tabstatus" 2>/dev/null)"
# --- the half-states, which are the whole point of the write ordering ------
# install writes settings.json (Claude Code's own title OFF) and then the plugin
# symlink (which paints the replacement). Between those two writes there is a
# state that paints NO tab title at all, so every refusal has to happen before
# the first of them. Two paths used to reach step 3 with settings already edited.
rm -rf "$_cfg"; mkdir -p "$_cfg"
: >"$_cfg/skills"                      # a regular file where the directory goes
_out=$(_ins install)
check 'install: refuses a skills path that is not a directory' 'yes' \
    "$(printf '%s' "$_out" | grep -q 'not a directory' && printf yes)"
check 'install: and settings.json was never written' '' \
    "$(ls "$_cfg/settings.json" 2>/dev/null)"
rm -rf "$_cfg"; mkdir -p "$_cfg/skills"; chmod 500 "$_cfg/skills"
_out=$(_ins install)
chmod 700 "$_cfg/skills"
check 'install: refuses an unwritable skills directory' 'yes' \
    "$(printf '%s' "$_out" | grep -q 'is not writable, and the plugin symlink' && printf yes)"
check 'install: and settings.json was never written either' '' \
    "$(ls "$_cfg/settings.json" 2>/dev/null)"
# Installing with no bin/tabstatus is strictly worse than not installing: the env
# key switches Claude Code's title painting off and all eleven hooks then exit 127.
# It used to WARN and exit 0.
rm -rf "$_cfg"; mkdir -p "$_cfg"
_fake=$tmp/fakerepo
mkdir -p "$_fake/.claude-plugin" "$_fake/hooks" "$_fake/bin"
printf '{}\n' >"$_fake/.claude-plugin/plugin.json"
printf '{}\n' >"$_fake/hooks/hooks.json"
cp "$bin" "$_fake/bin/runner"
_fins() { ( cd -- "$_fake" && HOME=$tmp CLAUDE_CONFIG_DIR=$_cfg "$_fake/bin/runner" "$@" </dev/null 2>&1 ); }
_out=$(_fins install)
check 'install: refuses when bin/tabstatus is missing' 'yes' \
    "$(printf '%s' "$_out" | grep -q 'refused rather than warned about' && printf yes)"
( cd -- "$_fake" && HOME=$tmp CLAUDE_CONFIG_DIR=$_cfg "$_fake/bin/runner" install </dev/null >/dev/null 2>&1 )
check 'install: and exits nonzero so a wrapper can see it' '1' "$?"
check 'install: nothing was written without the binary' '' \
    "$(ls "$_cfg/settings.json" "$_cfg/skills/claude-tabstatus" 2>/dev/null)"
check 'install: --force installs anyway, for the build-it-next case' 'yes' \
    "$(_fins install --force | grep -q 'WARNING' && printf yes)"
check 'install: --force really did write the key' 'yes' \
    "$(grep -q CLAUDE_CODE_DISABLE_TERMINAL_TITLE "$_cfg/settings.json" && printf yes)"
check 'install: an unknown option is refused' 'yes' \
    "$(_fins install --nonsense | grep -q 'unknown option' && printf yes)"
# uninstall is the MIRROR: settings.json first, the link last. Its one reachable
# refusal - the key is set but no state record proves we set it - used to fire
# AFTER the link had already been removed, leaving exactly the blank-tab state.
rm -rf "$_cfg"; mkdir -p "$_cfg/skills"
printf '{\n  "env": {\n    "CLAUDE_CODE_DISABLE_TERMINAL_TITLE": "1"\n  }\n}\n' >"$_cfg/settings.json"
ln -s "$repo" "$_cfg/skills/claude-tabstatus"
_out=$(_ins uninstall)
check 'uninstall: refuses to remove a key it cannot prove it set' 'yes' \
    "$(printf '%s' "$_out" | grep -q 'no record that we set it' && printf yes)"
check 'uninstall: and the plugin symlink is still there' "$repo" \
    "$(readlink "$_cfg/skills/claude-tabstatus")"
check 'uninstall: --force removes both' '' \
    "$(_ins uninstall --force >/dev/null; readlink "$_cfg/skills/claude-tabstatus" 2>/dev/null)"
# Duplicate members: this parser resolves first-wins, JSON.parse (hence Claude
# Code) resolves last-wins. Splicing into the first one reported success while the
# key was not in the effective environment.
rm -rf "$_cfg"; mkdir -p "$_cfg"
printf '{"env":{"A":"1"},"model":"x","env":{"B":"2"}}\n' >"$_cfg/settings.json"
cp "$_cfg/settings.json" "$tmp/settings.dup"
_out=$(_ins install)
check 'install: refuses a document with duplicate keys' 'yes' \
    "$(printf '%s' "$_out" | grep -q 'appears more than once at the top level' && printf yes)"
check 'install: and the duplicate document is untouched' '' \
    "$(diff "$tmp/settings.dup" "$_cfg/settings.json")"
rm -rf "$_cfg"; mkdir -p "$_cfg"
printf '{"env":{"A":"1","A":"2"}}\n' >"$_cfg/settings.json"
check 'install: refuses duplicates inside env too' 'yes' \
    "$(_ins install | grep -q 'appears more than once inside "env"' && printf yes)"
# A zero-byte settings.json is a real state (a truncated write, an editor that
# creates the file before it saves) and Claude Code reads it as no settings.
rm -rf "$_cfg"; mkdir -p "$_cfg"
: >"$_cfg/settings.json"
chmod 640 "$_cfg/settings.json"
_ins install >/dev/null
check 'install: writes an empty settings.json fresh' \
    '{
  "env": {
    "CLAUDE_CODE_DISABLE_TERMINAL_TITLE": "1"
  }
}' "$(cat "$_cfg/settings.json")"
check 'install: and keeps the mode the empty file had' '640' \
    "$(stat -c %a "$_cfg/settings.json" 2>/dev/null || printf 640)"
# doctor has to run on the config that is BROKEN - that is its whole job.
rm -rf "$_cfg"; mkdir -p "$_cfg"
ln -s "$tmp/no-such-settings.json" "$_cfg/settings.json"
check 'doctor: reports a dangling settings symlink and carries on' 'yes' \
    "$(_ins doctor | grep -q 'which does not exist' && _ins doctor | grep -q '^title:' && printf yes)"
check 'install: refuses the same dangling symlink' 'yes' \
    "$(_ins install | grep -q 'could not be resolved' && printf yes)"
check 'doctor: exits 0 on a broken config' '0' \
    "$(_ins doctor >/dev/null 2>&1; printf %s $?)"
# A killed run leaves a pid-named probe or temp file; the next run sweeps the ones
# whose pid is gone, and leaves a live pid's alone.
rm -rf "$_cfg"; mkdir -p "$_cfg"
: >"$_cfg/.cctab-wtest.999999"
: >"$_cfg/.settings.json.cctab-tmp.999999"
: >"$_cfg/.cctab-wtest.$$"
_ins install >/dev/null
check 'install: sweeps scratch files from a killed run' '' \
    "$(ls "$_cfg/.cctab-wtest.999999" "$_cfg/.settings.json.cctab-tmp.999999" 2>/dev/null)"
check "install: leaves a LIVE pid's scratch file alone" 'yes' \
    "$([ -f "$_cfg/.cctab-wtest.$$" ] && printf yes)"
rm -rf "$_cfg"

# The subcommands must not collide with the edge names, in either direction.
check 'an edge name is not a subcommand' '⚪ ~/plaindir' "$(dry idle "$tmp/plaindir")"
check 'a subcommand name never paints' '' \
    "$(cd -- "$tmp/plaindir" && HOME=$tmp CCTAB_DRY_RUN=1 "$bin" doctor </dev/null | grep '^⚪')"

# --- the committed binaries must match the sources -----------------------
# bin/ holds binaries that are COMMITTED, so they can go stale against src/ and
# ship a version nobody built. Git does not preserve mtimes, so "older than the
# newest source file" cannot be answered after a clone; a digest of the sources
# the binary was built from can, and it also catches an edit that kept its
# timestamp. scripts/build.sh writes both manifests; this recomputes them.
check 'bin/tabstatus exists and is executable' 'yes' \
    "$([ -x "$repo/bin/tabstatus" ] && printf yes)"
check 'bin/tabstatus resolves to a committed platform binary' 'yes' \
    "$(_t=$(readlink "$repo/bin/tabstatus") && [ -f "$repo/bin/$_t" ] && printf yes)"
_sources="Cargo.toml $(cd -- "$repo" && ls src/*.rs | sort)"
if command -v sha256sum >/dev/null 2>&1 && [ -f "$repo/bin/sources.sha256" ]; then
    check 'the committed binaries are not stale (sha256 of src/ and Cargo.toml)' '' \
        "$(cd -- "$repo" && sha256sum $_sources | diff - bin/sources.sha256)"
elif [ -f "$repo/bin/sources.cksum" ]; then
    check 'the committed binaries are not stale (cksum of src/ and Cargo.toml)' '' \
        "$(cd -- "$repo" && cksum $_sources | diff - bin/sources.cksum)"
else
    printf 'FAIL  no source manifest in bin/ - run sh scripts/build.sh\n'
    fail=$((fail + 1))
fi
# And the binary really is the version Cargo.toml describes, which catches a
# manifest refreshed without a rebuild.
check 'bin/tabstatus reports the version in Cargo.toml' \
    "$(sed -n 's/^version = "\(.*\)"/\1/p' "$repo/Cargo.toml" | head -1)" \
    "$("$repo/bin/tabstatus" version </dev/null | awk '{print $2}')"

# --- cross-check against a real git ---------------------------------------
# Everything above is hand-built, which is what keeps this suite dependency
# free and lets it assert HEAD bytes that git will not write on demand. This
# section is the guard against those fixtures drifting from what git actually
# produces on disk. It is SKIPPED, never failed, where there is no git.
#
# Hermetic: no user or system config is read, the author identity comes from
# the environment, and the branch name is one we chose, so the assertions do
# not depend on the machine's init.defaultBranch.
if command -v git >/dev/null 2>&1; then
    GIT_CONFIG_GLOBAL=/dev/null
    GIT_CONFIG_SYSTEM=/dev/null
    GIT_AUTHOR_NAME=cctab
    GIT_AUTHOR_EMAIL=cctab@example.invalid
    GIT_COMMITTER_NAME=cctab
    GIT_COMMITTER_EMAIL=cctab@example.invalid
    export GIT_CONFIG_GLOBAL GIT_CONFIG_SYSTEM GIT_AUTHOR_NAME GIT_AUTHOR_EMAIL \
        GIT_COMMITTER_NAME GIT_COMMITTER_EMAIL
    rg=$tmp/realgit
    mkdir -p "$rg/one"
    (
        cd -- "$rg/one" \
            && git -c init.defaultBranch=trunk init -q . \
            && : >f && git add f && git commit -qm one
    ) >/dev/null 2>&1
    check 'real git: repo@branch' '⚪ one@trunk' "$(dry idle "$rg/one")"
    mkdir -p "$rg/one/sub/dir"
    check 'real git: from a subdirectory' '⚪ one@trunk' "$(dry idle "$rg/one/sub/dir")"
    # A linked worktree writes a .git FILE holding an absolute gitdir, and its
    # HEAD lives under the main repo's .git/worktrees/<name>/.
    (cd -- "$rg/one" && git worktree add -q -b feature/tab-title "$rg/wt") >/dev/null 2>&1
    if [ -f "$rg/wt/.git" ]; then
        check 'real git: a linked worktree' '⚪ wt@feature/tab-title' "$(dry idle "$rg/wt")"
    else
        printf 'SKIP  real git: a linked worktree (git worktree add unavailable)\n'
    fi
    _sha=$(cd -- "$rg/one" && git rev-parse --short=7 HEAD 2>/dev/null) || _sha=
    (cd -- "$rg/one" && git checkout -q --detach HEAD) >/dev/null 2>&1
    check 'real git: a detached HEAD is a short sha' "⚪ one@$_sha" "$(dry idle "$rg/one")"
else
    printf 'SKIP  real-git cross-check (no git binary)\n'
fi

# --- CCTAB_TERMINAL --------------------------------------------------------
# `KONSOLE_*` is inherited environment and does not survive an ssh, so over ssh
# or inside tmux there is nothing to detect. This says it explicitly, and any
# other value says explicitly NOT Konsole - which is how a leaked `KONSOLE_*`
# from a locally launched xterm is turned off.
check 'CCTAB_TERMINAL=konsole picks the Konsole glyph position' '~/plaindir ⚪' \
    "$(cd -- "$tmp/plaindir" && HOME=$tmp CCTAB_TERMINAL=konsole CCTAB_DRY_RUN=1 \
          "$bin" idle </dev/null)"
check 'CCTAB_TERMINAL=konsole survives a multiplexer' '~/plaindir ⚪' \
    "$(cd -- "$tmp/plaindir" && HOME=$tmp CCTAB_TERMINAL=konsole TMUX=nonsense \
          CCTAB_DRY_RUN=1 "$bin" idle </dev/null)"
check 'any other value says explicitly NOT Konsole' '⚪ ~/plaindir' \
    "$(cd -- "$tmp/plaindir" && HOME=$tmp KONSOLE_VERSION=260801 \
          CCTAB_TERMINAL=wezterm CCTAB_DRY_RUN=1 "$bin" idle </dev/null)"
# A byte compare made CCTAB_TERMINAL=Konsole mean "explicitly NOT Konsole" and
# silently turned the arming and the suffix layout off - the opposite of what was
# typed. ASCII case-insensitive, and still exact: konsol and konsolex are not it.
check 'CCTAB_TERMINAL=Konsole is Konsole' '~/plaindir ⚪' \
    "$(cd -- "$tmp/plaindir" && HOME=$tmp CCTAB_TERMINAL=Konsole CCTAB_DRY_RUN=1 \
          "$bin" idle </dev/null)"
check 'CCTAB_TERMINAL=KONSOLE is Konsole' '~/plaindir ⚪' \
    "$(cd -- "$tmp/plaindir" && HOME=$tmp CCTAB_TERMINAL=KONSOLE CCTAB_DRY_RUN=1 \
          "$bin" idle </dev/null)"
check 'a near miss is still NOT Konsole' '⚪ ~/plaindir' \
    "$(cd -- "$tmp/plaindir" && HOME=$tmp KONSOLE_VERSION=260801 \
          CCTAB_TERMINAL=konsolex CCTAB_DRY_RUN=1 "$bin" idle </dev/null)"
check 'an empty CCTAB_TERMINAL is unset' '⚪ ~/plaindir' \
    "$(cd -- "$tmp/plaindir" && HOME=$tmp CCTAB_TERMINAL= CCTAB_DRY_RUN=1 \
          "$bin" idle </dev/null)"
check 'doctor names CCTAB_TERMINAL as the reason' '1' \
    "$(cd -- "$tmp/plaindir" && HOME=$tmp CLAUDE_CONFIG_DIR=$tmp/tcfg \
          CCTAB_TERMINAL=konsole "$bin" doctor 2>&1 \
       | grep -c '^terminal: *Konsole, from CCTAB_TERMINAL=konsole')"
check 'doctor names it when it says NOT Konsole too' '1' \
    "$(cd -- "$tmp/plaindir" && HOME=$tmp CLAUDE_CONFIG_DIR=$tmp/tcfg \
          KONSOLE_VERSION=260801 CCTAB_TERMINAL=wezterm "$bin" doctor 2>&1 \
       | grep -c '^terminal: *not Konsole: CCTAB_TERMINAL=wezterm')"
check 'doctor points at it when a multiplexer hides KONSOLE_*' '1' \
    "$(cd -- "$tmp/plaindir" && HOME=$tmp CLAUDE_CONFIG_DIR=$tmp/tcfg \
          KONSOLE_VERSION=260801 TMUX=nonsense "$bin" doctor 2>&1 \
       | grep -c 'set CCTAB_TERMINAL=konsole if the outer terminal really is Konsole')"
# The likeliest misconfiguration in the topology this slice exists for is a
# Konsole tab that ssh'ed into a tmux, where nothing can be detected: doctor has
# to name the remedy, not just the problem.
check 'doctor names the remedy on the multiplexer line' '1' \
    "$(cd -- "$tmp/plaindir" && HOME=$tmp CLAUDE_CONFIG_DIR=$tmp/tcfg \
          TMUX=nonsense "$bin" doctor 2>&1 \
       | grep -c 'If the outer terminal IS Konsole, set CCTAB_TERMINAL=konsole')"

# --- the state layer: wait ownership --------------------------------------
# Everything above this point is the STATELESS program, because the suite unsets
# XDG_RUNTIME_DIR and CCTAB_STATE_DIR at the top. This section is the other half:
# it replays the real capture the record exists for, through the shipped binary,
# with the clock pinned so the record's bytes can be asserted rather than believed.
#
# The sequence, from tests captured under a throwaway config (timings relative to
# that session's SessionStart):
#
#    68.946  Stop               background_tasks=[subagent:running:aec99e]
#    69.960  PermissionRequest  agent_id=aec99e1f          -> ORANGE
#    75.983  Notification       permission_prompt, NO agent_id (the 6s backstop)
#    98.459  SubagentStop       agent_id=a8e90c10, agent_type=""   (a GHOST)
#   107.496  PostToolUse        agent_id=aec99e1f   (you approved; the tool ran)
#
# Stateless, 68.946 painted white over nothing, 69.960 painted orange, and 107.496
# painted NOTHING - so the tab read orange until the Task returned, which is
# minutes. Inside tmux that heals itself after CCTAB_TTL_WAITING; in a plain
# Konsole tab nothing healed it at all.
#
# CLAUDE_PID IS PINNED PER CASE here, and that is not cosmetic. The layer reads it
# to stamp a record's origin, so a suite run from inside a Claude Code session -
# which is how this project is developed, and the only place a developer would run
# it - would otherwise have the ambient session's pid land in records the cases
# below assert byte for byte. `st` sets it EMPTY (config::var_nonempty reads that
# as absent); `stp` sets one on purpose, for the reaper's origin rule.
_sd=$tmp/state
_sid=aec0f2b1-4d31-4e11-9a41-2c7d55e1a900
_ag=aec99e1f4bda1972b
_ag2=bbb17c4e9a2d3f001
_ghost=a8e90c10430da8891

# st <edge> <payload> -- one hook run against the shared record, clock pinned.
st() {
    (cd -- "$tmp/repos/plain" && printf '%s\n' "$2" \
        | HOME=$tmp CCTAB_DRY_RUN=1 CCTAB_STATE_DIR=$_sd CCTAB_NOW=${_now:-1000000} \
          CLAUDE_PID= "$bin" "$1")
}
# rec -- the record as one line, so a check can pin every field at once.
rec() { tr '\n' '|' <"$_sd/$_sid" 2>/dev/null; }
# pl <event> [extra] -- a payload with the session id and, optionally, more members
pl() { printf '{"session_id":"%s","hook_event_name":"%s"%s}' "$_sid" "$1" "${2-}"; }
# The two shapes of Stop, which is what decides whether a wait the main loop does
# NOT own may be retired. `background_tasks` holds one entry per live subagent, so
# an empty array proves nothing outside the main loop is running; a non-empty one -
# the capture's 68.946 Stop, which lists the agent that raises the dialog a second
# later - proves the opposite, and an ABSENT one means "I do not know".
stop_busy() {
    pl Stop ",\"background_tasks\":[{\"id\":\"$_ag\",\"type\":\"subagent\",\"status\":\"running\"}]"
}
stop_quiet() { pl Stop ',"background_tasks":[]'; }

rm -rf "$_sd"
check 'state: Stop with nothing waiting paints idle and records the base' '⚪ plain@master' \
    "$(st idle "$(stop_quiet)")"
# ...and writes NOTHING, because the base it would record is the base a session
# with no record is already assumed to have. That is `write_if_changed`, and it is
# what keeps the per-edge cost a READ: the common case never touches the disk.
check 'state: ...and an idle session with nothing waiting writes no record' 'gone' \
    "$([ -e "$_sd/$_sid" ] && printf present || printf gone)"
check 'state: the state directory is created mode 700' '700' \
    "$(ls -ld "$_sd" | awk '{print substr($1,2,9)}' \
        | sed 's/rwx/7/;s/r-x/5/;s/---/0/g;s/r--/4/' | tr -d '\n')"

check 'state: a subagent PermissionRequest paints waiting, as it always did' '🟠 plain@master' \
    "$(st waiting "$(pl PermissionRequest ",\"agent_id\":\"$_ag\"")")"
# The epoch is PER WAIT, not one field for the whole list. With one shared epoch
# every later dialog pushed a stale wait's expiry out with it, so a session raising
# dialogs more often than the TTL never expired it - and nothing paints while a
# wait is held, so the tab froze orange for the rest of the session.
check 'state: ...and the wait is recorded against that agent, with its own epoch' \
    "cts1|b i|w $_ag:1000000|" "$(rec)"

# The backstop is for the SAME dialog. Recording a second, UNKNOWN owner for it
# would mean two waits for one dialog and only one of them retirable - so it is
# only recorded when nothing is waiting at all.
check 'state: the 6s notification backstop adds no second owner' '🟠 plain@master' \
    "$(st notify "$(pl Notification ',"notification_type":"permission_prompt"')")"
check 'state: ...and the record is unchanged' "cts1|b i|w $_ag:1000000|" "$(rec)"

# A main-thread tool call while the dialog is up. Stateless this painted BLUE over
# the open dialog, once per tool call. The base still moves - main IS working -
# but the tab keeps the dialog.
check 'state: a main-thread PostToolUse does not repaint over the dialog' '' \
    "$(st working "$(pl PostToolUse)")"
check 'state: ...but the base it would have painted is remembered' \
    "cts1|b w|w $_ag:1000000|" "$(rec)"

# The ghost. No SubagentStart ever announced a8e90c10, its agent_type is empty,
# and it fires nine seconds before the user answers. Matching on the OWNER is what
# stops it un-painting a live dialog.
check 'state: a SubagentStop for an agent that owns nothing does nothing' '' \
    "$(st subagent-stop "$(pl SubagentStop ",\"agent_id\":\"$_ghost\"")")"
check 'state: ...and leaves the record alone' "cts1|b w|w $_ag:1000000|" "$(rec)"

# THE FIX. The subagent's own PostToolUse is the un-paint, and what comes back is
# the BASE the wait covered up - not a guess at it.
check 'state: the owning agent PostToolUse clears the wait and restores the base' \
    '🔵 plain@master' "$(st working "$(pl PostToolUse ",\"agent_id\":\"$_ag\"")")"
check 'state: ...and nothing is waiting any more' 'cts1|b w|' "$(rec)"

# With nothing waiting, a background subagent's tool call is the ORIGINAL filter,
# unchanged - and it writes nothing, which is what keeps the hot edge a read.
_before=$(rec)
check 'state: a background subagent tool call still paints nothing' '' \
    "$(st working "$(pl PostToolUse ",\"agent_id\":\"$_ag\"")")"
check 'state: ...and writes nothing' "$_before" "$(rec)"

# Stop must not paint idle over a dialog that is still up. This is the capture's
# 68.946/69.960 pair with the order reversed, and it is why the idle nudge matters:
# this machine's messageIdleNotifThresholdMs is 3000, not the 60000 default. The
# agent is LISTED in background_tasks, so the main loop has nothing to say about it.
rm -rf "$_sd"
st waiting "$(pl PermissionRequest ",\"agent_id\":\"$_ag\"")" >/dev/null
check 'state: Stop paints nothing while a listed agent dialog is outstanding' '' \
    "$(st idle "$(stop_busy)")"
check 'state: the 3s idle nudge is guarded the same way' '' \
    "$(st notify "$(pl Notification ',"notification_type":"idle_prompt"')")"
# A Stop with NO background_tasks at all is the same answer: absent has to mean "I
# do not know", or a Claude Code that renamed the member would paint white over
# every live dialog.
check 'state: a Stop that does not mention background_tasks retires nothing' '' \
    "$(st idle "$(pl Stop)")"

# ...but a Stop whose background_tasks is EMPTY proves the agent is gone, whatever
# hook it failed to fire on the way out. Esc at a subagent's dialog fires NO hook
# at all - measured, capture s2 - so without this the tab stayed orange for the
# whole 900s TTL, and outside tmux nothing else decays.
check 'state: an empty background_tasks at Stop retires an abandoned agent wait' \
    '⚪ plain@master' "$(st idle "$(stop_quiet)")"
check 'state: ...and the record no longer holds it' 'cts1|b i|' "$(rec)"

# The OTHER main-thread retirement: you cannot type at the prompt while a modal
# dialog is up, so a UserPromptSubmit proves the screen is clear whoever owned it.
# That bounds a stale agent wait to one turn even when no Stop ever arrives - a
# Ctrl+C mid-tool fires nothing at all, measured on capture s5.
rm -rf "$_sd"
st waiting "$(pl PermissionRequest ",\"agent_id\":\"$_ag\"")" >/dev/null
check 'state: a main-thread PostToolUse does not retire an agent wait' '' \
    "$(st working "$(pl PostToolUse)")"
# ...and neither does the UserPromptSubmit the PRODUCT injects when an async agent
# finishes - capture s4, 110.251, prompt "<task-notification>...". With two agents
# running, the first one's completion would otherwise retire the second one's live
# dialog, and the injected event is redundant anyway: that agent's own SubagentStop
# fires 20ms earlier.
check 'state: an injected task-notification prompt retires nothing' '' \
    "$(st working "$(pl UserPromptSubmit ',"prompt":"<task-notification>\n<task-id>x</task-id>"')")"
check 'state: a UserPromptSubmit the USER typed retires every wait' '🔵 plain@master' \
    "$(st working "$(pl UserPromptSubmit ',"prompt":"carry on"')")"
check 'state: ...and the record is clean' 'cts1|b w|' "$(rec)"

# ...but a MAIN wait at Stop is stale by construction: the loop could not have
# stopped while a main-thread dialog blocked it. A rejected ExitPlanMode is
# exactly that, and it needs no help from background_tasks - this Stop's array is
# deliberately non-empty to prove so.
rm -rf "$_sd"
st waiting "$(pl PreToolUse ',"tool_name":"ExitPlanMode"')" >/dev/null
check 'state: Stop does clear a stale main-thread wait' '⚪ plain@master' \
    "$(st idle "$(stop_busy)")"

# A declined dialog: the tool never runs, so no PostToolUse ever comes. The
# agent's own SubagentStop is the only signal left.
rm -rf "$_sd"
st waiting "$(pl PermissionRequest ",\"agent_id\":\"$_ag\"")" >/dev/null
check 'state: a declined dialog is cleared by that agent SubagentStop' '⚪ plain@master' \
    "$(st subagent-stop "$(pl SubagentStop ",\"agent_id\":\"$_ag\"")")"

# Two dialogs at once. A single owner slot gets this wrong whichever one it keeps:
# answering the main one must not restore the base while the agent's is still up.
rm -rf "$_sd"
st waiting "$(pl PermissionRequest ",\"agent_id\":\"$_ag\"")" >/dev/null
st waiting "$(pl PermissionRequest)" >/dev/null
check 'state: two overlapping dialogs are both recorded, newest last' \
    "cts1|b i|w $_ag:1000000 -:1000000|" "$(rec)"
check 'state: answering the main one keeps the tab orange' '' \
    "$(st working "$(pl PostToolUse)")"
check 'state: answering the agent one finally restores the base' '🔵 plain@master' \
    "$(st working "$(pl PostToolUse ",\"agent_id\":\"$_ag\"")")"

# THE PER-WAIT EPOCH, which is the fix for the worst shape this layer could take:
# a stale wait that unrelated later dialogs kept alive for ever, freezing the tab
# orange with nothing able to repaint it.
rm -rf "$_sd"
st waiting "$(pl PermissionRequest ",\"agent_id\":\"$_ag\"")" >/dev/null
for _t in 1000002 1000004 1000006 1000008; do
    _now=$_t st waiting "$(pl PermissionRequest ",\"agent_id\":\"$_ag2\"")" >/dev/null
done
check 'state: an unrelated dialog does not touch another wait epoch' \
    "cts1|b i|w $_ag:1000000 $_ag2:1000008|" "$(rec)"
_now=1000009
check 'state: the stale wait expires on its OWN clock and the fresh one holds' '' \
    "$(cd -- "$tmp/repos/plain" && printf '%s\n' "$(stop_busy)" \
        | HOME=$tmp CCTAB_DRY_RUN=1 CCTAB_STATE_DIR=$_sd CCTAB_NOW=$_now \
          CCTAB_TTL_WAITING=3 CLAUDE_PID= "$bin" idle)"
# ...and the expiry was PERSISTED, so a later, larger CCTAB_TTL_WAITING cannot
# resurrect a wait that has already been declared dead.
check 'state: ...and the expired wait is written out of the record' \
    "cts1|b i|w $_ag2:1000008|" "$(rec)"
_now=1000000

# The unattributable `?`. It used to survive Stop AND the 3s nudge, so once the
# user had dealt with the dialog the tab sat orange through the whole idle period,
# where the STATELESS binary painted white. All five waiting kinds behaved that
# way, not the two the README named.
for _kind in permission_prompt worker_permission_prompt agent_needs_input \
             elicitation_dialog elicitation_url_dialog; do
    rm -rf "$_sd"
    check "state: the $_kind backstop paints waiting against an unknown owner" \
        '🟠 plain@master' "$(st notify "$(pl Notification ",\"notification_type\":\"$_kind\"")")"
    check "state: ...and a quiet Stop retires it rather than leaving the tab orange" \
        '⚪ plain@master' "$(st idle "$(stop_quiet)")"
done
# A subagent's own completion retires a lone `?` too: agent_needs_input and
# worker_permission_prompt are raised BY a subagent and carry no id, so its tool
# running, or its SubagentStop, is what answers them.
rm -rf "$_sd"
st notify "$(pl Notification ',"notification_type":"agent_needs_input"')" >/dev/null
check 'state: a subagent PostToolUse retires a lone unknown owner' '⚪ plain@master' \
    "$(st working "$(pl PostToolUse ",\"agent_id\":\"$_ag\"")")"
rm -rf "$_sd"
st notify "$(pl Notification ',"notification_type":"worker_permission_prompt"')" >/dev/null
check 'state: a SubagentStop retires a lone unknown owner' '⚪ plain@master' \
    "$(st subagent-stop "$(pl SubagentStop ",\"agent_id\":\"$_ag\"")")"
# And the reverse arrival order - backstop first, which the wait() guard cannot
# catch - collapses to ONE wait, the attributable one.
rm -rf "$_sd"
st notify "$(pl Notification ',"notification_type":"worker_permission_prompt"')" >/dev/null
st waiting "$(pl PermissionRequest ",\"agent_id\":\"$_ag\"")" >/dev/null
check 'state: an attributable dialog supersedes a lone unknown owner' \
    "cts1|b i|w $_ag:1000000|" "$(rec)"

# A wait nothing ever clears must not hold the tab orange forever: a subagent
# killed mid-dialog fires no SubagentStop at all, and this Stop still says it is
# running. The horizon is the same CCTAB_TTL_WAITING tmux decays an orange title
# with.
rm -rf "$_sd"
st waiting "$(pl PermissionRequest ",\"agent_id\":\"$_ag\"")" >/dev/null
_now=1000900
check 'state: a wait still inside CCTAB_TTL_WAITING holds the tab' '' \
    "$(st idle "$(stop_busy)")"
_now=1000901
check 'state: a wait past CCTAB_TTL_WAITING has expired' '⚪ plain@master' \
    "$(st idle "$(stop_busy)")"
_now=1000000
# The knob is the tmux one, and one grammar covers both.
rm -rf "$_sd"
st waiting "$(pl PermissionRequest ",\"agent_id\":\"$_ag\"")" >/dev/null
_now=1000061
check 'state: CCTAB_TTL_WAITING moves the horizon' '⚪ plain@master' \
    "$(cd -- "$tmp/repos/plain" && printf '%s\n' "$(stop_busy)" \
        | HOME=$tmp CCTAB_DRY_RUN=1 CCTAB_STATE_DIR=$_sd CCTAB_NOW=$_now \
          CCTAB_TTL_WAITING=60 CLAUDE_PID= "$bin" idle)"
_now=1000000

# SessionStart starts over - a --resume must not inherit a dialog that is long
# gone - and it is the reaper: a session killed with SIGKILL fires no SessionEnd,
# so a stale record is normal rather than exceptional.
rm -rf "$_sd"
st waiting "$(pl PermissionRequest ",\"agent_id\":\"$_ag\"")" >/dev/null
mkdir -p "$_sd"
printf 'cts1\nb i\n' >"$_sd/dead-session-0000"
touch -d '2 days ago' "$_sd/dead-session-0000" 2>/dev/null \
    || touch -t 200001010000 "$_sd/dead-session-0000"
st session-start "$(pl SessionStart ',"source":"resume"')" >/dev/null 2>&1
check 'state: SessionStart resets the record' 'cts1|b i|' "$(rec)"
check 'state: SessionStart reaps a record nothing has touched for a day' 'gone' \
    "$([ -e "$_sd/dead-session-0000" ] && printf present || printf gone)"
check 'state: SessionStart does not reap a live record' 'present' \
    "$([ -e "$_sd/$_sid" ] && printf present || printf gone)"

# WHAT THE REAPER MAY NOT TOUCH. CCTAB_STATE_DIR is a documented user knob, so the
# directory is not always one we created - and `reapable` used to fall back to
# mtime for anything it could not parse, which deleted a 30-day-old private key out
# of a directory that had other things in it. The name grammar cannot tell `id_rsa`
# from a session id, so the rule is now CONTENT: only a record, or a `<id>.<pid>.tmp`,
# is ever a candidate.
rm -rf "$_sd"; mkdir -p "$_sd"
printf 'a shopping list\n' >"$_sd/notes.txt"
printf -- '-----BEGIN OPENSSH PRIVATE KEY-----\n' >"$_sd/id_rsa"
printf 'cts2\nb w\n' >"$_sd/from-a-newer-version"
mkdir -p "$_sd/a-subdir"
printf 'cts1\nb i\n' >"$_sd/clock-went-backwards"
for _f in notes.txt id_rsa from-a-newer-version a-subdir; do
    touch -d '30 days ago' "$_sd/$_f" 2>/dev/null || touch -t 200001010000 "$_sd/$_f"
done
# An mtime in the FUTURE used to make a file immortal: duration_since errors for
# it, and the error read as "not reapable".
touch -d '+3 days' "$_sd/clock-went-backwards" 2>/dev/null \
    || touch -t 203001010000 "$_sd/clock-went-backwards"
st session-start "$(pl SessionStart ',"source":"startup"')" >/dev/null 2>&1
check 'state: the reaper leaves a file that is not a record of ours, however old' \
    'notes.txt id_rsa' \
    "$([ -e "$_sd/notes.txt" ] && printf 'notes.txt '; [ -e "$_sd/id_rsa" ] && printf id_rsa)"
check 'state: ...and a directory, which remove_file could never have taken' 'present' \
    "$([ -d "$_sd/a-subdir" ] && printf present || printf gone)"
check 'state: ...but takes an aged record from a NEWER version of this tool' 'gone' \
    "$([ -e "$_sd/from-a-newer-version" ] && printf present || printf gone)"
check 'state: ...and a record whose mtime is in the future is not immortal' 'gone' \
    "$([ -e "$_sd/clock-went-backwards" ] && printf present || printf gone)"

# The reaper's OTHER rule, and the only one that can PROVE a live session safe.
# mtime cannot: a session sitting at its prompt touches nothing, so an mtime
# horizon short enough to be useful would delete a LIVE session's record. So every
# write stamps the record with ($CLAUDE_PID, that pid's start time), and the reaper
# unlinks only when that pair no longer names a running process. A hook is a child
# of $CLAUDE_PID, so that process exists whenever its own hooks fire, and a start
# time never changes - which is the whole proof.
#
# stp <edge> <payload> <pid> -- the same hook run, with a CLAUDE_PID to stamp.
stp() {
    (cd -- "$tmp/repos/plain" && printf '%s\n' "$2" \
        | HOME=$tmp CCTAB_DRY_RUN=1 CCTAB_STATE_DIR=$_sd CCTAB_NOW=${_now:-1000000} \
          CLAUDE_PID=$3 "$bin" "$1")
}
# starttime <pid> -- field 22 of /proc/<pid>/stat, read after the LAST ") ". Field
# 2 is the executable name in parentheses and may itself contain spaces and
# parens - measured on this machine, one reads `(npm exec chrome...)` - so the
# naive `awk '{print $22}'` reads the wrong field for exactly the processes a
# claude session spawns. The binary parses it the same way.
starttime() { sed 's/.*) //' "/proc/$1/stat" | cut -d' ' -f20; }

sleep 300 &
_livepid=$!
_livest=$(starttime "$_livepid")
sleep 0 &
_deadpid=$!
wait "$_deadpid" 2>/dev/null || :

rm -rf "$_sd"; mkdir -p "$_sd"
# A live session's record, stamped with a process that IS running, and given an
# ancient mtime so that the ONLY thing that can keep it is the origin rule.
printf 'cts1\nb w\np %s %s\n' "$_livepid" "$_livest" >"$_sd/live-session-0001"
touch -d '2 days ago' "$_sd/live-session-0001" 2>/dev/null \
    || touch -t 200001010000 "$_sd/live-session-0001"
# A dead one: that pid has exited, so nothing under it can match.
printf 'cts1\nb w\np %s %s\n' "$_deadpid" "$_livest" >"$_sd/dead-session-0002"
# The live pid with a start time that is NOT its own: a RECYCLED pid, which a
# pid-only rule would have read as alive and kept forever.
printf 'cts1\nb w\np %s %s\n' "$_livepid" "$((_livest + 1))" >"$_sd/recycled-0003"
check 'state: the setup dead pid really is gone' 'gone' \
    "$([ -d "/proc/$_deadpid" ] && printf present || printf gone)"
stp session-start "$(pl SessionStart ',"source":"startup"')" "$_livepid" >/dev/null 2>&1
check 'state: the reaper keeps a live session record whatever its mtime says' 'present' \
    "$([ -e "$_sd/live-session-0001" ] && printf present || printf gone)"
check 'state: the reaper takes a record whose pid has exited' 'gone' \
    "$([ -e "$_sd/dead-session-0002" ] && printf present || printf gone)"
check 'state: the reaper takes a record whose pid was recycled' 'gone' \
    "$([ -e "$_sd/recycled-0003" ] && printf present || printf gone)"
check 'state: SessionStart stamps its own origin into the record' \
    "cts1|b i|p $_livepid $_livest|" "$(rec)"
# And the stamp is carried by the next painting edge without a second /proc read.
stp working "$(pl PostToolUse)" "$_livepid" >/dev/null 2>&1
check 'state: ...and a later edge carries the origin rather than dropping it' \
    "cts1|b w|p $_livepid $_livest|" "$(rec)"
# A record that carries NO origin - one written before this field existed, or by a
# session whose SessionStart never ran - gains one on its next write, so the
# reaper's liveness proof covers every record this version writes. Without that,
# an open-but-quiet session could cross the 24h mtime horizon; one such record
# existed on this machine, for a session that was running.
printf 'cts1\nb i\n' >"$_sd/$_sid"
stp working "$(pl PostToolUse)" "$_livepid" >/dev/null 2>&1
check 'state: a write stamps an origin the record was missing' \
    "cts1|b w|p $_livepid $_livest|" "$(rec)"

# doctor is how you find out which of the above happened. Read-only, deliberately:
# the command you run when something is already wrong must not delete the evidence,
# so it NAMES the stale files the next session-start will take.
_doc() {
    (cd -- "$tmp/repos/plain" \
        && HOME=$tmp CCTAB_DRY_RUN=1 CCTAB_STATE_DIR=$_sd CCTAB_NOW=${_now:-1000000} \
           CLAUDE_PID= "$bin" doctor </dev/null 2>&1)
}
rm -f "$_sd/live-session-0001"
printf 'cts1\nb w\np %s %s\nw %s:999990\n' "$_livepid" "$_livest" "$_ag" >"$_sd/$_sid"
printf 'cts1\nb w\np %s %s\n' "$_deadpid" "$_livest" >"$_sd/dead-session-0004"
check 'doctor: names the record directory and counts what is stale' '1' \
    "$(_doc | grep -c "^record:    $_sd (2 records, 1 stale)")"
check 'doctor: says what a record holds' '1' \
    "$(_doc | grep -c "^ *$_sid: base working, session pid $_livepid live, waiting on 1 ($_ag raised 10s ago)$")"
check 'doctor: names the stale record and what will take it' '1' \
    "$(_doc | grep -c "dead-session-0004: .*pid $_deadpid GONE.*STALE.*next session-start reaps it")"
check 'doctor: does not itself delete the stale record' 'present' \
    "$([ -e "$_sd/dead-session-0004" ] && printf present || printf gone)"
check 'doctor: reports a record too big to be one of ours as unreadable' '1' \
    "$(dd if=/dev/zero of="$_sd/huge-0005" bs=1 count=5000 2>/dev/null
       _doc | grep -c 'huge-0005: unreadable')"
# Five broken shapes used to print the same healthy-looking line as an idle
# record, which made the report the wrong place to look when something was wrong.
check 'doctor: tells a file that is not a record from a healthy idle one' '1' \
    "$(printf '\001\002junk' >"$_sd/corrupt-0006"
       _doc | grep -c "corrupt-0006: not a record in any version's shape")"
check 'doctor: names a NEWER version record as one it will never reap' '1' \
    "$(printf 'cts9\nb w\n' >"$_sd/newer-0007"
       _doc | grep -c 'newer-0007: a NEWER version')"
check 'doctor: names a file that is not ours at all as one the reaper leaves' '1' \
    "$(printf 'not mine\n' >"$_sd/theirs.txt"
       _doc | grep -c 'theirs.txt: .*the reaper leaves it alone')"
# THE ONE FAILURE MODE EVERY OTHER LINE RENDERS AS HEALTHY: a directory that can be
# read but not written records no wait at all, so a subagent's PostToolUse finds
# none to clear and paints nothing - the slice-3 defect, back in silence.
check 'doctor: reports a state directory that cannot be written' '1' \
    "$(chmod 500 "$_sd"
       _doc | grep -c '^ *FAIL not writable - no wait is ever recorded'
       chmod 700 "$_sd")"
# "disabled" is the silent answer to almost every question about this layer: a tab
# behaving exactly as it did before wait ownership existed looks identical to one
# where it is working. So doctor prints the reason rather than implying it.
check 'doctor: says the layer is disabled, and why, when there is nowhere to keep a record' '1' \
    "$( (cd -- "$tmp/repos/plain" && env -u XDG_RUNTIME_DIR -u CCTAB_STATE_DIR \
           HOME=$tmp CCTAB_DRY_RUN=1 "$bin" doctor </dev/null 2>&1) \
        | grep -c '^record:    disabled - no CCTAB_STATE_DIR and no XDG_RUNTIME_DIR')"
kill "$_livepid" 2>/dev/null || :

# uninstall is the other half of the lifecycle, and the records are the other thing
# an install leaves on the disk. After it the plugin is unlinked, so no hook of any
# session runs again to write another, and a live session with no record simply
# paints what it painted before this plugin existed.
rm -rf "$_sd"; mkdir -p "$_sd"
printf 'cts1\nb w\nw %s:1000000\n' "$_ag" >"$_sd/$_sid"
_unc=$tmp/config-records
_insr() {
    ( cd -- "$repo" && HOME=$tmp CLAUDE_CONFIG_DIR=$_unc CCTAB_STATE_DIR=$_sd \
        "$bin" "$@" </dev/null 2>&1 )
}
_insr install >/dev/null
check 'uninstall: says it removed the wait-ownership records' '1' \
    "$(_insr uninstall | grep -c "^records:  removed $_sd (1 record")"
check 'uninstall: ...and the directory is actually gone' 'gone' \
    "$([ -d "$_sd" ] && printf present || printf gone)"
# ...but it draws the same line the reaper does: a CCTAB_STATE_DIR the user pointed
# at a shared directory must not be emptied by an uninstaller either.
mkdir -p "$_sd"
printf 'cts1\nb w\n' >"$_sd/$_sid"
printf 'my own notes\n' >"$_sd/notes.txt"
_insr install >/dev/null
check 'uninstall: leaves a file that is not a record of ours' '1' \
    "$(_insr uninstall | grep -c "^records:  removed 1 record(s) from $_sd, which holds other files")"
check 'uninstall: ...and that file is still there' 'notes.txt' "$(ls -A "$_sd")"
rm -rf "$_unc" "$_sd"

# The edges that used to only DRAIN stdin - waiting, idle, session-end - now read a
# window, because the record is filed under the payload's session_id. `waiting` and
# `session-end` read the FRONT window only: session_id and agent_id are both front
# members. `idle` is the exception - it reads background_tasks, which a Stop
# serializes LAST - so it pays for a tail as well, once per turn. Either way it is a
# bound on the SEARCH, not on the drain: a megabyte of tool_input still has to be
# read to EOF.
rm -rf "$_sd"; mkdir -p "$_sd"
_big=$(awk 'BEGIN{while(i++<1048576)printf "a"}' </dev/null)
check 'state: a 1 MiB PermissionRequest still finds the session and the owner' '🟠 plain@master' \
    "$(st waiting "$(printf '{"session_id":"%s","hook_event_name":"PermissionRequest","agent_id":"%s","tool_input":{"content":"%s"}}' \
        "$_sid" "$_ag" "$_big")")"
check 'state: ...and the wait was recorded against that agent, not lost' \
    "cts1|b i|w $_ag:1000000|" "$(rec)"
check 'state: a 1 MiB Stop with no background_tasks is still refused over that dialog' '' \
    "$(st idle "$(printf '{"session_id":"%s","hook_event_name":"Stop","blob":"%s"}' "$_sid" "$_big")")"
# ...and the same payload with an EMPTY background_tasks as its last member does
# retire it, which is the assertion that the tail window is actually read.
check 'state: ...but a 1 MiB Stop whose LAST member is an empty array retires it' \
    '⚪ plain@master' \
    "$(st idle "$(printf '{"session_id":"%s","hook_event_name":"Stop","blob":"%s","background_tasks":[]}' \
        "$_sid" "$_big")")"
unset _big

rm -rf "$_sd"; mkdir -p "$_sd"
st waiting "$(pl PermissionRequest ",\"agent_id\":\"$_ag\"")" >/dev/null
st session-start "$(pl SessionStart ',"source":"resume"')" >/dev/null 2>&1

# A mid-turn compaction re-fires SessionStart. It paints nothing, and it must
# reset nothing either.
st waiting "$(pl PermissionRequest ",\"agent_id\":\"$_ag\"")" >/dev/null
st session-start "$(pl SessionStart ',"source":"compact"')" >/dev/null 2>&1
check 'state: a compaction SessionStart resets nothing' "cts1|b i|w $_ag:1000000|" "$(rec)"
check 'state: SessionEnd removes the record' 'gone' \
    "$(st session-end "$(pl SessionEnd)" >/dev/null 2>&1
       [ -e "$_sd/$_sid" ] && printf present || printf gone)"

# The session id names a FILE, and it arrives from the payload.
rm -rf "$_sd"
check 'state: a session_id that is not [A-Za-z0-9_-] writes nothing' '⚪ plain@master' \
    "$(st idle '{"session_id":"../../../etc/x","hook_event_name":"Stop"}')"
check 'state: ...and left no file anywhere' '0' \
    "$(find "$_sd" -type f 2>/dev/null | wc -l | tr -d ' ')"
check 'state: a payload with no session_id is the stateless answer' '⚪ plain@master' \
    "$(st idle '{"hook_event_name":"Stop"}')"
check 'state: ...and still left no file' '0' \
    "$(find "$_sd" -type f 2>/dev/null | wc -l | tr -d ' ')"
# A record that is not ours is ignored rather than misread, in both directions:
# a future tag, and a future base letter this version cannot paint.
rm -rf "$_sd"; mkdir -p "$_sd"
printf 'cts2\nb w\nw %s:1000000\n' "$_ag" >"$_sd/$_sid"
check 'state: a record with a newer tag is ignored, not misread' '⚪ plain@master' \
    "$(st idle "$(pl Stop)")"
printf 'cts1\nb p\ng %s\nn 5 a session title\nw %s:1000000\n' "$_ag" "$_ag" >"$_sd/$_sid"
check 'state: a reserved key is skipped and an unknown base reads as idle' '' \
    "$(st idle "$(pl Stop)")"
# The wait it carried was honoured - that is why nothing painted - and because
# honouring it changed nothing, the record was not rewritten, so the `g` and `n`
# lines a NEWER version wrote are still there. An older binary passing through a
# newer record leaves it intact unless it has something to say.
check 'state: ...and a record it did not change is left byte for byte' \
    "cts1|b p|g $_ag|n 5 a session title|w $_ag:1000000|" "$(rec)"
# A record in the older SINGLE-EPOCH `w <epoch> <owner>` shape degrades to
# "nothing waiting" rather than to a wait with an invented clock, which is the safe
# direction across an upgrade: a spurious repaint self-corrects, a phantom wait
# does not.
printf 'cts1\nb w\nw 1000000 %s\n' "$_ag" >"$_sd/$_sid"
check 'state: a wait in the older single-epoch shape is dropped, not misread' \
    '⚪ plain@master' "$(st idle "$(pl Stop)")"

unset _now
rm -rf "$_sd"

# --- tmux ------------------------------------------------------------------
# Inside tmux the OSC 0 the plugin emits never reaches the outer terminal: tmux
# stores it as `pane_title` and emits a title of its OWN, computed from
# `set-titles-string`. So inside tmux that OSC 0 stops being a tab title and
# becomes a RECORD - `<location> ct1 <state> <epoch>` - and the format
# SessionStart installs turns the records of every claude pane into a strip of
# glyphs that DECAYS: the painted glyph while fresh, the idle glyph once stale,
# nothing at all once old. Nothing has to fire for the tab to stop lying, which
# is what makes an abandoned dialog, a local slash command and Ctrl+C heal
# themselves.
#
# Everything below drives a PRIVATE server, `tmux -L cctabprobe -f /dev/null`,
# and kills it afterwards. The user's own server is never listed, attached,
# configured or killed, `-f /dev/null` means no ~/.tmux.conf is read, and no
# client is ever attached, so no pty is touched.
#
# WHAT THIS CANNOT SEE: tmux RE-EMITTING the title to an attached client. That
# needs a pty, which this suite cannot allocate (the corpus can, and does, for
# the two direct-write edges). What is asserted instead is the whole of the
# server side: that the format is a function of the clock, that re-rendering it
# after a wait gives a different answer with no process running and no hook
# firing, and that the two settings tmux re-renders ON are in force.
ts=cctabprobe
tmux_gone() { tmux -L "$ts" kill-server 2>/dev/null; }
if ! command -v tmux >/dev/null 2>&1; then
    printf 'SKIP  tmux section (no tmux binary on PATH)\n'
else
    # Fold the probe server into the existing cleanup, so a failure below cannot
    # leave a server running.
    cleanup() { tmux_gone; rm -rf "$tmp"; }
    tmux_gone
    tsock=/tmp/tmux-$(id -u)/$ts
    mkdir -p "$tmp/code/one" "$tmp/code/two" "$tmp/plain"
    # tm <args> -- a command on the private server
    tm() { tmux -L "$ts" -f /dev/null "$@"; }
    # trec <edge> <cwd> <epoch> -- the record the binary emits inside tmux,
    # taken from the binary's OWN hook line so the test and the product cannot
    # drift apart.
    trec() {
        (cd -- "$2" && HOME=$tmp TMUX="$tsock,1,0" TMUX_PANE=%0 CCTAB_NOW=$3 \
            "$bin" "$1" </dev/null) \
            | sed 's/.*terminalSequence":"\\u001b]0;//; s/\\u0007".*//'
    }
    # tput_title <pane> <record> -- write the record from INSIDE the pane, which
    # is the only route that reaches pane_title, and wait for it to land.
    tput_title() {
        tm send-keys -t "$1" "printf '\\033]0;$2\\a'" Enter
        _i=0
        while [ "$_i" -lt 60 ]; do
            [ "$(tm display-message -p -t "$1" '#{pane_title}')" = "$2" ] && return 0
            sleep 0.1 2>/dev/null || sleep 1
            _i=$((_i + 1))
        done
        return 1
    }
    # tsession_start <cwd> [env...] -- the real SessionStart edge, against the
    # private server. CLAUDE_PID is unset, so no pty is written.
    tsession_start() {
        _d=$1
        shift
        (cd -- "$_d" && env HOME=$tmp TMUX="$tsock,1,0" TMUX_PANE=%0 "$@" \
            "$bin" session-start </dev/null >/dev/null 2>&1)
    }
    # trender [pane] -- what tmux would send the outer terminal right now
    trender() {
        tm display-message -p -t "${1:-t:w0.0}" "$tsts"
    }

    tmux -V >/dev/null 2>&1
    tm new-session -d -s t -n w0 -x 200 -y 50 /bin/sh
    # A user who already has both of these set, so the save and restore have
    # something real to preserve.
    TUSER='MY OWN #{pane_title} TITLE'
    tm set -g set-titles-string "$TUSER"
    tm set -g set-titles off
    tm set -g status on
    tm set -g status-interval 1

    # The product prints the two strings it installs, so this suite drives the
    # server with EXACTLY the product's format and not a copy of it.
    tsts=$(env -u TMUX "$bin" tmux-format | sed -n 1p)
    tfmt=$(env -u TMUX "$bin" tmux-format | sed -n 2p)
    check 'tmux-format prints the prefix trim'  '#{s|^ ||:#{T:@cctab_title}}' "$tsts"
    check 'tmux-format suffix swaps the trim'   '#{s| $||:#{T:@cctab_title}}' \
        "$(env -u TMUX CCTAB_GLYPH_POS=suffix "$bin" tmux-format | sed -n 1p)"
    check 'the format is one strip and one label' '1' \
        "$(printf '%s' "$tfmt" | grep -c '#{W:#{P:')"

    tsession_start "$tmp/code/one"
    check 'session-start installs our set-titles-string' "$tsts" \
        "$(tm show -gv set-titles-string)"
    check 'session-start installs the generated format' "$tfmt" \
        "$(tm display-message -p '#{@cctab_title}')"
    check 'session-start turns set-titles on' 'on' "$(tm show -gv set-titles)"
    check 'session-start writes the three glyphs' '🔵|🟠|⚪' \
        "$(tm display-message -p '#{@cctab_gw}|#{@cctab_ga}|#{@cctab_gi}')"
    check 'session-start writes the three TTLs' '1200|900|3600' \
        "$(tm display-message -p '#{@cctab_tw}|#{@cctab_ta}|#{@cctab_tg}')"
    check 'session-start saves the string it replaced' "$TUSER" \
        "$(tm display-message -p '#{@cctab_prev_string}')"
    check 'session-start saves set-titles as it found it' '0' \
        "$(tm display-message -p '#{@cctab_prev_titles}')"
    # The save is guarded, so a second claude cannot record OUR string as the
    # user's - the defect that would make uninstall install our own format.
    tsession_start "$tmp/code/two"
    check 'a second session-start does not clobber the save' "$TUSER" \
        "$(tm display-message -p '#{@cctab_prev_string}')"
    check 'the TTLs are configurable' '5|6|7' \
        "$(tsession_start "$tmp/code/one" CCTAB_TTL_WORKING=5 CCTAB_TTL_WAITING=6 \
              CCTAB_TTL_GONE=7
           tm display-message -p '#{@cctab_tw}|#{@cctab_ta}|#{@cctab_tg}')"
    check 'TTL 0 is a deadline no age can reach' '2147483647' \
        "$(tsession_start "$tmp/code/one" CCTAB_TTL_GONE=0
           tm display-message -p '#{@cctab_tg}')"
    tsession_start "$tmp/code/one"

    # The record itself, which is what the format reads back.
    check 'the working record names the state' "~/code/one ct1 w 1700000000" \
        "$(trec working "$tmp/code/one" 1700000000)"
    check 'the waiting record names the state' "~/code/two ct1 a 1700000000" \
        "$(trec waiting "$tmp/code/two" 1700000000)"
    check 'the idle record names the state' "~/plain ct1 i 1700000000" \
        "$(trec idle "$tmp/plain" 1700000000)"
    check 'outside tmux nothing is tagged' \
        '{"terminalSequence":"\u001b]0;🔵 ~/plain\u0007","suppressOutput":true}' \
        "$(cd -- "$tmp/plain" && HOME=$tmp "$bin" working </dev/null)"

    # A cell per claude PANE - `#{W:#{P:}}` and not `#{W:}`, because
    # `#{pane_title}` inside a window loop reads only that window's ACTIVE pane,
    # so two claudes split in one window would show one cell.
    tm new-window -d -t t -n w1 /bin/sh
    tm split-window -d -t t:w1 /bin/sh
    tm new-window -d -t t -n shell /bin/sh
    tnow=$(tm display-message -p '%s')
    tput_title t:w0.0 "$(trec working "$tmp/code/one" "$tnow")"
    tput_title t:w1.0 "$(trec waiting "$tmp/code/two" "$tnow")"
    tput_title t:w1.1 "$(trec idle "$tmp/plain" "$tnow")"
    check 'a cell per claude pane, in window then pane order' \
        '🔵🟠⚪ ~/code/one' "$(trender)"
    check 'a plain shell contributes no cell' '🔵🟠⚪ t:2:shell' "$(trender t:shell.0)"

    # The decay, by backdating the record the binary itself produced. 1300s is
    # past the 1200s working TTL and short of the 3600s disappear horizon; the
    # flip itself is strictly-greater over integer seconds, so the earliest
    # possible one is TTL+1s.
    tput_title t:w0.0 "$(trec working "$tmp/code/one" $((tnow - 1300)))"
    check 'working past its TTL decays to the idle glyph' '⚪🟠⚪ ~/code/one' "$(trender)"
    tput_title t:w0.0 "$(trec working "$tmp/code/one" $((tnow - 4000)))"
    check 'past the disappear horizon the cell is gone' '🟠⚪ ~/code/one' "$(trender)"
    tput_title t:w1.0 "$(trec waiting "$tmp/code/two" $((tnow - 400)))"
    check 'waiting is a summons and survives a coffee break' '🟠⚪ ~/code/one' \
        "$(trender)"
    tput_title t:w1.0 "$(trec waiting "$tmp/code/two" $((tnow - 1000)))"
    check 'waiting past its own TTL decays too' '⚪⚪ ~/code/one' "$(trender)"

    # THE POINT OF THE SLICE: the title changes because time passed. No process
    # is running, no hook fires, nothing is notified - the only input that moved
    # is the clock tmux reads for itself.
    tsession_start "$tmp/code/one" CCTAB_TTL_WORKING=2 CCTAB_TTL_GONE=600
    for _p in t:w0.0 t:w1.0 t:w1.1; do tput_title "$_p" ''; done
    tput_title t:w0.0 "$(trec working "$tmp/code/one" "$(tm display-message -p '%s')")"
    tfresh=$(trender)
    sleep 3
    tstale=$(trender)
    check 'a fresh paint renders the working glyph' '🔵 ~/code/one' "$tfresh"
    check 'the SAME paint renders white three seconds later' '⚪ ~/code/one' "$tstale"
    check 'and the decay ran with nothing running' 'changed' \
        "$([ "$tfresh" != "$tstale" ] && echo changed || echo 'the same')"
    # The clock that re-renders it. Without these two the flip above still
    # happens on demand, but tmux never re-emits it to the terminal on its own -
    # measured: with status off the title is re-evaluated once, about five
    # seconds after a client attaches, and then never again.
    check 'the decay clock is status plus status-interval' 'on|1' \
        "$(tm display-message -p '#{status}|#{status-interval}')"

    # Session end clears the record, which is what removes the cell, and with no
    # claude left the tab falls back to a tmux-shaped label rather than blanking.
    for _p in t:w0.0 t:w1.0 t:w1.1; do tput_title "$_p" ''; done
    check 'with no claude anywhere the label stands alone' 't:0:w0' "$(trender)"

    # No tmux invocation on the hot path. This is the assertion, not the timing:
    # one `tmux set-option` costs 2.84ms against a 0.37ms fork floor, so a hot
    # edge that touched the server would be a tenfold regression on a binary that
    # runs in 370us. If it ever does exec, it will configure the server, and the
    # server says so.
    check 'the hot edges exec no tmux at all' 'off|' \
        "$(tm set -g set-titles off
           tm set -su @cctab_title 2>/dev/null
           for _e in working waiting idle notify; do
               (cd -- "$tmp/plain" && HOME=$tmp TMUX="$tsock,1,0" TMUX_PANE=%0 \
                   "$bin" "$_e" </dev/null >/dev/null 2>&1)
           done
           printf '%s|%s' "$(tm show -gv set-titles)" \
               "$(tm display-message -p '#{@cctab_title}')")"
    tsession_start "$tmp/code/one"

    # The suffix layout is the same two pieces the other way round, for a tab that
    # elides from the LEFT.
    tput_title t:w0.0 "$(trec working "$tmp/code/one" "$(tm display-message -p '%s')")"
    check 'the prefix layout puts the strip first' '🔵 ~/code/one' "$(trender)"
    check 'the suffix layout puts the strip last' '~/code/one 🔵' \
        "$(tsession_start "$tmp/code/one" CCTAB_GLYPH_POS=suffix
           tm display-message -p -t t:w0.0 "$(env -u TMUX CCTAB_GLYPH_POS=suffix \
               "$bin" tmux-format | sed -n 1p)")"
    tsession_start "$tmp/code/one"

    # Konsole, inside tmux: the arming goes to the attached CLIENT's pty, never
    # to our own pane, and with no client attached there is nothing to write and
    # nothing to fail. The strip also moves to the end Konsole does not elide.
    check 'CCTAB_TERMINAL=konsole moves the strip to the suffix end' \
        '#{s| $||:#{T:@cctab_title}}' \
        "$(tsession_start "$tmp/code/one" CCTAB_TERMINAL=konsole
           tm show -gv set-titles-string)"
    check 'and session-end with no client attached is inert' '0' \
        "$(cd -- "$tmp/code/one" && HOME=$tmp TMUX="$tsock,1,0" TMUX_PANE=%0 \
              CCTAB_TERMINAL=konsole "$bin" session-end </dev/null >/dev/null 2>&1
           echo $?)"

    # THE RE-ARM. SessionStart's arming goes to the ptys `list-clients` names, so
    # an arming sent while DETACHED reaches nobody, and on reattach tmux replays
    # the TITLE but never the arming - measured with a real pty, the reattached
    # terminal was governed by Konsole's RemoteTabTitleFormat again and the glyph
    # was invisible. "Close the laptop while Claude keeps working" is the whole
    # point of the topology, so the server itself re-arms, from a session hook.
    tsession_start "$tmp/code/one" CCTAB_TERMINAL=konsole
    check 'konsole mode installs the client-attached hook' \
        "run-shell -b \"'#{@cctab_exe}' tmux-arm '#{client_tty}'\"" \
        "$(tm display-message -p -t t:w0.0 '#{client-attached[1971]}')"
    # The path is an OPTION and not text inside the hook: a hook value is parsed
    # when it is SET, and measured, `$rd` inside tmux's double quotes is expanded
    # there - a path holding `$` would lose a piece of itself. An option's value is
    # substituted literally.
    check 'and the binary travels as an option the hook expands' 'executable' \
        "$([ -x "$(tm display-message -p '#{@cctab_exe}')" ] && echo executable \
            || echo "[$(tm display-message -p '#{@cctab_exe}')]")"
    check 'the hook is scoped to our session, not the server' '' \
        "$(tm show-hooks -g | grep 'client-attached\[1971\]')"
    # Not Konsole takes it back off: the layout and the TTLs are already
    # last-SessionStart-wins, and an arming in force while the strip moved back to
    # the end Konsole elides is worse than either.
    tsession_start "$tmp/code/one"
    check 'a later plain session-start removes the hook again' '' \
        "$(tm display-message -p -t t:w0.0 '#{client-attached[1971]}')"
    # A user's own hooks live in the same array, and a BARE `set-hook -g
    # client-attached` replaces the WHOLE of it - measured. Ours is one index.
    tm set-hook -ga client-attached 'run-shell -b "true"'
    tsession_start "$tmp/code/one" CCTAB_TERMINAL=konsole
    check "a user's own client-attached hook survives ours" '1' \
        "$(tm show-hooks -g | grep -c 'client-attached\[0\]')"
    tm set-hook -gu client-attached

    # uninstall is the ONLY thing that puts the user's own pair back: the options
    # are server-wide, so a SessionEnd doing it would unpaint the other claude
    # windows still running.
    tsession_start "$tmp/code/one"
    tucfg=$tmp/tmuxcfg
    mkdir -p "$tucfg"
    (cd -- "$tmp/code/one" && HOME=$tmp CLAUDE_CONFIG_DIR=$tucfg \
        TMUX="$tsock,1,0" TMUX_PANE=%0 "$bin" uninstall --force) >"$tmp/unin.txt" 2>&1
    check 'uninstall restores the string SessionStart replaced' "$TUSER" \
        "$(tm show -gv set-titles-string)"
    check 'uninstall restores set-titles as it was found' 'off' \
        "$(tm show -gv set-titles)"
    check 'uninstall says what it restored' '1' \
        "$(grep -c '^tmux:.*restored' "$tmp/unin.txt")"
    check 'uninstall leaves none of our options behind' '' \
        "$(tm display-message -p \
            '#{@cctab_gw}#{@cctab_ga}#{@cctab_gi}#{@cctab_tw}#{@cctab_ta}#{@cctab_tg}#{@cctab_title}#{@cctab_string}#{@cctab_saved}#{@cctab_prev_string}#{@cctab_prev_titles}')"
    # THE SHARPEST WAY TO GET UNINSTALL WRONG: on a server no SessionStart has
    # ever reached, there is nothing of ours to put back, and a blind
    # `set -gu set-titles-string` would silently unset a string the user had set
    # themselves. The save flag is what decides, and nothing would report it if it
    # did not.
    # The uninstall above removed every option of ours, so the server is already
    # in the "no SessionStart ever reached me" state. Give it the user's pair back
    # and ask again.
    tm set -g set-titles-string "$TUSER"
    tm set -g set-titles on
    (cd -- "$tmp/code/one" && HOME=$tmp CLAUDE_CONFIG_DIR=$tucfg \
        TMUX="$tsock,1,0" TMUX_PANE=%0 "$bin" uninstall --force) >"$tmp/unin2.txt" 2>&1
    check 'uninstall leaves a string that was never ours alone' "$TUSER" \
        "$(tm show -gv set-titles-string)"
    check 'and leaves set-titles alone too' 'on' "$(tm show -gv set-titles)"
    check 'and says so rather than claiming a restore' '1' \
        "$(grep -c '^tmux:.*not ours to change' "$tmp/unin2.txt")"

    # THE OTHER SHARP WAY TO GET UNINSTALL WRONG: a saved string that POINTS AT our
    # own option. Pinning the pair from `tabstatus tmux-format` into ~/.tmux.conf -
    # which this README invites - means the first SessionStart saves OURS as the
    # user's, and putting it back after @cctab_title has been unset renders the
    # EMPTY string: a permanently blank tab title, reported as "restored".
    tm set -g set-titles-string "$tsts"
    tm set -g set-titles on
    tsession_start "$tmp/code/one"
    check 'the save really did record our own string' "$tsts" \
        "$(tm display-message -p '#{@cctab_prev_string}')"
    (cd -- "$tmp/code/one" && HOME=$tmp CLAUDE_CONFIG_DIR=$tucfg \
        TMUX="$tsock,1,0" TMUX_PANE=%0 "$bin" uninstall --force) >"$tmp/unin3.txt" 2>&1
    check 'uninstall does not put back a saved string of our own' '1' \
        "$(grep -c 'was one of OURS' "$tmp/unin3.txt")"
    check 'and set-titles-string is left at tmux own default' 'default' \
        "$([ "$(tm show -gv set-titles-string)" = "$tsts" ] && echo ours || echo default)"
    check 'so the tab title is not blank' 'renders' \
        "$([ -n "$(tm display-message -p -t t:w0.0 \
             "$(tm show -gv set-titles-string)")" ] && echo renders || echo blank)"

    # uninstall stops every claude window on this server, not just this one, and
    # has to say so: their records stay in their pane titles and then show up raw
    # in whatever title the restored string renders.
    tsession_start "$tmp/code/one"
    tput_title t:w1.0 "$(trec waiting "$tmp/code/two" "$(tm display-message -p '%s')")"
    (cd -- "$tmp/code/one" && HOME=$tmp CLAUDE_CONFIG_DIR=$tucfg \
        TMUX="$tsock,1,0" TMUX_PANE=%0 "$bin" uninstall --force) >"$tmp/unin4.txt" 2>&1
    check 'uninstall warns that other claude panes stop updating' '1' \
        "$(grep -c 'other claude pane' "$tmp/unin4.txt")"
    tput_title t:w1.0 ''

    # Outside tmux it cannot restore anything, and says so rather than claiming
    # success.
    check 'uninstall outside tmux says it could not restore' '1' \
        "$(cd -- "$tmp/code/one" && HOME=$tmp CLAUDE_CONFIG_DIR=$tucfg \
              env -u TMUX -u TMUX_PANE "$bin" uninstall --force 2>&1 \
           | grep -c 'not inside tmux')"

    # doctor, which is the one command whose job is to explain a tmux that is not
    # cooperating.
    tsession_start "$tmp/code/one"
    tdoc=$(cd -- "$tmp/code/one" && HOME=$tmp CLAUDE_CONFIG_DIR=$tucfg \
        TMUX="$tsock,1,0" TMUX_PANE=%0 "$bin" doctor 2>&1)
    check 'doctor reports the server it is inside' '1' \
        "$(printf '%s\n' "$tdoc" | grep -c "^tmux: *OK .*$tsock, pane %0")"
    check 'doctor reports the decay clock as OK' '1' \
        "$(printf '%s\n' "$tdoc" | grep -c 'decay: OK')"
    check 'doctor recognises its own set-titles-string' '1' \
        "$(printf '%s\n' "$tdoc" | grep -c 'title: OK')"
    check 'doctor reports the TTLs in force' '1' \
        "$(printf '%s\n' "$tdoc" | grep -c 'ttl: working 1200s, waiting 900s, gone 3600s')"
    # `0` is a deadline no age can reach, and doctor must say so in words: it used
    # to print the internal 2147483647 on a line whose own legend says 0 = never.
    check 'doctor says never rather than the sentinel' '1' \
        "$(cd -- "$tmp/code/one" && HOME=$tmp CLAUDE_CONFIG_DIR=$tucfg \
              TMUX="$tsock,1,0" TMUX_PANE=%0 CCTAB_TTL_WORKING=0 "$bin" doctor 2>&1 \
           | grep -c 'ttl: working never, waiting 900s')"
    tm set -g status off
    tm set -g status-interval 0
    check 'doctor warns loudly when the decay cannot tick' '1' \
        "$(cd -- "$tmp/code/one" && HOME=$tmp CLAUDE_CONFIG_DIR=$tucfg \
              TMUX="$tsock,1,0" TMUX_PANE=%0 "$bin" doctor 2>&1 \
           | grep -c 'decay: WARN status off, status-interval 0')"
    check 'doctor names the remedy' '1' \
        "$(cd -- "$tmp/code/one" && HOME=$tmp CLAUDE_CONFIG_DIR=$tucfg \
              TMUX="$tsock,1,0" TMUX_PANE=%0 "$bin" doctor 2>&1 \
           | grep -c 'set -g status on ; set -g status-interval 5')"
    tm set -g set-titles off
    check 'doctor catches a set-titles that is off' '1' \
        "$(cd -- "$tmp/code/one" && HOME=$tmp CLAUDE_CONFIG_DIR=$tucfg \
              TMUX="$tsock,1,0" TMUX_PANE=%0 "$bin" doctor 2>&1 \
           | grep -c 'title: FAIL set-titles is off')"
    # And it blames the right thing. With OUR save in place a session-local
    # override is the explanation and `set -u set-titles` is the remedy; on a server
    # no SessionStart has reached, the same branch used to prescribe that no-op.
    check 'doctor blames a session-local override when we did install' '1' \
        "$(cd -- "$tmp/code/one" && HOME=$tmp CLAUDE_CONFIG_DIR=$tucfg \
              TMUX="$tsock,1,0" TMUX_PANE=%0 "$bin" doctor 2>&1 \
           | grep -c 'a session-local `set-titles off` is beating that')"
    check 'and blames tmux own default when no SessionStart has run here' '1' \
        "$(tm set -su @cctab_saved
           cd -- "$tmp/code/one" && HOME=$tmp CLAUDE_CONFIG_DIR=$tucfg \
              TMUX="$tsock,1,0" TMUX_PANE=%0 "$bin" doctor 2>&1 \
           | grep -c 'No SessionStart has configured this server')"
    tm set -g set-titles on

    # doctor on the re-arm hook, in Konsole mode only - the one place a user can
    # see whether a reattach will be armed.
    tsession_start "$tmp/code/one" CCTAB_TERMINAL=konsole
    check 'doctor reports the re-arm hook as OK' '1' \
        "$(cd -- "$tmp/code/one" && HOME=$tmp CLAUDE_CONFIG_DIR=$tucfg \
              TMUX="$tsock,1,0" TMUX_PANE=%0 CCTAB_TERMINAL=konsole "$bin" doctor 2>&1 \
           | grep -c 'arm: OK')"
    check 'and warns, with the remedy, when the hook is gone' '1' \
        "$(tm set-hook -u -t t:w0.0 'client-attached[1971]'
           cd -- "$tmp/code/one" && HOME=$tmp CLAUDE_CONFIG_DIR=$tucfg \
              TMUX="$tsock,1,0" TMUX_PANE=%0 CCTAB_TERMINAL=konsole "$bin" doctor 2>&1 \
           | grep -c 'arm: WARN no client-attached hook')"
    check 'and says nothing about arming outside konsole mode' '0' \
        "$(cd -- "$tmp/code/one" && HOME=$tmp CLAUDE_CONFIG_DIR=$tucfg \
              TMUX="$tsock,1,0" TMUX_PANE=%0 "$bin" doctor 2>&1 | grep -c 'arm:')"
    # The LAYOUT is server-wide too, and `title: OK` can never catch a drift: the
    # SessionStart that changed the layout rewrote @cctab_string in the same batch,
    # so those two always agree. The server is on suffix here, from the konsole
    # SessionStart above, and this session would install prefix.
    check 'doctor catches a layout the server does not have' '1' \
        "$(cd -- "$tmp/code/one" && HOME=$tmp CLAUDE_CONFIG_DIR=$tucfg \
              TMUX="$tsock,1,0" TMUX_PANE=%0 "$bin" doctor 2>&1 \
           | grep -c 'layout: WARN the server has the strip last')"
    check 'and says nothing when the layouts agree' '0' \
        "$(cd -- "$tmp/code/one" && HOME=$tmp CLAUDE_CONFIG_DIR=$tucfg \
              TMUX="$tsock,1,0" TMUX_PANE=%0 CCTAB_TERMINAL=konsole "$bin" doctor 2>&1 \
           | grep -c 'layout:')"
    tsession_start "$tmp/code/one"
    # A missing tmux(1) and a socket that is gone took the same FAIL branch and the
    # same hedged sentence, although one is fixed by installing tmux and the other
    # by unsetting a stale exported $TMUX.
    check 'doctor names a missing tmux binary' '1' \
        "$(mkdir -p "$tmp/nobin"
           cd -- "$tmp/code/one" && HOME=$tmp CLAUDE_CONFIG_DIR=$tucfg PATH=$tmp/nobin \
              TMUX="$tsock,1,0" TMUX_PANE=%0 "$bin" doctor 2>&1 \
           | grep -c 'tmux(1) is not on PATH')"
    check 'and names a socket that is gone as the other thing' '1' \
        "$(cd -- "$tmp/code/one" && HOME=$tmp CLAUDE_CONFIG_DIR=$tucfg \
              TMUX="$tmp/no-such-socket,1,0" TMUX_PANE=%0 "$bin" doctor 2>&1 \
           | grep -c 'the socket it names is gone')"
    # And the three shapes that are NOT a tmux server, each of which must leave
    # the paint path exactly as it is outside tmux.
    for _t in nonsense /tmp/s,1 /tmp/s,1,0,2 relative,1,0 /tmp/s,x,0; do
        check "TMUX=$_t is not a tmux server" '🔵 ~/plain' \
            "$(cd -- "$tmp/plain" && HOME=$tmp TMUX=$_t CCTAB_DRY_RUN=1 \
                  "$bin" working </dev/null)"
        check "TMUX=$_t emits an untagged title" \
            '{"terminalSequence":"\u001b]0;🔵 ~/plain\u0007","suppressOutput":true}' \
            "$(cd -- "$tmp/plain" && HOME=$tmp TMUX=$_t "$bin" working </dev/null)"
    done
    check 'CCTAB_NO_TMUX backs the whole slice out' \
        '{"terminalSequence":"\u001b]0;🔵 ~/plain\u0007","suppressOutput":true}' \
        "$(cd -- "$tmp/plain" && HOME=$tmp TMUX="$tsock,1,0" CCTAB_NO_TMUX=1 \
              "$bin" working </dev/null)"
    check 'and then execs no tmux at all' 'off' \
        "$(tm set -g set-titles off
           cd -- "$tmp/plain" && HOME=$tmp TMUX="$tsock,1,0" TMUX_PANE=%0 \
              CCTAB_NO_TMUX=1 "$bin" session-start </dev/null >/dev/null 2>&1
           tm show -gv set-titles)"
    check 'screen gets nothing, deliberately' \
        '{"terminalSequence":"\u001b]0;🔵 ~/plain\u0007","suppressOutput":true}' \
        "$(cd -- "$tmp/plain" && HOME=$tmp STY=1234.pts-0.host "$bin" working </dev/null)"

    tmux_gone
    printf 'tmux section: private server %s, killed.\n' "$tsock"
fi

# --- summary --------------------------------------------------------------
printf '\n----------------------------------------\n'
printf '%d passed, %d failed\n' "$pass" "$fail"
if [ "$fail" -ne 0 ]; then
    printf 'RESULT: FAIL\n'
    exit 1
fi
printf 'RESULT: PASS\n'
exit 0
