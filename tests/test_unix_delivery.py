#!/usr/bin/env python3
"""Native Linux/macOS delivery contract, independent of the Linux golden corpus.

Only disposable processes and newly allocated PTYs are targets. A stand-in owns
the PTY as its controlling terminal even when stdout is redirected, so falling
back to the controlling terminal would fail the headless assertions.
"""
import errno
from concurrent.futures import ThreadPoolExecutor
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


class UnixDeliveryFixture(unittest.TestCase):
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

    def allocate_pty(self):
        # A fresh pair for each session: do not rely on a controlling terminal
        # remaining usable after its previous session leader exits on either OS.
        self.master, self.slave = os.openpty()
        self.addCleanup(os.close, self.master)
        self.addCleanup(os.close, self.slave)
        tty.setraw(self.slave)

    def stand_in(self, mode="pty"):
        self.allocate_pty()
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
            capture_output=True, start_new_session=True, timeout=10)
        self.assertEqual((result.returncode, result.stderr), (0, b""))
        if edge in ("session-start", "session-end"):
            self.assertEqual(result.stdout, b"", "direct delivery must not emit protocol JSON")
        return result.stdout


class UnixDeliveryTests(UnixDeliveryFixture):
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
                                    capture_output=True, start_new_session=True, timeout=10)
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

    def test_tmux_arm_delivers_exact_bytes_without_a_tmux_server(self):
        self.stand_in()
        result = subprocess.run([str(BIN), "tmux-arm", os.ttyname(self.slave)],
                                cwd=self.cwd, env=self.env, input=b"",
                                capture_output=True, start_new_session=True, timeout=10)
        self.assertEqual((result.returncode, result.stdout, result.stderr), (0, b"", b""))
        self.assertEqual(self.drain(), ARM)


@unittest.skipUnless(sys.platform == "darwin", "requires native Darwin open observation")
class DarwinTerminalOpenTests(UnixDeliveryFixture):
    @classmethod
    def setUpClass(cls):
        cls.native = tempfile.TemporaryDirectory(prefix="cctab-tty-observer-")
        cls.addClassCleanup(cls.native.cleanup)
        cls.dylib = Path(cls.native.name) / "observer.dylib"
        cls.control = Path(cls.native.name) / "control"
        source = str(ROOT / "tests/fixtures/darwin_tty_observer.c")
        for flags, output in ((["-dynamiclib"], cls.dylib),
                              (["-DOBSERVER_CONTROL"], cls.control)):
            subprocess.run(["cc", "-Wall", "-Wextra", "-Werror", *flags,
                            source, "-o", str(output)], check=True, capture_output=True)
        # The native CI job has already built the locked libc dependency. Reuse
        # it to expose API results under faults, without a production test switch.
        target = "aarch64-apple-darwin" if os.uname().machine == "arm64" else "x86_64-apple-darwin"
        build = Path(os.environ.get("CARGO_TARGET_DIR", ROOT / "target"))
        if not build.is_absolute():
            build = ROOT / build
        libraries = list((build / target).glob("*/deps/liblibc-*.rlib"))
        if not libraries:
            raise RuntimeError("build the native Rust target before testing the API contract")
        library = max(libraries, key=lambda p: p.stat().st_mtime_ns)
        cls.probe = Path(cls.native.name) / "contract"
        subprocess.run(["rustc", "--edition=2021", "--target", target,
                        str(ROOT / "tests/fixtures/unix_tty_contract.rs"),
                        "--extern", f"libc={library}", "-L", f"dependency={library.parent}",
                        "-o", str(cls.probe)], check=True, capture_output=True)

    def observed(self, command, process=None, fault="", binary=BIN, expected_stdout=b""):
        log = self.root / "open.jsonl"
        log.unlink(missing_ok=True)
        replacement = self.root / "replacement"
        replacement.write_bytes(b"")
        env = dict(self.env, DYLD_INSERT_LIBRARIES=str(self.dylib),
                   CCTAB_TEST_TTY_LOG=str(log), CCTAB_TEST_TTY_PATH=os.ttyname(self.slave),
                   CCTAB_TEST_TTY_FILE=str(replacement), CCTAB_TEST_TTY_FAULT=fault,
                   CCTAB_TERMINAL="konsole")
        if process is not None:
            env["CLAUDE_PID"] = str(process.pid)
        args = dict(cwd=self.cwd, env=env, input=b'{"session_id":"s1","source":"startup"}',
                    capture_output=True, start_new_session=True, timeout=10)
        if binary == self.control:
            # If this kernel does acquire the unowned PTY, close/exit can wait
            # for output to drain. Capture it while the control is still running.
            with ThreadPoolExecutor(max_workers=1) as pool:
                running = pool.submit(subprocess.run, [str(binary), *command], **args)
                self.assertTrue(select.select([self.master], [], [], 5)[0])
                self.control_bytes = os.read(self.master, 65536)
                result = running.result(timeout=10)
        else:
            result = subprocess.run([str(binary), *command], **args)
        self.assertEqual((result.returncode, result.stdout, result.stderr), (0, expected_stdout, b""))
        self.assertTrue(log.exists(), "native observer never ran")
        records = [json.loads(line) for line in log.read_text().splitlines()]
        return records

    @staticmethod
    def values(records, event):
        return [r["value"] for r in records if r["event"] == event]

    def check_open(self, records, opened=True):
        # Missing observations and absent flags both fail; byte transport alone
        # is not evidence of O_NOCTTY or of controlling-terminal state.
        flags = self.values(records, "open_flags")
        self.assertEqual(len(flags), 1, "must observe the production terminal open")
        self.assertTrue(flags[0] & os.O_NOCTTY, "actual open omitted O_NOCTTY")
        self.assertEqual(self.values(records, "noctty"), [1])
        for event in ("initial_ctty", "before_ctty", "after_ctty", "final_ctty", "stdio_tty", "unclosed"):
            self.assertEqual(self.values(records, event), [0], event)
        self.assertEqual(self.values(records, "session_leader"), [1])
        self.assertEqual(self.values(records, "open_result"), [int(opened)])
        self.assertEqual(self.values(records, "closed"), [1] if opened else [])
        self.assertEqual(self.values(records, "cloexec"), [1] if opened else [])

    def test_real_session_and_tmux_arm_opens_request_noctty(self):
        process = self.stand_in()
        for command, expected in ((["session-start"], ARM + TITLE),
                                  (["session-end"], RESTORE + CLEAR),
                                  (["tmux-arm", os.ttyname(self.slave)], ARM)):
            with self.subTest(command=command):
                records = self.observed(command, process)
                self.check_open(records)
                self.assertIn(1, self.values(records, "metadata"))
                self.assertEqual(self.values(records, "character"), [1])
                self.assertEqual(self.values(records, "terminal"), [1])
                self.assertEqual(self.drain(), expected)

    def test_rejected_acquired_descriptors_receive_no_bytes(self):
        process = self.stand_in()
        for fault in ("regular", "null", "metadata", "terminal"):
            for command in (["session-start"], ["tmux-arm", os.ttyname(self.slave)]):
                with self.subTest(fault=fault, command=command):
                    records = self.observed(command, process, fault)
                    self.check_open(records)
                    self.assertIn(1, self.values(records, "metadata"))
                    if fault == "regular":
                        self.assertEqual(self.values(records, "character"), [0])
                    elif fault == "null":
                        self.assertEqual(self.values(records, "character"), [1])
                        self.assertEqual(self.values(records, "terminal"), [0])
                    elif fault == "terminal":
                        self.assertEqual(self.values(records, "terminal_check"), [1])
                    self.assertEqual(self.values(records, "write_attempt"), [])
                    self.assertEqual(self.drain(), b"")
                    self.assertEqual((self.root / "replacement").read_bytes(), b"")
                    if command == ["session-start"]:
                        self.assertIn(b"s konsole\n", self.record.read_bytes())
                        # Refusal must retain the policy for a later successful
                        # restore, even with a different surface at SessionEnd.
                        self.hook("session-end", dict(self.env, CLAUDE_PID=str(process.pid)))
                        self.assertEqual(self.drain(), RESTORE + CLEAR)

    def test_open_and_write_faults_keep_partial_transport_and_restore_policy(self):
        process = self.stand_in()
        for fault in ("open", "write", "partial"):
            for command in (["session-start"], ["tmux-arm", os.ttyname(self.slave)]):
                with self.subTest(fault=fault, command=command):
                    records = self.observed(command, process, fault)
                    self.check_open(records, opened=fault != "open")
                    if fault != "open":
                        self.assertIn(1, self.values(records, "metadata"))
                        self.assertEqual(self.values(records, "terminal"), [1])
                        self.assertEqual(self.values(records, "write_result"), [3] if fault == "partial" else [])
                    self.assertEqual(self.drain(), ARM[:3] if fault == "partial" else b"")
                    if command == ["session-start"]:
                        self.assertIn(b"s konsole\n", self.record.read_bytes())
                        self.hook("session-end", dict(self.env, CLAUDE_PID=str(process.pid)))
                        self.assertEqual(self.drain(), RESTORE + CLEAR)

    def test_observer_controls_fail_for_missing_open_and_missing_noctty(self):
        # No session owns this extra PTY. The hardening must also work when a
        # detached tmux-arm child opens an unowned terminal.
        self.allocate_pty()
        records = self.observed(["tmux-arm", os.ttyname(self.slave)])
        self.check_open(records)
        self.assertEqual(self.drain(), ARM)
        # An empty observation cannot pass even when byte expectations are empty.
        with self.assertRaises(AssertionError):
            self.check_open([])
        records = self.observed([os.ttyname(self.slave)], binary=self.control)
        self.assertEqual(self.values(records, "noctty"), [0])
        with self.assertRaises(AssertionError):
            self.check_open(records)
        self.assertEqual(self.control_bytes + self.drain(), b"control")
        # Report what this kernel did. Acquisition is not required for the
        # negative control: many Darwin PTY opens leave ctty unchanged.
        print("Darwin ordinary PTY open ctty before/after:",
              self.values(records, "before_ctty"), self.values(records, "after_ctty"))

    def test_native_api_refusal_and_inspection_error_mapping(self):
        process = self.stand_in()
        for route, destination in (("session", str(process.pid)),
                                   ("write", os.ttyname(self.slave))):
            faults = ("", "regular", "null", "metadata", "terminal", "open")
            if route == "write":
                faults += ("write", "partial")
            for fault in faults:
                with self.subTest(route=route, fault=fault):
                    if route == "session":
                        outcome = b"none\n" if fault else b"some\n"
                    elif fault in ("regular", "null", "terminal"):
                        outcome = b"false\n"
                    elif fault == "open":
                        outcome = f"error:{errno.EACCES}\n".encode()
                    elif fault in ("metadata", "write", "partial"):
                        outcome = f"error:{errno.EIO}\n".encode()
                    else:
                        outcome = b"true\n"
                    records = self.observed([route, destination], process, fault,
                                            binary=self.probe, expected_stdout=outcome)
                    self.check_open(records, opened=fault != "open")
                    expected_bytes = b""
                    if route == "write" and not fault:
                        expected_bytes = b"probe"
                    elif fault == "partial":
                        expected_bytes = b"pro"
                    self.assertEqual(self.drain(), expected_bytes)


if __name__ == "__main__":
    unittest.main()
