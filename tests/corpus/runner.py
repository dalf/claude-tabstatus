#!/usr/bin/env python3
"""Shared execution core for gencases.py and replay.py.

One case is run with execve and NOTHING inherited: the environment is exactly
the case's `env`, so no ambient KONSOLE_VERSION, SSH_TTY, TMUX, LANG or PATH can
change the answer. That is what makes the corpus reproducible on another machine.

Three modes:

  pipe     stdin is the case payload, stdout/stderr captured. Default.
  pidfile  a helper process is spawned whose fd 1 is a REGULAR FILE, and
           @@PID_FILE@@ in the environment becomes its pid. That is the
           redirected `claude -p` shape the headless guard must refuse.
  pty      a helper process is spawned whose fd 1 is a freshly allocated pty,
           and @@PID_PTY@@ becomes its pid. Whatever the implementation writes
           to that pty is captured as `pty_out`. The pty is allocated by the
           kernel as a new pair, independent of existing terminal sessions,
           and put in raw mode so the bytes come back exactly as written.
"""
import base64
import errno
import fcntl
import json
import os
import select
import signal
import subprocess
import sys
import termios
import tty

TIMEOUT = 60


def short_host():
    try:
        with open("/proc/sys/kernel/hostname", "rb") as f:
            h = f.readline().strip()
    except OSError:
        return b""
    return h.split(b".", 1)[0]


def _b(s):
    return s.encode("utf-8", "surrogateescape") if isinstance(s, str) else s


def tokenize(value, fix, host, pids):
    """token -> real bytes"""
    b = _b(value)
    b = b.replace(b"@@FIX@@", _b(fix))
    if len(host) >= 3:
        b = b.replace(b"@@SHORTHOST@@", host)
    for name, pid in pids.items():
        b = b.replace(name, str(pid).encode())
    return b


def detokenize(b, fix, host, host_token=False):
    """real bytes -> token, for recording an expectation.

    The hostname is only tokenized for a case that asked for it. Substituting it
    everywhere is destructive: a machine called `fedora` would rewrite the
    fixture `code/bug_fedora` into `code/bug_@@SHORTHOST@@`.
    """
    b = b.replace(_b(fix), b"@@FIX@@")
    if host_token and len(host) >= 3:
        b = b.replace(host, b"@@SHORTHOST@@")
    return b


class Helpers:
    """Lazily created pty / regular-file stand-ins for a Claude Code process."""

    def __init__(self, tmpdir):
        self.tmpdir = tmpdir
        self.pty_master = None
        self.pty_path = None
        self.pty_proc = None
        self.file_proc = None
        self.file_path = None

    def pty_pid(self):
        if self.pty_proc is None:
            # openpty allocates a NEW pair; its number says nothing about
            # whether another terminal is live. On an otherwise empty runner,
            # rejecting and closing pts/0 just lets the kernel offer it again.
            master, slave = os.openpty()
            self.pty_path = os.ttyname(slave)
            tty.setraw(slave)
            fcntl.fcntl(master, fcntl.F_SETFL,
                        fcntl.fcntl(master, fcntl.F_GETFL) | os.O_NONBLOCK)
            self.pty_master = master
            self.pty_proc = subprocess.Popen(
                ["/bin/sh", "-c", "exec sleep 3600"], stdin=subprocess.DEVNULL,
                stdout=slave, stderr=subprocess.DEVNULL)
            os.close(slave)
        return self.pty_proc.pid

    def file_pid(self):
        if self.file_proc is None:
            self.file_path = os.path.join(self.tmpdir, "fd1-is-a-file")
            fh = open(self.file_path, "wb")
            self.file_proc = subprocess.Popen(
                ["/bin/sh", "-c", "exec sleep 3600"], stdin=subprocess.DEVNULL,
                stdout=fh, stderr=subprocess.DEVNULL)
            fh.close()
        return self.file_proc.pid

    def drain_pty(self):
        if self.pty_master is None:
            return b""
        chunks = []
        while True:
            r, _, _ = select.select([self.pty_master], [], [], 0.05)
            if not r:
                break
            try:
                data = os.read(self.pty_master, 65536)
            except OSError as e:
                if e.errno in (errno.EAGAIN, errno.EIO):
                    break
                raise
            if not data:
                break
            chunks.append(data)
        return b"".join(chunks)

    def close(self):
        for p in (self.pty_proc, self.file_proc):
            if p is not None:
                try:
                    p.send_signal(signal.SIGKILL)
                    p.wait(timeout=5)
                except Exception:
                    pass
        if self.pty_master is not None:
            try:
                os.close(self.pty_master)
            except OSError:
                pass


def run(case, cmd, fix, helpers=None):
    """Execute one case. cmd is the argv prefix, e.g. ['/bin/sh', '.../x.sh']."""
    host = short_host()
    mode = case.get("mode", "pipe")
    pids = {}
    if mode == "pty":
        pids[b"@@PID_PTY@@"] = helpers.pty_pid()
    elif mode == "pidfile":
        pids[b"@@PID_FILE@@"] = helpers.file_pid()

    env = {}
    for k, v in case["env"].items():
        env[_b(k)] = tokenize(v, fix, host, pids)

    cwd = case["cwd"]
    cwd_b = _b(cwd)
    if not cwd_b.startswith(b"/"):
        cwd_b = _b(fix) + b"/" + cwd_b if cwd_b != b"." else _b(fix)

    if "stdin_file" in case:
        with open(os.path.join(fix, case["stdin_file"]), "rb") as f:
            stdin = f.read()
    else:
        stdin = tokenize(case.get("stdin", ""), fix, host, pids)

    if mode == "pty":
        helpers.drain_pty()

    argv = [_b(a) for a in cmd] + [_b(a) for a in case["argv"]]
    proc = subprocess.Popen(argv, cwd=cwd_b, env=env, stdin=subprocess.PIPE,
                            stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                            close_fds=True)
    try:
        out, err = proc.communicate(stdin, timeout=TIMEOUT)
        rc = proc.returncode
    except subprocess.TimeoutExpired:
        proc.kill()
        out, err = proc.communicate()
        rc = "TIMEOUT"
    res = {"stdout": out, "stderr": err, "exit": rc}
    if mode == "pty":
        res["pty_out"] = helpers.drain_pty()
    return res


def expected(case, key, fix, host, pids):
    """The recorded expectation for stdout / stderr / pty_out, as real bytes."""
    if key + "_b64" in case:
        b = base64.b64decode(case[key + "_b64"])
    elif key in case:
        b = _b(case[key])
    else:
        return None
    return tokenize(b, fix, host, pids)


def load(path):
    out = []
    with open(path, "r", encoding="utf-8") as f:
        for line in f:
            line = line.strip()
            if line:
                out.append(json.loads(line))
    return out
