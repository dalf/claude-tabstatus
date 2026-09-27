#!/usr/bin/env python3
"""End-to-end hook input policy and state-preservation regressions.

CCTAB_TEST_BIN=/path/to/tabstatus python3 tests/test_payload.py
Every invocation uses an isolated HOME/state directory and dry-run output.
"""
import json
import os
from pathlib import Path
import subprocess
import tempfile
import threading
import unittest

BIN = Path(os.environ.get("CCTAB_TEST_BIN", Path(__file__).resolve().parents[1] / "bin/tabstatus")).resolve()
LIMIT = 16 * 1024 * 1024


class HookInputTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory(prefix="cctab-payload-")
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name)
        self.state = self.root / "state"
        self.env = {"PATH": "/usr/bin:/bin", "HOME": str(self.root),
                    "CCTAB_DRY_RUN": "1", "CCTAB_NOW": "1000000",
                    "CCTAB_STATE_DIR": str(self.state), "CLAUDE_PID": "0"}

    def run_hook(self, edge, data, stateful=True):
        env = dict(self.env)
        if not stateful:
            env.pop("CCTAB_STATE_DIR")
        if isinstance(data, dict):
            data = json.dumps(data).encode()
        elif isinstance(data, str):
            data = data.encode()
        p = subprocess.run([str(BIN), edge], input=data, env=env, cwd=self.root,
                           capture_output=True, timeout=10)
        self.assertEqual(p.returncode, 0, p.stderr)
        self.assertEqual(p.stderr, b"")
        return p.stdout

    def snapshot(self):
        if not self.state.exists():
            return None
        return {str(p.relative_to(self.state)): (p.read_bytes(), p.stat().st_ino,
                                                p.stat().st_mtime_ns)
                for p in self.state.rglob("*") if p.is_file()}

    def wait(self):
        self.assertTrue(self.run_hook("working", {"session_id": "s1"}))
        self.assertTrue(self.run_hook("waiting", {"session_id": "s1", "agent_id": "a1"}))
        self.assertTrue((self.state / "s1").is_file())

    def test_rejected_documents_do_not_paint_create_lock_reap_or_modify_records(self):
        bad = [b" ", b"\n", b"[]", b"null", b'{"session_id":"s1"',
               b'{"session_id":"s1"} trailing', b'{"session_id":"s1"}\n{}',
               b'{"session_id":"s1","agent_id":42}',
               b'{"session_id":"s1","agent_id":"a1","agent\\u005fid":null}',
               b'{"session_id":"s1","background_tasks":{}}',
               b'{"session_id":"s1","tool_response":"\xff"}',
               b'{"session_id":"s1","tool_response":"a\0b"}',
               b'{"session_id":"s1"}' + b' ' * LIMIT]
        # A SessionStart normally reaps old records; malformed input must never
        # reach even that preparatory side effect.
        self.wait()
        stale = self.state / "stale"
        stale.write_bytes(b"cts1\nb i\n")
        os.utime(stale, (1, 1))
        before = self.snapshot()
        for edge in ["working", "waiting", "idle", "notify", "session-start", "session-end",
                     "subagent-stop", "elicitation", "elicitation-result", "unknown"]:
            for data in bad:
                with self.subTest(edge=edge, preview=data[:100]):
                    self.assertEqual(self.run_hook(edge, data), b"")
                    self.assertEqual(self.snapshot(), before)
        # A fresh directory must not be created either.
        self.state = self.root / "never-created"
        self.env["CCTAB_STATE_DIR"] = str(self.state)
        self.assertEqual(self.run_hook("waiting", bad[3]), b"")
        self.assertFalse(self.state.exists())

    def test_nested_metadata_cannot_clear_another_owners_wait(self):
        self.wait()
        before = self.snapshot()
        nested = {"agent_id": "a1", "session_id": "other", "source": "startup",
                  "notification_type": "idle_prompt", "hook_event_name": "UserPromptSubmit",
                  "prompt": "answer", "background_tasks": []}
        self.assertEqual(self.run_hook("working", {"session_id": "s1", "tool_response": nested}), b"")
        self.assertEqual(self.snapshot(), before)
        self.assertEqual(self.run_hook("idle", {"session_id": "s1", "tool_response": nested}), b"")
        # idle changes the base only if necessary; the wait stays owned by a1.
        self.assertIn(b"w a1:", (self.state / "s1").read_bytes())
        self.assertEqual(self.run_hook("notify", {"session_id": "s1", "tool_response": nested}), b"")
        self.assertIn(b"w a1:", (self.state / "s1").read_bytes())

    def test_late_escaped_owner_resolves_the_same_record(self):
        self.wait()
        data = ('{\n"tool_response":"' + 'x' * 40000 + '",'
                '"agent\\u005fid" : "a\\u0031", "session_id" : "s\\u0031"\n}')
        self.assertTrue(self.run_hook("working", data))
        self.assertNotIn(b"w a1:", (self.state / "s1").read_bytes())

    def test_stateless_nested_ids_and_whitespace_are_structural(self):
        self.assertTrue(self.run_hook("working", {"tool_response": {"agent_id": "a"}}, False))
        self.assertEqual(self.run_hook("working", {"agent_id": "a"}, False), b"")
        self.assertTrue(self.run_hook("notify", '{\n"notification_type" : "permission_prompt"\n}', False))
        self.assertEqual(self.run_hook("session-start", '{\n"source" : "compact"\n}', False), b"")

    def test_manual_empty_input_and_stateless_drain_path_are_preserved(self):
        for edge in ["working", "waiting", "idle", "session-start", "unknown"]:
            self.assertTrue(self.run_hook(edge, b"", False))
        for edge in ["waiting", "idle", "unknown"]:
            self.assertTrue(self.run_hook(edge, b"not json\n", False))
        self.assertEqual(self.run_hook("working", b"not json\n", False), b"")

    def test_oversize_input_is_drained_without_broken_pipe(self):
        # Write directly, unlike communicate(), which intentionally suppresses
        # BrokenPipeError and could hide a regression in the drain requirement.
        p = subprocess.Popen([str(BIN), "working"], stdin=subprocess.PIPE,
                             stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                             env=self.env, cwd=self.root)
        errors = []

        def write_payload():
            try:
                p.stdin.write(b'{"tool_response":"')
                for _ in range(20):
                    p.stdin.write(b"x" * (1024 * 1024))
                p.stdin.write(b'"}')
            except Exception as exc:
                errors.append(exc)
            finally:
                try:
                    p.stdin.close()
                except Exception as exc:
                    errors.append(exc)

        # The deadline must also cover a writer blocked on a child that stops
        # reading. Keep writes explicit so BrokenPipeError remains observable.
        writer = threading.Thread(target=write_payload, daemon=True)
        writer.start()
        try:
            p.wait(timeout=10)
            out, err = p.stdout.read(), p.stderr.read()
        finally:
            if p.poll() is None:
                p.kill()
            p.wait(timeout=5)
            writer.join(timeout=5)
            p.stdout.close()
            p.stderr.close()
        self.assertFalse(writer.is_alive(), "payload writer did not finish")
        self.assertEqual(errors, [], "payload writer failed before draining")
        self.assertEqual((p.returncode, out, err), (0, b"", b""))
        self.assertFalse(self.state.exists())


if __name__ == "__main__":
    unittest.main()
