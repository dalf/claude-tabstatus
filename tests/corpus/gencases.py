#!/usr/bin/env python3
"""Generate cases.jsonl by running the REFERENCE implementation once per case.

    python3 gencases.py [--ref 'sh /path/to/tabstatus.sh']

Every case in cases.jsonl is a fully reproducible invocation:

  id        stable name, also the label replay.sh prints
  argv      argv[1:] handed to the implementation under test
  env       the COMPLETE environment (the runner uses execve with exactly this,
            never the ambient one), so nothing here depends on the caller's shell
  cwd       relative to the fixtures directory, or absolute if it starts with /
  stdin     the hook payload, "" for none; stdin_file names a fixture instead
  stdout    expected bytes, or stdout_b64 when they are not valid UTF-8
  stderr    expected bytes, or stderr_b64
  exit      expected exit status (always 0 - the hook contract)
  mode      "pipe" (default), "pty", or "pidfile"; see replay.py
  diverge   set when the Rust port is EXPECTED to change this case's output.
            Named after the README limitation it fixes.

Tokens expanded by the runner, in env values, cwd, stdin and in the expected
output: @@FIX@@ the fixtures directory, @@SHORTHOST@@ this machine's hostname up
to the first dot, @@PID_PTY@@ / @@PID_FILE@@ the pid of a helper the runner
spawns (see replay.py). Everything else is literal bytes.
"""
import base64
import json
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
TOK = "@@FIX@@"

REF = ["/bin/sh", os.path.join(HERE, os.pardir, "oracle", "tabstatus.sh")]
# The shell implementation was DELETED from the repo when the binary took over
# hooks.json. tests/oracle/tabstatus.sh is the same bytes - sha256
# 9d08ff74c23f31bab07ad4fc7b9e357d9aa082dc0bf8e6dcc71043a1d2cf67e5, which is what
# `git show <the deleting commit>^:scripts/tabstatus.sh` hashes to - kept in the
# repo so that a re-freeze of the cases that predate the deliberate fixes is
# still possible by anyone, not only in the session that did the port.

cases = []
seen = set()


def E(**kw):
    """Base environment. None removes a key; everything else is a string."""
    env = {
        "PATH": "/usr/bin:/bin",
        "HOME": TOK,
        "LC_ALL": "C.UTF-8",
        "CCTAB_DRY_RUN": "1",
    }
    for k, v in kw.items():
        if v is None:
            env.pop(k, None)
        else:
            env[k] = v
    return env


def C(cid, argv, env=None, cwd="repos/plain", stdin="", mode="pipe",
      diverge=None, note=None, stdin_file=None, pwd=True, host_token=False):
    """Declare one case. PWD is injected from cwd unless env already sets it."""
    assert cid not in seen, "duplicate case id " + cid
    seen.add(cid)
    env = E() if env is None else env
    if pwd and "PWD" not in env:
        env["PWD"] = cwd if cwd.startswith("/") else (TOK + "/" + cwd if cwd != "." else TOK)
    c = {"id": cid, "argv": argv, "env": env, "cwd": cwd}
    if stdin_file is not None:
        c["stdin_file"] = stdin_file
    else:
        c["stdin"] = stdin
    if mode != "pipe":
        c["mode"] = mode
    if host_token:
        c["host_token"] = True
    if diverge:
        c["diverge"] = diverge
    if note:
        c["note"] = note
    cases.append(c)
    return c


# ===========================================================================
# 1. every edge, in dry run and on the real emission path
# ===========================================================================
for e in ("working", "waiting", "idle", "session-start", "session-end",
          "no-such-edge"):
    C("edge-dry-" + e, [e], cwd="plaindir")
C("edge-dry-noargv", [], cwd="plaindir",
  note="no argument at all: falls through to the idle glyph")
# The real path: CLAUDE_PID unset, so the direct-write edges are inert and the
# others print the terminalSequence line.
for e in ("working", "waiting", "idle", "notify", "session-start",
          "session-end", "no-such-edge"):
    C("edge-json-" + e, [e], env=E(CCTAB_DRY_RUN=None), cwd="plaindir")
C("edge-json-noargv", [], env=E(CCTAB_DRY_RUN=None), cwd="plaindir")
C("edge-dryrun-0-is-not-dry", ["working"], env=E(CCTAB_DRY_RUN="0"),
  cwd="plaindir", note="the flag is compared to 1, not to truthiness")
C("edge-dryrun-yes-is-not-dry", ["working"], env=E(CCTAB_DRY_RUN="yes"),
  cwd="plaindir")
C("edge-dryrun-empty-is-not-dry", ["working"], env=E(CCTAB_DRY_RUN=""),
  cwd="plaindir")

# ===========================================================================
# 2. glyph knobs
# ===========================================================================
C("glyph-working-override", ["working"], env=E(CCTAB_GLYPH_WORKING=">"))
C("glyph-waiting-override", ["waiting"], env=E(CCTAB_GLYPH_WAITING="?"))
C("glyph-idle-override", ["idle"], env=E(CCTAB_GLYPH_IDLE="."))
C("glyph-working-empty", ["working"], env=E(CCTAB_GLYPH_WORKING=""))
C("glyph-waiting-empty", ["waiting"], env=E(CCTAB_GLYPH_WAITING=""))
C("glyph-idle-empty", ["idle"], env=E(CCTAB_GLYPH_IDLE=""))
C("glyph-multibyte-override", ["idle"], env=E(CCTAB_GLYPH_IDLE="★"))
C("glyph-multichar-override", ["idle"], env=E(CCTAB_GLYPH_IDLE="[..]"))
C("glyph-override-not-applied-to-other-edge", ["idle"],
  env=E(CCTAB_GLYPH_WORKING=">"))
C("glyph-session-end-ignores-override", ["session-end"],
  env=E(CCTAB_GLYPH_IDLE="."), note="session-end has no glyph and no title")

for pos in ("prefix", "suffix", "both", "sideways", ""):
    C("glyphpos-" + (pos or "empty"), ["idle"], env=E(CCTAB_GLYPH_POS=pos))
C("glyphpos-suffix-empty-glyph", ["idle"],
  env=E(CCTAB_GLYPH_POS="suffix", CCTAB_GLYPH_IDLE=""),
  note="no trailing space when the glyph is empty")
C("glyphpos-both-empty-glyph", ["idle"],
  env=E(CCTAB_GLYPH_POS="both", CCTAB_GLYPH_IDLE=""))
C("glyphpos-suffix-with-ssh-prefix", ["idle"],
  env=E(CCTAB_GLYPH_POS="suffix", SSH_CONNECTION="1 2 3 4", CCTAB_HOST="srv"),
  note="the host prefix stays at the front")

# ===========================================================================
# 3. Konsole detection x multiplexer (decides the glyph position)
# ===========================================================================
KV = "260801"
KDS = "/Sessions/6"
for name, kv, kds in (("neither", None, None), ("kv", KV, None),
                      ("kds", None, KDS), ("both", KV, KDS)):
    for mux, tmux, sty in (("plain", None, None),
                           ("tmux", "/tmp/tmux-1000/default,1234,0", None),
                           ("sty", None, "1234.pts-0.host"),
                           ("tmuxsty", "/tmp/tmux-1000/default,1234,0",
                            "1234.pts-0.host")):
        C("konsole-%s-%s" % (name, mux), ["idle"],
          env=E(KONSOLE_VERSION=kv, KONSOLE_DBUS_SESSION=kds, TMUX=tmux,
                STY=sty))
C("konsole-kv-empty-is-not-konsole", ["idle"],
  env=E(KONSOLE_VERSION="", KONSOLE_DBUS_SESSION=""))
C("konsole-tmux-empty-does-not-suppress", ["idle"],
  env=E(KONSOLE_VERSION=KV, TMUX="", STY=""))
C("konsole-glyphpos-wins", ["idle"],
  env=E(KONSOLE_VERSION=KV, CCTAB_GLYPH_POS="prefix"))

# ===========================================================================
# 4. location: not a repository
# ===========================================================================
C("loc-home-itself", ["idle"], cwd=".")
C("loc-home-trailing-slash", ["idle"], env=E(HOME=TOK + "/"), cwd=".")
C("loc-under-home", ["idle"], cwd="code/bug_fedora")
C("loc-outside-home", ["idle"],
  env=E(HOME="/nonexistent-home", CCTAB_MAX_LOCATION="0"), cwd="plaindir")
C("loc-home-empty", ["idle"], env=E(HOME="", CCTAB_MAX_LOCATION="0"),
  cwd="plaindir")
C("loc-home-unset", ["idle"], env=E(HOME=None, CCTAB_MAX_LOCATION="0"),
  cwd="plaindir")
C("loc-home-string-prefix-sibling", ["idle"],
  env=E(HOME=TOK + "/homeprefix", CCTAB_MAX_LOCATION="0"), cwd="homeprefixed")
C("loc-filesystem-root", ["idle"], env=E(HOME="/nonexistent-home"), cwd="/")
C("loc-very-long-path", ["idle"], cwd="verylong/" + "/".join(["c" * 30] * 10))
C("loc-very-long-path-uncapped", ["idle"], env=E(CCTAB_MAX_LOCATION="0"),
  cwd="verylong/" + "/".join(["c" * 30] * 10))
C("loc-symlinked-cwd-keeps-logical-path", ["idle"], cwd="link-to-plain")
C("loc-pwd-stale", ["idle"], env=E(PWD="/etc"), cwd="repos/plain",
  note="the shell re-verifies an inherited PWD; the port must do the same")
C("loc-pwd-relative", ["idle"], env=E(PWD="relative"), cwd="repos/plain")
C("loc-pwd-nonexistent", ["idle"], env=E(PWD="/no/such/dir"), cwd="repos/plain")
C("loc-pwd-unset", ["idle"], env=E(PWD=None), cwd="repos/plain", pwd=False)
C("loc-pwd-empty", ["idle"], env=E(PWD=""), cwd="repos/plain")
# The six cases above all sit INSIDE a repo, where the walk uses the physical
# path and the answer is the same whether or not an implementation re-verifies
# the inherited PWD. So none of them can fail an implementation that trusts a
# stale PWD - a differential fuzz against the shell caught that, this corpus did
# not. These five put the same values on a NON-repo cwd, where $PWD is the
# location and the difference is visible. bash's set_pwd() keeps an inherited PWD
# only when it is absolute AND names the same directory as "." by device and
# inode, and it keeps it VERBATIM when it does - it does not canonicalize.
C("loc-pwd-stale-nonrepo", ["idle"], env=E(PWD="/etc"), cwd="plaindir",
  note="a stale PWD outside a repo: bash replaces it with getcwd(), so the tab "
       "says plaindir and not /etc")
C("loc-pwd-relative-nonrepo", ["idle"], env=E(PWD="relative"), cwd="plaindir")
C("loc-pwd-nonexistent-nonrepo", ["idle"], env=E(PWD="/no/such/dir"), cwd="plaindir")
C("loc-pwd-wrong-dir-nonrepo", ["idle"], env=E(PWD=TOK), cwd="plaindir",
  note="absolute and existing, but not this directory: still replaced")
C("loc-pwd-dotdot-kept-verbatim", ["idle"], env=E(PWD=TOK + "/plaindir/../plaindir"),
  cwd="plaindir",
  note="passes bash's same-file test, so it is kept UNCANONICALIZED and the "
       "tab shows the .. path")

# ===========================================================================
# 5. location: inside a repository
# ===========================================================================
repo_cases = [
    ("repo-plain", "repos/plain"),
    ("repo-deep-subdir", "repos/plain/deep/er/still"),
    ("repo-innermost-wins", "repos/plain/inner"),
    ("repo-branch-with-slashes", "repos/slashy"),
    ("repo-head-no-trailing-newline", "repos/nonl"),
    ("repo-head-crlf", "repos/crlf"),
    ("repo-detached-sha1", "repos/detached"),
    ("repo-detached-sha256", "repos/sha256"),
    ("repo-detached-uppercase", "repos/upper"),
    ("repo-ref-outside-refs-heads", "repos/otherref"),
    ("repo-ref-remote-tracking", "repos/remoteref"),
    ("repo-ref-tag", "repos/tagref"),
    ("repo-head-trailing-blanks", "repos/trailws"),
    ("repo-unborn-branch", "repos/unborn"),
    ("repo-head-absurdly-long", "repos/hugehead"),
    ("repo-worktree-absolute-gitdir", "wt-linked"),
    ("repo-worktree-gitdir-no-newline", "wt-nonl"),
    ("repo-worktree-gitdir-crlf", "wt-crlf"),
    ("repo-submodule-relative-gitdir", "super/mysub"),
    ("repo-submodule-superproject", "super"),
    ("repo-dotgit-symlink", "symrepo"),
    ("repo-symlink-into-subdir", "link-into-repo"),
    ("repo-symlink-to-toplevel", "link-to-top"),
    ("repo-junk-inside-repo-falls-through", "repos/plain/hasjunk"),
    ("nonrepo-empty-dotgit-dir", "junk/emptygit"),
    ("nonrepo-garbage-dotgit-file", "junk/garbagegit"),
    ("nonrepo-stale-gitdir-pointer", "junk/stalegit"),
    ("nonrepo-empty-head", "junk/emptyhead"),
    ("nonrepo-head-is-a-directory", "junk/dirhead"),
    ("nonrepo-head-too-short", "junk/shorthead"),
    ("nonrepo-unreadable-head", "junk/noreadhead"),
    ("nonrepo-unreadable-dotgit-file", "junk/noreadgitfile"),
]
for cid, cwd in repo_cases:
    C(cid, ["idle"], cwd=cwd)
C("repo-at-walk-bound-63", ["idle"], cwd="bound/" + "/".join(["a"] * 63))
C("repo-past-walk-bound-64", ["idle"], cwd="bound/" + "/".join(["a"] * 64))
C("repo-past-walk-bound-64-uncapped", ["idle"],
  env=E(CCTAB_MAX_LOCATION="0"), cwd="bound/" + "/".join(["a"] * 64))

# GIT_DIR
C("gitdir-absolute", ["idle"], env=E(GIT_DIR=TOK + "/repos/plain/.git"),
  cwd="plaindir")
C("gitdir-relative", ["idle"], env=E(GIT_DIR=".git"), cwd="repos/plain")
C("gitdir-bare-repo", ["idle"], env=E(GIT_DIR=TOK + "/bare/proj.git"),
  cwd="plaindir")
C("gitdir-not-a-repo", ["idle"], env=E(GIT_DIR=TOK + "/junk/no-such-gitdir"),
  cwd="plaindir")
C("gitdir-empty-is-ignored", ["idle"], env=E(GIT_DIR=""), cwd="repos/plain")
C("gitdir-dot-in-bare-repo", ["idle"], env=E(GIT_DIR="."), cwd="bare/proj.git")
C("gitdir-dot-component", ["idle"],
  env=E(GIT_DIR=TOK + "/repos/plain/./.git"), cwd="repos/plain")
C("gitdir-dotdot-component", ["idle"],
  env=E(GIT_DIR=TOK + "/repos/plain/sub/../.git"), cwd="repos/plain")
C("gitdir-dotgit-dot", ["idle"], env=E(GIT_DIR=".git/."), cwd="repos/plain")
C("gitdir-worktree-pointer", ["idle"], env=E(GIT_DIR=TOK + "/wt-linked/.git"),
  cwd="plaindir")

# ===========================================================================
# 6. the length cap
# ===========================================================================
LONGPATH = "one/two/three/four/five/six/seven/eight"
# "020" and "0032" are the leading-zero values that DISCRIMINATE: bash's test
# builtin reads "032" as decimal 32, which coincides with the default, so that
# value alone cannot tell an accepted leading zero from a rejected one.
for val in ("0", "2", "8", "16", "40", "999", "1000", "lots", "08", "032",
            "020", "0032", "-1", "3.5", "", "32"):
    C("maxloc-" + (val or "empty") if val != "-1" else "maxloc-negative",
      ["idle"], env=E(CCTAB_MAX_LOCATION=val), cwd=LONGPATH)
C("maxloc-unset", ["idle"], env=E(CCTAB_MAX_LOCATION=None), cwd=LONGPATH)
C("maxloc-one-overlong-component", ["idle"],
  cwd="aaaaaaaaaabbbbbbbbbbccccccccccdddddddddd",
  note="no boundary left, so the cut goes inside and the marker loses its /")
C("maxloc-repo-cut-at-the-back", ["idle"], cwd="repos/longbranch")
C("maxloc-repo-under-the-cap", ["idle"], cwd="repos/plain")
C("maxloc-nonascii-path-exempt", ["idle"],
  cwd="ééééééééééééééééééééééééééééééééééééé",
  diverge="limitation-3-4: the cap unit is locale-dependent, so a non-ASCII "
          "location is exempted from the cap entirely and overflows")
C("maxloc-nonascii-branch-exempt", ["idle"], cwd="repos/accentlong",
  diverge="limitation-3-4: same exemption on the branch side")
C("maxloc-nonascii-path-c-locale", ["idle"], env=E(LC_ALL="C"),
  cwd="ééééééééééééééééééééééééééééééééééééé",
  diverge="limitation-3: ${#var} counts bytes in the C locale and characters "
          "in a UTF-8 one")
C("maxloc-nonascii-mixed-path", ["idle"], env=E(CCTAB_MAX_LOCATION="12"),
  cwd="café-déjà",
  diverge="limitation-3: 11 characters or 13 bytes, so bash keeps it whole and "
          "dash peels it - the one case that actually differs between the two "
          "shells the plugin ships against")
C("maxloc-ascii-path-c-locale", ["idle"], env=E(LC_ALL="C"), cwd=LONGPATH,
  note="pure ASCII: the two locales must agree")
MULTIACC = "multiacc/" + "/".join(["\u00e9" * 9] * 3)
C("maxloc-nonascii-components-utf8-locale", ["idle"], cwd=MULTIACC,
  diverge="limitation-3: the component-peeling loop runs BEFORE the ASCII "
          "guard, so how many components it drops depends on the count unit")
C("maxloc-nonascii-components-c-locale", ["idle"], env=E(LC_ALL="C"),
  cwd=MULTIACC,
  diverge="limitation-3: same path, byte counting, a different answer")
for ell in ("...", "", ">", "…"):
    C("ellipsis-" + (ell or "empty"), ["idle"], env=E(CCTAB_ELLIPSIS=ell),
      cwd=LONGPATH)
C("ellipsis-unset", ["idle"], env=E(CCTAB_ELLIPSIS=None), cwd=LONGPATH)
C("ellipsis-on-repo-cut", ["idle"], env=E(CCTAB_ELLIPSIS="..."),
  cwd="repos/longbranch")
C("ellipsis-multibyte", ["idle"], env=E(CCTAB_ELLIPSIS="→"), cwd=LONGPATH)

# ===========================================================================
# 7. the ssh prefix
# ===========================================================================
C("ssh-none", ["idle"])
C("ssh-tty-only", ["idle"], env=E(SSH_TTY="/dev/pts/9", CCTAB_HOST="srv"))
C("ssh-connection-only", ["idle"],
  env=E(SSH_CONNECTION="10.0.0.1 22 10.0.0.2 22", CCTAB_HOST="srv"))
C("ssh-both", ["idle"],
  env=E(SSH_TTY="/dev/pts/9", SSH_CONNECTION="10.0.0.1 22 10.0.0.2 22",
        CCTAB_HOST="srv"))
C("ssh-both-empty", ["idle"],
  env=E(SSH_TTY="", SSH_CONNECTION="", CCTAB_HOST="srv"))
C("ssh-tty-empty-connection-set", ["idle"],
  env=E(SSH_TTY="", SSH_CONNECTION="1 2 3 4", CCTAB_HOST="srv"))
C("ssh-fqdn-loses-domain", ["idle"],
  env=E(SSH_TTY="x", CCTAB_HOST="srv.example.com"))
C("ssh-dotted-quad-keeps-dots", ["idle"],
  env=E(SSH_TTY="x", CCTAB_MAX_HOST="0", CCTAB_HOST="192.168.1.5"))
C("ssh-dotted-quad-capped", ["idle"],
  env=E(SSH_TTY="x", CCTAB_HOST="192.168.100.200"))
C("ssh-host-resolving-to-nothing-says-ssh", ["idle"],
  env=E(SSH_TTY="x", CCTAB_HOST=".example.com"))
C("ssh-prefix-on-a-path-location", ["idle"],
  env=E(SSH_TTY="x", CCTAB_HOST="srv"), cwd="plaindir")
C("ssh-prefix-not-eaten-by-the-cap", ["idle"],
  env=E(SSH_TTY="x", CCTAB_HOST="srv"), cwd=LONGPATH)
C("ssh-host-from-proc", ["idle"], env=E(SSH_TTY="x"), host_token=True,
  note="CCTAB_HOST unset: the name comes from /proc/sys/kernel/hostname")
C("ssh-host-from-proc-no-path", ["idle"], env=E(SSH_TTY="x", PATH=""),
  host_token=True, note="and needs no external command")
C("ssh-host-empty-falls-through-to-proc", ["idle"],
  env=E(SSH_TTY="x", CCTAB_HOST=""), host_token=True)
C("ssh-hostname-env-loses-to-proc", ["idle"],
  env=E(SSH_TTY="x", HOSTNAME="from-env"), host_token=True,
  note="the $HOSTNAME branch is unreachable while /proc is readable")
K8S = "my-cluster-worker-pool-a-7f9d8c6b5-x2kqz"
for val in ("0", "3", "4", "7", "16", "08", "020", "nope", "", "999"):
    C("maxhost-" + (val or "empty"), ["idle"],
      env=E(SSH_TTY="x", CCTAB_HOST=K8S, CCTAB_MAX_HOST=val))
C("maxhost-unset", ["idle"],
  env=E(SSH_TTY="x", CCTAB_HOST=K8S, CCTAB_MAX_HOST=None))
C("maxhost-under-the-cap", ["idle"],
  env=E(SSH_TTY="x", CCTAB_HOST="build-runner-eu"))
C("maxhost-ellipsis-override", ["idle"],
  env=E(SSH_TTY="x", CCTAB_HOST=K8S, CCTAB_ELLIPSIS="..."))
C("maxhost-nonascii-host-exempt", ["idle"],
  env=E(SSH_TTY="x", CCTAB_HOST="hôte-très-long-pour-le-cap"),
  diverge="limitation-3-4: the host cap has the same non-ASCII exemption")

# ===========================================================================
# 8. hostile names
# ===========================================================================
C("hostile-double-quote", ["idle"], cwd='we"ird')
C("hostile-backslash", ["idle"], cwd="back\\slash")
C("hostile-quote-and-backslash", ["idle"], cwd='both"x\\y')
C("hostile-newline", ["idle"], cwd="new\nline")
C("hostile-tab", ["idle"], cwd="ta\tb")
C("hostile-only-hostile-bytes", ["idle"], cwd='"\\',
  note="sanitizes to nothing, leaving the path skeleton")
C("hostile-glob-characters", ["idle"], cwd="gl*b?[x]")
C("hostile-backtick", ["idle"], cwd="back`tick")
C("hostile-percent", ["idle"], cwd="pct-100%s%d-x",
  note="the title is always an argument, never a printf format")
C("hostile-percent-branch", ["idle"], cwd="repos/pct")
C("hostile-nonascii-path", ["idle"], cwd="café-déjà")
C("hostile-nonascii-branch", ["idle"], cwd="repos/accent")
C("hostile-branch", ["idle"], cwd="repos/hostile")
C("hostile-control-byte-in-branch", ["idle"], cwd="repos/ctrlbranch")
C("hostile-hostname", ["idle"], env=E(SSH_TTY="x", CCTAB_HOST='s"r\\v'))
C("hostile-invalid-utf8-path", ["idle"], cwd="bad\udcffutf8",
  diverge="limitation-2: a non-UTF-8 name yields invalid JSON, so no title "
          "paints at all")
C("hostile-invalid-utf8-path-json", ["idle"], env=E(CCTAB_DRY_RUN=None),
  cwd="bad\udcffutf8",
  diverge="limitation-2: the emitted terminalSequence line is invalid JSON")
C("hostile-sanitizer-bound", ["idle"], env=E(CCTAB_MAX_LOCATION="0"),
  cwd='sanbound/q"' + "aaaaaaaaaabbbbbbbbbbccccccccccddddddddddeeeeeeeeeeffffffffffgggggggggghhhhhhhhhhiiiiiiiiiijjjjjjjjjjkkkkkkkkkkllllllllllmmmmmmmmmmnnnnnnnnnnoooooooooopppppppppp/"
      + "aaaaaaaaaabbbbbbbbbbccccccccccddddddddddeeeeeeeeeeffffffffffgggggggggghhhhhhhhhhiiiiiiiiiijjjjjjjjjjkkkkkkkkkkllllllllllmmmmmmmmmmnnnnnnnnnnoooooooooopppppppppp",
  note="the sanitizer loop is quadratic and stops at 256 iterations")
C("hostile-no-path-still-sanitizes", ["idle"], env=E(PATH=""), cwd='we"ird')
C("hotpath-repo-no-path", ["idle"], env=E(PATH=""), cwd="repos/plain")
C("hotpath-path-no-path", ["idle"], env=E(PATH=""), cwd="code/bug_fedora")
C("hotpath-elided-no-path", ["idle"], env=E(PATH=""), cwd=LONGPATH)

# ===========================================================================
# 9. stdin handling on the edges that must NOT read the payload
# ===========================================================================
PL = ('{"session_id":"abc","transcript_path":"/tmp/t.jsonl","cwd":"/x",'
      '"hook_event_name":"Stop"}')
C("stdin-oneline-drained", ["idle"], stdin=PL + "\n")
C("stdin-no-trailing-newline", ["idle"], stdin=PL)
C("stdin-multiline-drained", ["idle"], stdin=(PL + "\n") * 3)
C("stdin-empty", ["idle"], stdin="")
C("stdin-binary-drained", ["idle"], stdin="\x00\x01\x02not json\n")
C("stdin-big-payload-drained", ["working"],
  stdin_file="payloads/big-posttooluse.json",
  note="256KB carrying a verbatim idle_prompt: still working")
C("stdin-toolresponse-mentions-idle-prompt", ["working"],
  stdin='{"hook_event_name":"PostToolUse","tool_response":'
        '"{\\"notification_type\\":\\"idle_prompt\\"}"}\n')
C("stdin-subagent-posttooluse", ["working"],
  stdin='{"agent_id":"aec99e1f4bda1972b","agent_type":"general-purpose",'
        '"hook_event_name":"PostToolUse","tool_name":"Read"}\n',
  note="a subagent tool call paints working; the port gains agent_id here")
C("stdin-subagent-permissionrequest", ["waiting"],
  stdin='{"agent_id":"aec99e1f4bda1972b","agent_type":"general-purpose",'
        '"hook_event_name":"PermissionRequest","tool_name":"Write"}\n',
  note="limitation 1 lives here: the port may want to read agent_id")
C("stdin-mainthread-permissionrequest", ["waiting"],
  stdin='{"hook_event_name":"PermissionRequest","tool_name":"Write",'
        '"permission_suggestions":[]}\n')
BOTH = ('{"hook_event_name":"PostToolUse","notification_type":"idle_prompt",'
        '"source":"compact","tool_response":"x"}\n')
for e in ("working", "waiting", "idle"):
    C("stdin-cannot-redirect-" + e, [e], stdin=BOTH)

# ===========================================================================
# 10. notify: the payload decides the state
# ===========================================================================
def notif(kind, message="Claude needs your permission"):
    return (
        '{"session_id":"s1","transcript_path":"%s/t.jsonl","cwd":"%s/repos/plain",'
        '"scratchpad_dir":"%s/sp","hook_event_name":"Notification","message":"%s",'
        '"notification_type":"%s"}\n' % (TOK, TOK, TOK, message, kind)
    )


WAITING_KINDS = ("permission_prompt", "worker_permission_prompt",
                 "agent_needs_input", "elicitation_dialog",
                 "elicitation_url_dialog")
SILENT_KINDS = ("agent_completed", "elicitation_complete",
                "elicitation_response", "computer_use_exit",
                "push_notification", "auth_success")
C("notify-idle-prompt", ["notify"],
  stdin=notif("idle_prompt", "Claude is waiting for your input"))
for k in WAITING_KINDS:
    C("notify-" + k.replace("_", "-"), ["notify"], stdin=notif(k))
for k in SILENT_KINDS:
    C("notify-silent-" + k.replace("_", "-"), ["notify"], stdin=notif(k))
C("notify-unknown-kind", ["notify"], stdin=notif("some_kind_invented_later"))
C("notify-kind-containing-known-one", ["notify"],
  stdin=notif("not_really_idle_prompt_either"))
C("notify-empty-payload", ["notify"], stdin="")
C("notify-non-json", ["notify"], stdin="this is not json at all\n")
C("notify-non-json-mentioning-kind", ["notify"],
  stdin='notification_type=permission_prompt\n',
  note="the glob wants the quoted JSON spelling")
C("notify-message-mentions-idle-prompt", ["notify"],
  stdin=notif("permission_prompt", "the idle_prompt kind is not this one"))
C("notify-both-kinds-idle-wins", ["notify"],
  stdin='{"notification_type":"permission_prompt","x":'
        '"\\"notification_type\\":\\"idle_prompt\\""}\n',
  note="idle_prompt is matched first, anywhere on the line")
C("notify-odd-field-order", ["notify"],
  stdin='{"notification_type":"permission_prompt","message":"m",'
        '"hook_event_name":"Notification","session_id":"s1"}\n')
C("notify-kind-first-field", ["notify"],
  stdin='{"notification_type":"idle_prompt","session_id":"s1"}\n')
C("notify-spaced-out-kind-is-silent", ["notify"],
  stdin='{"hook_event_name":"Notification","notification_type": '
        '"permission_prompt"}\n')
C("notify-pretty-printed-is-silent", ["notify"],
  stdin='{\n  "notification_type": "idle_prompt"\n}\n')
C("notify-no-trailing-newline", ["notify"],
  stdin='{"notification_type":"idle_prompt"}')
C("notify-multiline-read-and-drained", ["notify"],
  stdin='{"notification_type":"permission_prompt"}\ntrailing\ntrailing\n')
C("notify-second-line-carries-the-kind", ["notify"],
  stdin='{"a":1}\n{"notification_type":"permission_prompt"}\n',
  note="only the FIRST line is examined")
# The real shape at a real size. `notif()` already puts notification_type LAST,
# after `message`, because that is the order Claude Code serializes - and `message`
# is unbounded: on an MCP elicitation the server writes it. A port that read only a
# bounded PREFIX of the payload therefore lost the discriminator once the message
# passed ~7.35 KB and painted nothing at all, on exactly the kinds for which this
# Notification is the only signal. The shell, which read the whole line, painted
# correctly at any size - so these cases are frozen from it as ordinary
# non-divergent cases and the port has to match them.
_LONGMSG = "x" * 9000
C("notify-permission-prompt-after-a-long-message", ["notify"],
  stdin=notif("permission_prompt", _LONGMSG),
  note="the discriminator sits past any 8 KiB prefix; the tail window finds it")
C("notify-idle-prompt-after-a-long-message", ["notify"],
  stdin=notif("idle_prompt", _LONGMSG))
C("notify-agent-needs-input-after-a-long-message", ["notify"],
  stdin=notif("agent_needs_input", _LONGMSG),
  note="an MCP-supplied message is the realistic way this gets long")
C("notify-silent-kind-after-a-long-message", ["notify"],
  stdin=notif("agent_completed", _LONGMSG),
  note="the tail window must not invent a state either")
C("notify-json-idle", ["notify"], env=E(CCTAB_DRY_RUN=None),
  stdin=notif("idle_prompt"))
C("notify-json-waiting", ["notify"], env=E(CCTAB_DRY_RUN=None),
  stdin=notif("permission_prompt"))
C("notify-json-silent", ["notify"], env=E(CCTAB_DRY_RUN=None),
  stdin=notif("agent_completed"))
C("notify-big-payload", ["notify"], stdin_file="payloads/big-posttooluse.json",
  note="notify takes the bounded-read branch; 256KB is the timeout risk of "
       "limitation 5")

# ===========================================================================
# 11. session-start: the source decides whether to paint at all
# ===========================================================================
def sstart(source):
    return (
        '{"session_id":"s1","transcript_path":"%s/t.jsonl","cwd":"%s/repos/plain",'
        '"scratchpad_dir":"%s/sp","hook_event_name":"SessionStart","source":"%s",'
        '"model":"claude-haiku-4-5"}\n' % (TOK, TOK, TOK, source)
    )


for s in ("startup", "resume", "clear", "fork"):
    C("sstart-" + s, ["session-start"], stdin=sstart(s))
C("sstart-compact", ["session-start"], stdin=sstart("compact"))
C("sstart-compact-spaced", ["session-start"],
  stdin='{"hook_event_name":"SessionStart","source": "compact"}\n')
C("sstart-compact-pretty-printed-escapes-the-belt", ["session-start"],
  stdin='{\n  "hook_event_name": "SessionStart",\n  "source": "compact"\n}\n',
  note="fails OPEN: hooks.json's matcher is the load-bearing guard")
C("sstart-word-compact-elsewhere", ["session-start"],
  stdin='{"cwd":"%s/compact","hook_event_name":"SessionStart",'
        '"source":"startup"}\n' % TOK)
C("sstart-no-payload", ["session-start"], stdin="")
C("sstart-unknown-source", ["session-start"], stdin=sstart("teleported"))
C("sstart-no-trailing-newline", ["session-start"],
  stdin='{"source":"compact"}')
C("sstart-multiline-drained", ["session-start"],
  stdin='{"source":"startup"}\ntrailing\n')
C("sstart-compact-on-second-line", ["session-start"],
  stdin='{"source":"startup"}\n{"source":"compact"}\n')

# ===========================================================================
# 12. the headless guard on the direct-write edges
# ===========================================================================
for e in ("session-start", "session-end"):
    C("guard-%s-no-claude-pid" % e, [e], env=E(CCTAB_DRY_RUN=None,
                                               CLAUDE_PID=None))
    C("guard-%s-dead-pid" % e, [e],
      env=E(CCTAB_DRY_RUN=None, CLAUDE_PID="4194303"))
    C("guard-%s-pid-1" % e, [e], env=E(CCTAB_DRY_RUN=None, CLAUDE_PID="1"),
      note="fd 1 of pid 1 is unreadable for an unprivileged user")
    C("guard-%s-nonsense-pid" % e, [e],
      env=E(CCTAB_DRY_RUN=None, CLAUDE_PID="not-a-pid"))
    C("guard-%s-empty-pid" % e, [e], env=E(CCTAB_DRY_RUN=None, CLAUDE_PID=""))
    C("guard-%s-fd1-is-a-file" % e, [e],
      env=E(CCTAB_DRY_RUN=None, CLAUDE_PID="@@PID_FILE@@"), mode="pidfile",
      note="a redirected `claude -p`: the guard must keep this inert")
C("guard-session-start-dry-run-wins", ["session-start"],
  env=E(CLAUDE_PID="@@PID_PTY@@"), mode="pty",
  note="dry run prints the title and never reaches the pty")

# ===========================================================================
# 13. the two edges that write the pty directly, over a real allocated pty
# ===========================================================================
for name, extra in (("konsole", {"KONSOLE_VERSION": KV}),
                    ("konsole-dbus", {"KONSOLE_DBUS_SESSION": KDS}),
                    ("plain", {}),
                    ("konsole-in-tmux", {"KONSOLE_VERSION": KV,
                                         "TMUX": "/tmp/t,1,0"}),
                    ("konsole-in-screen", {"KONSOLE_VERSION": KV,
                                           "STY": "1.pts-0"})):
    for e in ("session-start", "session-end"):
        env = E(CCTAB_DRY_RUN=None, CLAUDE_PID="@@PID_PTY@@")
        env.update(extra)
        C("pty-%s-%s" % (e, name), [e], env=env, mode="pty",
          stdin=sstart("startup") if e == "session-start" else "")
C("pty-session-start-compact-emits-nothing", ["session-start"],
  env=E(CCTAB_DRY_RUN=None, CLAUDE_PID="@@PID_PTY@@",
        KONSOLE_VERSION=KV), mode="pty", stdin=sstart("compact"),
  note="the compact belt on the emitting path, which tests/run.sh could not "
       "cover without a pty")
C("pty-working-does-not-touch-the-pty", ["working"],
  env=E(CCTAB_DRY_RUN=None, CLAUDE_PID="@@PID_PTY@@", KONSOLE_VERSION=KV),
  mode="pty", note="normal edges go to stdout, never to the pty")

# ===========================================================================
# run every case against the reference implementation
# ===========================================================================
def enc(b):
    try:
        return b.decode("utf-8"), False
    except UnicodeDecodeError:
        return base64.b64encode(b).decode("ascii"), True


def dumps(rec):
    """JSON, UTF-8, with any lone surrogate (an invalid-UTF-8 byte carried
    through surrogateescape) written as a \\uXXXX escape so the corpus file
    itself stays valid UTF-8 and valid JSON."""
    s = json.dumps(rec, ensure_ascii=False, sort_keys=False)
    if any(0xD800 <= ord(c) <= 0xDFFF for c in s):
        s = "".join("\\u%04x" % ord(c) if 0xD800 <= ord(c) <= 0xDFFF else c
                    for c in s)
    return s


def main():
    import runner
    host = runner.short_host()
    tmpdir = tempfile.mkdtemp(prefix="cctab-gen-")
    helpers = runner.Helpers(tmpdir)
    out = []
    try:
        for c in cases:
            res = runner.run(c, REF, FIX, helpers)
            rec = dict(c)
            for key in ("stdout", "stderr", "pty_out"):
                if key not in res:
                    continue
                v, b64 = enc(runner.detokenize(
                    res[key], FIX, host, c.get("host_token", False)))
                rec[key + "_b64" if b64 else key] = v
            rec["exit"] = res["exit"]
            out.append(rec)
    finally:
        helpers.close()
    with open(os.path.join(HERE, "cases.jsonl"), "w", encoding="utf-8") as f:
        for rec in out:
            f.write(dumps(rec) + "\n")
    print("%d cases written" % len(out))
    if helpers.pty_path:
        print("pty used: %s" % helpers.pty_path)


if __name__ == "__main__":
    sys.path.insert(0, HERE)
    main()
