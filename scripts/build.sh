#!/bin/sh
# Build the binaries that ship in bin/, and record what they were built from.
#
#   sh scripts/build.sh                 the host target
#   sh scripts/build.sh --all           every target listed in TARGETS
#
# bin/ holds a binary per platform, plus bin/tabstatus, a relative
# symlink to the one for this machine - that is the binary you RUN to install,
# so a user needs no toolchain. It is not the path hooks.json invokes: install
# copies it into the generated plugin tree, and hooks.json invokes the copy
# there, so a git checkout cannot change what a running session executes.
#
# bin/ is gitignored build output; binaries ship as GitHub release assets.
# bin/sources.sha256 and bin/sources.cksum are the staleness guard. Git does not
# preserve mtimes, so "is the binary older than the newest source file" cannot be
# answered after a clone; a digest of the sources it was built from can. The test
# suite recomputes them and fails when they do not match, which is what stops a
# stale binary from shipping silently. sha256sum is preferred and cksum is the
# POSIX fallback, so the check works wherever either exists.
#
# ONE MANIFEST PER TRIPLE, plus an unsuffixed pair for the host. Writing a single
# unsuffixed manifest for ALL sources after building only the host left the other
# triple's binary silently behind while `sha256sum -c bin/sources.sha256` read
# fully green - measured: a gnu binary built 37 minutes before the musl one, with
# a manifest that vouched for both. A manifest now names the binary it vouches
# for, and a triple this run did not build keeps whatever manifest it had, so a
# verify of it fails instead of inheriting somebody else's freshness. The
# unsuffixed pair is the HOST's, because bin/tabstatus is the host binary and that
# is what the test suite and the installed plugin run.
#
# cargo comes from mise here. A non-interactive shell does not have it on PATH,
# hence the full path.
set -eu

here=$(cd -- "$(dirname -- "$0")/.." && pwd) || exit 1
cd -- "$here"

CARGO=${CARGO:-}
if [ -z "$CARGO" ]; then
    if command -v cargo >/dev/null 2>&1; then
        CARGO="cargo"
    elif [ -x "$HOME/.local/bin/mise" ]; then
        CARGO="$HOME/.local/bin/mise exec rust@latest -- cargo"
    else
        printf 'error: no cargo on PATH and no mise at ~/.local/bin/mise\n' >&2
        exit 1
    fi
fi

# musl, not gnu, and deliberately so. Measured on this machine, best-of-N with a
# 319us exec floor: musl static-pie 211us, glibc dynamic 528us - the static build
# beats even /bin/true, because it never enters ld.so. It also drops a hard
# GLIBC_2.34 requirement, which matters because the binary's main home is a remote
# Linux box reached over ssh whose glibc we do not control: a gnu build simply
# refuses to start on an older distro.
host=x86_64-unknown-linux-musl

# Only targets that actually COMPILE. x86_64-pc-windows-gnu is deliberately not
# here: the source is Unix-only by construction (std::os::unix, /proc, symlinks,
# character devices) and does not compile for it at all - measured, 53 errors
# across six source files. Listing it made `sh scripts/build.sh --all`, one of the
# two commands the README documents, exit 1 on every run even when the host build
# had succeeded, so the documented release step was permanently red and useless as
# a success signal. Windows is a port, not a cross-compile; README's Build table
# says so.
TARGETS="x86_64-unknown-linux-musl x86_64-unknown-linux-gnu"

build_one() {
    _t=$1
    printf '\n=== %s ===\n' "$_t"
    # Always --target, even for the host: musl is a cross-target on a glibc box,
    # and routing every build the same way keeps the output path predictable.
    ${CARGO} build --release --target "$_t" || return 1
    _out=target/$_t/release/tabstatus
    [ -f "$_out" ] || _out=target/$_t/release/tabstatus.exe
    case $_t in
    *windows*) _name=tabstatus-$_t.exe ;;
    *) _name=tabstatus-$_t ;;
    esac
    mkdir -p bin
    cp -f "$_out" "bin/$_name"
    chmod 755 "bin/$_name"
    printf 'bin/%s  %s bytes\n' "$_name" "$(wc -c <"bin/$_name")"
}

case ${1-} in
--all) list=$TARGETS ;;
*) list=$host ;;
esac

failed=
for t in $list; do
    build_one "$t" || failed="$failed $t"
done

# bin/tabstatus -> the host binary, as a RELATIVE link so moving the checkout
# does not break it.
ln -sfn "tabstatus-$host" bin/tabstatus

# The staleness manifests. Sorted by path so the file is stable, and listing
# exactly the inputs a rebuild depends on - which now includes the two manifests,
# because src/embedded.rs pulls both in with include_str!. rustc already tracks
# them (they appear in target/<triple>/release/tabstatus.d, so editing only the
# JSON re-triggers a compile), and this is the layer that covers what rustc cannot:
# a PREBUILT binary, already copied into bin/ or uploaded as a release asset, going
# stale against an edited hooks.json without anyone rebuilding. tests/run.sh
# recomputes this exact list, so both spellings must stay in the same order.
sources="Cargo.toml .claude-plugin/plugin.json hooks/hooks.json $(ls src/*.rs | sort)"
have_sha256=
command -v sha256sum >/dev/null 2>&1 && have_sha256=yes
for t in $list; do
    case " $failed " in *" $t "*) continue ;; esac
    # shellcheck disable=SC2086
    [ -z "$have_sha256" ] || sha256sum $sources >"bin/sources.$t.sha256"
    # shellcheck disable=SC2086
    cksum $sources >"bin/sources.$t.cksum"
    # The host's manifest is also the unsuffixed one, because bin/tabstatus is the
    # host binary.
    if [ "$t" = "$host" ]; then
        [ -z "$have_sha256" ] || cp -f "bin/sources.$t.sha256" bin/sources.sha256
        cp -f "bin/sources.$t.cksum" bin/sources.cksum
    fi
done

printf '\nbin/:\n'
ls -l bin/
[ -z "$failed" ] || {
    printf '\nFAILED to build:%s\n' "$failed" >&2
    exit 1
}
