#!/bin/sh
# Replay the golden corpus against an implementation of tabstatus.
#
#   sh replay.sh /home/alexandre/code/claude-tabstatus/scripts/tabstatus.sh
#   sh replay.sh target/release/tabstatus
#   sh replay.sh -v -k notify target/release/tabstatus
#
# The target is run under /bin/sh when it is a script (a `#!` magic or a .sh
# name) and executed directly when it is a binary. Nothing else differs between
# the two, which is the point: the corpus proves the port byte for byte.
#
#   -v            print a line per passing case too
#   -k SUBSTRING  replay only the cases whose id contains SUBSTRING
#
# Exit status is 0 only if every case passed. A case carrying a `diverge` field
# is one the Rust port is EXPECTED to change (a README limitation it fixes);
# those are counted and printed separately and never fail the run.
#
# Needs python3: each case is executed with execve and an environment built from
# scratch, stdin fed from the case, and - for the two direct-write edges - a
# freshly allocated pty whose bytes are captured. Doing that byte-exactly from
# POSIX sh would need a shell JSON parser and a pty allocator.
set -u
here=$(cd -- "$(dirname -- "$0")" && pwd) || exit 1
exec python3 "$here/replay.py" "$@"
