#!/usr/bin/env python3
"""Native Linux/macOS delivery contract, independent of the Linux golden corpus.

Only disposable processes and newly allocated PTYs are targets. A stand-in owns
the PTY as its controlling terminal even when stdout is redirected, so falling
back to the controlling terminal would fail the headless assertions.
"""
import errno
import json
import os
from pathlib import Path
import select
import subprocess
import sys
import tempfile
import tty
import unittest


ROOT = Path(__file__).resolve().parents[1]
BIN = Path(os.environ.get("CCTAB_TEST_BIN", ROOT / "bin/tabstatus")).resolve()
TITLE = b"\x1b]0;IDLE ~/project\x07"
CLEAR = b"\x1b]0;\x07"
ARM = b"\x1b]50;LocalTabTitleFormat=%w;RemoteTabTitleFormat=%w\x07"
RESTORE = b"\x1b]50;LocalTabTitleFormat=%d : %n;RemoteTabTitleFormat=(%u) %H\x07"
STAND_IN = """
import fcntl, os, sys, termios
slave = int(sys.argv[1])
fcntl.ioctl(slave, termios.TIOCSCTTY, 0)
assert os.tcgetpgrp(slave) == os.getpgrp()
os.close(slave)
os.write(2, b'ready\\n')
sys.stdin.buffer.read()
"""


class UnixDeliveryTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory(prefix="cctab-unix-delivery-")
        self.addCleanup(self.tmp.cleanup)
        # macOS /var is a symlink; use the same spelling as getcwd for HOME.
        self.root = Path(self.tmp.name).resolve()
        self.cwd = self.root / "project"
        self.cwd.mkdir()
        self.env = {"PATH": "/usr/bin:/bin", "HOME": str(self.root),
                    "CLAUDE_CONFIG_DIR": str(self.root / "config"),
                    "XDG_DATA_HOME": str(self.root / "data"),
                    "CCTAB_STATE_DIR": str(self.root / "state"),
                    "CCTAB_TERMINAL": "apple-terminal", "CCTAB_GLYPH_IDLE": "IDLE",
                    "CCTAB_GLYPH_POS": "prefix"}
        self.record = self.root / "state" / "s1"

    @staticmethod
    def stop(process):
        if process.poll() is None:
            process.kill()
        process.communicate(timeout=5)

    def stand_in(self, mode="pty"):
        # A fresh pair for each session: do not rely on a controlling terminal
        # remaining usable after its previous session leader exits on either OS.
        self.master, self.slave = os.openpty()
        self.addCleanup(os.close, self.master)
        self.addCleanup(os.close, self.slave)
        tty.setraw(self.slave)
        output = self.slave
        if mode == "file":
            output = (self.root / "redirected").open("wb")
            self.addCleanup(output.close)
        elif mode == "null":
            output = subprocess.DEVNULL
        elif mode == "pipe":
            output = subprocess.PIPE
        process = subprocess.Popen(
            [sys.executable, "-c", STAND_IN, str(self.slave)],
            stdin=subprocess.PIPE, stdout=output, stderr=subprocess.PIPE,
            pass_fds=(self.slave,), start_new_session=True, env=self.env,
            cwd=self.cwd)
        self.addCleanup(self.stop, process)
        ready, _, _ = select.select([process.stderr], [], [], 5)
        self.assertTrue(ready, "stand-in did not acquire its disposable PTY")
        self.assertEqual(process.stderr.readline(), b"ready\n")
        self.assertIsNone(process.poll())
        return process

    def drain(self):
        chunks = []
        while select.select([self.master], [], [], 0.05)[0]:
            try:
                data = os.read(self.master, 65536)
            except OSError as error:
                if error.errno == errno.EIO:
                    break
                raise
            if not data:
                break
            chunks.append(data)
        return b"".join(chunks)

    def hook(self, edge, env, source="startup"):
        result = subprocess.run(
            [str(BIN), edge], cwd=self.cwd, env=env,
            input=json.dumps({"session_id": "s1", "source": source}).encode(),
            capture_output=True, timeout=10)
        self.assertEqual((result.returncode, result.stderr), (0, b""))
        if edge in ("session-start", "session-end"):
            self.assertEqual(result.stdout, b"", "direct delivery must not emit protocol JSON")
        return result.stdout

    def test_start_and_end_deliver_exact_title_bytes(self):
        process = self.stand_in()
        for surface in ("apple-terminal", "iterm2", "other", "konsole"):
            with self.subTest(surface=surface):
                env = dict(self.env, CLAUDE_PID=str(process.pid), CCTAB_TERMINAL=surface)
                self.hook("session-start", env)
                self.assertEqual(self.drain(), (ARM if surface == "konsole" else b"") + TITLE)
                data = self.record.read_bytes()
                key = b"r" if sys.platform == "darwin" else b"p"
                self.assertRegex(data, rb"\n" + key + b" " + str(process.pid).encode() + rb" [1-9][0-9]*\n")
                # The ordinary hook's protocol payload agrees with direct delivery.
                protocol = json.loads(self.hook("idle", env))
                self.assertEqual(protocol["terminalSequence"].encode(), TITLE)
                self.assertEqual(self.drain(), b"")
                # A changed surface must still honour a remembered Konsole restore.
                self.hook("session-end", dict(env, CCTAB_TERMINAL="apple-terminal"))
                self.assertEqual(self.drain(), (RESTORE if surface == "konsole" else b"") + CLEAR)
                self.assertFalse(self.record.exists())

    def test_compaction_and_dry_run_never_write_to_the_pty(self):
        process = self.stand_in()
        env = dict(self.env, CLAUDE_PID=str(process.pid))
        self.hook("session-start", env, source="compact")
        self.assertEqual(self.drain(), b"")
        self.assertFalse(self.record.exists())
        for edge in ("session-start", "session-end"):
            result = subprocess.run([str(BIN), edge], cwd=self.cwd,
                                    env=dict(env, CCTAB_DRY_RUN="1"), input=b"{}",
                                    capture_output=True, timeout=10)
            self.assertEqual((result.returncode, result.stderr), (0, b""))
            self.assertEqual(result.stdout, b"IDLE ~/project\n" if edge == "session-start" else b"\n")
            self.assertEqual(self.drain(), b"")

    def test_missing_malformed_and_exited_pid_never_write(self):
        process = self.stand_in()
        for pid in (None, "", "0", "not-a-pid", "4294967295", "0" + str(process.pid)):
            with self.subTest(pid=pid):
                env = dict(self.env, CCTAB_TERMINAL="konsole")
                if pid is not None:
                    env["CLAUDE_PID"] = pid
                for edge in ("session-start", "session-end"):
                    self.hook(edge, env)
                    self.assertEqual(self.drain(), b"")
        self.stop(process)
        for edge in ("session-start", "session-end"):
            self.hook(edge, dict(self.env, CLAUDE_PID=str(process.pid)))
            self.assertEqual(self.drain(), b"")

    def test_redirected_stdout_never_falls_back_to_controlling_terminal(self):
        for mode in ("file", "pipe", "null"):
            with self.subTest(mode=mode):
                process = self.stand_in(mode)
                env = dict(self.env, CLAUDE_PID=str(process.pid), CCTAB_TERMINAL="konsole")
                for edge in ("session-start", "session-end"):
                    self.hook(edge, env)
                    self.assertEqual(self.drain(), b"", "the controlling terminal is not stdout")
                    if edge == "session-start":
                        self.assertIn(b"s konsole\n", self.record.read_bytes())
                    else:
                        self.assertFalse(self.record.exists())
                # EOF releases the stand-in; inspect the redirected output too.
                out, err = process.communicate(timeout=5)
                self.assertEqual((process.returncode, err), (0, b""))
                if mode == "pipe":
                    self.assertEqual(out, b"")
                if mode == "file":
                    self.assertEqual((self.root / "redirected").read_bytes(), b"")


if __name__ == "__main__":
    unittest.main()
