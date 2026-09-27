#!/usr/bin/env python3
"""Isolated direct MCP hook sequence tests; no live MCP server is required.

CCTAB_TEST_BIN=/path/to/tabstatus python3 tests/test_elicitation.py
The versioned fixture describes synthetic contract scenarios, not captured traffic.
"""
from concurrent.futures import ThreadPoolExecutor
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[1]
BIN = Path(os.environ.get("CCTAB_TEST_BIN", ROOT / "bin/tabstatus")).resolve()
EVENTS = {"elicitation": "Elicitation", "elicitation-result": "ElicitationResult",
          "working": "PostToolUse", "waiting": "PermissionRequest",
          "idle": "Stop", "notify": "Notification", "subagent-stop": "SubagentStop",
          "session-start": "SessionStart", "session-end": "SessionEnd"}


class ElicitationTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory(prefix="cctab-elicitation-")
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name)
        self.state = self.root / "state"
        self.env = {"PATH": "/usr/bin:/bin", "HOME": str(self.root), "CLAUDE_PID": "0",
                    "CCTAB_STATE_DIR": str(self.state), "CCTAB_DRY_RUN": "1",
                    "CCTAB_NOW": "1000000", "CCTAB_TERMINAL": "other",
                    "CCTAB_GLYPH_POS": "prefix", "CCTAB_GLYPH_WORKING": "WORKING",
                    "CCTAB_GLYPH_WAITING": "WAITING", "CCTAB_GLYPH_IDLE": "IDLE"}

    def invoke(self, edge, data, env=None):
        if isinstance(data, dict):
            data = json.dumps(data).encode()
        elif isinstance(data, str):
            data = data.encode()
        p = subprocess.run([str(BIN), edge], input=data, env=env or self.env,
                           cwd=self.root, capture_output=True, timeout=10)
        self.assertEqual((p.returncode, p.stderr), (0, b""))
        return p.stdout

    def send(self, edge, session="s1", **fields):
        payload = {"session_id": session, "hook_event_name": EVENTS[edge], **fields}
        return self.invoke(edge, payload).decode().split(" ", 1)[0].strip()

    def start(self, request="a", server="mcp", session="s1", **fields):
        return self.send("elicitation", session=session, mcp_server_name=server,
                         elicitation_id=request, **fields)

    def finish(self, request="a", server="mcp", session="s1", action="accept", **fields):
        return self.send("elicitation-result", session=session, mcp_server_name=server,
                         elicitation_id=request, action=action, **fields)

    def record(self, session="s1"):
        p = self.state / session
        return p.read_bytes() if p.is_file() else b""

    def begin(self, session="s1"):
        self.assertEqual(self.send("working", session=session,
                                   hook_event_name="UserPromptSubmit", prompt="go"), "WORKING")

    def test_multiple_requests_restore_base_for_each_action_and_mode(self):
        for mode in ("form", "url"):
            for action in ("accept", "decline", "cancel"):
                with self.subTest(mode=mode, action=action):
                    session = mode + action
                    self.begin(session)
                    self.assertEqual(self.start("a", session=session, mode=mode), "WAITING")
                    self.assertEqual(self.start("b", session=session, mode=mode), "WAITING")
                    self.assertEqual(self.finish("a", session=session, action=action, mode=mode), "")
                    self.assertEqual(self.finish("b", session=session, action=action, mode=mode), "WORKING")

    def test_identity_is_scoped_by_session_and_server_without_lossy_encoding(self):
        for session in ("s1", "s2"):
            self.begin(session)
            self.start("same:id", "server one/é", session)
            self.start("same:id", "server-two", session)
        untouched = self.record("s2")
        self.assertEqual(self.finish("same:id", "server one/é"), "")
        self.assertEqual(self.record("s2"), untouched)
        self.assertEqual(self.finish("same:id", "server-two"), "WORKING")
        self.assertEqual(self.record("s2"), untouched)
        self.assertEqual(self.finish("same:id", "server one/é", "s2"), "")
        self.assertEqual(self.finish("same:id", "server-two", "s2"), "WORKING")

    def test_permission_and_elicitation_results_retire_only_their_own_wait(self):
        self.begin()
        self.send("waiting", agent_id="owner")
        self.start()
        self.assertEqual(self.finish(), "")
        self.assertIn(b"owner:", self.record())
        self.assertEqual(self.send("working", agent_id="unrelated"), "")
        self.assertEqual(self.send("subagent-stop", agent_id="unrelated"), "")
        self.assertEqual(self.send("working", agent_id="owner"), "WORKING")

    def test_main_progress_and_quiet_stop_preserve_direct_wait_and_update_base(self):
        self.begin()
        self.start()
        self.assertEqual(self.send("working"), "")
        self.assertEqual(self.send("idle", background_tasks=[]), "")
        self.assertEqual(self.finish(), "IDLE")

    def test_duplicates_and_result_before_start_do_not_resurrect_requests(self):
        self.begin()
        self.start()
        pending = self.record()
        self.env["CCTAB_NOW"] = "1000001"
        self.start()
        self.assertEqual(self.record(), pending, "duplicate start must not refresh the wait")
        self.assertEqual(self.finish(), "WORKING")
        completed = self.record()
        self.assertEqual(self.finish(), "")
        self.assertEqual(self.start(), "")
        self.assertEqual(self.record(), completed)
        self.assertEqual(self.finish("out-of-order"), "")
        self.assertEqual(self.start("out-of-order"), "")
        self.env["CCTAB_NOW"] = "1000902"
        self.assertEqual(self.start("out-of-order"), "WAITING")

    def test_keyed_notifications_coalesce_in_either_order_and_after_result(self):
        for kind in ("elicitation_dialog", "elicitation_url_dialog"):
            for first in ("notification", "direct"):
                session = kind + first
                self.begin(session)
                def notification():
                    return self.send("notify", session=session, notification_type=kind,
                                     mcp_server_name="mcp", elicitation_id="a")
                if first == "notification":
                    notification()
                self.start(session=session)
                notification()
                self.assertEqual(self.finish(session=session), "WORKING")
                self.assertEqual(notification(), "")

    def test_unkeyed_notification_is_independent_in_either_order(self):
        for first in ("notification", "direct"):
            self.begin(first)
            if first == "notification":
                self.send("notify", session=first, notification_type="elicitation_dialog")
            self.start(session=first)
            self.send("notify", session=first, notification_type="elicitation_dialog")
            self.assertEqual(self.finish(session=first), "")
            self.assertEqual(self.send("working", session=first, agent_id="unrelated"), "")
            self.assertEqual(self.send("working", session=first), "WORKING")
            # An unkeyed delayed notification cannot be identified as a duplicate;
            # it raises a recoverable wait instead of suppressing a new request.
            self.assertEqual(self.send("notify", session=first,
                                       notification_type="elicitation_dialog"), "WAITING")
            self.assertEqual(self.send("idle", session=first, background_tasks=[]), "IDLE")

    def test_anonymous_direct_wait_requires_prompt_expiry_or_reset(self):
        self.begin()
        self.assertEqual(self.send("elicitation", mcp_server_name="mcp", mode="form"), "WAITING")
        for payload in ({"mcp_server_name": "mcp"}, {"elicitation_id": "a"}, {}):
            self.assertEqual(self.send("elicitation-result", action="accept", **payload), "")
        self.assertEqual(self.send("working"), "")
        self.assertEqual(self.send("idle", background_tasks=[]), "")
        self.assertEqual(self.finish(), "")
        self.assertEqual(self.send("working", hook_event_name="UserPromptSubmit",
                                   prompt="<task-notification>done"), "")
        self.assertEqual(self.send("working", hook_event_name="UserPromptSubmit", prompt="continue"), "WORKING")

    def test_unusable_identity_never_prefix_matches_an_existing_request(self):
        self.begin()
        self.start("a" * 64, "s" * 64)
        before = self.record()
        for request, server in [("a" * 65, "s" * 64), ("a" * 64, "s" * 65),
                                ("", "s" * 64), (None, "s" * 64),
                                ("a\n", "s" * 64), ("a" * 64, "s\x00"),
                                ("é" * 33, "s" * 64)]:
            with self.subTest(request=request, server=server):
                self.assertEqual(self.finish(request, server), "")
                self.assertEqual(self.record(), before)
        self.assertEqual(self.finish("a" * 64, "s" * 64), "WORKING")

    def test_invalid_actions_and_wrong_types_cannot_resolve_waits(self):
        self.begin()
        self.start()
        before = self.record()
        for action in (None, "", "approve", "ACCEPT", 1, {}, []):
            self.assertEqual(self.finish(action=action), "")
            self.assertEqual(self.record(), before)
        for field in ("mcp_server_name", "elicitation_id", "mode"):
            for value in (42, [], {}):
                payload = {"session_id": "s1", "hook_event_name": "ElicitationResult",
                           "mcp_server_name": "mcp", "elicitation_id": "a", "action": "accept",
                           field: value}
                self.assertEqual(self.invoke("elicitation-result", payload), b"")
                self.assertEqual(self.record(), before)
        self.assertEqual(self.finish(), "WORKING")

    def test_invalid_identifier_strings_create_only_anonymous_waits(self):
        for index, value in enumerate(("", None, "x" * 65, "x\n", "é" * 33)):
            session = "invalid" + str(index)
            self.begin(session)
            self.assertEqual(self.start(value, session=session), "WAITING")
            self.assertEqual(self.finish(value, session=session), "")
            self.assertEqual(self.send("idle", session=session, background_tasks=[]), "")
            self.assertEqual(self.send("working", session=session,
                                       hook_event_name="UserPromptSubmit", prompt="continue"), "WORKING")

    def test_unsupported_mode_is_silent_for_start_and_result(self):
        self.begin()
        self.start()
        before = self.record()
        for mode in ("", "unknown", "URL"):
            self.assertEqual(self.start("other", mode=mode), "")
            self.assertEqual(self.finish(mode=mode), "")
            self.assertEqual(self.record(), before)

    def test_malformed_input_is_silent_and_does_not_change_state(self):
        self.begin()
        self.start()
        before = self.record()
        for edge in ("elicitation", "elicitation-result"):
            for data in (b"null", b"[]", b"{", b"{} trailing", b'{"elicitation_id":42}',
                         b'{"elicitation_id":"a","elicitation\\u005fid":"b"}'):
                self.assertEqual(self.invoke(edge, data), b"")
                self.assertEqual(self.record(), before)

    def test_only_top_level_decoded_identity_can_match_a_result(self):
        self.begin()
        self.start()
        before = self.record()
        nested = {"mcp_server_name": "mcp", "elicitation_id": "a", "action": "accept"}
        self.assertEqual(self.send("elicitation-result", content=nested), "")
        self.assertEqual(self.record(), before)
        payload = ('{"session_id":"s1","hook_event_name":"ElicitationResult",'
                   '"mcp_server_name":"m\\u0063p","elicitation\\u005fid":"\\u0061",'
                   '"action":"accept"}')
        self.assertTrue(self.invoke("elicitation-result", payload).startswith(b"WORKING "))

    def test_hooks_emit_only_terminal_updates_and_never_retain_sensitive_fields(self):
        env = dict(self.env, CCTAB_DRY_RUN="0")
        secret = "unique-secret-form-answer-8e35"
        common = {"session_id": "s1", "mcp_server_name": "mcp", "elicitation_id": "safe-id",
                  "mode": "url", "message": secret, "url": "https://example.invalid/" + secret,
                  "requested_schema": {"properties": {secret: {"type": "string"}}},
                  "elicitation": {"mcp_server_name": secret}, "content": {"answer": secret}}
        for edge, extra in (("elicitation", {}), ("elicitation-result", {"action": "accept"})):
            out = self.invoke(edge, dict(common, hook_event_name=EVENTS[edge], **extra), env)
            self.assertNotIn(secret.encode(), out)
            if out:
                parsed = json.loads(out)
                self.assertEqual(set(parsed), {"terminalSequence", "suppressOutput"})
                self.assertTrue(parsed["suppressOutput"])
                self.assertTrue(parsed["terminalSequence"].startswith("\x1b]0;"))
            self.assertNotIn(secret.encode(), self.record())

    def test_unavailable_state_fails_harmlessly_without_response_decisions(self):
        blocker = self.root / "not-a-directory"
        blocker.write_text("keep")
        for configured in (None, str(blocker / "state")):
            env = dict(self.env)
            if configured is None:
                env.pop("CCTAB_STATE_DIR")
            else:
                env["CCTAB_STATE_DIR"] = configured
            for edge in ("elicitation", "elicitation-result"):
                payload = {"session_id": "s1", "hook_event_name": EVENTS[edge],
                           "mcp_server_name": "mcp", "elicitation_id": "a", "action": "accept"}
                out = self.invoke(edge, payload, env)
                self.assertNotIn(b"decision", out)
                self.assertNotIn(b"hookSpecificOutput", out)
            self.assertEqual(blocker.read_text(), "keep")

    def test_previous_state_versions_migrate_without_losing_permission_waits(self):
        self.state.mkdir()
        for tag in ("cts1", "cts2"):
            session = tag
            (self.state / session).write_text(tag + "\nb w\nw owner:1000000\n")
            self.assertEqual(self.start(session=session), "WAITING")
            self.assertTrue(self.record(session).startswith(b"cts3\n"))
            self.assertIn(b"owner:1000000", self.record(session))
            self.assertEqual(self.finish(session=session), "")
            self.assertEqual(self.send("working", session=session, agent_id="owner"), "WORKING")

    def test_future_records_and_unusable_record_paths_are_not_overwritten(self):
        self.state.mkdir()
        future = b"cts9\nb w\nw future:1000000\n"
        (self.state / "future").write_bytes(future)
        for edge in ("elicitation", "elicitation-result"):
            self.send(edge, session="future", mcp_server_name="mcp", elicitation_id="a", action="accept")
            self.assertEqual(self.record("future"), future)
        (self.state / "directory").mkdir()
        sentinel = self.root / "sentinel"
        sentinel.write_text("keep")
        (self.state / "symlink").symlink_to(sentinel)
        for session in ("directory", "symlink"):
            self.start(session=session)
            self.finish(session=session)
        self.assertTrue((self.state / "directory").is_dir())
        self.assertTrue((self.state / "symlink").is_symlink())
        self.assertEqual(sentinel.read_text(), "keep")

    def test_wait_expiry_and_session_cleanup(self):
        for identified in (True, False):
            session = "identified" if identified else "anonymous"
            self.begin(session)
            self.start("a" if identified else None, session=session)
            self.env["CCTAB_NOW"] = "1000900"
            self.assertEqual(self.send("idle", session=session, background_tasks=[]), "")
            self.env["CCTAB_NOW"] = "1000901"
            self.assertEqual(self.send("idle", session=session, background_tasks=[]), "IDLE")
            self.env["CCTAB_NOW"] = "1000000"
            self.start("new", session=session)
            self.send("session-start", session=session, source="resume")
            self.assertNotIn(b"\nw ", self.record(session))
            self.start("last", session=session)
            self.send("session-end", session=session)
            self.assertFalse((self.state / session).exists())

    def test_wait_and_completion_metadata_stays_bounded(self):
        self.begin()
        self.send("waiting", agent_id="owner")
        for i in range(40):
            self.start(str(i), "s" * 64)
            self.assertLessEqual(len(self.record()), 8192)
            self.assertIn(b"owner:1000000", self.record())
            pending = next(line for line in self.record().splitlines() if line.startswith(b"w "))
            self.assertLessEqual(len(pending.split()) - 1, 9)
        for i in range(40):
            self.assertEqual(self.finish(str(i), "s" * 64), "")
            self.assertLessEqual(len(self.record()), 8192)
            completed = next(line for line in self.record().splitlines() if line.startswith(b"e "))
            self.assertLessEqual(len(completed.split()) - 1, 8)
        self.assertEqual(self.send("working", agent_id="owner"), "")
        self.assertEqual(self.send("working"), "")
        self.assertEqual(self.send("idle", background_tasks=[]), "")
        self.send("working", hook_event_name="UserPromptSubmit", prompt="continue")
        self.assertNotIn(b"\nw ", self.record())

    def test_concurrent_first_starts_and_results_preserve_all_requests(self):
        # Deliberately begin without an existing record to exercise creation races.
        for round_number in range(10):
            session = "race" + str(round_number)
            with self.subTest(session=session), ThreadPoolExecutor(max_workers=6) as pool:
                started = list(pool.map(lambda request: self.start(request, session=session),
                                        [str(i) for i in range(6)]))
                self.assertEqual(started, ["WAITING"] * 6)
                pending = next(line for line in self.record(session).splitlines()
                               if line.startswith(b"w "))
                self.assertEqual(len(pending.split()) - 1, 6)
                self.assertEqual(len(set(pending.split()[1:])), 6)
                finished = list(pool.map(lambda request: self.finish(request, session=session),
                                         [str(i) for i in range(6)]))
                self.assertEqual(finished.count("IDLE"), 1,
                                 f"results={finished!r}, persisted={self.record(session)!r}")
                self.assertEqual(finished.count(""), 5)
                self.assertNotIn(b"\nw ", self.record(session))

    def test_concurrent_start_and_result_leave_a_completed_request_retired(self):
        for round_number in range(10):
            session = "samekey" + str(round_number)
            with self.subTest(session=session), ThreadPoolExecutor(max_workers=2) as pool:
                start = pool.submit(self.start, session=session)
                result = pool.submit(self.finish, session=session)
                self.assertIn(start.result(), ("", "WAITING"))
                self.assertIn(result.result(), ("", "IDLE"))
            self.assertNotIn(b"\nw ", self.record(session))
            self.assertIn(b"\ne ", self.record(session))
            self.assertEqual(self.start(session=session), "")

    def test_versioned_synthetic_sequences(self):
        fixture = json.loads((ROOT / "tests/fixtures/elicitation-v1.json").read_text())
        self.assertEqual(fixture["provenance"], "synthetic-contract-scenarios-not-live-captures")
        for scenario in fixture["scenarios"]:
            with self.subTest(scenario=scenario["name"]):
                for event in scenario["events"]:
                    self.assertEqual(self.send(event["edge"], session=scenario["name"],
                                               **event["payload"]), event["paint"])


if __name__ == "__main__":
    unittest.main()
