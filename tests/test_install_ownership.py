"""Actual-binary ownership/recovery lifecycles, portable to all three CI hosts.

Filesystem barriers prove the operation reached the intended point. Removing a
closed staging file forces the production rename to fail without replacing the
original. No timing-window miss or missing candidate binary counts as a pass.
"""
import json
import os
from pathlib import Path
import subprocess
import tempfile
import time
import unittest

ROOT = Path(__file__).resolve().parents[1]
BIN = Path(os.environ.get("CCTAB_TEST_BIN", ROOT / "bin/tabstatus")).resolve()
KEY = "CLAUDE_CODE_DISABLE_TERMINAL_TITLE"
ORIGINAL = b'{"env":{"kept":"value"},"other":1}\n'


class OwnershipTests(unittest.TestCase):
    def setUp(self):
        self.assertTrue(BIN.is_file(), f"missing candidate binary: {BIN}")
        self.tmp = tempfile.TemporaryDirectory(prefix="cctab-ownership-")
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name).resolve()
        self.config = self.root / "config"
        self.config.mkdir()
        self.settings = self.config / "settings.json"
        self.record = self.config / "claude-tabstatus.state"
        self.link = self.config / "skills/claude-tabstatus"
        self.tree = self.root / "data/claude-tabstatus"
        self.settings.write_bytes(ORIGINAL)
        self.env = os.environ.copy()
        for name in ("CLAUDE_PID", "TMUX", "STY", "XDG_RUNTIME_DIR", "LOCALAPPDATA", "TMPDIR",
                     "CCTAB_TEST_MANAGE_DIR", "CCTAB_TEST_MANAGE_POINT"):
            self.env.pop(name, None)
        self.env.update(HOME=str(self.root / "home"), CLAUDE_CONFIG_DIR=str(self.config),
                        XDG_DATA_HOME=str(self.root / "data"), CCTAB_STATE_DIR=str(self.root / "state"),
                        CCTAB_DRY_RUN="1")
        self.children = []
        self.addCleanup(self.stop_children)

    def stop_children(self):
        for child in self.children:
            if child.poll() is None:
                child.kill()
            child.communicate(timeout=10)

    def cli(self, *args, ok=True):
        result = subprocess.run([str(BIN), *map(str, args)], cwd=self.root, env=self.env,
                                capture_output=True, timeout=30)
        self.assertEqual(result.returncode, 0 if ok else 1, result.stdout + result.stderr)
        return result

    def start(self, point, *args):
        control = Path(tempfile.mkdtemp(prefix="control-", dir=self.root))
        env = dict(self.env, CCTAB_TEST_MANAGE_DIR=str(control), CCTAB_TEST_MANAGE_POINT=point)
        child = subprocess.Popen([str(BIN), *map(str, args or ("install",))], cwd=self.root,
                                 env=env, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        self.children.append(child)
        self.observe(child, control)
        return child, control

    def observe(self, child, control):
        deadline = time.monotonic() + 15
        reached = control / "reached"
        def ready():
            return reached.is_file() and reached.stat().st_size > 0
        while not ready() and child.poll() is None and time.monotonic() < deadline:
            time.sleep(0.005)
        if not ready():
            if child.poll() is None:
                child.kill()
            out, err = child.communicate(timeout=10)
            self.fail(f"required management observation missing: {out!r} {err!r}")

    def release(self, child, control, fail=False, ok=True):
        (control / "release").write_bytes(b"fail" if fail else b"")
        out, err = child.communicate(timeout=30)
        self.assertEqual(child.returncode, 0 if ok else 1, out + err)
        return out, err

    def kill(self, child):
        child.kill()
        child.communicate(timeout=10)
        self.assertNotEqual(child.returncode, 0)

    def value(self):
        return json.loads(self.settings.read_bytes()).get("env", {}).get(KEY)

    def history(self):
        state = json.loads(self.record.read_bytes())
        return {k: state[k] for k in ("env_key_before", "env_object_before", "symlink_before")}

    def refuse_unchanged(self, *args):
        before = self.settings.read_bytes(), self.record.read_bytes()
        linked = self.link.exists()
        result = self.cli(*args, ok=False)
        self.assertIn(b"restoration history", result.stderr)
        self.assertEqual((self.settings.read_bytes(), self.record.read_bytes()), before)
        self.assertEqual(self.link.exists(), linked)
        return result

    def test_failed_first_install_then_user_key_is_never_removed(self):
        child, control = self.start("history-saved")
        self.assertTrue(self.record.is_file())
        self.assertFalse(self.history()["env_key_before"]["had"])
        self.assertIsNone(self.value())
        # Forces the real byte-comparison guard, preserving a valid external edit.
        external = ORIGINAL + b" "
        self.settings.write_bytes(external)
        _, err = self.release(child, control, ok=False)
        self.assertIn(b"changed while this installer was running", err)
        self.assertEqual(self.settings.read_bytes(), external)
        self.assertIsNone(self.value())
        independent = json.dumps({"env": {"kept": "value", KEY: "1"}, "other": 2}).encode() + b"\n"
        self.settings.write_bytes(independent)
        result = subprocess.run([str(BIN), "uninstall"], cwd=self.root, env=self.env,
                                capture_output=True, timeout=30)
        # The principal assertion is behaviour, independent of any new phase field.
        self.assertEqual(self.settings.read_bytes(), independent,
                         f"uninstall deleted a user-owned key: {result.stdout!r} {result.stderr!r}")
        self.assertIn(result.returncode, (0, 1))
        self.assertIn(b"restoration history", result.stderr)

    def test_killed_before_settings_replacement_preserves_independent_key(self):
        child, _ = self.start("history-saved")
        saved = self.record.read_bytes()
        self.assertEqual(self.settings.read_bytes(), ORIGINAL)
        self.kill(child)
        self.settings.write_text(json.dumps({"env": {KEY: "1", "new": "kept"}}))
        self.refuse_unchanged("uninstall")
        self.refuse_unchanged("install", "--force")
        self.refuse_unchanged("uninstall", "--force")
        self.assertEqual(self.record.read_bytes(), saved)

    def test_killed_after_settings_replacement_retains_original_for_explicit_recovery(self):
        child, _ = self.start("settings-written")
        self.assertEqual(self.value(), "1")
        self.assertFalse(self.history()["env_key_before"]["had"])
        self.assertFalse(self.link.exists())
        self.kill(child)
        self.refuse_unchanged("uninstall")
        self.refuse_unchanged("install")
        self.cli("uninstall", "--restore-backup")
        self.assertEqual(self.settings.read_bytes(), ORIGINAL)
        self.assertFalse(self.record.exists())

    def test_actual_settings_rename_failure_keeps_original_and_pending_history(self):
        child, control = self.start("settings-staged")
        staged = Path(os.fsdecode((control / "reached").read_bytes()))
        self.assertEqual(staged.parent, self.settings.parent)
        self.assertEqual(json.loads(staged.read_bytes())["env"][KEY], "1")
        staged.unlink()  # The production rename now fails with ENOENT.
        _, err = self.release(child, control, ok=False)
        self.assertIn(b"could not write", err)
        self.assertEqual(self.settings.read_bytes(), ORIGINAL)
        self.assertFalse(staged.exists())
        self.assertTrue(self.record.is_file())
        self.settings.write_text(json.dumps({"env": {KEY: "1"}}))
        self.refuse_unchanged("uninstall")

    def test_actual_ownership_finalisation_failure_retains_history_and_reports_failure(self):
        child, control = self.start("ownership-staged")
        saved = self.record.read_bytes()
        staged = Path(os.fsdecode((control / "reached").read_bytes()))
        self.assertEqual(staged.parent, self.config)
        self.assertEqual(self.value(), "1")
        self.assertEqual(json.loads(staged.read_bytes())["settings_ownership"], "confirmed")
        staged.unlink()
        out, err = self.release(child, control, ok=False)
        self.assertNotIn(b"Done.", out)
        self.assertIn(b"ownership could not be finalised", err)
        self.assertEqual(self.record.read_bytes(), saved)
        self.assertFalse(self.link.exists())
        self.refuse_unchanged("uninstall")
        self.cli("uninstall", "--restore-backup")
        self.assertEqual(self.settings.read_bytes(), ORIGINAL)

    def test_success_restores_absence_raw_values_unrelated_settings_and_empty_env(self):
        for original in (b'{"model":"opus"}\n', b'{"env":{},"model":"opus"}\n',
                         ORIGINAL, b'{"env":{"' + KEY.encode() + b'":"\\u0030","X":"\\/"},"n":1e+02}\n',
                         b'{"env":{"' + KEY.encode() + b'":false},"other":1}\n',
                         b'{"env":{"' + KEY.encode() + b'":null},"other":1}\n'):
            with self.subTest(original=original):
                self.settings.write_bytes(original)
                self.cli("install")
                self.assertEqual(self.value(), "1")
                self.cli("uninstall")
                self.assertEqual(self.settings.read_bytes(), original)

    def test_ordinary_uninstall_preserves_later_unrelated_edits(self):
        self.cli("install")
        self.settings.write_bytes(self.settings.read_bytes().replace(b'"other":1', b'"other":2,"new":"kept"'))
        self.cli("uninstall")
        self.assertEqual(self.settings.read_bytes(), b'{"env":{"kept":"value"},"other":2,"new":"kept"}\n')

    def test_killed_refresh_retains_established_receipt_and_original_raw_value(self):
        original = b'{"env":{"' + KEY.encode() + b'":"\\u0030","keep":"x"}}\n'
        self.settings.write_bytes(original)
        self.cli("install")
        history = self.history()
        self.settings.write_bytes(original)
        child, _ = self.start("settings-written")
        self.kill(child)
        self.assertEqual(self.value(), "1")
        self.assertEqual(self.history(), history)
        self.cli("uninstall")
        self.assertEqual(self.settings.read_bytes(), original)

    def test_preexisting_one_unchanged_path_is_preserved_byte_for_byte(self):
        original = b'{"env":{"' + KEY.encode() + b'":"\\u0031","X":"y"}}\n'
        self.settings.write_bytes(original)
        self.cli("install")
        self.assertEqual(self.settings.read_bytes(), original)
        self.assertFalse(Path(str(self.settings) + ".cctab-preinstall").exists())
        self.cli("install")
        self.cli("uninstall")
        self.assertEqual(self.settings.read_bytes(), original)

    def test_missing_and_blank_settings_interrupted_recovery_is_conservative(self):
        for original in (None, b" \n\t"):
            with self.subTest(original=original):
                if original is None:
                    self.settings.unlink(missing_ok=True)
                else:
                    self.settings.write_bytes(original)
                child, _ = self.start("settings-written")
                self.kill(child)
                self.assertFalse(self.history()["env_key_before"]["had"])
                self.refuse_unchanged("uninstall")
                # Manual narrow recovery keeps the journal until the user has
                # inspected it; moving it aside makes existing --force deliberate.
                self.record.replace(self.root / "saved-record")
                self.cli("uninstall", "--force")
                self.assertIsNone(self.value())

    def test_repeated_install_and_failed_refresh_keep_established_ownership(self):
        self.settings.write_bytes(b'{"env":{"' + KEY.encode() + b'":"\\u0030","keep":"x"}}\n')
        original = self.settings.read_bytes()
        self.cli("install")
        history = self.history()
        self.cli("install")
        self.assertEqual(self.history(), history)
        # A failed refresh after record publication must keep the earlier receipt.
        child, control = self.start("history-saved", "install", "--tree", self.root / "moved-tree")
        self.release(child, control, fail=True, ok=False)
        self.assertEqual(self.history(), history)
        self.assertEqual(json.loads(self.record.read_bytes())["settings_ownership"], "confirmed")
        self.assertEqual(Path(json.loads(self.record.read_bytes())["tree"]), self.root / "moved-tree")
        self.cli("install", "--tree", self.root / "moved-tree")
        self.assertEqual(self.history(), history)
        self.cli("uninstall")
        self.assertEqual(self.settings.read_bytes(), original)

    def test_failed_refresh_settings_rename_keeps_original_restoration_history(self):
        self.cli("install")
        history = self.history()
        self.settings.write_bytes(ORIGINAL)  # A refresh now needs to write again.
        child, control = self.start("settings-staged")
        Path(os.fsdecode((control / "reached").read_bytes())).unlink()
        self.release(child, control, ok=False)
        self.assertEqual(self.history(), history)
        self.assertEqual(self.settings.read_bytes(), ORIGINAL)
        self.cli("uninstall")
        self.assertEqual(self.settings.read_bytes(), ORIGINAL)

    def test_prior_link_and_tree_moves_retain_original_link(self):
        # Make a real prior installation, then treat its linked tree as the
        # user's original link while the focused installation has no history.
        self.cli("install", "--tree", self.root / "prior")
        self.record.unlink()
        self.settings.write_bytes(ORIGINAL)
        self.cli("install", "--tree", self.tree)
        history = self.history()
        self.cli("install", "--tree", self.root / "new-tree")
        self.assertEqual(self.history(), history)
        self.cli("uninstall")
        self.assertTrue(os.path.samefile(self.link, self.root / "prior"))
        self.assertEqual(self.settings.read_bytes(), ORIGINAL)

    def test_new_metadata_rejects_malformed_mismatched_and_missing_fields(self):
        self.cli("install")
        original = self.record.read_bytes()
        edits = [lambda s: s.pop("settings_ownership"),
                 lambda s: s.update(settings_ownership="done"),
                 lambda s: s.update(settings_path=str(self.root / "other-settings.json")),
                 lambda s: s.update(env_key="OTHER"),
                 lambda s: s.update(env_key_before={"had": True, "raw": None}),
                 lambda s: s.update(env_key_before={"had": False, "raw": '"0"'}),
                 lambda s: s.update(env_key_before={"had": True, "raw": "not json"}),
                 lambda s: s.pop("env_object_before"),
                 lambda s: s.update(symlink_before={"had": True, "target": None}),
                 lambda s: s.update(tree="relative/tree"),
                 lambda s: s.update(state_version=5),
                 lambda s: s.update(state_version=3),
                 lambda s: s.update(settings_ownership="legacy"),
                 lambda s: s.update(settings_ownership="legacy", legacy_completion_link="relative/link"),
                 lambda s: s.update(legacy_completion_link=str(self.tree))]
        for edit in edits:
            with self.subTest(edit=edit):
                state = json.loads(original)
                edit(state)
                self.record.write_text(json.dumps(state))
                self.refuse_unchanged("uninstall", "--force")
                self.refuse_unchanged("install", "--force")
        self.record.write_bytes(original[:-2] + b',"settings_ownership":"confirmed"}\n')
        self.refuse_unchanged("uninstall")
        self.record.write_bytes(original)
        self.cli("uninstall")
        self.assertEqual(self.settings.read_bytes(), ORIGINAL)

    def test_saved_legacy_witness_cannot_be_incomplete_or_contradictory(self):
        self.cli("install")
        self.legacy(3)
        self.cli("install")
        original = self.record.read_bytes()
        for edit in (lambda s: s.pop("legacy_completion_link"),
                     lambda s: s.update(legacy_completion_link=None),
                     lambda s: s.update(symlink_before={"had": True, "target": str(self.tree)})):
            state = json.loads(original)
            edit(state)
            self.record.write_text(json.dumps(state))
            self.refuse_unchanged("uninstall")
            self.refuse_unchanged("install")
        self.record.write_bytes(original)
        self.cli("uninstall")
        self.assertEqual(self.settings.read_bytes(), ORIGINAL)

    def legacy(self, version, ambiguous=False):
        state = json.loads(self.record.read_bytes())
        state["state_version"] = version
        state.pop("settings_ownership")
        if version in (1, 2):
            state["repo"] = state.pop("tree")
        if version == 1:
            before = state["env_key_before"]
            before["value"] = json.loads(before.pop("raw")) if before["had"] else None
            state.pop("env_object_before")
        if ambiguous:
            state["symlink_before"] = {"had": True, "target": str(self.tree)}
        self.record.write_text(json.dumps(state))

    def test_legacy_linked_completion_restores_original_without_claiming_new_receipt(self):
        for version in (1, 2, 3):
            with self.subTest(version=version):
                self.settings.write_bytes(b'{"env":{"' + KEY.encode() + b'":"0","X":"y"}}\n')
                original = self.settings.read_bytes()
                self.cli("install")
                self.legacy(version)
                self.cli("install")
                self.assertEqual(json.loads(self.record.read_bytes())["settings_ownership"], "legacy")
                self.assertEqual(Path(json.loads(self.record.read_bytes())["legacy_completion_link"]), self.tree)
                child, control = self.start("history-saved", "install", "--tree", self.root / "legacy-new-tree")
                self.release(child, control, fail=True, ok=False)
                self.cli("install", "--tree", self.root / "legacy-new-tree")
                self.cli("uninstall")
                self.assertEqual(self.settings.read_bytes(), original)

    def test_legacy_orphan_and_unchanged_prior_link_are_ambiguous(self):
        for version in (1, 2, 3):
            with self.subTest(version=version):
                self.cli("install")
                self.legacy(version, ambiguous=True)
                self.refuse_unchanged("uninstall")
                self.refuse_unchanged("install")
                self.cli("uninstall", "--restore-backup")
                self.assertEqual(self.settings.read_bytes(), ORIGINAL)
                self.cli("install")
                self.legacy(version)
                # Actual installed tree/marker and matching settings path/value
                # alone are insufficient when no completion link remains.
                if os.name == "nt":
                    self.link.rmdir()
                else:
                    self.link.unlink()
                self.refuse_unchanged("uninstall")
                self.cli("uninstall", "--restore-backup")

    def test_force_without_record_and_explicit_whole_backup_remain_deliberate(self):
        self.settings.write_text(json.dumps({"env": {KEY: "1", "user": "kept"}}))
        before = self.settings.read_bytes()
        result = self.cli("uninstall")
        self.assertIn(b"cannot prove", result.stderr)
        self.assertEqual(self.settings.read_bytes(), before)
        self.cli("uninstall", "--force")
        self.assertIsNone(self.value())
        self.assertEqual(json.loads(self.settings.read_bytes())["env"]["user"], "kept")
        self.settings.write_bytes(ORIGINAL)
        self.cli("install")
        self.settings.write_text(json.dumps({"env": {KEY: "1"}, "unrelated": "new"}))
        self.cli("uninstall", "--restore-backup")
        self.assertEqual(self.settings.read_bytes(), ORIGINAL)

    def test_management_lock_serialises_and_killed_owner_releases_it(self):
        child, control = self.start("history-saved")
        blocked_control = self.root / "blocked-control"
        blocked_control.mkdir()
        env = dict(self.env, CCTAB_TEST_MANAGE_DIR=str(blocked_control), CCTAB_TEST_MANAGE_POINT="lock-acquired")
        waiting = subprocess.Popen([str(BIN), "uninstall"], cwd=self.root, env=env,
                                   stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        self.children.append(waiting)
        deadline = time.monotonic() + 10
        while not (blocked_control / "requested").is_file() and waiting.poll() is None and time.monotonic() < deadline:
            time.sleep(0.005)
        self.assertTrue((blocked_control / "requested").is_file(), "second manager never requested the lock")
        # A second config remains independent while this config is locked.
        other = dict(self.env, CLAUDE_CONFIG_DIR=str(self.root / "other-config"),
                     XDG_DATA_HOME=str(self.root / "other-data"))
        result = subprocess.run([str(BIN), "install"], cwd=self.root, env=other,
                                capture_output=True, timeout=10)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIsNone(waiting.poll())
        self.assertFalse((blocked_control / "reached").exists())
        self.release(child, control)
        self.observe(waiting, blocked_control)
        self.release(waiting, blocked_control)
        self.assertEqual(self.settings.read_bytes(), ORIGINAL)
        self.assertFalse(self.record.exists())
        # No stale PID ownership: an interrupted holder releases the kernel lock.
        child, _ = self.start("history-saved")
        self.kill(child)
        self.refuse_unchanged("uninstall")


if __name__ == "__main__":
    unittest.main()
