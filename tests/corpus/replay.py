#!/usr/bin/env python3
"""Replay cases.jsonl against an implementation. Invoked through replay.sh."""
import base64
import os
import subprocess
import sys
import tempfile

HERE = os.path.dirname(os.path.abspath(__file__))
# The fixture tree lives OUTSIDE the repo by default: the corpus HOME is the
# fixture root, and a tree inside a git checkout makes tabstatus's upward .git
# walk answer `repo@branch` for every location case. $CCTAB_FIXTURES overrides.
def _fixtures():
    env = os.environ.get("CCTAB_FIXTURES")
    if env:
        return env
    return os.path.join(tempfile.gettempdir(), "cctab-corpus", "fixtures")


FIX = _fixtures()
sys.path.insert(0, HERE)
import runner  # noqa: E402


def hexdump(b, limit=160):
    out = []
    for i in range(0, min(len(b), limit), 16):
        chunk = b[i:i + 16]
        hexs = " ".join("%02x" % c for c in chunk)
        txt = "".join(chr(c) if 32 <= c < 127 else "." for c in chunk)
        out.append("      %04x  %-47s  %s" % (i, hexs, txt))
    if len(b) > limit:
        out.append("      ... %d more bytes" % (len(b) - limit))
    return "\n".join(out) or "      <empty>"


def first_diff(a, b):
    n = min(len(a), len(b))
    for i in range(n):
        if a[i] != b[i]:
            return i
    return n if len(a) != len(b) else -1


def show(label, want, got):
    print("      --- %s: expected %d bytes, got %d bytes, first differ at %s"
          % (label, len(want), len(got), first_diff(want, got)))
    print("      expected:")
    print(hexdump(want))
    print("      actual:")
    print(hexdump(got))


def main(argv):
    verbose = False
    only = None
    args = []
    i = 0
    while i < len(argv):
        a = argv[i]
        if a in ("-v", "--verbose"):
            verbose = True
        elif a in ("-k", "--filter"):
            i += 1
            only = argv[i]
        else:
            args.append(a)
        i += 1
    if not args:
        print("usage: replay.sh [-v] [-k SUBSTRING] <executable> [prefix args...]",
              file=sys.stderr)
        return 2

    target = os.path.abspath(args[0])
    cmd = [target] + args[1:]
    if not os.path.exists(target):
        print("no such executable: " + target, file=sys.stderr)
        return 2
    # A shell script is handed to /bin/sh, exactly as hooks.json does; a binary
    # is executed directly. That is the ONLY difference between replaying the
    # reference implementation and replaying the Rust port.
    with open(target, "rb") as f:
        magic = f.read(2)
    if magic == b"#!" or target.endswith(".sh"):
        # CCTAB_REPLAY_SH replays a SCRIPT under another interpreter. The corpus
        # was frozen against /bin/sh; pointing this at dash or busybox ash shows
        # exactly which cases are interpreter-dependent, and it is meaningless
        # for a binary target.
        cmd = [os.environ.get("CCTAB_REPLAY_SH", "/bin/sh")] + cmd
    elif not os.access(target, os.X_OK):
        print("not executable and not a script: " + target, file=sys.stderr)
        return 2

    if not os.path.isdir(FIX):
        print("fixtures missing, building them")
        subprocess.check_call(["/bin/sh", os.path.join(HERE, "mkfixtures.sh")])

    cases = runner.load(os.path.join(HERE, "cases.jsonl"))
    if only:
        cases = [c for c in cases if only in c["id"]]
    host = runner.short_host()
    tmpdir = tempfile.mkdtemp(prefix="cctab-replay-")
    helpers = runner.Helpers(tmpdir)
    npass = nfail = ndiv = 0
    failed = []
    print("replaying %d cases against: %s" % (len(cases), " ".join(cmd)))
    try:
        for c in cases:
            res = runner.run(c, cmd, FIX, helpers)
            pids = {}
            if c.get("mode") == "pty":
                pids[b"@@PID_PTY@@"] = helpers.pty_pid()
            elif c.get("mode") == "pidfile":
                pids[b"@@PID_FILE@@"] = helpers.file_pid()
            problems = []
            for key in ("stdout", "stderr", "pty_out"):
                want = runner.expected(c, key, FIX, host, pids)
                if want is None:
                    continue
                got = res.get(key, b"")
                if want != got:
                    problems.append((key, want, got))
            if res["exit"] != c["exit"]:
                problems.append(("exit", str(c["exit"]).encode(),
                                 str(res["exit"]).encode()))
            if not problems:
                npass += 1
                if verbose:
                    print("PASS      %s" % c["id"])
            elif c.get("diverge"):
                ndiv += 1
                print("DIVERGE   %s" % c["id"])
                print("      expected divergence: %s" % c["diverge"])
                for key, want, got in problems:
                    show(key, want, got)
            else:
                nfail += 1
                failed.append(c["id"])
                print("FAIL      %s" % c["id"])
                if c.get("note"):
                    print("      note: %s" % c["note"])
                # ascii(): a case cwd can carry a surrogate-escaped invalid
                # byte, and printing that raw raises UnicodeEncodeError - which
                # used to abort the whole run on the first such FAILURE, exactly
                # when the report was needed.
                print("      argv=%s cwd=%s mode=%s"
                      % (ascii(c["argv"]), ascii(c["cwd"]), c.get("mode", "pipe")))
                for key, want, got in problems:
                    show(key, want, got)
    finally:
        helpers.close()
    print("\n----------------------------------------")
    print("%d passed, %d failed, %d diverged as expected" % (npass, nfail, ndiv))
    if failed:
        print("failed: %s" % " ".join(failed))
        print("RESULT: FAIL")
        return 1
    print("RESULT: PASS")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
