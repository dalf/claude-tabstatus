#!/usr/bin/env python3
"""Replay sanitized live lifecycle projections with authored state expectations."""
import json
import os
from pathlib import Path
import subprocess
import tempfile
import time
import unittest
from concurrent.futures import ThreadPoolExecutor

ROOT = Path(__file__).resolve().parents[1]
BIN = Path(os.environ.get("CCTAB_TEST_BIN", ROOT / "bin/tabstatus")).resolve()
FIXTURE = json.loads((ROOT / "tests/fixtures/background-v1.json").read_text())
EDGES = {"SessionStart": "session-start", "SessionEnd": "session-end",
         "UserPromptSubmit": "working", "PostToolUse": "working",
         "PostToolUseFailure": "working", "Stop": "idle", "StopFailure": "idle",
         "PermissionRequest": "waiting", "SubagentStop": "subagent-stop",
         "Notification": "notify", "Elicitation": "elicitation",
         "ElicitationResult": "elicitation-result"}
STATES = {"working", "waiting", "background", "idle"}


class BackgroundTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory(prefix="cctab-background-")
        self.addCleanup(self.tmp.cleanup)
        # Match getcwd's physical path, including macOS's /var -> /private/var.
        self.root = Path(self.tmp.name).resolve()
        self.state = self.root / "state"
        self.env = {"PATH": "/usr/bin:/bin", "HOME": str(self.root), "CLAUDE_PID": "0",
                    "CCTAB_STATE_DIR": str(self.state), "CCTAB_DRY_RUN": "1",
                    "CCTAB_TERMINAL": "other", "CCTAB_GLYPH_POS": "prefix"}
        for state in STATES:
            self.env["CCTAB_GLYPH_" + state.upper()] = state.upper()

    def send(self, edge, payload, now=1000000):
        result = subprocess.run([str(BIN), edge], input=json.dumps(payload).encode(),
                                env=dict(self.env, CCTAB_NOW=str(now)), cwd=self.root,
                                capture_output=True, timeout=10)
        self.assertEqual((result.returncode, result.stderr), (0, b""))
        if not result.stdout:
            return "silent"
        if result.stdout == b"\n":
            return "clear"
        paint = result.stdout.decode().split(" ", 1)[0].lower()
        self.assertIn(paint, STATES)
        return paint

    def record(self):
        path = self.state / "s1"
        if not path.exists():
            return {"base": "idle", "background": False}
        lines = path.read_text().splitlines()
        self.assertEqual(lines[0], "cts5")
        fields = dict(line.split(" ", 1) for line in lines[1:])
        return {"base": {"w": "working", "i": "idle", "a": "waiting"}[fields["b"]],
                "background": "g" in fields}

    def replay(self, scenario):
        displayed = "unpainted"
        for index, step in enumerate(scenario["steps"]):
            with self.subTest(scenario=scenario["id"], step=index):
                payload = step["payload"]
                edge = EDGES.get(payload["hook_event_name"])
                # Production only registers PreToolUse for these two tools.
                if payload["hook_event_name"] == "PreToolUse" and payload.get("tool_name") in {
                    "AskUserQuestion", "ExitPlanMode"
                }:
                    edge = "waiting"
                paint = self.send(edge, payload, 1000000 + step["offset_ms"] // 1000) if edge else "silent"
                if paint != "silent":
                    displayed = "cleared" if paint == "clear" else paint
                self.assertEqual({**self.record(), "paint": paint, "displayed": displayed}, step["expect"])

    def event(self, edge, event, now=1000000, **fields):
        return self.send(edge, {"session_id": "s1", "hook_event_name": event, **fields}, now)

    def background(self):
        self.assertEqual(self.event("idle", "Stop", background_tasks=[{"type": "workflow"}]), "background")

    def test_missing_metadata_main_replies_failure_and_child_completion_preserve_work(self):
        self.background()
        # Synthetic time travel, independently of the bounded real captures.
        for day in [1, 30, 365]:
            now = 1000000 + day * 86400
            self.assertEqual(self.event("working", "UserPromptSubmit", now, prompt="progress?"), "working")
            for fields in [{}, {"background_tasks": None}]:
                self.assertEqual(self.event("idle", "Stop", now, **fields), "background")
            self.assertEqual(self.event("idle", "StopFailure", now, background_tasks=[]), "background")
            self.assertEqual(self.event("notify", "Notification", now, notification_type="idle_prompt"), "background")
            self.assertEqual(self.event("subagent-stop", "SubagentStop", now, agent_id="child", background_tasks=[]), "silent")
            self.assertEqual(self.record(), {"base": "idle", "background": True})
        self.assertEqual(self.event("idle", "Stop", now, background_tasks=[]), "idle")
        self.assertFalse(self.record()["background"])

    def test_direct_waits_and_background_clear_independently(self):
        self.background()
        request = {"mcp_server_name": "server", "elicitation_id": "request", "mode": "form"}
        self.assertEqual(self.event("elicitation", "Elicitation", **request), "waiting")
        self.assertEqual(self.event("idle", "Stop", background_tasks=[]), "waiting")
        self.assertFalse(self.record()["background"])
        self.assertEqual(self.event("elicitation-result", "ElicitationResult", action="accept", **request), "idle")
        self.background()
        request["elicitation_id"] = "another"
        self.assertEqual(self.event("elicitation", "Elicitation", **request), "waiting")
        self.assertEqual(self.event("elicitation-result", "ElicitationResult", action="cancel", **request), "background")

    def test_wait_expiry_restores_background_without_expiring_it(self):
        self.background()
        self.assertEqual(self.event("waiting", "PermissionRequest", agent_id="child"), "waiting")
        self.assertEqual(self.event("idle", "Stop", 1000901), "background")
        self.assertTrue(self.record()["background"])

    def test_unknown_kinds_large_snapshots_and_partial_completion_are_conservative(self):
        # No task slots can overflow: one bounded fact represents the complete
        # registry. Even unfamiliar entries mean that the registry is nonempty.
        tasks = [{"id": str(i), "type": "future-kind", "status": "pending"} for i in range(10000)]
        self.assertEqual(self.event("idle", "Stop", background_tasks=tasks), "background")
        self.assertLess((self.state / "s1").stat().st_size, 100)
        self.assertEqual(self.event("idle", "Stop", background_tasks=tasks[:1]), "background")
        self.assertEqual(self.event("idle", "Stop", background_tasks=[]), "idle")

    def test_scheduled_future_work_does_not_count_as_in_flight(self):
        self.assertEqual(self.event("idle", "Stop", session_crons=[{"id": "schedule"}], background_tasks=[]), "idle")
        self.assertFalse(self.record()["background"])

    def test_restart_compaction_end_and_session_isolation(self):
        self.background()
        self.assertEqual(self.send("idle", {"session_id": "s2", "hook_event_name": "Stop", "background_tasks": []}), "idle")
        self.assertTrue(self.record()["background"])
        self.assertEqual(self.event("session-start", "SessionStart", source="compact"), "silent")
        self.assertTrue(self.record()["background"])
        self.assertEqual(self.event("session-start", "SessionStart", source="resume"), "idle")
        self.assertFalse(self.record()["background"])
        self.background()
        self.assertEqual(self.event("session-end", "SessionEnd"), "clear")
        self.assertFalse((self.state / "s1").exists())

    def test_legacy_records_do_not_invent_background_from_reserved_fields(self):
        self.state.mkdir()
        for version in range(1, 5):
            (self.state / "s1").write_text(f"cts{version}\nb w\ng 999999\n")
            self.assertEqual(self.event("idle", "Stop"), "idle")
            self.assertEqual(self.record(), {"base": "idle", "background": False})
        self.background()
        report = subprocess.run([str(BIN), "doctor"], env=dict(self.env, CCTAB_NOW="1000901"),
                                cwd=self.root, capture_output=True, timeout=10)
        self.assertIn(b"background reported 901s ago", report.stdout)
        self.assertIn(b"last known snapshot; not expired", report.stdout)

    def test_concurrent_waits_do_not_lose_background_or_other_owners(self):
        self.background()
        with ThreadPoolExecutor(max_workers=4) as workers:
            results = list(workers.map(lambda i: self.event("waiting", "PermissionRequest", agent_id=f"a{i}"), range(4)))
        self.assertEqual(results, ["waiting"] * 4)
        self.assertTrue(self.record()["background"])
        for i in range(4):
            self.assertEqual(self.event("subagent-stop", "SubagentStop", agent_id=f"a{i}"),
                             "background" if i == 3 else "silent")

    def test_background_glyph_override_can_be_empty(self):
        self.env["CCTAB_GLYPH_BACKGROUND"] = ""
        result = subprocess.run([str(BIN), "idle"], input=json.dumps({
            "session_id": "s1", "hook_event_name": "Stop", "background_tasks": [{}]
        }).encode(), env=self.env, cwd=self.root, capture_output=True, timeout=10)
        self.assertEqual((result.returncode, result.stderr), (0, b""))
        self.assertEqual(result.stdout, b"~\n")
        self.assertTrue(self.record()["background"])

    def test_startup_reaper_cannot_age_out_background_without_process_metadata(self):
        self.background()
        record = self.state / "s1"
        before = record.read_bytes()
        old = time.time() - 365 * 86400
        os.utime(record, (old, old))
        self.assertEqual(self.send("session-start", {"session_id": "s2", "hook_event_name": "SessionStart", "source": "startup"}), "idle")
        self.assertEqual(record.read_bytes(), before)
        self.assertEqual(self.event("idle", "Stop"), "background")


assert FIXTURE["schema_version"] == 1
for scenario in FIXTURE["scenarios"]:
    def test(self, scenario=scenario):
        self.replay(scenario)
    setattr(BackgroundTests, "test_capture_" + scenario["id"], test)

if __name__ == "__main__":
    unittest.main()
