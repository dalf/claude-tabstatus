#!/usr/bin/env python3
"""Native compiled-binary hostname contract, with SDK observers and dyld faults.

All CLI environments are fresh; only temporary home/config/data/state paths are
used. No inherited terminal/mux signals or CLAUDE_PID, hostname mutation, PTY or
developer session. Rendering assertions use dry-run, not protocol delivery.
On Darwin, missing binaries, SDK tools or required observations fail the suite.
"""
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[1]
BIN = Path(os.environ.get("CCTAB_TEST_BIN", ROOT / "bin/tabstatus")).resolve()


@unittest.skipUnless(sys.platform == "darwin", "requires native macOS execution")
class MacHostnameTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        native = tempfile.TemporaryDirectory(prefix="cctab-host-native-")
        cls.addClassCleanup(native.cleanup)
        root = Path(native.name)
        cls.observer = root / "observer"
        cls.dylib = root / "fault.dylib"
        cls.command = root / "recording-hostname"
        source = ROOT / "tests/fixtures/darwin_hostname_observer.c"
        builds = [(source, [], cls.observer),
                  (source, ["-DRECORD_HOSTNAME"], cls.command),
                  (ROOT / "tests/fixtures/darwin_hostname_fault.c",
                   ["-dynamiclib"], cls.dylib)]
        for source, flags, output in builds:
            result = subprocess.run(
                ["/usr/bin/xcrun", "clang", "-std=c11", "-Wall", "-Wextra", "-Werror",
                 *flags, str(source), "-o", str(output)],
                env={"PATH": os.defpath, "HOME": str(root), "TMPDIR": str(root)},
                capture_output=True, timeout=60)
            if result.returncode:
                raise RuntimeError(f"native hostname helper compilation failed:\n"
                                   f"{result.stdout.decode()}{result.stderr.decode()}")

    def setUp(self):
        tmp = tempfile.TemporaryDirectory(prefix="cctab-host-")
        self.addCleanup(tmp.cleanup)
        self.root = Path(tmp.name).resolve()
        self.home = self.root / "home"
        self.cwd = self.home / "project"
        self.cwd.mkdir(parents=True)
        self.fake_path = self.root / "commands"
        self.fake_path.mkdir()
        shutil.copy2(self.command, self.fake_path / "hostname")
        self.native_log = self.root / "native.log"
        self.command_log = self.root / "command.log"
        self.env = {
            "PATH": "",
            "HOME": str(self.home),
            "CLAUDE_CONFIG_DIR": str(self.root / "config"),
            "XDG_DATA_HOME": str(self.root / "data"),
            "CCTAB_STATE_DIR": str(self.root / "state"),
            "CCTAB_GLYPH_WORKING": "WORK",
            "CCTAB_DRY_RUN": "1",
            "SSH_CONNECTION": "192.0.2.1 22 192.0.2.2 22",
        }
        # Validate PATH resolution and recording under this case's environment.
        control = self.run_process(["hostname"], {
            "PATH": str(self.fake_path), "CCTAB_TEST_COMMAND_LOG": str(self.command_log)})
        self.assertEqual(control, b"command.example\n")
        self.assertEqual(self.command_log.read_bytes(), b"hostname command\n")
        self.command_log.unlink()
        # Prove dyld observation and substitution on this fixture before using
        # absence of native observations to establish bypasses in the candidate.
        control = self.run_process([str(self.observer), "query"], self.fault("success"))
        self.assertEqual(control, b"native.example")
        self.assert_query_count(1)
        self.native_log.unlink()

    def run_process(self, command, values=None):
        env = dict(self.env, **(values or {}))
        result = subprocess.run(command, cwd=self.cwd, env=env, input=b"{}",
                                capture_output=True, start_new_session=True, timeout=10)
        self.assertEqual((result.returncode, result.stderr), (0, b""))
        return result.stdout

    def run_binary(self, edge="working", values=None):
        result = self.run_process([str(BIN), edge], values)
        self.assertFalse(self.command_log.exists(), "candidate launched hostname on PATH")
        return result

    def fault(self, mode):
        return {"DYLD_INSERT_LIBRARIES": str(self.dylib),
                "CCTAB_TEST_HOST_FAULT": mode,
                "CCTAB_TEST_HOST_LOG": str(self.native_log),
                "PATH": str(self.fake_path),
                "CCTAB_TEST_COMMAND_LOG": str(self.command_log)}

    def assert_query_count(self, count):
        lines = self.native_log.read_bytes().splitlines() if self.native_log.exists() else []
        self.assertEqual(lines, [b"gethostname 257"] * count)

    @staticmethod
    def host_row(output):
        return next(line.split(maxsplit=2)[1:] for line in output.decode().splitlines()
                    if line.startswith("  hostname "))

    def test_real_kernel_hostname_with_empty_path_and_environment_names_absent(self):
        expected = self.run_process([str(self.observer)])
        # Compare the independent sysctl observation to the previous OS utility.
        previous = self.run_process(["/bin/hostname"]).rstrip(b"\n")
        self.assertEqual(expected, previous)
        self.assertTrue(expected)
        # No cap/sanitiser assumptions about an arbitrary runner's actual name:
        # feed those independently observed bytes through the existing override.
        reference = self.run_binary(values={"CCTAB_HOST": os.fsdecode(expected)})
        automatic = self.run_binary()  # PATH empty; both names absent
        self.assertEqual(automatic, reference)
        self.assertTrue(automatic.startswith(b"WORK "))
        self.assertIn(b":~/project\n", automatic)
        observed = self.run_binary(values=self.fault("observe"))
        self.assertEqual(observed, reference)
        self.assert_query_count(1)

    def test_override_and_hostname_environment_precedence_and_empty_fallthrough(self):
        values = self.fault("success")
        cases = [("explicit.example", "environment.example", "explicit", 0),
                 ("explicit.example", "", "explicit", 0),
                 (None, "environment.example", "environment", 0),
                 ("", "environment.example", "environment", 0),
                 (None, None, "native", 1),
                 ("", "", "native", 1),
                 ("", None, "native", 1),
                 (None, "", "native", 1)]
        for override, environment, prefix, queries in cases:
            with self.subTest(override=override, environment=environment):
                self.native_log.unlink(missing_ok=True)
                env = values.copy()
                if override is not None:
                    env["CCTAB_HOST"] = override
                if environment is not None:
                    env["HOSTNAME"] = environment
                self.assertEqual(self.run_binary(values=env), f"WORK {prefix}:~/project\n".encode())
                self.assert_query_count(queries)

    def test_local_rendering_bypasses_native_lookup(self):
        self.assertEqual(self.run_binary(values={**self.fault("failure"), "SSH_CONNECTION": ""}),
                         b"WORK ~/project\n")
        self.assert_query_count(0)

    def test_native_failure_empty_and_incomplete_output_have_no_command_fallback(self):
        for mode in ["failure", "empty", "unterminated", "partial"]:
            with self.subTest(mode=mode):
                self.native_log.unlink(missing_ok=True)
                self.assertEqual(self.run_binary(values=self.fault(mode)), b"WORK ssh:~/project\n")
                self.assert_query_count(1)

    def test_native_bytes_and_existing_display_processing(self):
        cases = [("success", {}, "native"),
                 ("unicode", {}, "hôte"),
                 ("invalid_utf8", {}, "s�v��"),
                 ("numeric", {}, "192.168.1.5"),
                 ("hostile", {}, "srv"),
                 ("boundary", {}, "b" * 15 + "…"),
                 ("boundary", {"CCTAB_MAX_HOST": "7", "CCTAB_ELLIPSIS": "..."}, "b" * 6 + "..."),
                 ("boundary", {"CCTAB_MAX_HOST": "0"}, "b" * 256)]
        for mode, config, prefix in cases:
            with self.subTest(mode=mode, config=config):
                self.native_log.unlink(missing_ok=True)
                self.assertEqual(self.run_binary(values={**self.fault(mode), **config}),
                                 f"WORK {prefix}:~/project\n".encode())
                self.assert_query_count(1)

    def test_doctor_explicit_query_failure_and_override_reporting(self):
        # Local doctor explicitly queries the name; its preview stays local.
        values = {**self.fault("success"), "SSH_CONNECTION": ""}
        self.assertEqual(self.host_row(self.run_binary("doctor", values)), ["ok", "native.example"])
        self.assert_query_count(1)
        self.native_log.unlink()
        self.assertEqual(self.host_row(self.run_binary("doctor", {
            **values, "CCTAB_HOST": "explicit.example", "HOSTNAME": "environment.example"})),
            ["off", "CCTAB_HOST"])
        self.assert_query_count(0)
        self.assertEqual(self.host_row(self.run_binary("doctor", {
            **values, "CCTAB_HOST": "", "HOSTNAME": "environment.example"})),
            ["ok", "environment.example"])
        self.assert_query_count(0)
        for mode in ["failure", "empty", "unterminated", "partial"]:
            with self.subTest(mode=mode):
                self.native_log.unlink(missing_ok=True)
                self.assertEqual(self.host_row(self.run_binary("doctor", {
                    **self.fault(mode), "SSH_CONNECTION": ""})),
                    ["n/a", "nothing here names this machine"])
                self.assert_query_count(1)


if __name__ == "__main__":
    unittest.main(verbosity=2)
