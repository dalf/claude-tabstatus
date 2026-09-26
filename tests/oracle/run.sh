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

# Section 2c picks which END of the title the glyph goes on, from the terminal.
# Left unpinned, this suite would render differently depending on where it is
# run - a Konsole tab elides from the left, so the glyph goes last there, and
# every assertion below would flip. Neutralise the detection here; the
# glyph-position section sets these explicitly, per case.
unset KONSOLE_VERSION KONSOLE_DBUS_SESSION TMUX STY CCTAB_GLYPH_POS

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
    (cd -- "$_dir" 2>/dev/null && HOME=$tmp CCTAB_DRY_RUN=1 "$sh_under_test" "$script" "$_edge" </dev/null)
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

printf 'interpreter under test: %s\n' "$sh_under_test"
printf 'script under test:      %s\n\n' "$script"

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
    "$(cd -- "$tmp/plaindir" && HOME=$tmp CCTAB_DRY_RUN=1 "$sh_under_test" "$script" </dev/null)"

# --- location: not a repo, so the path -------------------------------------
# The asymmetry with the repo form below is the design: no branch to show means
# the path has to carry the information, so it is the whole home-relative path
# and not just the basename.
mkdir -p "$tmp/code/bug_fedora"
check 'a non-repo renders the home-relative path' '⚪ ~/code/bug_fedora' \
    "$(dry idle "$tmp/code/bug_fedora")"
check '$PWD == $HOME renders as ~' '⚪ ~' "$(dry idle "$tmp")"
check 'a HOME with a trailing slash still renders as ~' '⚪ ~' \
    "$(cd -- "$tmp" && HOME=$tmp/ CCTAB_DRY_RUN=1 "$sh_under_test" "$script" idle </dev/null)"
check 'a path outside HOME stays absolute' "⚪ $tmp/plaindir" \
    "$(cd -- "$tmp/plaindir" && HOME=/nonexistent-home CCTAB_MAX_LOCATION=0 CCTAB_DRY_RUN=1 "$sh_under_test" "$script" idle </dev/null)"
check 'root renders as /' '⚪ /' \
    "$(cd -- / && HOME=/nonexistent-home CCTAB_DRY_RUN=1 "$sh_under_test" "$script" idle </dev/null)"
check 'an empty HOME does not produce a stray ~' "⚪ $tmp/plaindir" \
    "$(cd -- "$tmp/plaindir" && HOME= CCTAB_MAX_LOCATION=0 CCTAB_DRY_RUN=1 "$sh_under_test" "$script" idle </dev/null)"
# A HOME *prefix* is not a HOME component: /tmp/x/homeless is not under /tmp/x/home.
mkdir -p "$tmp/homeprefix" "$tmp/homeprefixed"
check 'a sibling whose name only starts with HOME is not abbreviated' \
    "⚪ $tmp/homeprefixed" \
    "$(cd -- "$tmp/homeprefixed" && HOME=$tmp/homeprefix CCTAB_MAX_LOCATION=0 CCTAB_DRY_RUN=1 "$sh_under_test" "$script" idle </dev/null)"

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
    "$(cd -- "$tmp/plaindir" && GIT_DIR=$tmp/repos/plain/.git HOME=$tmp CCTAB_DRY_RUN=1 "$sh_under_test" "$script" idle </dev/null)"
check 'GIT_DIR (relative) is resolved against $PWD' '⚪ plain@master' \
    "$(cd -- "$tmp/repos/plain" && GIT_DIR=.git HOME=$tmp CCTAB_DRY_RUN=1 "$sh_under_test" "$script" idle </dev/null)"
mkdir -p "$tmp/bare/proj.git"
printf 'ref: refs/heads/bare-branch\n' >"$tmp/bare/proj.git/HEAD"
check 'GIT_DIR on a bare repo drops the .git suffix' '⚪ proj@bare-branch' \
    "$(cd -- "$tmp/plaindir" && GIT_DIR=$tmp/bare/proj.git HOME=$tmp CCTAB_DRY_RUN=1 "$sh_under_test" "$script" idle </dev/null)"
check 'a GIT_DIR that is not a repo falls back to the path' '⚪ ~/plaindir' \
    "$(cd -- "$tmp/plaindir" && GIT_DIR=$tmp/junk/no-such-gitdir HOME=$tmp CCTAB_DRY_RUN=1 "$sh_under_test" "$script" idle </dev/null)"
check 'an empty GIT_DIR is ignored, not honoured' '⚪ plain@master' \
    "$(cd -- "$tmp/repos/plain" && GIT_DIR= HOME=$tmp CCTAB_DRY_RUN=1 "$sh_under_test" "$script" idle </dev/null)"
# A GIT_DIR carrying dot components would otherwise put a bare `.` or `..` in the
# tab, and `GIT_DIR=.` inside a bare repo is a real idiom. Name it after the
# working directory in that case, minus a `.git` suffix so that a bare repo reads
# the same as it does when GIT_DIR names it absolutely.
check 'GIT_DIR=. in a bare repo does not render as a dot' '⚪ proj@bare-branch' \
    "$(cd -- "$tmp/bare/proj.git" && GIT_DIR=. HOME=$tmp CCTAB_DRY_RUN=1 "$sh_under_test" "$script" idle </dev/null)"
check 'a GIT_DIR with a /./ component names the repo' '⚪ plain@master' \
    "$(cd -- "$tmp/repos/plain" && GIT_DIR=$tmp/repos/plain/./.git HOME=$tmp CCTAB_DRY_RUN=1 "$sh_under_test" "$script" idle </dev/null)"
check 'a GIT_DIR with a /../ component names the repo' '⚪ plain@master' \
    "$(cd -- "$tmp/repos/plain" && GIT_DIR=$tmp/repos/plain/sub/../.git HOME=$tmp CCTAB_DRY_RUN=1 "$sh_under_test" "$script" idle </dev/null)"
check 'a GIT_DIR of .git/. names the repo' '⚪ plain@master' \
    "$(cd -- "$tmp/repos/plain" && GIT_DIR=.git/. HOME=$tmp CCTAB_DRY_RUN=1 "$sh_under_test" "$script" idle </dev/null)"

# --- location: $PWD comes from the environment ----------------------------
# A hook subprocess inherits its environment, so $PWD can arrive stale, absent
# or nonsense. Every shell this runs under verifies $PWD against the real cwd
# at startup and replaces it when it does not match, which is what makes the
# whole block able to trust it - assert that rather than assume it.
check 'a stale PWD naming a real directory is not trusted' '⚪ plain@master' \
    "$(cd -- "$tmp/repos/plain" && PWD=/etc HOME=$tmp CCTAB_DRY_RUN=1 "$sh_under_test" "$script" idle </dev/null)"
check 'a relative PWD is not trusted' '⚪ plain@master' \
    "$(cd -- "$tmp/repos/plain" && PWD=relative HOME=$tmp CCTAB_DRY_RUN=1 "$sh_under_test" "$script" idle </dev/null)"
check 'a PWD naming a directory that does not exist is not trusted' '⚪ plain@master' \
    "$(cd -- "$tmp/repos/plain" && PWD=/no/such/dir HOME=$tmp CCTAB_DRY_RUN=1 "$sh_under_test" "$script" idle </dev/null)"
check 'an unset PWD still resolves' '⚪ plain@master' \
    "$(cd -- "$tmp/repos/plain" && unset PWD; HOME=$tmp CCTAB_DRY_RUN=1 "$sh_under_test" "$script" idle </dev/null)"

# --- location: the length cap ---------------------------------------------
# A path is cut at the FRONT on a component boundary, because the trailing
# components say where you are, and both Konsole (which elides from the left)
# and Windows Terminal (which truncates from the right) then show the same
# informative end.
mkdir -p "$tmp/one/two/three/four/five/six/seven/eight"
check 'a long path is elided from the left, on a boundary' '⚪ …/four/five/six/seven/eight' \
    "$(dry idle "$tmp/one/two/three/four/five/six/seven/eight")"
check 'CCTAB_MAX_LOCATION=0 turns the cap off' "⚪ ~/one/two/three/four/five/six/seven/eight" \
    "$(cd -- "$tmp/one/two/three/four/five/six/seven/eight" && CCTAB_MAX_LOCATION=0 HOME=$tmp CCTAB_DRY_RUN=1 "$sh_under_test" "$script" idle </dev/null)"
check 'CCTAB_MAX_LOCATION widens the cap' "⚪ …/two/three/four/five/six/seven/eight" \
    "$(cd -- "$tmp/one/two/three/four/five/six/seven/eight" && CCTAB_MAX_LOCATION=40 HOME=$tmp CCTAB_DRY_RUN=1 "$sh_under_test" "$script" idle </dev/null)"
check 'CCTAB_MAX_LOCATION narrows the cap' '⚪ …/seven/eight' \
    "$(cd -- "$tmp/one/two/three/four/five/six/seven/eight" && CCTAB_MAX_LOCATION=16 HOME=$tmp CCTAB_DRY_RUN=1 "$sh_under_test" "$script" idle </dev/null)"
check 'a garbage CCTAB_MAX_LOCATION falls back to the default' '⚪ …/four/five/six/seven/eight' \
    "$(cd -- "$tmp/one/two/three/four/five/six/seven/eight" && CCTAB_MAX_LOCATION=lots HOME=$tmp CCTAB_DRY_RUN=1 "$sh_under_test" "$script" idle </dev/null)"
# A leading zero is an illegal octal constant to the shell's arithmetic, which
# would abort the script and emit an empty title; it has to be rejected, not
# clamped. A three-digit 032 is legal arithmetic but still not what anyone
# meant, so it gets the default too.
check 'a leading-zero cap is rejected, not evaluated' '⚪ …/four/five/six/seven/eight' \
    "$(cd -- "$tmp/one/two/three/four/five/six/seven/eight" && CCTAB_MAX_LOCATION=08 HOME=$tmp CCTAB_DRY_RUN=1 "$sh_under_test" "$script" idle </dev/null)"
check 'a leading-zero cap writes nothing to stderr' '' \
    "$(cd -- "$tmp/one/two/three/four/five/six/seven/eight" && CCTAB_MAX_LOCATION=08 HOME=$tmp CCTAB_DRY_RUN=1 "$sh_under_test" "$script" idle </dev/null 2>&1 >/dev/null)"
(cd -- "$tmp/plaindir" && CCTAB_MAX_LOCATION=08 HOME=$tmp CCTAB_DRY_RUN=1 "$sh_under_test" "$script" idle </dev/null >/dev/null 2>&1)
check 'a leading-zero cap still exits 0' '0' "$?"
check 'a padded cap like 032 is rejected too' '⚪ …/four/five/six/seven/eight' \
    "$(cd -- "$tmp/one/two/three/four/five/six/seven/eight" && CCTAB_MAX_LOCATION=032 HOME=$tmp CCTAB_DRY_RUN=1 "$sh_under_test" "$script" idle </dev/null)"
check 'a cap below 8 is raised to 8' '⚪ …/eight' \
    "$(cd -- "$tmp/one/two/three/four/five/six/seven/eight" && CCTAB_MAX_LOCATION=2 HOME=$tmp CCTAB_DRY_RUN=1 "$sh_under_test" "$script" idle </dev/null)"
# One component that does not fit has no boundary left to cut on, so it is cut
# inside, and the marker loses its slash to say so.
mkdir -p "$tmp/aaaaaaaaaabbbbbbbbbbccccccccccdddddddddd"
check 'one over-long component is cut inside itself' '⚪ …abbbbbbbbbbccccccccccdddddddddd' \
    "$(dry idle "$tmp/aaaaaaaaaabbbbbbbbbbccccccccccdddddddddd")"
# A count-based cut must never land inside a UTF-8 sequence. The unit `?` and
# ${#var} count is a BYTE unless the shell has multibyte support and the locale is
# UTF-8: bash-as-sh and busybox ash count characters in a UTF-8 locale, dash
# counts bytes always, and every shell counts bytes in C. So cutting by count
# would put an invalid byte into the JSON string literal. A location carrying any
# non-ASCII byte therefore keeps its full length - deliberately over the cap - and
# these two assertions are identical in all three shells, which is the point: a
# mid-sequence cut would make them differ.
mkdir -p "$tmp/ééééééééééééééééééééééééééééééééééééé"
check 'a long non-ASCII component is not cut mid-character' '⚪ …/ééééééééééééééééééééééééééééééééééééé' \
    "$(dry idle "$tmp/ééééééééééééééééééééééééééééééééééééé")"
mkrepo "$tmp/repos/accentlong" 'ref: refs/heads/ééééééééééééééééééééééééééééééééééééé
'
check 'a long non-ASCII branch is not cut mid-character' '⚪ accentlong@ééééééééééééééééééééééééééééééééééééé' \
    "$(dry idle "$tmp/repos/accentlong")"

check 'CCTAB_ELLIPSIS overrides the marker' '⚪ .../four/five/six/seven/eight' \
    "$(cd -- "$tmp/one/two/three/four/five/six/seven/eight" && CCTAB_ELLIPSIS=... HOME=$tmp CCTAB_DRY_RUN=1 "$sh_under_test" "$script" idle </dev/null)"
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
    "$(cd -- "$tmp/repos/plain" && SSH_TTY=/dev/pts/9 CCTAB_HOST=srv HOME=$tmp CCTAB_DRY_RUN=1 "$sh_under_test" "$script" idle </dev/null)"
check 'SSH_CONNECTION alone also adds it' '⚪ srv:plain@master' \
    "$(cd -- "$tmp/repos/plain" && SSH_CONNECTION='10.0.0.1 22 10.0.0.2 22' CCTAB_HOST=srv HOME=$tmp CCTAB_DRY_RUN=1 "$sh_under_test" "$script" idle </dev/null)"
check 'an empty SSH_TTY is not an ssh session' '⚪ plain@master' \
    "$(cd -- "$tmp/repos/plain" && SSH_TTY= SSH_CONNECTION= CCTAB_HOST=srv HOME=$tmp CCTAB_DRY_RUN=1 "$sh_under_test" "$script" idle </dev/null)"
check 'the domain is stripped from the hostname' '⚪ srv:plain@master' \
    "$(cd -- "$tmp/repos/plain" && SSH_TTY=x CCTAB_HOST=srv.example.com HOME=$tmp CCTAB_DRY_RUN=1 "$sh_under_test" "$script" idle </dev/null)"
check 'the ssh prefix also applies to a path location' '⚪ srv:~/plaindir' \
    "$(cd -- "$tmp/plaindir" && SSH_TTY=x CCTAB_HOST=srv HOME=$tmp CCTAB_DRY_RUN=1 "$sh_under_test" "$script" idle </dev/null)"
# The prefix is added after the cap and deliberately not counted by it: the
# host must not be the thing that gets eaten.
check 'the host prefix is not eaten by the cap' '⚪ srv:…/four/five/six/seven/eight' \
    "$(cd -- "$tmp/one/two/three/four/five/six/seven/eight" && SSH_TTY=x CCTAB_HOST=srv HOME=$tmp CCTAB_DRY_RUN=1 "$sh_under_test" "$script" idle </dev/null)"
# With no CCTAB_HOST, Linux answers from /proc, with no fork. Compare against
# what this machine says rather than hard-coding a hostname.
if [ -r /proc/sys/kernel/hostname ]; then
    IFS= read -r _realhost 2>/dev/null </proc/sys/kernel/hostname
    check 'the hostname comes from /proc when CCTAB_HOST is unset' "⚪ ${_realhost%%.*}:plain@master" \
        "$(cd -- "$tmp/repos/plain" && SSH_TTY=x HOME=$tmp CCTAB_DRY_RUN=1 "$sh_under_test" "$script" idle </dev/null)"
    check 'reading the hostname needs no external command' "⚪ ${_realhost%%.*}:plain@master" \
        "$(cd -- "$tmp/repos/plain" && SSH_TTY=x HOME=$tmp PATH= CCTAB_DRY_RUN=1 "$sh_under_test" "$script" idle </dev/null)"
else
    printf 'SKIP  /proc hostname assertions (no /proc/sys/kernel/hostname)\n'
fi
# `%%.*` only removes a DOMAIN, and the hosts one ssh into on cloud and k8s
# machines are single labels up to 64 bytes long. Uncapped, the prefix eats the
# whole tab and Windows Terminal - which truncates from the right - would show the
# host and nothing else. So the host has a cap of its own.
_k8s=my-cluster-worker-pool-a-7f9d8c6b5-x2kqz
check 'a long single-label host is capped' '⚪ my-cluster-work…:plain@master' \
    "$(cd -- "$tmp/repos/plain" && SSH_TTY=x CCTAB_HOST=$_k8s HOME=$tmp CCTAB_DRY_RUN=1 "$sh_under_test" "$script" idle </dev/null)"
check 'CCTAB_MAX_HOST narrows the host cap' '⚪ my-clu…:plain@master' \
    "$(cd -- "$tmp/repos/plain" && SSH_TTY=x CCTAB_MAX_HOST=7 CCTAB_HOST=$_k8s HOME=$tmp CCTAB_DRY_RUN=1 "$sh_under_test" "$script" idle </dev/null)"
check 'CCTAB_MAX_HOST=0 turns the host cap off' "⚪ $_k8s:plain@master" \
    "$(cd -- "$tmp/repos/plain" && SSH_TTY=x CCTAB_MAX_HOST=0 CCTAB_HOST=$_k8s HOME=$tmp CCTAB_DRY_RUN=1 "$sh_under_test" "$script" idle </dev/null)"
check 'a garbage CCTAB_MAX_HOST falls back to the default' '⚪ my-cluster-work…:plain@master' \
    "$(cd -- "$tmp/repos/plain" && SSH_TTY=x CCTAB_MAX_HOST=08 CCTAB_HOST=$_k8s HOME=$tmp CCTAB_DRY_RUN=1 "$sh_under_test" "$script" idle </dev/null)"
check 'a host under the cap is untouched' '⚪ build-runner-eu:plain@master' \
    "$(cd -- "$tmp/repos/plain" && SSH_TTY=x CCTAB_HOST=build-runner-eu HOME=$tmp CCTAB_DRY_RUN=1 "$sh_under_test" "$script" idle </dev/null)"
# A dotted quad is not host.domain: chopping 192.168.1.5 to `192` names nothing.
check 'an all-digits-and-dots host keeps its dots' '⚪ 192.168.1.5:plain@master' \
    "$(cd -- "$tmp/repos/plain" && SSH_TTY=x CCTAB_MAX_HOST=0 CCTAB_HOST=192.168.1.5 HOME=$tmp CCTAB_DRY_RUN=1 "$sh_under_test" "$script" idle </dev/null)"
# An ssh session whose hostname resolves to nothing must not render byte for byte
# like a local one: that would invert the one signal the whole design rests on.
check 'an ssh session with no resolvable host says ssh' '⚪ ssh:plain@master' \
    "$(cd -- "$tmp/repos/plain" && SSH_TTY=x CCTAB_HOST=.example.com HOME=$tmp CCTAB_DRY_RUN=1 "$sh_under_test" "$script" idle </dev/null)"

# --- location: no forks on the hot path ----------------------------------
# Slice 3 runs this script on every PostToolUse, so the location must cost no
# process. An empty PATH is the cheap proof: `git`, `hostname` and `basename`
# would all be unreachable, and the answer must still be right.
check 'a repo location needs no external command' '⚪ plain@master' \
    "$(cd -- "$tmp/repos/plain" && PATH= HOME=$tmp CCTAB_DRY_RUN=1 "$sh_under_test" "$script" idle </dev/null)"
check 'a path location needs no external command' '⚪ ~/code/bug_fedora' \
    "$(cd -- "$tmp/code/bug_fedora" && PATH= HOME=$tmp CCTAB_DRY_RUN=1 "$sh_under_test" "$script" idle </dev/null)"
check 'an elided path needs no external command' '⚪ …/four/five/six/seven/eight' \
    "$(cd -- "$tmp/one/two/three/four/five/six/seven/eight" && PATH= HOME=$tmp CCTAB_DRY_RUN=1 "$sh_under_test" "$script" idle </dev/null)"

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
    "$(cd -- "$tmp/repos/plain" && SSH_TTY=x CCTAB_HOST='s"r\v' HOME=$tmp CCTAB_DRY_RUN=1 "$sh_under_test" "$script" idle </dev/null)"
# The sanitizer is a pure-shell loop, so an empty PATH must not degrade it.
check 'sanitizing needs no external command' '⚪ ~/weird' \
    "$(cd -- "$tmp/we\"ird" && PATH= HOME=$tmp CCTAB_DRY_RUN=1 "$sh_under_test" "$script" idle </dev/null)"
# That loop is QUADRATIC, and the 1d cap does not bound its input: a non-ASCII
# location is exempt from cutting by count, and CCTAB_MAX_LOCATION=0 turns the cap
# off outright. Unbounded, a few hundred hostile characters cost seconds and a few
# thousand never finish - which breaks both "always exit 0" and "never an empty
# title", on a script slice 3 runs per tool call. So the loop stops at 256.
_c200=aaaaaaaaaabbbbbbbbbbccccccccccddddddddddeeeeeeeeeeffffffffffgggggggggghhhhhhhhhhiiiiiiiiiijjjjjjjjjjkkkkkkkkkkllllllllllmmmmmmmmmmnnnnnnnnnnoooooooooopppppppppp
_deepq=$tmp/sanbound/q\"$_c200/$_c200
mkdir -p "$_deepq"
_out=$(cd -- "$_deepq" && CCTAB_MAX_LOCATION=0 HOME=$tmp CCTAB_DRY_RUN=1 "$sh_under_test" "$script" idle </dev/null)
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
    "$(cd -- "$tmp/repos/plain" && CCTAB_GLYPH_WORKING='>' CCTAB_DRY_RUN=1 "$sh_under_test" "$script" working </dev/null)"
check 'CCTAB_GLYPH_WAITING override' '? plain@master' \
    "$(cd -- "$tmp/repos/plain" && CCTAB_GLYPH_WAITING='?' CCTAB_DRY_RUN=1 "$sh_under_test" "$script" waiting </dev/null)"
check 'CCTAB_GLYPH_IDLE override' '. plain@master' \
    "$(cd -- "$tmp/repos/plain" && CCTAB_GLYPH_IDLE='.' CCTAB_DRY_RUN=1 "$sh_under_test" "$script" idle </dev/null)"
check 'an empty glyph override leaves no leading space' 'plain@master' \
    "$(cd -- "$tmp/repos/plain" && CCTAB_GLYPH_IDLE= CCTAB_DRY_RUN=1 "$sh_under_test" "$script" idle </dev/null)"

# --- glyph position ---------------------------------------------------------
# Konsole elides the tab label from the LEFT, so a leading glyph is the first
# thing cut. The glyph therefore goes last under Konsole and first everywhere
# else, with CCTAB_GLYPH_POS overriding both.
_gp() { # _gp <env assignments...> -- runs `idle` in the plain repo
    ( cd -- "$tmp/repos/plain" && env "$@" CCTAB_DRY_RUN=1 \
        "$sh_under_test" "$script" idle </dev/null )
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
    "$(cd -- "$tmp/repos/plain" && printf '%s\n' "$payload" | CCTAB_DRY_RUN=1 "$sh_under_test" "$script" idle)"
check 'a payload with no trailing newline is drained' '⚪ plain@master' \
    "$(cd -- "$tmp/repos/plain" && printf '%s' "$payload" | CCTAB_DRY_RUN=1 "$sh_under_test" "$script" idle)"
check 'a multi-line payload is drained' '⚪ plain@master' \
    "$(cd -- "$tmp/repos/plain" && printf '%s\n%s\n%s\n' "$payload" "$payload" "$payload" | CCTAB_DRY_RUN=1 "$sh_under_test" "$script" idle)"

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
    (cd -- "$_dir" 2>/dev/null && HOME=$tmp CCTAB_DRY_RUN=1 "$sh_under_test" "$script" "$_edge")
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
    "$(cd -- "$tmp/repos/plain" && printf '{"notification_type":"idle_prompt"}' | HOME=$tmp CCTAB_DRY_RUN=1 "$sh_under_test" "$script" notify)"
check 'notify: a multi-line payload is read and drained' '🟠 plain@master' \
    "$(cd -- "$tmp/repos/plain" && printf '{"notification_type":"permission_prompt"}\ntrailing\ntrailing\n' | HOME=$tmp CCTAB_DRY_RUN=1 "$sh_under_test" "$script" notify)"
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
    "$(cd -- "$tmp/repos/plain" && printf '{"cwd":"%s/compact","hook_event_name":"SessionStart","source":"startup"}\n' "$tmp" | HOME=$tmp CCTAB_DRY_RUN=1 "$sh_under_test" "$script" session-start)"
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
    "$(cd -- "$tmp/repos/plain" && printf '{"hook_event_name":"SessionStart","source": "compact"}\n' | HOME=$tmp CCTAB_DRY_RUN=1 "$sh_under_test" "$script" session-start | wc -c | tr -d ' ')"
check 'session-start: a pretty-printed compact payload escapes the belt' '⚪ plain@master' \
    "$(cd -- "$tmp/repos/plain" && printf '{\n  "hook_event_name": "SessionStart",\n  "source": "compact"\n}\n' | HOME=$tmp CCTAB_DRY_RUN=1 "$sh_under_test" "$script" session-start)"
# Same whitespace variant on the notify side falls through to silence, which is
# the safe direction: an unpainted tab keeps the state it already showed.
check 'notify: a spaced-out kind is silent, not misread' '0' \
    "$(cd -- "$tmp/repos/plain" && printf '{"hook_event_name":"Notification","notification_type": "permission_prompt"}\n' | HOME=$tmp CCTAB_DRY_RUN=1 "$sh_under_test" "$script" notify | wc -c | tr -d ' ')"

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
    "$(cd -- "$tmp/repos/plain" && HOME=$tmp CCTAB_DRY_RUN=1 "$sh_under_test" "$script" working <"$_bigpl")"
(cd -- "$tmp/repos/plain" && HOME=$tmp CCTAB_DRY_RUN=1 "$sh_under_test" "$script" working <"$_bigpl" >/dev/null 2>&1)
check 'working: a 256KB payload still exits 0' '0' "$?"
check 'working: a tool_response mentioning idle_prompt is still working' '🔵 plain@master' \
    "$(cd -- "$tmp/repos/plain" && printf '{"hook_event_name":"PostToolUse","tool_response":"{\\"notification_type\\":\\"idle_prompt\\"}"}\n' | HOME=$tmp CCTAB_DRY_RUN=1 "$sh_under_test" "$script" working)"
# A subagent's tool call paints working too, and that is correct: the main
# session really is working while a subagent runs.
check 'working: a subagent PostToolUse still paints working' '🔵 plain@master' \
    "$(cd -- "$tmp/repos/plain" && printf '{"agent_id":"aec99e1f4bda1972b","agent_type":"general-purpose","hook_event_name":"PostToolUse","tool_name":"Read"}\n' | HOME=$tmp CCTAB_DRY_RUN=1 "$sh_under_test" "$script" working)"
# The deliberate reversal of the intended table: a PermissionRequest carrying
# agent_id was to be a NO-OP. Measured, that paints the wrong state - an async
# subagent's dialog arrives AFTER the main session's Stop, so the tab would read
# idle while a prompt only the human can answer is on screen, which is the exact
# failure this whole state is for. The edge paints waiting either way and the
# script never looks at agent_id.
check 'waiting: a subagent PermissionRequest still paints waiting' '🟠 plain@master' \
    "$(cd -- "$tmp/repos/plain" && printf '{"agent_id":"aec99e1f4bda1972b","agent_type":"general-purpose","hook_event_name":"PermissionRequest","tool_name":"Write"}\n' | HOME=$tmp CCTAB_DRY_RUN=1 "$sh_under_test" "$script" waiting)"
check 'waiting: a main-thread PermissionRequest paints waiting' '🟠 plain@master' \
    "$(cd -- "$tmp/repos/plain" && printf '{"hook_event_name":"PermissionRequest","tool_name":"Write","permission_suggestions":[]}\n' | HOME=$tmp CCTAB_DRY_RUN=1 "$sh_under_test" "$script" waiting)"
# The invariant behind section 0's split, asserted from the outside: ONLY notify
# and session-start read the payload, so nothing a payload says can talk another
# edge out of its state. One line carrying BOTH discriminators, in the compact
# spelling, at the front of the payload where a glob would certainly see it.
_both='{"hook_event_name":"PostToolUse","notification_type":"idle_prompt","source":"compact","tool_response":"x"}'
for _c in 'working:🔵' 'waiting:🟠' 'idle:⚪'; do
    _e=${_c%%:*}
    _g=${_c#*:}
    check "the $_e edge cannot be redirected by a payload" "$_g plain@master" \
        "$(cd -- "$tmp/repos/plain" && printf '%s\n' "$_both" | HOME=$tmp CCTAB_DRY_RUN=1 "$sh_under_test" "$script" "$_e")"
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
_known=' session-start working waiting idle notify session-end '
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
    *'tabstatus.sh\" '*)
        _edge=${_l#*'tabstatus.sh\" '}
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
check 'hooks.json registers exactly ten hooks' '10' \
    "$(printf '%s' "$_rows" | wc -l | tr -d ' ')"
# One command per group, so that a second hook smuggled into an existing group -
# which the row above would not show - fails here instead.
check 'every hooks.json group holds exactly one command' '' "$_badgroup"
# SubagentStop is DELIBERATELY absent: a subagent finishing must not read as the
# session going idle, and it also fires for the internal compaction summarizer
# (with an empty agent_type, which no matcher could filter) right before a
# SessionStart/compact. Pinned by name too, so that adding it has to be a
# decision and not a diff nobody reads.
_conf=$(cat "$_hooks")
check 'hooks.json does not register SubagentStop' 'absent' \
    "$(case $_conf in *SubagentStop*) printf present ;; *) printf absent ;; esac)"

# --- exit status ----------------------------------------------------------
for e in session-start working waiting idle notify session-end no-such-edge; do
    (cd -- "$tmp/repos/plain" && CCTAB_DRY_RUN=1 "$sh_under_test" "$script" "$e" </dev/null >/dev/null 2>&1)
    check "exit 0 on dry-run edge $e" '0' "$?"
done

# --- real emission paths (CLAUDE_PID unset: no pty is ever touched) -------
# The terminalSequence line is asserted byte for byte: the \u001b / \u0007
# escapes are what keep it valid JSON, and a raw control byte here would be the
# subtle bug this test exists to catch.
expect_json='{"terminalSequence":"\u001b]0;🔵 plain@master\u0007","suppressOutput":true}'
check 'working emits the terminalSequence JSON line' "$expect_json" \
    "$(cd -- "$tmp/repos/plain" && unset CLAUDE_PID; "$sh_under_test" "$script" working </dev/null)"
check 'idle emits the terminalSequence JSON line' \
    '{"terminalSequence":"\u001b]0;⚪ plain@master\u0007","suppressOutput":true}' \
    "$(cd -- "$tmp/repos/plain" && unset CLAUDE_PID; "$sh_under_test" "$script" idle </dev/null)"
check 'the emitted JSON line has no raw ESC byte' '0' \
    "$(cd -- "$tmp/repos/plain" && unset CLAUDE_PID; "$sh_under_test" "$script" working </dev/null | tr -dc '\033' | wc -c | tr -d ' ')"
check 'the emitted JSON is exactly one line' '1' \
    "$(cd -- "$tmp/repos/plain" && unset CLAUDE_PID; "$sh_under_test" "$script" working </dev/null | wc -l | tr -d ' ')"
# The notify edge on the real path, where a silently-painting no-op would
# actually reach a tab: an ignored kind must put NOTHING on stdout, not a
# terminalSequence carrying an empty title.
check 'notify idle_prompt emits the idle terminalSequence' \
    '{"terminalSequence":"\u001b]0;⚪ plain@master\u0007","suppressOutput":true}' \
    "$(cd -- "$tmp/repos/plain" && unset CLAUDE_PID; notif idle_prompt | "$sh_under_test" "$script" notify)"
check 'notify permission_prompt emits the waiting terminalSequence' \
    '{"terminalSequence":"\u001b]0;🟠 plain@master\u0007","suppressOutput":true}' \
    "$(cd -- "$tmp/repos/plain" && unset CLAUDE_PID; notif permission_prompt | "$sh_under_test" "$script" notify)"
check 'notify: an ignored kind emits no JSON line at all' '0' \
    "$(cd -- "$tmp/repos/plain" && unset CLAUDE_PID; notif agent_completed | "$sh_under_test" "$script" notify | wc -c | tr -d ' ')"
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
    "$(cd -- "$tmp/repos/plain" && unset CLAUDE_PID; "$sh_under_test" "$script" session-start </dev/null 2>&1)"
check 'session-end with no CLAUDE_PID emits nothing' '' \
    "$(cd -- "$tmp/repos/plain" && unset CLAUDE_PID; "$sh_under_test" "$script" session-end </dev/null 2>&1)"
(cd -- "$tmp/repos/plain" && unset CLAUDE_PID; "$sh_under_test" "$script" session-start </dev/null >/dev/null 2>&1)
check 'session-start with no CLAUDE_PID still exits 0' '0' "$?"

# A CLAUDE_PID that resolves to something that is not a tty must also be inert.
check 'a CLAUDE_PID whose fd 1 is not a tty emits nothing' '' \
    "$(cd -- "$tmp/repos/plain" && CLAUDE_PID=1 "$sh_under_test" "$script" session-start </dev/null 2>&1)"
check 'a nonsense CLAUDE_PID emits nothing' '' \
    "$(cd -- "$tmp/repos/plain" && CLAUDE_PID=not-a-pid "$sh_under_test" "$script" session-start </dev/null 2>&1)"

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

# --- summary --------------------------------------------------------------
printf '\n----------------------------------------\n'
printf '%d passed, %d failed\n' "$pass" "$fail"
if [ "$fail" -ne 0 ]; then
    printf 'RESULT: FAIL\n'
    exit 1
fi
printf 'RESULT: PASS\n'
exit 0
