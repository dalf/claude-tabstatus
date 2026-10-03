#!/usr/bin/env python3
"""Persistence and concurrency guarantees supplemental to semantic traces.

CCTAB_TEST_BIN=/path/to/tabstatus python3 tests/test_state_guarantees.py
Payload size/draining is covered by test_payload.py; capacity and expiry belong
to the state contract fixtures. All data and subprocesses here are synthetic.
"""
from concurrent.futures import ThreadPoolExecutor
import fcntl
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import threading
import unittest

from corpus.runner import Helpers


ROOT = Path(__file__).resolve().parents[1]
BIN = Path(os.environ.get("CCTAB_TEST_BIN", ROOT / "bin/tabstatus")).resolve()
EVENTS = {"working": "PostToolUse", "waiting": "PermissionRequest", "idle": "Stop",
          "subagent-stop": "SubagentStop", "notify": "Notification",
          "elicitation": "Elicitation", "elicitation-result": "ElicitationResult"}
MISSING = object()


class StateGuaranteesTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory(prefix="cctab-state-guarantees-")
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name)
        self.state = self.root / "state"
        self.env = {"PATH": "/usr/bin:/bin", "HOME": str(self.root), "CLAUDE_PID": "0",
                    "CCTAB_STATE_DIR": str(self.state), "CCTAB_DRY_RUN": "1",
                    "CCTAB_NOW": "1000000", "CCTAB_TERMINAL": "other",
                    "CCTAB_GLYPH_POS": "prefix", "CCTAB_GLYPH_WORKING": "WORKING",
                    "CCTAB_GLYPH_WAITING": "WAITING", "CCTAB_GLYPH_IDLE": "IDLE"}

    def payload(self, edge, session, fields):
        payload = {"hook_event_name": EVENTS[edge], **fields}
        if session is not MISSING:
            payload["session_id"] = session
        return json.dumps(payload).encode()

    def output(self, returncode, stdout, stderr):
        self.assertEqual((returncode, stderr), (0, b""))
        return stdout.decode().split(" ", 1)[0].strip()

    def send(self, edge, session="s1", env=None, **fields):
        p = subprocess.run([str(BIN), edge], input=self.payload(edge, session, fields),
                           env=env or self.env, cwd=self.root, capture_output=True, timeout=5)
        return self.output(p.returncode, p.stdout, p.stderr)

    def record(self, session="s1"):
        return (self.state / session).read_bytes()

    def waits(self, session="s1"):
        lines = [line for line in self.record(session).decode().splitlines() if line.startswith("w ")]
        return dict(token.rsplit(":", 1) for token in lines[0].split()[1:]) if lines else {}

    def seed(self, session="s1"):
        self.assertEqual(self.send("working", session), "WORKING")
        self.assertEqual(self.send("waiting", session, agent_id="owner"), "WAITING")

    def test_missing_or_invalid_session_id_uses_stateless_semantics_without_creating_state(self):
        for session in (MISSING, None, "", "../escape", "a" * 65, "é", "agent:session"):
            with self.subTest(session=session):
                self.assertEqual(self.send("waiting", session, agent_id="owner"), "WAITING")
                # No record means the main thread cannot protect that owner's
                # wait; this is deliberately the documented stateless answer.
                self.assertEqual(self.send("working", session), "WORKING")
                self.assertEqual(self.send("idle", session), "IDLE")
                self.assertEqual(self.send("working", session, agent_id="owner"), "")
                self.assertEqual(self.send("subagent-stop", session, agent_id="owner"), "")
                self.assertFalse(self.state.exists())
        self.assertEqual(list(self.root.iterdir()), [])

    def test_absent_or_uncreatable_state_directory_has_explicit_stateless_fallback(self):
        blocker = self.root / "file"
        blocker.write_bytes(b"preserve")
        for configured in (None, str(blocker / "state")):
            with self.subTest(state_directory=configured):
                env = dict(self.env)
                if configured is None:
                    env.pop("CCTAB_STATE_DIR")
                else:
                    env["CCTAB_STATE_DIR"] = configured
                self.assertEqual(self.send("waiting", env=env, agent_id="owner"), "WAITING")
                self.assertEqual(self.send("working", env=env), "WORKING")
                self.assertEqual(self.send("idle", env=env), "IDLE")
                self.assertEqual(self.send("subagent-stop", env=env, agent_id="owner"), "")
                self.assertEqual(blocker.read_bytes(), b"preserve")
                self.assertFalse(self.state.exists())

    def test_ordinary_updates_preserve_records_the_lock_refuses(self):
        self.state.mkdir()
        future = self.state / "future"
        future.write_bytes(b"cts9\nb w\nw owner:1000000\n")
        oversized = self.state / "oversized"
        oversized.write_bytes(b"cts3\nb w\nw owner:1000000\n" + b"x" * 8193)
        target = self.root / "link-target"
        target.write_bytes(b"cts3\nb w\nw owner:1000000\n")
        link = self.state / "link"
        link.symlink_to(target)
        for record in (future, oversized, link):
            with self.subTest(record=record.name):
                before = record.read_bytes()
                inode = record.lstat().st_ino
                for edge, fields in (("working", {}), ("working", {"agent_id": "owner"}),
                                     ("idle", {"background_tasks": []}),
                                     ("subagent-stop", {"agent_id": "owner"})):
                    self.assertEqual(self.send(edge, record.name, **fields), "")
                    self.assertEqual(record.read_bytes(), before)
                    self.assertEqual(record.lstat().st_ino, inode)
                self.assertEqual(self.send("waiting", record.name, agent_id="new-owner"), "WAITING")
                self.assertEqual(record.read_bytes(), before)
                self.assertEqual(record.lstat().st_ino, inode)
        self.assertTrue(link.is_symlink())
        self.assertEqual(set(p.name for p in self.state.iterdir()), {"future", "oversized", "link"})
        # SessionEnd is intentionally excluded: it is quiescent path cleanup,
        # not an ordinary locked update and has different documented guarantees.

    @unittest.skipIf(os.geteuid() == 0, "root bypasses the filesystem permission failures under test")
    def test_permission_failures_do_not_allow_unlocked_writes_or_false_completion(self):
        self.seed()
        record = self.state / "s1"
        before = record.read_bytes()
        inode = record.stat().st_ino
        record.chmod(0o400)
        try:
            # The directory stays writable: a missing lock check would still
            # permit temp-file replacement of this read-only record.
            for edge, fields in (("working", {"agent_id": "owner"}), ("working", {}),
                                 ("subagent-stop", {"agent_id": "owner"}),
                                 ("idle", {"background_tasks": []})):
                self.assertEqual(self.send(edge, **fields), "")
                self.assertEqual(record.read_bytes(), before)
                self.assertEqual(record.stat().st_ino, inode)
            self.assertEqual(self.send("waiting", agent_id="new-owner"), "WAITING")
            self.assertEqual(record.read_bytes(), before)
        finally:
            record.chmod(0o600)
        self.state.chmod(0o500)
        try:
            # Existing-but-unwritable state is not Session::open's stateless
            # fallback. Requests may paint; a failed main update stays silent.
            self.assertEqual(self.send("working", "new"), "")
            self.assertEqual(self.send("waiting", "new", agent_id="owner"), "WAITING")
            self.assertEqual(self.send("idle", "new"), "IDLE")
            self.assertFalse((self.state / "new").exists())
        finally:
            self.state.chmod(0o700)

    def parallel(self, jobs, session):
        barrier = threading.Barrier(len(jobs))
        def execute(job):
            edge, fields = job
            barrier.wait(timeout=5)
            return self.send(edge, session, **fields)
        with ThreadPoolExecutor(max_workers=len(jobs)) as pool:
            return list(pool.map(execute, jobs))

    @staticmethod
    def cleanup_process(process):
        if process.poll() is None:
            process.kill()
        process.communicate(timeout=5)

    def test_commutative_permission_and_mcp_updates_preserve_every_owner(self):
        for number in range(5):
            session = "mixed" + str(number)
            self.assertEqual(self.send("working", session), "WORKING")
            permissions = [("waiting", {"agent_id": "owner" + str(i)}) for i in range(3)]
            elicitations = [("elicitation", {"mcp_server_name": "server", "elicitation_id": str(i)})
                           for i in range(3)]
            self.assertEqual(self.parallel(permissions + elicitations, session), ["WAITING"] * 6)
            pending = self.waits(session)
            self.assertEqual(len(pending), 6)
            self.assertTrue({"owner0", "owner1", "owner2"}.issubset(pending))
            self.assertTrue(all(epoch == "1000000" for epoch in pending.values()))
            completions = [("working" if i % 2 else "subagent-stop", {"agent_id": "owner" + str(i)})
                           for i in range(3)]
            completions += [("elicitation-result", {"mcp_server_name": "server", "elicitation_id": str(i),
                                                    "action": "accept"}) for i in range(3)]
            outputs = self.parallel(completions, session)
            self.assertEqual(outputs.count("WORKING"), 1, (outputs, self.record(session)))
            self.assertEqual(outputs.count(""), 5)
            self.assertEqual(self.waits(session), {})
            self.assertIn(b"\nb w\n", self.record(session))

    def test_session_lock_blocks_its_own_completion_without_blocking_another_session(self):
        self.seed("locked")
        self.seed("independent")
        before = self.record("locked")
        process = None
        with (self.state / "locked").open("rb") as held:
            fcntl.flock(held, fcntl.LOCK_EX)
            try:
                with tempfile.TemporaryFile() as payload:
                    payload.write(self.payload("working", "locked", {"agent_id": "owner"}))
                    payload.seek(0)
                    process = subprocess.Popen([str(BIN), "working"], stdin=payload, env=self.env,
                                               cwd=self.root, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
                    self.addCleanup(self.cleanup_process, process)
                # The same owner name in a different session is independent,
                # including its lock and final overlay removal.
                self.assertEqual(self.send("working", "independent", agent_id="owner"), "WORKING")
                self.assertEqual(self.waits("independent"), {})
                # Give the child a scheduling opportunity while the lock is
                # still held; an immediate poll could pass before it even ran.
                with self.assertRaises(subprocess.TimeoutExpired):
                    process.wait(timeout=0.15)
                self.assertEqual(self.record("locked"), before)
            finally:
                fcntl.flock(held, fcntl.LOCK_UN)
        try:
            stdout, stderr = process.communicate(timeout=5)
            self.assertEqual(self.output(process.returncode, stdout, stderr), "WORKING")
            self.assertEqual(self.waits("locked"), {})
        finally:
            if process is not None and process.poll() is None:
                process.kill()
                process.communicate(timeout=5)


class ArmingDeliveryTests(unittest.TestCase):
    """The s line is a restore obligation even when startup delivers no bytes."""

    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory(prefix="cctab-arming-")
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name)
        self.helpers = Helpers(str(self.root))
        self.addCleanup(self.helpers.close)
        self.env = {"PATH": "/usr/bin:/bin", "HOME": str(self.root),
                    "CLAUDE_CONFIG_DIR": str(self.root / "config"),
                    "XDG_DATA_HOME": str(self.root / "data"),
                    "CCTAB_STATE_DIR": str(self.root / "state"),
                    "CCTAB_TERMINAL": "konsole"}
        self.record = self.root / "state" / "s1"

    def hook(self, edge, env, prefix=()):
        result = subprocess.run([*prefix, str(BIN), edge], env=env, cwd=self.root,
                                input=b'{"session_id":"s1","source":"startup"}',
                                capture_output=True, timeout=10)
        self.assertEqual((result.returncode, result.stdout, result.stderr), (0, b"", b""))

    def test_skipped_and_headless_starts_keep_a_conservative_obligation(self):
        for pid in (None, "0", str(self.helpers.file_pid())):
            with self.subTest(pid=pid):
                env = dict(self.env)
                if pid is not None:
                    env["CLAUDE_PID"] = pid
                self.hook("session-start", env)
                self.assertIn(b"s konsole\n", self.record.read_bytes())
                self.assertEqual(Path(self.helpers.file_path).read_bytes(), b"")
                self.hook("session-end", dict(env, CCTAB_TERMINAL="wezterm"))
                self.assertFalse(self.record.exists())

    def assert_restore(self, env):
        self.hook("session-end", dict(env, CCTAB_TERMINAL="wezterm"))
        self.assertEqual(self.helpers.drain_pty(),
                         b"\x1b]50;LocalTabTitleFormat=%d : %n;RemoteTabTitleFormat=(%u) %H\x07"
                         b"\x1b]0;\x07")
        self.assertFalse(self.record.exists())

    def test_completed_start_records_obligation_and_restores_remembered_surface(self):
        env = dict(self.env, CLAUDE_PID=str(self.helpers.pty_pid()))
        self.hook("session-start", env)
        data = self.helpers.drain_pty()
        self.assertTrue(data.startswith(b"\x1b]50;LocalTabTitleFormat=%w;RemoteTabTitleFormat=%w\x07"
                                        b"\x1b]0;"), data)
        self.assertIn(b"s konsole\n", self.record.read_bytes())
        self.assert_restore(env)

    @unittest.skipUnless(shutil.which("strace"), "strace required for write-failure injection")
    def test_failed_direct_start_keeps_obligation_and_still_restores(self):
        env = dict(self.env, CLAUDE_PID=str(self.helpers.pty_pid()))
        trace = self.root / "trace"
        # Only writes to our allocated pty are faulted; record I/O is unaffected.
        self.hook("session-start", env,
                  ("strace", "-qq", "-yy", "-o", str(trace), "-e", "trace=write",
                   "-e", "inject=write:error=EIO", "-P", self.helpers.pty_path))
        self.assertIn("(INJECTED)", trace.read_text())
        self.assertEqual(self.helpers.drain_pty(), b"")
        self.assertIn(b"s konsole\n", self.record.read_bytes())
        self.assert_restore(env)


if __name__ == "__main__":
    unittest.main()
