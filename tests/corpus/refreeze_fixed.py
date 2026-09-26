#!/usr/bin/env python3
"""Re-record the expectation of NAMED cases only, from a named implementation.

    python3 refreeze_fixed.py <implementation>

gencases.py re-freezes the WHOLE corpus from the shell oracle, which is the right
tool while the port is meant to be byte-identical to it. This one exists for the
step after that: a deliberate fix makes some cases differ on purpose, and the
corpus then has to record the new behaviour for exactly those cases and nothing
else. Every other line of cases.jsonl is copied through byte for byte.

FIXED lists each id with the fix that changes it, so `git diff`-style review of
cases.jsonl has the reason next to the change. A re-recorded case loses its
`diverge` flag - it is no longer a known deviation, it is the specification - and
gains `fixed`, naming the limitation that was closed.
"""
import os
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
import gencases  # noqa: E402  (enc/dumps live there)
import runner  # noqa: E402

# id -> why its expectation changes
FIXED = {
    # (c) invalid UTF-8 in a name no longer yields invalid JSON: the display
    # string is repaired to U+FFFD at the boundary, so a title paints.
    "hostile-invalid-utf8-path":
        "fix-c: invalid UTF-8 repaired to U+FFFD, so the line is valid JSON",
    "hostile-invalid-utf8-path-json":
        "fix-c: invalid UTF-8 repaired to U+FFFD, so the line is valid JSON",
    # (d) the cap counts characters in every locale, and (e) a location OUTSIDE
    # PRINTABLE ASCII is therefore cut like any other instead of being exempted.
    # "outside printable ASCII" and not "non-ASCII": the guard that went away was
    # a single `case $s in *[!\ -~]*)`, so it also exempted a location carrying an
    # ASCII control character or DEL, and those are now capped too. (Harmless
    # either way - sanitize() deletes the control character a step later - but the
    # narrower wording sent an auditor looking only at non-ASCII inputs.)
    "maxloc-nonascii-path-exempt":
        "fix-e: a path outside printable ASCII is cut, not exempted",
    "maxloc-nonascii-branch-exempt":
        "fix-e: a repo@branch outside printable ASCII is cut, not exempted",
    "maxhost-nonascii-host-exempt":
        "fix-e: an ssh host outside printable ASCII is cut, not exempted",
    "maxloc-nonascii-path-c-locale":
        "fix-d: characters, not bytes, in LC_ALL=C too",
    "maxloc-nonascii-components-c-locale":
        "fix-d: characters, not bytes, in LC_ALL=C too",
    # (b) a background subagent's PostToolUse no longer repaints working.
    "stdin-subagent-posttooluse":
        "fix-b: a payload carrying agent_id paints nothing on the working edge",
}

# Cases whose `diverge` flag became vestigial: the limitation they were named
# after is fixed, and their expectation is UNCHANGED because bash-as-sh in
# C.UTF-8 - what the corpus was frozen against - already counted characters.
# They only ever diverged under dash. The flag is dropped without re-recording.
UNFLAG = {
    "maxloc-nonascii-mixed-path":
        "fix-d closed the shell-dependence; the bash-as-sh answer was already this",
    "maxloc-nonascii-components-utf8-locale":
        "fix-d closed the shell-dependence; the bash-as-sh answer was already this",
}


def main(argv):
    if not argv:
        print("usage: refreeze_fixed.py <implementation>", file=sys.stderr)
        return 2
    target = os.path.abspath(argv[0])
    cmd = [target]
    with open(target, "rb") as f:
        if f.read(2) == b"#!":
            cmd = ["/bin/sh", target]
    cases = runner.load(os.path.join(HERE, "cases.jsonl"))
    known = {c["id"] for c in cases}
    for i in list(FIXED) + list(UNFLAG):
        if i not in known:
            print("no such case: " + i, file=sys.stderr)
            return 2
    host = runner.short_host()
    tmpdir = tempfile.mkdtemp(prefix="cctab-refreeze-")
    helpers = runner.Helpers(tmpdir)
    out = []
    changed = 0
    try:
        for c in cases:
            if c["id"] in UNFLAG:
                rec = dict(c)
                rec.pop("diverge", None)
                rec["fixed"] = UNFLAG[c["id"]]
                out.append(rec)
                changed += 1
                continue
            if c["id"] not in FIXED:
                out.append(c)
                continue
            res = runner.run(c, cmd, FIX, helpers)
            rec = dict(c)
            for key in ("stdout", "stderr", "pty_out"):
                if key not in res:
                    continue
                rec.pop(key, None)
                rec.pop(key + "_b64", None)
                v, b64 = gencases.enc(
                    runner.detokenize(res[key], FIX, host, c.get("host_token", False)))
                rec[key + "_b64" if b64 else key] = v
            rec["exit"] = res["exit"]
            rec.pop("diverge", None)
            rec["fixed"] = FIXED[c["id"]]
            out.append(rec)
            changed += 1
            print("re-recorded %-42s %s" % (c["id"], FIXED[c["id"]]))
    finally:
        helpers.close()
    with open(os.path.join(HERE, "cases.jsonl"), "w", encoding="utf-8") as f:
        for rec in out:
            f.write(gencases.dumps(rec) + "\n")
    print("%d cases written, %d touched" % (len(out), changed))
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
