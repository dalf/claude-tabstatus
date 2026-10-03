#!/usr/bin/env python3
"""Required Linux x86_64 subprocess gate; no timing and no skip-success paths.

Requires strace, tmux and a C compiler. Only the candidate is traced, not the
test harness or its private tmux server. Every hook environment is constructed
from scratch; CLAUDE_PID always names an owned disposable PTY stand-in.
"""
import argparse
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import tempfile
import time

sys.path.insert(0, str(Path(__file__).resolve().parent / "corpus"))
from runner import Helpers

ROOT = Path(__file__).resolve().parents[1]
SYSCALLS = ("fork", "vfork", "clone", "clone3", "execve", "execveat")
SID = "bench000-1111-2222-3333-444455556666"
CALL = re.compile(r"^(?:(\d+)\s+)?(" + "|".join(SYSCALLS) + r")\(")


class Violation(RuntimeError):
    pass


def check_trace(trace, returncode):
    """Allow only the initial successful exec. Count failed/unfinished calls too.

    strace -f -o prefixes PIDs. A resumed line is not a second call; the entry
    (including '<unfinished ...>') already counted it. Require normal completion
    of the initial PID as well as tracer success, never infer success from zero.
    """
    calls = [(line, CALL.match(line)) for line in trace.splitlines()]
    calls = [(line, match) for line, match in calls if match]
    if returncode != 0 or not calls:
        raise RuntimeError(f"tracer/tracee failed ({returncode}), or empty trace:\n{trace}")
    first, match = calls[0]
    if match[2] != "execve" or not first.endswith(" = 0"):
        raise RuntimeError(f"no successful initial exec:\n{trace}")
    pid = match[1]
    end = (rf"{pid}\s+" if pid else "") + r"\+\+\+ exited with 0 \+\+\+"
    if not re.search(r"^" + end + r"$", trace, re.M):
        raise RuntimeError(f"no normal tracee completion:\n{trace}")
    if len(calls) != 1:
        raise Violation("subprocess syscall(s):\n" + "\n".join(line for line, _ in calls[1:]))


def traced(tracer, command, env, cwd, log, payload=b""):
    # Remove any previous evidence so a broken tracer cannot inherit a clean log.
    log.unlink(missing_ok=True)
    result = subprocess.run(
        [tracer, "-f", "-e", "trace=" + ",".join(SYSCALLS), "-o", str(log),
         "--", *command], input=payload, capture_output=True, env=env, cwd=cwd,
        timeout=15)
    trace = log.read_text() if log.exists() else ""
    if result.stderr:
        raise RuntimeError(f"tracer/tracee stderr: {result.stderr!r}")
    check_trace(trace, result.returncode)
    return result.stdout


def controls(tracer, root, env):
    control = root / "control"
    subprocess.run(["cc", "-O0", "-Wall", "-Wextra", "-o", str(control),
                    str(ROOT / "tests/fixtures/subprocess-control.c")], check=True,
                   capture_output=True, timeout=30)
    traced(tracer, [str(control), "clean"], env, root, root / "control.trace")
    for mode in (*SYSCALLS, "failed-execve"):
        try:
            traced(tracer, [str(control), mode], env, root, root / "control.trace")
        except Violation as error:
            expected = "execve" if mode == "failed-execve" else mode
            if not re.search(r"\b" + expected + r"\(", str(error)):
                raise RuntimeError(f"control {mode} caught the wrong call: {error}")
            print(f"ok control: rejected {mode}", flush=True)
        else:
            raise RuntimeError(f"control {mode} incorrectly passed")


def exercise(binary, tracer, root):
    env = {"PATH": os.defpath, "HOME": str(root), "LC_ALL": "C.UTF-8",
           "TERM": "xterm-256color", "CCTAB_TERMINAL": "other",
           "CCTAB_GLYPH_POS": "prefix", "CCTAB_NOW": "1000000",
           "CLAUDE_CONFIG_DIR": str(root / "config"),
           "XDG_DATA_HOME": str(root / "data")}
    controls(tracer, root, env)
    work = root / "work"
    work.mkdir()
    # A repository location exercises the native git walk without invoking git.
    (work / ".git").mkdir()
    (work / ".git/HEAD").write_text("ref: refs/heads/main\n")
    helpers = Helpers(str(root))
    tmux = [shutil.which("tmux"), "-S", str(root / "tmux.sock"), "-f", "/dev/null"]

    def tm(*args):
        return subprocess.run(tmux + list(args), env=env, cwd=root, check=True,
                              capture_output=True, text=True, timeout=10).stdout.strip()

    try:
        bare_pid = helpers.pty_pid()
        pane, pane_pid = tm("new-session", "-d", "-s", "probe", "-P", "-F",
                            "#{pane_id} #{pane_pid}", "/bin/sleep 3600").split()
        for route in ("protocol", "tmux"):
            for state in (False, True):
                for edge, event, glyph, code in (
                    ("working", "PostToolUse", "🔵", "w"),
                    ("waiting", "PermissionRequest", "🟠", "a"),
                    ("idle", "Stop", "⚪", "i"),
                    ("notify", "Notification", "🟠", "a"),
                ):
                    case = f"{route}-{edge}-state-{int(state)}"
                    hook_env = dict(env, CLAUDE_PID=str(bare_pid if route == "protocol" else pane_pid))
                    if route == "tmux":
                        hook_env.update(TMUX=f"{root}/tmux.sock,1,0", TMUX_PANE=pane)
                    if state:
                        hook_env["CCTAB_STATE_DIR"] = str(root / case)
                        if edge == "idle":
                            # An already-idle fresh session deliberately writes no
                            # record. Seed working so Stop transitions, then reuses it.
                            (root / case).mkdir()
                            (root / case / SID).write_text("cts1\nb w\n")
                    payload = json.dumps({"session_id": SID,
                                          "hook_event_name": event, "tool_name": "Read",
                                          "tool_response": "z" * 2048,
                                          "notification_type": "permission_prompt"}).encode()
                    # First paint and repeated paint cover record writes and reuse.
                    for _ in range(2):
                        if route == "tmux":
                            tm("select-pane", "-t", pane, "-T", "sentinel")
                        out = traced(tracer, [str(binary), edge], hook_env, work,
                                     root / "hook.trace", payload)
                        if state and not (root / case / SID).is_file():
                            raise RuntimeError(f"{case}: no state record created")
                        if route == "protocol":
                            expected = {"terminalSequence": f"\x1b]0;{glyph} work@main\x07",
                                        "suppressOutput": True}
                            try:
                                actual = json.loads(out)
                            except ValueError as error:
                                raise RuntimeError(f"{case}: invalid protocol output: {out!r}") from error
                            if actual != expected or helpers.drain_pty():
                                raise RuntimeError(f"{case}: incorrect protocol delivery: {out!r}")
                        else:
                            if out:
                                raise RuntimeError(f"{case}: unexpected stdout: {out!r}")
                            expected = f"work@main ct1 {code} 1000000"
                            deadline = time.monotonic() + 3
                            while True:
                                title = tm("display-message", "-p", "-t", pane, "#{pane_title}")
                                if title == expected:
                                    break
                                if time.monotonic() >= deadline:
                                    raise RuntimeError(f"{case}: pane title {title!r}, wanted {expected!r}")
                                time.sleep(0.02)
                    print(f"ok {case}: delivered twice, zero subprocess calls", flush=True)
    finally:
        helpers.close()
        subprocess.run(tmux + ["kill-server"], env=env, capture_output=True, timeout=10)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", default=os.environ.get("CCTAB_TEST_BIN", ROOT / "bin/tabstatus"))
    parser.add_argument("--strace", default="strace")
    args = parser.parse_args()
    if sys.platform != "linux":
        parser.error("this gate requires Linux")
    for tool in (args.strace, "tmux", "cc"):
        if not shutil.which(tool):
            parser.error(f"required tool unavailable: {tool}")
    try:
        with tempfile.TemporaryDirectory(prefix="cctab-subprocess-") as tmp:
            exercise(Path(args.binary).resolve(), shutil.which(args.strace), Path(tmp))
    except (OSError, RuntimeError, ValueError, subprocess.SubprocessError) as error:
        print(f"FAIL: {error}", file=sys.stderr)
        return 1
    print("PASS: Linux hot-edge subprocess and delivery checks")
    return 0


if __name__ == "__main__":
    sys.exit(main())
