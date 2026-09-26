#!/bin/sh
# Rebuild the golden-corpus fixture tree from scratch.
#
#   sh mkfixtures.sh [fixtures-dir]
#
# The default is OUTSIDE the repo - $TMPDIR/cctab-corpus/fixtures, overridable
# with $CCTAB_FIXTURES - and that is load-bearing, not tidiness. The corpus HOME
# is the fixture root, and tabstatus walks UP from the cwd looking for a .git: a
# fixture tree inside a checkout makes every location case answer `repo@branch`
# instead of `~/...`, which failed 86 of the 288 cases when the corpus first moved
# into tests/. Keep the tree out of any git repository.
#
# Idempotent: the tree is removed and rebuilt, so a half-built tree from an
# interrupted run cannot leak into the corpus.
#
# `git` is NEVER called. tabstatus reads .git / .git/HEAD and nothing else, so a
# directory plus a one-line file reproduce exactly what it sees - on any machine,
# with no git installed, and with the HEAD bytes under our control. That is the
# same choice tests/run.sh makes, for the same reason.
#
# Every fixture here is referenced by an id in cases.jsonl. Do not rename one
# without regenerating the corpus.
set -u

here=$(cd -- "$(dirname -- "$0")" && pwd) || exit 1
F=${1:-${CCTAB_FIXTURES:-${TMPDIR:-/tmp}/cctab-corpus/fixtures}}
case $F in
/*) ;;
*) F=$PWD/$F ;;
esac

# The corpus HOME is $F itself, so every fixture beneath it renders as ~/...
# Refuse to operate on anything that is not our own fixtures directory.
case ${F##*/} in
fixtures) ;;
*) printf 'refusing: %s is not named "fixtures"\n' "$F" >&2; exit 1 ;;
esac

# Two fixtures are mode 000 on purpose; make the tree removable again first.
mkdir -p -- "$(dirname -- "$F")" || exit 1
if [ -e "$F" ]; then
    chmod -R u+rwX "$F" 2>/dev/null
    rm -rf "$F" || exit 1
fi
mkdir -p "$F" || exit 1

# mkrepo <dir> <HEAD bytes, printf format-free>
mkrepo() {
    mkdir -p "$1/.git" || return 1
    printf '%s' "$2" >"$1/.git/HEAD"
}

# --------------------------------------------------------------------------
# non-repo paths
# --------------------------------------------------------------------------
mkdir -p "$F/plaindir"
mkdir -p "$F/code/bug_fedora"
mkdir -p "$F/homeprefix" "$F/homeprefixed"
mkdir -p "$F/one/two/three/four/five/six/seven/eight"
mkdir -p "$F/aaaaaaaaaabbbbbbbbbbccccccccccdddddddddd"
mkdir -p "$F/ééééééééééééééééééééééééééééééééééééé"
mkdir -p "$F/café-déjà"
mkdir -p "$F/pct-100%s%d-x"
# A very long path: 10 components of 30 characters.
_lp=$F/verylong
_i=0
while [ "$_i" -lt 10 ]; do
    _lp=$_lp/cccccccccccccccccccccccccccccc
    _i=$((_i + 1))
done
mkdir -p "$_lp"
# Several non-ASCII components: the 1d component-peeling loop runs BEFORE the
# ASCII guard, so how many components it drops depends on whether ${#var} counts
# bytes or characters - which depends on the shell AND the locale.
mkdir -p "$F/multiacc/ééééééééé/ééééééééé/ééééééééé"

# --------------------------------------------------------------------------
# hostile names
# --------------------------------------------------------------------------
mkdir -p "$F/we$(printf '\42')ird"
mkdir -p "$F/back$(printf '\134')slash"
mkdir -p "$F/both$(printf '\42')x$(printf '\134')y"
mkdir -p "$F/$(printf 'new\nline')"
mkdir -p "$F/$(printf 'ta\tb')"
mkdir -p "$F/$(printf '\42\134')"
mkdir -p "$F/$(printf 'gl*b?[x]')"
mkdir -p "$F/$(printf 'back\140tick')"
# A directory name that is not valid UTF-8. Legal on Linux, illegal inside a
# JSON string literal: this is documented limitation 2.
mkdir -p "$F/$(printf 'bad\377utf8')"
# The sanitizer's iteration bound: a quote plus ~380 characters of path.
_c200=aaaaaaaaaabbbbbbbbbbccccccccccddddddddddeeeeeeeeeeffffffffffgggggggggghhhhhhhhhhiiiiiiiiiijjjjjjjjjjkkkkkkkkkkllllllllllmmmmmmmmmmnnnnnnnnnnoooooooooopppppppppp
mkdir -p "$F/sanbound/q$(printf '\42')$_c200/$_c200"

# --------------------------------------------------------------------------
# repositories
# --------------------------------------------------------------------------
mkrepo "$F/repos/plain" 'ref: refs/heads/master
'
mkdir -p "$F/repos/plain/deep/er/still"
mkdir -p "$F/repos/plain/sub/deeper"
mkdir -p "$F/repos/plain/hasjunk/.git"
mkrepo "$F/repos/plain/inner" 'ref: refs/heads/inner-branch
'
mkrepo "$F/repos/slashy" 'ref: refs/heads/feature/tab-title
'
mkrepo "$F/repos/nonl" 'ref: refs/heads/no-newline'
mkrepo "$F/repos/crlf" "$(printf 'ref: refs/heads/crlf-branch\r')"
mkrepo "$F/repos/detached" '0123456789abcdef0123456789abcdef01234567
'
mkrepo "$F/repos/sha256" '0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef
'
mkrepo "$F/repos/upper" 'ABCDEF0123456789ABCDEF0123456789ABCDEF01
'
mkrepo "$F/repos/otherref" 'ref: refs/bisect/bad
'
mkrepo "$F/repos/remoteref" 'ref: refs/remotes/origin/main
'
mkrepo "$F/repos/tagref" 'ref: refs/tags/v1.0
'
mkrepo "$F/repos/trailws" "$(printf 'ref: refs/heads/master \t ')"
mkrepo "$F/repos/longbranch" 'ref: refs/heads/some-very-long-branch-name-indeed
'
mkrepo "$F/repos/accent" 'ref: refs/heads/branché
'
mkrepo "$F/repos/accentlong" 'ref: refs/heads/ééééééééééééééééééééééééééééééééééééé
'
mkrepo "$F/repos/hostile" 'ref: refs/heads/we"ird\branch
'
mkrepo "$F/repos/ctrlbranch" "$(printf 'ref: refs/heads/a\001b')"
mkrepo "$F/repos/pct" 'ref: refs/heads/100%s%d
'
# An unborn branch: HEAD names a ref that does not exist yet (git init, no
# commit). tabstatus does not resolve the ref, so the name still renders.
mkrepo "$F/repos/unborn" 'ref: refs/heads/main
'
mkdir -p "$F/repos/unborn/.git/refs/heads"
# A HEAD first line far too long to be a HEAD. Rejected before the short-sha
# cut, which is quadratic in its length.
mkrepo "$F/repos/hugehead" ''
_huge=aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa
_i=0
while [ "$_i" -lt 6 ]; do
    _huge=$_huge$_huge
    _i=$((_i + 1))
done
printf '%s\n' "$_huge" >"$F/repos/hugehead/.git/HEAD"

# --------------------------------------------------------------------------
# .git that is not a plain directory
# --------------------------------------------------------------------------
# linked worktree: an ABSOLUTE gitdir pointer
mkdir -p "$F/wtstore/worktrees/wt-linked"
printf 'ref: refs/heads/feature/tab-title\n' >"$F/wtstore/worktrees/wt-linked/HEAD"
mkdir -p "$F/wt-linked"
printf 'gitdir: %s\n' "$F/wtstore/worktrees/wt-linked" >"$F/wt-linked/.git"
mkdir -p "$F/wt-nonl"
printf 'gitdir: %s' "$F/wtstore/worktrees/wt-linked" >"$F/wt-nonl/.git"
mkdir -p "$F/wt-crlf"
printf 'gitdir: %s\r\n' "$F/wtstore/worktrees/wt-linked" >"$F/wt-crlf/.git"
# submodule: a RELATIVE gitdir pointer
mkdir -p "$F/super/.git/modules/mysub" "$F/super/mysub"
printf 'ref: refs/heads/master\n' >"$F/super/.git/HEAD"
printf 'ref: refs/heads/sub-branch\n' >"$F/super/.git/modules/mysub/HEAD"
printf 'gitdir: ../.git/modules/mysub\n' >"$F/super/mysub/.git"
# .git as a symlink to a real git directory
mkdir -p "$F/symhost/.git"
printf 'ref: refs/heads/sym-branch\n' >"$F/symhost/.git/HEAD"
mkdir -p "$F/symrepo"
ln -s "$F/symhost/.git" "$F/symrepo/.git"
# cwd reached through a symlink
ln -s "$F/repos/plain/sub" "$F/link-into-repo"
ln -s "$F/repos/plain" "$F/link-to-top"
ln -s "$F/plaindir" "$F/link-to-plain"

# --------------------------------------------------------------------------
# a .git that is not a repository - the walk must continue past each of these
# --------------------------------------------------------------------------
mkdir -p "$F/junk/emptygit/.git"
mkdir -p "$F/junk/garbagegit"
printf 'this is not a gitdir pointer\n' >"$F/junk/garbagegit/.git"
mkdir -p "$F/junk/stalegit"
printf 'gitdir: %s/junk/no-such-gitdir\n' "$F" >"$F/junk/stalegit/.git"
mkdir -p "$F/junk/emptyhead/.git"
: >"$F/junk/emptyhead/.git/HEAD"
mkdir -p "$F/junk/dirhead/.git/HEAD"
mkdir -p "$F/junk/shorthead/.git"
printf 'abc123\n' >"$F/junk/shorthead/.git/HEAD"
# unreadable: a mode-000 HEAD, and a mode-000 .git pointer file. Both are FILES,
# never directories, so that `rm -rf` can still remove this tree.
mkdir -p "$F/junk/noreadhead/.git"
printf 'ref: refs/heads/secret\n' >"$F/junk/noreadhead/.git/HEAD"
chmod 000 "$F/junk/noreadhead/.git/HEAD"
mkdir -p "$F/junk/noreadgitfile"
printf 'gitdir: %s/wtstore/worktrees/wt-linked\n' "$F" >"$F/junk/noreadgitfile/.git"
chmod 000 "$F/junk/noreadgitfile/.git"

# --------------------------------------------------------------------------
# the upward walk is bounded at 64 probes, the first being $PWD itself
# --------------------------------------------------------------------------
mkrepo "$F/bound" 'ref: refs/heads/at-the-top
'
_p=$F/bound
_i=0
while [ "$_i" -lt 63 ]; do
    _p=$_p/a
    _i=$((_i + 1))
done
mkdir -p "$_p/a"

# --------------------------------------------------------------------------
# GIT_DIR targets
# --------------------------------------------------------------------------
mkdir -p "$F/bare/proj.git"
printf 'ref: refs/heads/bare-branch\n' >"$F/bare/proj.git/HEAD"

# --------------------------------------------------------------------------
# a payload file too big for the JSONL corpus to carry inline
# --------------------------------------------------------------------------
mkdir -p "$F/payloads"
_pad=aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa
_i=0
while [ "$_i" -lt 13 ]; do
    _pad=$_pad$_pad
    _i=$((_i + 1))
done
printf '{"session_id":"s1","cwd":"%s","hook_event_name":"PostToolUse","tool_name":"Read","tool_use_id":"tu1","duration_ms":23,"tool_response":"%s{\\"notification_type\\":\\"idle_prompt\\"}"}\n' \
    "$F/repos/plain" "$_pad" >"$F/payloads/big-posttooluse.json"

printf 'fixtures built under %s\n' "$F"
