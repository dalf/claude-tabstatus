#!/usr/bin/env python3
"""Real tmux window-status and restoration regressions on private servers.

CCTAB_TEST_BIN=/path/to/tabstatus python3 tests/test_tmux_status.py
CCTAB_TEST_TMUX=/path/to/tmux selects another tmux build for every invocation.
CCTAB_TEST_REQUIRE_TMUX=1 makes missing prerequisites a failure (required in CI).
Every test owns a unique socket, isolated HOME and disposable shell panes.
Forced terminal rows exercise protocols, not real terminal applications.
"""
import contextlib
import errno
import fcntl
from concurrent.futures import ThreadPoolExecutor
import json
import locale
import os
from pathlib import Path
import pty
import re
import select
import signal
import shutil
import struct
import subprocess
import sys
import tempfile
import termios
import time
import unittest


ROOT = Path(__file__).resolve().parents[1]
BIN = Path(os.environ.get("CCTAB_TEST_BIN", ROOT / "bin/tabstatus")).resolve()
TMUX_OVERRIDE = os.environ.get("CCTAB_TEST_TMUX")
TMUX = TMUX_OVERRIDE or shutil.which("tmux")
REQUIRE_TMUX = os.environ.get("CCTAB_TEST_REQUIRE_TMUX") == "1"
FORMATS = ("window-status-format", "window-status-current-format")
# Exact rounded-pill formats from the reported display regression. The glyph
# belongs inside the colored body, alongside the existing index and label.
PILL_FORMATS = (
    "#[fg=#374151,bg=#1f2329]#[fg=#ffffff,bg=#374151,bold] #I:#W #[fg=#374151,bg=#1f2329] ",
    "#[fg=#0ea5e9,bg=#1f2329]#[fg=#0b1220,bg=#0ea5e9,bold] #I:#W #[fg=#0ea5e9,bg=#1f2329]",
)
MARKED_PILL_FORMATS = tuple(fmt.replace("#I:#W", "#{T:@cctab_window_strip} #I:#W")
                            for fmt in PILL_FORMATS)
SAVED = ("@cctab_window_format_saved", "@cctab_prev_window_format",
         "@cctab_prev_window_format_local", "@cctab_window_current_saved",
         "@cctab_prev_window_current", "@cctab_prev_window_current_local")
ARM = b"\x1b]50;LocalTabTitleFormat=%w;RemoteTabTitleFormat=%w\x07"
RESTORE = b"\x1b]50;LocalTabTitleFormat=%d : %n;RemoteTabTitleFormat=(%u) %H\x07"


def terminal_text_and_backgrounds(data, foreground=False):
    """Remove terminal controls while retaining each printed character's SGR background."""
    # A PTY read may end mid-OSC. Hide that unfinished title as well, so its
    # payload cannot satisfy a status assertion before the terminator arrives.
    data = re.sub(rb"\x1b\].*?(?:\x07|\x1b\\|$)", b"", data, flags=re.S)
    text, backgrounds = [], []
    background = None
    reset, base, bright, extended = (39, 30, 90, 38) if foreground else (49, 40, 100, 48)
    for token in re.findall(r"\x1b\[[0-?]*[ -/]*[@-~]|.", data.decode(errors="replace"), re.S):
        if token.startswith("\x1b["):
            if re.fullmatch(r"\x1b\[[0-9;]*m", token):
                codes = [int(n or "0") for n in token[2:-1].split(";")]
                index = 0
                while index < len(codes):
                    code = codes[index]
                    if code in (0, reset):
                        background = None
                    elif base <= code <= base + 7 or bright <= code <= bright + 7:
                        background = (code,)
                    elif code in (38, 48) and index + 1 < len(codes):
                        count = {2: 3, 5: 1}.get(codes[index + 1], 0)
                        if count and index + 1 + count < len(codes):
                            if code == extended:
                                background = tuple(codes[index + 1:index + 2 + count])
                            index += 1 + count
                    index += 1
            continue
        text.append(token)
        backgrounds.append(background)
    return "".join(text), backgrounds


@unittest.skipUnless(REQUIRE_TMUX or TMUX, "tmux is required for window-status integration tests")
class TmuxStatusTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        if not TMUX:
            raise RuntimeError("required tmux acceptance cannot run: tmux is unavailable")
        selected = Path(TMUX)
        if not selected.is_absolute() or not selected.is_file() or not os.access(selected, os.X_OK):
            raise RuntimeError("CCTAB_TEST_TMUX must be an absolute executable path")
        if not BIN.is_file() or not os.access(BIN, os.X_OK):
            raise RuntimeError(f"required compiled binary is unavailable: {BIN}")
        cls.tmux = str(selected.resolve())
        # Darwin does not supply Linux's C.UTF-8. Validate a native UTF-8 locale
        # before starting the server; do not silently fall back to ASCII widths.
        cls.utf8_locale = "en_US.UTF-8" if sys.platform == "darwin" else "C.UTF-8"
        previous = locale.setlocale(locale.LC_CTYPE)
        try:
            locale.setlocale(locale.LC_CTYPE, cls.utf8_locale)
            if locale.nl_langinfo(locale.CODESET).upper().replace("-", "") != "UTF8":
                raise RuntimeError(f"not a UTF-8 locale: {cls.utf8_locale}")
        finally:
            locale.setlocale(locale.LC_CTYPE, previous)
        version = subprocess.run([cls.tmux, "-V"], check=True, capture_output=True,
                                 text=True, timeout=10).stdout.strip()
        print(f"tmux acceptance: {version}; {os.uname().sysname} {os.uname().machine}; "
              f"locale={cls.utf8_locale}; binary={BIN}", flush=True)

    def setUp(self):
        # Darwin TMPDIR can nearly fill sockaddr_un.sun_path (104 bytes).
        # Own a short socket beneath /tmp, canonicalising its /private alias.
        self.tmp = tempfile.TemporaryDirectory(prefix="cctm-", dir="/tmp")
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name).resolve()
        self.socket = self.root / "tmux.sock"
        self.assertLess(len(os.fsencode(self.socket)), 104)
        # Fresh environment: no ambient terminal, mux, SSH, PID or Darwin TMPDIR
        # state fallback. Pin state storage; stateless controls clear the knob.
        self.env = {"PATH": "/usr/bin:/bin", "HOME": str(self.root),
                    "CLAUDE_CONFIG_DIR": str(self.root / "config"),
                    "XDG_DATA_HOME": str(self.root / "data"),
                    "CCTAB_STATE_DIR": str(self.root / "state"),
                    "TERM": "xterm-256color", "LC_ALL": self.utf8_locale, "PS1": ""}
        # The binary's cold paths invoke tmux by name; Homebrew is outside the
        # isolated /usr/bin:/bin PATH even when the harness has found it.
        self.env["PATH"] = str(Path(self.tmux).parent) + os.pathsep + self.env["PATH"]
        self.base = [self.tmux, "-S", str(self.socket), "-f", "/dev/null"]
        self.addCleanup(lambda: subprocess.run(self.base + ["kill-server"], env=self.env,
                                              stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
                                              timeout=10))
        self.pane = self.tm("new-session", "-d", "-s", "alpha", "-n", "current",
                            "-x", "160", "-y", "24", "-P", "-F", "#{pane_id}", "/bin/sh")
        self.tm("set", "-g", "status-left", "")
        self.tm("set", "-g", "status-right", "")
        self.tm("set", "-g", "status-interval", "1")
        self.tm("set", "-gw", FORMATS[0], "N:#W")
        self.tm("set", "-gw", FORMATS[1], "C:#W")

    def tm(self, *args):
        p = subprocess.run(self.base + list(args), env=self.env, cwd=self.root,
                           capture_output=True, text=True, timeout=10)
        self.assertEqual(p.returncode, 0, f"tmux {args!r}: {p.stderr}")
        return p.stdout.removesuffix("\n")

    def hook(self, edge, pane=None, extra_env=None, payload=None):
        env = dict(self.env, TMUX=f"{self.socket},1,0", CLAUDE_PID="0",
                   CCTAB_TERMINAL="other", CCTAB_GLYPH_POS="prefix")
        if pane is not None:
            env["TMUX_PANE"] = pane
        env.update(extra_env or {})
        data = json.dumps(payload).encode() if payload is not None else b""
        p = subprocess.run([str(BIN), edge], input=data, env=env, cwd=self.root,
                           capture_output=True, timeout=10)
        self.assertEqual((p.returncode, p.stderr), (0, b""))
        return p.stdout

    def start(self, pane=None, **extra_env):
        return self.hook("session-start", pane or self.pane, extra_env)

    def uninstall(self, pane=None):
        env = dict(self.env, TMUX=f"{self.socket},1,0", TMUX_PANE=pane or self.pane,
                   CCTAB_STATE_DIR=str(self.root / "state"), CLAUDE_PID="0")
        p = subprocess.run([str(BIN), "uninstall", "--force"], env=env, cwd=self.root,
                           capture_output=True, timeout=10)
        self.assertEqual(p.returncode, 0, p.stderr)
        return p.stdout.decode()

    def new_window(self, name, session="alpha"):
        return self.tm("new-window", "-d", "-t", session, "-n", name,
                       "-P", "-F", "#{pane_id}", "/bin/sh")

    def local(self, pane, option):
        # The listing without -A contains explicit locals only. A literal empty
        # value and an inherited value must remain distinguishable on restore.
        listing = self.tm("show-options", "-w", "-t", pane)
        present = any(line.split(" ", 1)[0] == option for line in listing.splitlines())
        value = self.tm("show-options", "-wqv", "-t", pane, option) if present else None
        return present, value

    def formats(self, pane):
        return tuple(self.local(pane, option) for option in FORMATS)

    def globals(self):
        return tuple(self.tm("show-options", "-gwv", option) for option in FORMATS)

    def rendered(self, pane, current=False):
        return self.tm("display-message", "-p", "-t", pane,
                       "#{E:" + FORMATS[int(current)] + "}")

    def wait_for(self, getter, expected, timeout=5):
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            actual = getter()
            if actual == expected:
                return
            time.sleep(0.025)
        self.assertEqual(actual, expected,
                         f"observation timed out; socket={self.socket}; "
                         f"panes={self.tm('list-panes', '-a', '-F', '#{pane_id}|#{pane_pid}|#{pane_tty}|#{pane_title}')!r}")

    @contextlib.contextmanager
    def attached_client(self, session="alpha"):
        """A real tmux client on a pty THIS process owns both ends of.

        The appearance bytes go to the ptys `list-clients` names, never through
        tmux's allow-passthrough, so there is no other way to see them: the client
        writes its redraw into the slave and the plugin writes its OSC 50 into the
        same slave, and both come back out of the master interleaved. Reading has
        to keep going the whole time, because a full pty buffer blocks the tmux
        client.
        """
        master, slave = pty.openpty()
        try:
            fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 24, 160, 0, 0))
            client = subprocess.Popen(self.base + ["attach-session", "-t", session],
                                      env=self.env, cwd=self.root,
                                      stdin=slave, stdout=slave, stderr=slave,
                                      start_new_session=True)
        except BaseException:
            os.close(master)
            raise
        finally:
            os.close(slave)

        class Client:
            captured = b""
            eof = False

            def read(self, budget=0.1):
                if select.select([master], [], [], budget)[0]:
                    try:
                        data = os.read(master, 65536)
                        self.captured += data
                        self.eof = not data
                    except OSError as error:
                        if error.errno != errno.EIO:
                            raise
                        self.eof = True

            def saw(self, wanted, timeout=5, status=False):
                deadline = time.monotonic() + timeout
                while time.monotonic() < deadline:
                    observed = terminal_text_and_backgrounds(self.captured)[0] if status else self.captured
                    if wanted in observed:
                        return True
                    if self.eof:
                        break
                    self.read()
                observed = terminal_text_and_backgrounds(self.captured)[0] if status else self.captured
                return wanted in observed

            def clear(self):
                deadline = time.monotonic() + 1
                while (time.monotonic() < deadline and not self.eof
                       and select.select([master], [], [], 0)[0]):
                    self.read(0)
                self.captured = b""

            def send(self, data):
                os.write(master, data)

            def diagnostic(self):
                return f"client exit={client.poll()}; tty={client_tty}; bytes={self.captured[-4096:]!r}"

        watcher = Client()
        try:
            deadline = time.monotonic() + 5
            while time.monotonic() < deadline:
                if client.poll() is not None:
                    break
                if self.tm("list-clients", "-t", session, "-F", "#{client_tty}"):
                    break
                watcher.read(0.05)
            client_tty = self.tm("list-clients", "-t", session, "-F", "#{client_tty}")
            self.assertTrue(client_tty,
                            f"attachment timed out: exit={client.poll()}; socket={self.socket}; "
                            f"bytes={watcher.captured[-4096:]!r}")
            watcher.tty = client_tty
            yield watcher
        finally:
            client.terminate()
            try:
                client.wait(timeout=3)
            except subprocess.TimeoutExpired:
                client.kill()
                client.wait(timeout=3)
            os.close(master)

    def assert_carrier(self, pane, state, epoch, timeout=5):
        self.wait_for(lambda: self.tm("display-message", "-p", "-t", pane,
                                     "#{pane_title}").rsplit(" ct1 ", 1)[-1],
                      f"{state} {epoch}", timeout=timeout)

    def assert_status(self, client, wanted, timeout=5):
        self.assertTrue(client.saw(wanted, timeout=timeout, status=True),
                        f"status {wanted!r} missing: {client.diagnostic()}")

    def redraw_status(self, client):
        # Forget old bytes, then request a full client redraw. A status-only
        # refresh (-S) can emit nothing when tmux's cached status is unchanged.
        client.clear()
        self.tm("refresh-client", "-t", client.tty)

    def publish(self, pane, edge="working", age=0):
        # Invoke the real hook with the disposable pane shell as Claude's tty
        # owner. Replaying JSON ourselves would hide a broken hook transport.
        epoch = int(self.tm("display-message", "-p", "%s")) - age
        pid = self.tm("display-message", "-p", "-t", pane, "#{pane_pid}")
        output = self.hook(edge, pane, {"CCTAB_NOW": str(epoch), "CLAUDE_PID": pid})
        self.assertEqual(output, b"", "tmux hook updates must be delivered directly to the pane tty")
        state = {"working": "w", "waiting": "a", "idle": "i"}[edge]
        self.assert_carrier(pane, state, epoch)

    def test_real_hook_updates_need_no_stdout_protocol_consumer(self):
        self.start()
        for edge, glyph in (("working", "🔵"), ("waiting", "🟠"), ("idle", "⚪")):
            self.publish(self.pane, edge)
            self.assertEqual(self.rendered(self.pane, True), glyph + " C:current")

    def test_delivery_and_status_observers_reject_missing_and_broken_carriers(self):
        self.start()
        self.tm("select-pane", "-t", self.pane, "-T", "no-carrier")
        epoch = int(self.tm("display-message", "-p", "%s"))
        # A successful no-op hook cannot satisfy the positive carrier observer.
        self.assertEqual(self.hook("working", self.pane, {"CLAUDE_PID": "0", "CCTAB_NOW": str(epoch)}), b"")
        with self.assertRaises(AssertionError):
            self.assert_carrier(self.pane, "w", epoch, timeout=0.15)
        # This deliberately corrupt control is not a candidate paint. The real
        # delivery test above always invokes the compiled hook for its carriers.
        self.tm("select-pane", "-t", self.pane, "-T", f"project ct9 w {epoch}")
        with self.attached_client() as client:
            self.assert_status(client, "C:current")
            with self.assertRaises(AssertionError):
                self.assert_status(client, "🔵 C:current", timeout=0.2)
            # Even matching outer-title bytes must not pass a status assertion.
            for terminator in ("\x07", "\x1b\\", ""):
                client.captured = ("\x1b]0;🔵 C:current" + terminator).encode()
                with self.assertRaises(AssertionError):
                    self.assert_status(client, "🔵 C:current", timeout=0)
            client.clear()
            self.publish(self.pane)
            self.assert_status(client, "🔵 C:current")

    def test_cached_status_redraw_delivers_fresh_bytes_after_capture_clear(self):
        # Keep the already displayed status unchanged, with no periodic redraw
        # or hook activity. The after-hook marks execution without setting a
        # tmux option (which itself would invalidate the cached display).
        self.tm("set", "-g", "status-interval", "0")
        self.tm("set", "-w", "-t", self.pane, "automatic-rename", "off")
        marker = self.root / "refresh-seen"
        self.tm("set-hook", "-g", "after-refresh-client", f'run-shell "touch {marker}"')
        with self.attached_client() as client:
            # Answer the actual extended device-attributes query. Otherwise
            # tmux's startup query timeout can cause an unrelated full redraw
            # and rescue the old observer. Require tmux to acknowledge the reply.
            self.assertTrue(client.saw(b"\x1b[>q"), client.diagnostic())
            client.clear()
            client.send(b"\x1bP>|cctab-status-observer\x1b\\")
            self.wait_for(lambda: self.tm("display-message", "-p", "-c", client.tty,
                                          "#{client_termtype}"), "cctab-status-observer")
            self.assert_status(client, "C:current")
            client.clear()
            with self.assertRaises(AssertionError):
                self.assert_status(client, "C:current", timeout=0.2)
            # Reproduce the old observer's false failure, and require proof that
            # it reached refresh-client rather than failing before attachment.
            self.tm("refresh-client", "-S", "-t", client.tty)
            self.wait_for(marker.exists, True)
            with self.assertRaises(AssertionError):
                self.assert_status(client, "C:current", timeout=0.2)
            marker.unlink()
            self.redraw_status(client)
            self.wait_for(marker.exists, True)
            self.assert_status(client, "C:current")

    def test_status_redraw_rejects_stale_capture_when_delivery_is_disabled(self):
        self.tm("set", "-g", "status-interval", "0")
        marker = self.root / "refresh-seen"
        self.tm("set-hook", "-g", "after-refresh-client", f'run-shell "touch {marker}"')
        with self.attached_client() as client:
            self.assert_status(client, "C:current")
            stale = client.captured
            self.tm("set", "-g", "status", "off")
            client.captured = stale
            self.redraw_status(client)
            self.wait_for(marker.exists, True)
            with self.assertRaises(AssertionError):
                self.assert_status(client, "C:current", timeout=0.2)

    def test_attached_status_shows_states_and_expires_on_tmux_clock_without_hooks(self):
        self.start(CCTAB_TTL_WORKING="2", CCTAB_TTL_WAITING="2", CCTAB_TTL_GONE="4")
        with self.attached_client() as client:
            for edge, glyph in (("working", "🔵"), ("waiting", "🟠"), ("idle", "⚪")):
                client.clear()
                self.publish(self.pane, edge)
                self.assert_status(client, glyph + " C:current")
            client.clear()
            self.publish(self.pane)
            self.assert_status(client, "🔵 C:current")
            carrier = self.tm("display-message", "-p", "-t", self.pane, "#{pane_title}")
            self.assert_status(client, "⚪ C:current", timeout=7)
            self.wait_for(lambda: self.rendered(self.pane, True), "C:current", timeout=7)
            # A full redraw after decay makes absence observable independently
            # of tmux's cached status and cursor-delta optimisation. No hook runs.
            self.redraw_status(client)
            self.assert_status(client, "C:current")
            self.assertNotRegex(terminal_text_and_backgrounds(client.captured)[0], "[🔵🟠⚪]")
            self.assertEqual(self.tm("display-message", "-p", "-t", self.pane, "#{pane_title}"), carrier)
            # Known background remains visible past all display deadlines.
            pid = self.tm("display-message", "-p", "-t", self.pane, "#{pane_pid}")
            old = str(int(self.tm("display-message", "-p", "%s")) - 86400)
            client.clear()
            self.assertEqual(self.hook("idle", self.pane,
                                      {"CLAUDE_PID": pid, "CCTAB_NOW": old,
                                       "CCTAB_STATE_DIR": str(self.root / "state")},
                                      {"session_id": "bg", "hook_event_name": "Stop", "background_tasks": [{}]}), b"")
            self.assert_status(client, "🟣 C:current")

    def test_background_survives_decay_and_updates_waiting_fallback_in_both_directions(self):
        self.start(CCTAB_TTL_WORKING="1", CCTAB_TTL_WAITING="1", CCTAB_TTL_GONE="1")
        epoch = int(self.tm("display-message", "-p", "%s")) - 365 * 86400
        pid = self.tm("display-message", "-p", "-t", self.pane, "#{pane_pid}")
        env = {"CLAUDE_PID": pid, "CCTAB_STATE_DIR": str(self.root / "state"), "CCTAB_NOW": str(epoch), "CCTAB_TTL_WAITING": "0"}

        def emit(edge, event, state, **fields):
            self.hook(edge, self.pane, env, {"session_id": "s1", "hook_event_name": event, **fields})
            self.wait_for(lambda: self.tm("display-message", "-p", "-t", self.pane, "#{pane_title}").split()[-2], state)

        request = {"mcp_server_name": "server", "elicitation_id": "request", "mode": "form"}
        emit("elicitation", "Elicitation", "a", **request)
        emit("idle", "Stop", "A", background_tasks=[{"type": "workflow"}])
        self.assertEqual(self.rendered(self.pane, True), "🟣 C:current")
        # Missing metadata preserves both the request and the background fact.
        emit("idle", "Stop", "A")
        emit("elicitation-result", "ElicitationResult", "p", action="accept", **request)
        self.assertEqual(self.rendered(self.pane, True), "🟣 C:current")
        emit("working", "UserPromptSubmit", "W", prompt="progress?")
        self.assertEqual(self.rendered(self.pane, True), "🟣 C:current")
        self.assertIn("🟣", self.tm("display-message", "-p", "-t", self.pane, "#{T:@cctab_title}"))
        request["elicitation_id"] = "next"
        emit("elicitation", "Elicitation", "A", **request)
        env["CCTAB_NOW"] = self.tm("display-message", "-p", "%s")
        emit("idle", "Stop", "a", background_tasks=[])
        self.assertEqual(self.rendered(self.pane, True), "🟠 C:current")
        emit("elicitation-result", "ElicitationResult", "i", action="accept", **request)
        self.assertEqual(self.rendered(self.pane, True), "⚪ C:current")

    def test_ct1_and_ct2_panes_coexist_with_custom_background_glyph_and_theme(self):
        self.tm("source-file", str(ROOT / "examples/tmux.conf"))
        self.start(CCTAB_GLYPH_BACKGROUND="BG")
        other = self.new_window("legacy")
        self.start(other, CCTAB_GLYPH_BACKGROUND="BG")
        self.publish(other, "working")
        pid = self.tm("display-message", "-p", "-t", self.pane, "#{pane_pid}")
        epoch = self.tm("display-message", "-p", "%s")
        self.hook("idle", self.pane, {"CLAUDE_PID": pid, "CCTAB_STATE_DIR": str(self.root / "state"),
                                     "CCTAB_NOW": epoch},
                  {"session_id": "s1", "hook_event_name": "Stop", "background_tasks": [{"type": "workflow"}]})
        self.wait_for(lambda: self.tm("display-message", "-p", "-t", self.pane, "#{pane_title}").split()[-3:],
                      ["ct2", "p", epoch])
        self.assertEqual(self.tm("display-message", "-p", "-t", self.pane, "#{T:@cctab_window_strip}"), "BG")
        label = self.tm("display-message", "-p", "-t", self.pane, "#{E:@claude_window_label}")
        self.assertNotIn("ct2", label)
        self.assertNotEqual(label, "current")
        outer = self.tm("display-message", "-p", "-t", self.pane, "#{T:@cctab_title}")
        self.assertIn("BG", outer)
        self.assertIn("🔵", outer)

    def test_doctor_reports_consumers_that_do_not_understand_background(self):
        self.start()
        env = dict(self.env, TMUX=f"{self.socket},1,0", TMUX_PANE=self.pane, CLAUDE_PID="0")
        def doctor():
            return subprocess.run([str(BIN), "doctor"], env=env, cwd=self.root,
                                  capture_output=True, text=True, timeout=10).stdout
        self.assertIn("background: OK", doctor())
        self.tm("set", "-s", "@cctab_window_strip", "old ct1 format")
        self.assertIn("background: WARN", doctor())
        self.start()
        self.assertIn("background: OK", doctor())

    @unittest.skipIf(os.geteuid() == 0, "root bypasses read-only record permissions")
    def test_unwritable_record_keeps_background_in_the_waiting_carrier(self):
        self.start(CCTAB_TTL_WAITING="1", CCTAB_TTL_GONE="1")
        epoch = str(int(self.tm("display-message", "-p", "%s")) - 1000)
        env = {"CCTAB_STATE_DIR": str(self.root / "state"), "CCTAB_NOW": epoch,
               "CLAUDE_PID": self.tm("display-message", "-p", "-t", self.pane, "#{pane_pid}")}
        self.hook("idle", self.pane, env, {"session_id": "s1", "hook_event_name": "Stop", "background_tasks": [{}]})
        record = self.root / "state" / "s1"
        before = record.read_bytes()
        record.chmod(0o400)
        try:
            for edge, event, fields in [("waiting", "PermissionRequest", {"agent_id": "child"}),
                                        ("elicitation", "Elicitation", {"mode": "form", "mcp_server_name": "s", "elicitation_id": "r"})]:
                self.hook(edge, self.pane, env, {"session_id": "s1", "hook_event_name": event, **fields})
                self.wait_for(lambda: self.tm("display-message", "-p", "-t", self.pane, "#{pane_title}").split()[-3:],
                              ["ct2", "A", epoch])
                self.assertEqual(self.rendered(self.pane, True), "🟣 C:current")
                self.assertEqual(record.read_bytes(), before)
        finally:
            record.chmod(0o600)

    def test_window_color_uses_all_panes_priority_and_shared_decay(self):
        self.start(CCTAB_TTL_WORKING="2", CCTAB_TTL_WAITING="2", CCTAB_TTL_GONE="4",
                   CCTAB_GLYPH_WORKING="", CCTAB_GLYPH_WAITING="custom")
        other = self.tm("split-window", "-d", "-t", self.pane, "-P", "-F", "#{pane_id}", "/bin/sh")
        elsewhere = self.new_window("separate")
        epoch = int(self.tm("display-message", "-p", "%s"))
        def carrier(pane, state, age=0):
            tag = "ct2" if state in "pWA" else "ct1"
            self.tm("select-pane", "-t", pane, "-T", f"project {tag} {state} {epoch-age}")
        def color(pane=self.pane):
            return self.tm("display-message", "-p", "-t", pane, "#{T:@cctab_window_color}")
        carrier(self.pane, "i")
        self.assertEqual(color(), "#e5e7eb")
        carrier(other, "p", 365 * 86400)
        self.assertEqual(color(), "#c084fc")
        carrier(self.pane, "w")
        self.assertEqual(color(), "#60a5fa")
        carrier(other, "A")
        self.assertEqual(color(), "#fb923c")
        # Selection and a different window do not hide this inactive pane's wait.
        self.tm("select-pane", "-t", self.pane)
        carrier(elsewhere, "p")
        self.assertEqual(color(), "#fb923c")
        self.assertEqual(color(elsewhere), "#c084fc")
        # Both foreground carriers age, but A and W retain known background.
        carrier(self.pane, "w", 10)
        carrier(other, "A", 10)
        self.assertEqual(color(), "#c084fc")
        carrier(other, "W", 10)
        self.assertEqual(color(), "#c084fc")
        carrier(other, "a", 3)
        self.assertEqual(color(), "#e5e7eb")
        carrier(other, "i", 10)
        self.assertEqual(color(), "")
        self.tm("select-pane", "-t", other, "-T", "shell ct9 a 123")
        self.assertEqual(color(), "")

    def test_color_theme_has_no_extra_dots_and_survives_start_and_uninstall(self):
        self.tm("source-file", str(ROOT / "examples/tmux.conf"))
        before = self.globals()
        self.start()
        self.start()
        self.assertEqual(self.globals(), before)
        self.assertEqual(self.formats(self.pane), ((False, None), (False, None)))
        self.assertIn("window list: OK", self.hook("doctor", self.pane).decode())
        self.publish(self.pane, "waiting")
        for current in (False, True):
            rendered = self.rendered(self.pane, current)
            self.assertIn("#[fg=#fb923c,bg=#1f2329]", rendered)
            self.assertNotRegex(rendered, "[🔵🟠🟣⚪]")
        self.assertIn("🟠", self.tm("display-message", "-p", "-t", self.pane, "#{T:@cctab_title}"))
        label = self.tm("display-message", "-p", "-t", self.pane, "#{E:@claude_window_label}")
        self.uninstall()
        self.assertEqual(self.globals(), before)
        self.assertEqual(self.tm("show-options", "-sqv", "@cctab_window_color"), "")
        self.assertIn("#[fg=#303641,bg=#1f2329]", self.rendered(self.pane))
        self.assertIn("#[fg=#e5e7eb,bg=#1f2329]", self.rendered(self.pane, True))
        self.assertIn(label, self.rendered(self.pane, True))

    def test_actual_status_bar_draws_a_wide_left_end_in_the_status_color(self):
        self.tm("source-file", str(ROOT / "examples/tmux.conf"))
        self.start()
        self.publish(self.pane, "waiting")
        fragment = "   0:"
        with self.attached_client() as client:
            self.assert_status(client, fragment)
            self.assert_status(client, "")
            clean, foregrounds = terminal_text_and_backgrounds(client.captured, foreground=True)
            left = clean.index(fragment)
            right = clean.index("", left)
            _, backgrounds = terminal_text_and_backgrounds(client.captured)
            self.assertEqual(backgrounds[left + 1:left + 3], [(2, 251, 146, 60)] * 2)
            self.assertEqual(backgrounds[left + 3], (2, 229, 231, 235))
            self.assertEqual(foregrounds[left], (2, 251, 146, 60))
            self.assertEqual(foregrounds[right], (2, 229, 231, 235))
            self.assertNotRegex(clean[left:right], "[🔵🟠🟣⚪]")

    def test_the_armed_record_drives_the_restore_when_the_terminal_changes(self):
        """THE DEFECT, byte for byte, on a real client pty.

        `session_end` used to decide whether to restore the outer tab by
        re-deriving the arming condition from ITS OWN environment, an unbounded
        time after `session_start` derived it from the start hook's. Change
        CCTAB_TERMINAL in between - or land in a shell whose rc sets it
        differently - and the end hook wrote nothing at all, leaving Konsole's
        `LocalTabTitleFormat=%w` in force with nothing left that would ever put it
        back. What is armed is now RECORDED in `@cctab_armed`, and the restore is
        driven by the record.

        The arming bytes go to the ptys `list-clients` names, so seeing them needs
        a real client attached to a real pty. tests/run.sh asserts the decision and the
        record; this asserts what actually reaches the terminal.
        """
        arm = b"\x1b]50;LocalTabTitleFormat=%w;RemoteTabTitleFormat=%w\x07"
        restore = b"\x1b]50;LocalTabTitleFormat=%d : %n;RemoteTabTitleFormat=(%u) %H\x07"
        with self.attached_client() as client:
            self.hook("session-start", self.pane, {"CCTAB_TERMINAL": "konsole"})
            self.assertTrue(client.saw(arm), "the arming never reached the client's pty")
            self.assertEqual(self.tm("display-message", "-p", "-t", self.pane,
                                     "#{@cctab_armed}"), "konsole")
            # The terminal this hook can see is now WezTerm, which arms nothing.
            # Before the record that was the whole input to the decision.
            self.hook("session-end", self.pane, {"CCTAB_TERMINAL": "wezterm"})
            self.assertTrue(client.saw(restore), "the restore was lost with CCTAB_TERMINAL")
            self.assertEqual(self.tm("display-message", "-p", "-t", self.pane,
                                     "#{@cctab_armed}"), "")

    def test_shared_arming_survives_other_starts_and_either_exit_order(self):
        """Forced Konsole/WezTerm rows: shared protocol ownership, not GUI evidence."""
        arm = b"\x1b]50;LocalTabTitleFormat=%w;RemoteTabTitleFormat=%w\x07"
        restore = b"\x1b]50;LocalTabTitleFormat=%d : %n;RemoteTabTitleFormat=(%u) %H\x07"
        other = self.new_window("second")
        panes = (self.pane, other)
        original_formats = {pane: self.formats(pane) for pane in panes}
        original_titles = tuple(self.tm("show-options", "-gv", name)
                                for name in ("set-titles", "set-titles-string"))

        def option(name):
            return self.tm("display-message", "-p", "-t", self.pane, "#{" + name + "}")

        for state_record in (False, True):
            for second_terminal in ("wezterm", "konsole"):
                for first_end in (0, 1):
                    with self.subTest(state_record=state_record, second_terminal=second_terminal,
                                      first_end=first_end), self.attached_client() as client:
                        envs = [{"CLAUDE_PID": self.tm("display-message", "-p", "-t", pane,
                                                       "#{pane_pid}"),
                                 "CCTAB_TERMINAL": terminal}
                                for pane, terminal in zip(panes, ("konsole", second_terminal))]
                        for env in envs:
                            env["CCTAB_STATE_DIR"] = str(self.root / "state") if state_record else ""
                        for index, (pane, env) in enumerate(zip(panes, envs)):
                            self.hook("session-start", pane, env,
                                      {"session_id": f"s{index}", "source": "startup"})
                            # Real carriers make the other-pane lifetime check observable.
                            self.wait_for(lambda: self.tm("display-message", "-p", "-t", pane,
                                                          "#{pane_title}").split()[-2:-1], ["i"])
                            self.assertTrue(client.saw(arm))
                            self.assertEqual(option("@cctab_armed"), "konsole")
                        if state_record:
                            self.assertIn("s konsole", (self.root / "state" / "s0").read_text().splitlines())
                        rearm = option("client-attached[1971]")
                        self.assertIn("tmux-arm", rearm)
                        installed_formats = {pane: self.formats(pane) for pane in panes}
                        for step, index in enumerate((first_end, 1 - first_end)):
                            # Neither end hook can guess Konsole from its environment.
                            self.hook("session-end", panes[index],
                                      dict(envs[index], CCTAB_TERMINAL="wezterm"),
                                      {"session_id": f"s{index}"})
                            self.wait_for(lambda: self.tm("display-message", "-p", "-t", panes[index],
                                                          "#{pane_title}"), "")
                            # SessionEnd retires appearance ownership; the shared
                            # server renderer stays until explicit uninstall.
                            self.assertEqual({pane: self.formats(pane) for pane in panes}, installed_formats)
                            if step == 0:
                                self.assertFalse(client.saw(restore, timeout=0.5), "restored too early")
                                self.assertEqual(option("@cctab_armed"), "konsole")
                                self.assertEqual(option("client-attached[1971]"), rearm)
                            else:
                                self.assertTrue(client.saw(restore), "lost the shared restore")
                                self.assertEqual(client.captured.count(restore), 1)
                                self.assertEqual(option("@cctab_armed"), "")
                                self.assertEqual(option("client-attached[1971]"), "")
                    self.uninstall()
                    self.assertEqual({pane: self.formats(pane) for pane in panes}, original_formats)
                    self.assertEqual(tuple(self.tm("show-options", "-gv", name)
                                           for name in ("set-titles", "set-titles-string")), original_titles)

    def lifecycle_env(self, pane, terminal="konsole"):
        return {"CLAUDE_PID": self.tm("display-message", "-p", "-t", pane, "#{pane_pid}"),
                "CCTAB_TERMINAL": terminal, "CCTAB_STATE_DIR": str(self.root / "state")}

    def policy(self, pane=None):
        return tuple(self.tm("display-message", "-p", "-t", pane or self.pane, "#{" + name + "}")
                     for name in ("@cctab_armed", "client-attached[1971]"))

    def spawn_hook(self, edge, pane, extra_env, session):
        env = dict(self.env, TMUX=f"{self.socket},1,0", TMUX_PANE=pane,
                   CCTAB_TERMINAL="other", CCTAB_GLYPH_POS="prefix")
        env.update(extra_env)
        process = subprocess.Popen([str(BIN), edge], env=env, cwd=self.root,
                                   stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                   stderr=subprocess.PIPE, start_new_session=True)
        process.stdin.write(json.dumps({"session_id": session}).encode())
        process.stdin.close()
        process.stdin = None

        def cleanup():
            # Only this disposable hook group, including a barrier child.
            try:
                os.killpg(process.pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
            process.communicate(timeout=3)
        self.addCleanup(cleanup)
        return process

    def finish_hook(self, process):
        out, err = process.communicate(timeout=8)
        self.assertEqual((process.returncode, out, err), (0, b"", b""))

    def lifecycle_barrier(self, stage):
        """Delay RETURN of a completed real command, never fabricate its result.

        Registration/retirement is already in tmux before ready appears. The
        child inherits the candidate's lock on stdin. A second wrapper records
        its completed target query so we can launch a contender deterministically
        without requiring it to finish while the first hook holds the lock.
        """
        proxy = self.root / ("proxy-" + stage)
        proxy.mkdir()
        marker = self.root / ("barrier-" + stage)
        wrapper = proxy / "tmux"
        wrapper.write_text(
            "#!" + sys.executable + "\n"
            "import os,sys,subprocess,pathlib,time\n"
            "args=sys.argv[1:]\n"
            "r=subprocess.run([" + repr(self.tmux) + "]+args,capture_output=True)\n"
            "p=pathlib.Path(os.environ['CCTAB_BARRIER'])\n"
            "mode=os.environ['CCTAB_BARRIER_MODE']\n"
            "target=args[0]=='display-message' and '#{socket_path}' in args[-1]\n"
            "member=args[0]=='set-option' and '@cctab_members' in args\n"
            "hold=(mode=='member' and member) or (mode=='clients' and args[0]=='list-clients')\n"
            "if mode=='contender' and target: p.with_suffix('.attempt').touch()\n"
            "if hold:\n"
            " p.with_suffix('.result').write_bytes(r.stdout)\n"
            " p.with_suffix('.ready').touch()\n"
            " deadline=time.monotonic()+8\n"
            " while not p.with_suffix('.release').exists():\n"
            "  if time.monotonic()>deadline: sys.exit(97)\n"
            "  time.sleep(.005)\n"
            "sys.stdout.buffer.write(r.stdout)\n"
            "sys.stderr.buffer.write(r.stderr)\n"
            "sys.exit(r.returncode)\n")
        wrapper.chmod(0o755)
        return marker, {"PATH": str(proxy) + ":" + self.env["PATH"],
                        "CCTAB_BARRIER": str(marker), "CCTAB_BARRIER_MODE": "member"}

    def wait_marker(self, marker, suffix):
        self.wait_for(lambda: marker.with_suffix(suffix).exists(), True)

    def test_old_final_exit_serialises_new_start_and_successor_restores(self):
        other = self.new_window("successor")
        envs = [self.lifecycle_env(pane) for pane in (self.pane, other)]
        marker, barrier = self.lifecycle_barrier("overlap")
        with self.attached_client() as client:
            self.hook("session-start", self.pane, envs[0], {"session_id": "old"})
            self.assertTrue(client.saw(ARM))
            client.clear()
            old = self.spawn_hook("session-end", self.pane, dict(envs[0], **barrier), "old")
            self.wait_marker(marker, ".ready")
            members = json.loads(self.tm("display-message", "-p", "-t", self.pane, "#{@cctab_members}"))
            self.assertFalse(members["panes"][self.pane][3], "old owner was not retired")
            self.assertEqual(self.policy()[0], "konsole")
            new = self.spawn_hook("session-start", other,
                                  dict(envs[1], **dict(barrier, CCTAB_BARRIER_MODE="contender")), "new")
            self.wait_marker(marker, ".attempt")
            # The completed real identity query precedes acquisition of the
            # shared lock; startup must wait rather than publish into retirement.
            time.sleep(.1)
            self.assertIsNone(new.poll())
            marker.with_suffix(".release").touch()
            self.finish_hook(old)
            self.finish_hook(new)
            self.wait_for(lambda: self.tm("display-message", "-p", "-t", other,
                                          "#{pane_title}").split()[-2:-1], ["i"])
            self.assertTrue(client.saw(ARM))
            self.assertTrue(client.saw(RESTORE))
            self.assertLess(client.captured.rindex(RESTORE), client.captured.rindex(ARM))
            self.assertEqual(self.policy()[0], "konsole")
            self.assertIn("tmux-arm", self.policy()[1])
            client.clear()
            self.hook("session-end", self.pane, envs[0], {"session_id": "old"})
            self.assertFalse(client.saw(RESTORE, timeout=.2), "duplicate old exit restored successor")
            self.hook("session-end", other, envs[1], {"session_id": "new"})
            self.assertTrue(client.saw(RESTORE))
            self.assertEqual(client.captured.count(RESTORE), 1)
            self.assertEqual(self.policy(), ("", ""))
            client.clear()
            self.hook("session-end", other, envs[1], {"session_id": "new"})
            self.assertFalse(client.saw(RESTORE, timeout=.2), "duplicate final exit restored twice")

    def test_concurrent_final_exits_restore_once_despite_unparsed_carriers(self):
        other = self.new_window("second")
        panes = (self.pane, other)
        envs = [self.lifecycle_env(pane) for pane in panes]
        marker, barrier = self.lifecycle_barrier("exits")
        with self.attached_client() as client:
            for index, pane in enumerate(panes):
                self.hook("session-start", pane, envs[index], {"session_id": f"s{index}"})
            self.assertTrue(client.saw(ARM))
            client.clear()
            first = self.spawn_hook("session-end", self.pane, dict(envs[0], **barrier), "s0")
            self.wait_marker(marker, ".ready")
            second = self.spawn_hook("session-end", other,
                                     dict(envs[1], **dict(barrier, CCTAB_BARRIER_MODE="contender")), "s1")
            self.wait_marker(marker, ".attempt")
            self.assertIsNone(second.poll())
            marker.with_suffix(".release").touch()
            self.finish_hook(first)
            self.finish_hook(second)
            self.assertTrue(client.saw(RESTORE))
            self.assertEqual(client.captured.count(RESTORE), 1)
            self.assertEqual(self.policy(), ("", ""))
            for pane in panes:
                self.wait_for(lambda: self.tm("display-message", "-p", "-t", pane, "#{pane_title}"), "")

    def test_duplicate_start_and_old_end_cannot_retire_same_pane_replacement(self):
        env = self.lifecycle_env(self.pane)
        with self.attached_client() as client:
            for session in ("old", "new", "new"):
                self.hook("session-start", self.pane, env, {"session_id": session})
            self.assertTrue(client.saw(ARM))
            members = json.loads(self.tm("display-message", "-p", "-t", self.pane,
                                         "#{@cctab_members}"))
            self.assertEqual(len(members["panes"]), 1)
            self.assertEqual(members["panes"][self.pane][2:], ["new", True])
            self.wait_for(lambda: self.tm("display-message", "-p", "-t", self.pane,
                                          "#{pane_title}").split()[-2:-1], ["i"])
            carrier = self.tm("display-message", "-p", "-t", self.pane, "#{pane_title}")
            client.clear()
            self.hook("session-end", self.pane, env, {"session_id": "old"})
            self.assertFalse(client.saw(RESTORE, timeout=.2))
            self.assertEqual(self.policy()[0], "konsole")
            self.assertEqual(self.tm("display-message", "-p", "-t", self.pane, "#{pane_title}"), carrier)
            self.hook("session-end", self.pane, env, {"session_id": "new"})
            self.assertTrue(client.saw(RESTORE))
            self.assertEqual(self.policy(), ("", ""))

    def test_interrupted_retirement_keeps_child_lock_then_successor_recovers(self):
        other = self.new_window("successor")
        envs = [self.lifecycle_env(pane) for pane in (self.pane, other)]
        marker, barrier = self.lifecycle_barrier("interrupted-end")
        with self.attached_client() as client:
            self.hook("session-start", self.pane, envs[0], {"session_id": "old"})
            self.assertTrue(client.saw(ARM))
            old = self.spawn_hook("session-end", self.pane, dict(envs[0], **barrier), "old")
            self.wait_marker(marker, ".ready")
            old.kill()  # Leave its in-flight wrapper alive, with inherited lock.
            old.wait(timeout=3)
            new = self.spawn_hook("session-start", other,
                                  dict(envs[1], **dict(barrier, CCTAB_BARRIER_MODE="contender")), "new")
            self.wait_marker(marker, ".attempt")
            time.sleep(.1)
            self.assertIsNone(new.poll(), "successor overtook the surviving lifecycle child")
            marker.with_suffix(".release").touch()
            self.finish_hook(new)
            self.assertEqual(self.policy()[0], "konsole")
            self.assertIn("tmux-arm", self.policy()[1])
            client.clear()
            # The interrupted end never cleared its old carrier. Membership,
            # rather than that asynchronous title, still permits final restore.
            self.hook("session-end", other, envs[1], {"session_id": "new"})
            self.assertTrue(client.saw(RESTORE))
            self.assertEqual(self.policy(), ("", ""))

    def test_interrupted_start_recovers_and_dead_owner_does_not_strand_policy(self):
        other = self.new_window("successor")
        env = self.lifecycle_env(self.pane)
        marker, barrier = self.lifecycle_barrier("interrupted-start")
        barrier["CCTAB_BARRIER_MODE"] = "clients"
        with self.attached_client() as client:
            start = self.spawn_hook("session-start", self.pane, dict(env, **barrier), "old")
            self.wait_marker(marker, ".ready")
            self.assertEqual(self.policy()[0], "konsole")
            os.killpg(start.pid, signal.SIGKILL)
            start.communicate(timeout=3)
            # Repeat the actual interrupted startup, then publish a real carrier.
            self.hook("session-start", self.pane, env, {"session_id": "old"})
            self.assertTrue(client.saw(ARM))
            self.wait_for(lambda: self.tm("display-message", "-p", "-t", self.pane,
                                          "#{pane_title}").split()[-2:-1], ["i"])
            # Replace the pane's real shell process without a SessionEnd. Its
            # stale title may survive; native process identity retires the owner.
            self.tm("respawn-pane", "-k", "-t", self.pane, "/bin/sh")
            successor_env = self.lifecycle_env(other, "wezterm")
            self.hook("session-start", other, successor_env, {"session_id": "new"})
            self.assertEqual(self.policy()[0], "konsole")
            client.clear()
            self.hook("session-end", other, successor_env, {"session_id": "new"})
            self.assertTrue(client.saw(RESTORE))
            self.assertEqual(self.policy(), ("", ""))

    def test_retiring_one_tmux_session_does_not_block_another_session(self):
        other = self.tm("new-session", "-d", "-s", "beta", "-P", "-F", "#{pane_id}", "/bin/sh")
        env = self.lifecycle_env(self.pane)
        marker, barrier = self.lifecycle_barrier("isolation")
        with self.attached_client() as alpha, self.attached_client("beta") as beta:
            self.hook("session-start", self.pane, env, {"session_id": "old"})
            self.assertTrue(alpha.saw(ARM))
            old = self.spawn_hook("session-end", self.pane, dict(env, **barrier), "old")
            self.wait_marker(marker, ".ready")
            other_env = self.lifecycle_env(other)
            self.hook("session-start", other, other_env, {"session_id": "other"})
            self.assertTrue(beta.saw(ARM))
            marker.with_suffix(".release").touch()
            self.finish_hook(old)
            self.assertTrue(alpha.saw(RESTORE))
            self.assertFalse(beta.saw(RESTORE, timeout=.2))
            self.assertEqual(self.policy(other)[0], "konsole")
            self.hook("session-end", other, other_env, {"session_id": "other"})
            self.assertTrue(beta.saw(RESTORE))
            self.assertEqual(self.policy(other), ("", ""))

    def test_detached_policy_survives_other_start_and_reattach_until_last_owner(self):
        """Forced terminal rows observe protocol bytes on disposable clients."""
        arm = b"\x1b]50;LocalTabTitleFormat=%w;RemoteTabTitleFormat=%w\x07"
        restore = b"\x1b]50;LocalTabTitleFormat=%d : %n;RemoteTabTitleFormat=(%u) %H\x07"
        other = self.new_window("second")

        def option(name):
            return self.tm("display-message", "-p", "-t", self.pane, "#{" + name + "}")

        self.assertEqual(self.tm("list-clients", "-t", self.pane), "")
        envs = [{"CLAUDE_PID": self.tm("display-message", "-p", "-t", pane, "#{pane_pid}"),
                 "CCTAB_TERMINAL": surface, "CCTAB_STATE_DIR": str(self.root / "state")}
                for pane, surface in ((self.pane, "konsole"), (other, "wezterm"))]
        for index, pane in enumerate((self.pane, other)):
            self.hook("session-start", pane, envs[index], {"session_id": f"s{index}"})
            self.wait_for(lambda: self.tm("display-message", "-p", "-t", pane,
                                          "#{pane_title}").split()[-2:-1], ["i"])
        self.assertEqual(option("@cctab_armed"), "konsole")
        rearm = option("client-attached[1971]")
        self.assertIn("tmux-arm", rearm)
        self.assertIn("s konsole", (self.root / "state" / "s0").read_text().splitlines())
        report = self.hook("doctor", self.pane).decode()
        self.assertIn("retained policy, not confirmed delivery", report)
        self.assertIn("none attached", report)

        with self.attached_client() as client:
            self.assertTrue(client.saw(arm), "detached policy did not arm the attaching client")
            self.hook("session-end", self.pane, dict(envs[0], CCTAB_TERMINAL="wezterm"),
                      {"session_id": "s0"})
            self.wait_for(lambda: option("pane_title"), "")
            self.assertFalse(client.saw(restore, timeout=0.3))
            self.assertEqual(option("@cctab_armed"), "konsole")
            self.assertEqual(option("client-attached[1971]"), rearm)
        self.wait_for(lambda: self.tm("list-clients", "-t", self.pane), "")
        # The remaining owner never selected Konsole, but retains its policy.
        with self.attached_client() as client:
            self.assertTrue(client.saw(arm), "shared policy was lost before reattachment")
            self.hook("session-end", other, envs[1], {"session_id": "s1"})
            self.assertTrue(client.saw(restore))
            self.assertEqual(option("@cctab_armed"), "")
            self.assertEqual(option("client-attached[1971]"), "")
        self.wait_for(lambda: self.tm("list-clients", "-t", self.pane), "")
        with self.attached_client() as client:
            self.assertFalse(client.saw(arm, timeout=0.3), "retired policy armed a later client")

    def test_last_detached_owner_retires_policy_without_a_restore_receipt(self):
        self.hook("session-start", self.pane, {"CCTAB_TERMINAL": "konsole"})
        self.assertEqual(self.tm("list-clients", "-t", self.pane), "")
        self.hook("session-end", self.pane, {"CCTAB_TERMINAL": "wezterm"})
        for name in ("@cctab_armed", "client-attached[1971]"):
            self.assertEqual(self.tm("display-message", "-p", "-t", self.pane,
                                     "#{" + name + "}"), "")
        with self.attached_client() as client:
            self.assertFalse(client.saw(b"\x1b]50;", timeout=0.3))

    def test_a_session_that_armed_nothing_is_not_restored_by_a_late_terminal(self):
        """The other half of the same defect, and the reason `-` is a value.

        A store that says nothing was armed must not be talked out of it by an
        environment that names Konsole only at the end. Writing the restore here
        would be an un-arming with no arming - a tab handed the compiled-in
        Konsole defaults it may never have had.
        """
        restore = b"\x1b]50;LocalTabTitleFormat=%d : %n;RemoteTabTitleFormat=(%u) %H\x07"
        with self.attached_client() as client:
            self.hook("session-start", self.pane, {"CCTAB_TERMINAL": "wezterm"})
            self.assertEqual(self.tm("display-message", "-p", "-t", self.pane,
                                     "#{@cctab_armed}"), "-")
            self.hook("session-end", self.pane, {"CCTAB_TERMINAL": "konsole"})
            # Give the client the same budget the positive case gets, so this is a
            # real absence and not a race the timeout hid.
            self.assertFalse(client.saw(restore, timeout=2))

    def test_current_state_semantics_agree_with_plain_terminal(self):
        # Exercise all four states and ownership precedence through ordinary
        # terminalSequence output and real tmux pane-tty delivery.
        self.start()
        epoch = int(self.tm("display-message", "-p", "%s"))
        pid = self.tm("display-message", "-p", "-t", self.pane, "#{pane_pid}")
        plain_env = dict(self.env, CLAUDE_PID="0", CCTAB_TERMINAL="other",
                         CCTAB_GLYPH_POS="prefix", CCTAB_STATE_DIR=str(self.root / "plain-state"))
        mux_env = {"CLAUDE_PID": pid, "CCTAB_STATE_DIR": str(self.root / "tmux-state")}
        running = [{"id": "child", "type": "subagent", "status": "running"}]
        # Last item is an emitted state, or None for a silent hook.
        steps = [
            ("working", "UserPromptSubmit", {"prompt": "launch workflow"}, "w"),
            ("idle", "Stop", {"background_tasks": running}, "p"),
            ("working", "PostToolUse", {"agent_id": "child"}, None),
            ("waiting", "PermissionRequest", {"agent_id": "child"}, "A"),
            ("working", "PostToolUse", {}, None),
            ("idle", "StopFailure", {}, None),
            ("idle", "Stop", {"background_tasks": running}, None),
            ("subagent-stop", "SubagentStop", {"agent_id": "other"}, None),
            ("working", "PostToolUse", {"agent_id": "child"}, "p"),
            ("subagent-stop", "SubagentStop", {"agent_id": "child"}, None),
            ("notify", "Notification", {"notification_type": "idle_prompt"}, "p"),
            ("working", "UserPromptSubmit", {"prompt": "continue"}, "W"),
            ("idle", "StopFailure", {"background_tasks": []}, "p"),
            ("working", "UserPromptSubmit", {"prompt": "<task-notification>done</task-notification>"}, "W"),
            ("idle", "Stop", {"background_tasks": []}, "i"),
        ]
        glyphs = {"w": "🔵", "W": "🔵", "a": "🟠", "A": "🟠", "p": "🟣", "i": "⚪"}
        carrier = None
        displayed = None
        for index, (edge, event, fields, paint) in enumerate(steps):
            with self.subTest(event=event, step=index):
                now = str(epoch + index)
                payload = {"session_id": "s1", "hook_event_name": event, **fields}
                plain_env["CCTAB_NOW"] = now
                result = subprocess.run([str(BIN), edge], input=json.dumps(payload).encode(),
                                        env=plain_env, cwd=self.root, capture_output=True, timeout=10)
                self.assertEqual((result.returncode, result.stderr), (0, b""))
                self.assertEqual(self.hook(edge, self.pane, {**mux_env, "CCTAB_NOW": now}, payload), b"")
                if paint is None:
                    self.assertEqual(result.stdout, b"")
                else:
                    message = json.loads(result.stdout)
                    sequence = message["terminalSequence"]
                    self.assertTrue(sequence.startswith("\x1b]0;" + glyphs[paint] + " "), sequence)
                    self.assertTrue(sequence.endswith("\x07"), sequence)
                    self.assertTrue(message["suppressOutput"])
                    carrier = f"ct{2 if paint in 'pWA' else 1} {paint} {now}"
                    displayed = glyphs[paint]
                self.wait_for(lambda: self.tm("display-message", "-p", "-t", self.pane,
                                             "#{pane_title}").rsplit(" ", 3)[-3:], carrier.split())
                self.assertEqual(self.rendered(self.pane, True), displayed + " C:current")
                self.assertEqual(self.rendered(self.pane), displayed + " N:current")
                self.assertIn(displayed, self.tm("display-message", "-p", "-t", self.pane,
                                                "#{T:@cctab_title}"))
                if paint == "A":
                    record = self.root / "tmux-state" / "s1"
                    before = record.read_bytes()
                    other = self.new_window("focus-away")
                    self.tm("select-window", "-t", other)
                    self.tm("select-window", "-t", self.pane)
                    self.assertEqual(record.read_bytes(), before)
                    self.assertEqual(self.rendered(self.pane, True), "🟠 C:current")

    def test_invalid_exited_and_redirected_pids_never_change_pane_or_client_title(self):
        self.start()
        self.publish(self.pane)
        before = self.tm("display-message", "-p", "-t", self.pane, "#{pane_title}")
        exited = subprocess.Popen(["/usr/bin/true"], env=self.env, stdout=subprocess.DEVNULL)
        exited.wait(timeout=3)
        cases = (("working", {"hook_event_name": "PostToolUse"}),
                 ("waiting", {"hook_event_name": "PermissionRequest"}),
                 ("idle", {"hook_event_name": "Stop"}),
                 ("notify", {"hook_event_name": "Notification", "notification_type": "elicitation_dialog"}),
                 ("elicitation", {"hook_event_name": "Elicitation", "mcp_server_name": "mcp", "elicitation_id": "a"}))
        with contextlib.ExitStack() as stack, self.attached_client() as client:
            file_output = stack.enter_context((self.root / "redirected").open("wb"))
            owners = []
            for output in (subprocess.PIPE, subprocess.DEVNULL, file_output):
                process = subprocess.Popen(["/bin/sleep", "60"], stdout=output,
                                           stderr=subprocess.DEVNULL, env=self.env,
                                           start_new_session=True)
                owners.append(process)
                def stop(owner=process):
                    if owner.poll() is None:
                        owner.kill()
                    owner.communicate(timeout=3)
                stack.callback(stop)
            self.assert_status(client, "🔵 C:current")
            client.clear()
            for pid in ("0", "not-a-pid", "999999999", str(exited.pid), *(str(p.pid) for p in owners)):
                for edge, payload in cases:
                    with self.subTest(pid=pid, edge=edge):
                        self.assertEqual(self.hook(edge, self.pane, {"CLAUDE_PID": pid}, payload), b"")
                        self.assertEqual(self.tm("display-message", "-p", "-t", self.pane, "#{pane_title}"), before)
            # Read an actual client redraw too: no refused carrier may escape
            # into an unrelated outer terminal or be passed through as JSON.
            self.redraw_status(client)
            self.assert_status(client, "🔵 C:current")
            outer = self.tm("display-message", "-p", "-t", self.pane, "#{T:@cctab_title}").encode()
            titles = re.findall(rb"\x1b\](?:0|2);(.*?)(?:\x07|\x1b\\)", client.captured, re.S)
            self.assertTrue(all(title == outer for title in titles), client.diagnostic())
            self.assertNotIn(b"\x1b]50;", client.captured)
            file_output.flush()
            self.assertEqual((self.root / "redirected").read_bytes(), b"")

    def test_explicit_marker_preserves_rounded_pill_formats_before_install_and_on_uninstall(self):
        for option, value in zip(FORMATS, MARKED_PILL_FORMATS):
            self.tm("set", "-gw", option, value)
        before = self.formats(self.pane)
        self.start()
        self.publish(self.pane)
        self.assertEqual(self.formats(self.pane), before)
        self.assertEqual(self.globals(), MARKED_PILL_FORMATS)
        self.assertIn("window list: OK", self.hook("doctor", self.pane).decode())
        for current, value in enumerate(MARKED_PILL_FORMATS):
            expected = value.replace("#{T:@cctab_window_strip}", "🔵").replace("#I:#W", "0:current")
            self.assertEqual(self.rendered(self.pane, bool(current)), expected)
        self.uninstall()
        self.assertEqual(self.formats(self.pane), before)
        self.assertEqual(self.globals(), MARKED_PILL_FORMATS)
        self.assertEqual(self.tm("show-options", "-sqv", "@cctab_window_strip"), "")
        self.assertNotIn("🔵", self.rendered(self.pane))

    def test_user_can_replace_existing_wrappers_with_global_explicit_theme_markers(self):
        self.start()
        for option, value in zip(FORMATS, MARKED_PILL_FORMATS):
            self.tm("set", "-gw", option, value)
            self.tm("set", "-wu", "-t", self.pane, option)
        self.start()
        self.publish(self.pane, "waiting")
        self.assertEqual(self.formats(self.pane), ((False, None), (False, None)))
        expected = MARKED_PILL_FORMATS[0].replace("#{T:@cctab_window_strip}", "🟠").replace("#I:#W", "0:current")
        self.assertEqual(self.rendered(self.pane), expected)
        self.uninstall()
        self.assertEqual(self.formats(self.pane), ((False, None), (False, None)))
        self.assertEqual(self.globals(), MARKED_PILL_FORMATS)
        for name in SAVED:
            self.assertNotIn(name, self.tm("show-options", "-w", "-t", self.pane))

    def test_explicit_marker_user_edit_survives_while_other_owned_format_restores(self):
        self.start()
        self.tm("set", "-w", "-t", self.pane, FORMATS[0], MARKED_PILL_FORMATS[0])
        self.start()
        self.uninstall()
        self.assertEqual(self.local(self.pane, FORMATS[0]), (True, MARKED_PILL_FORMATS[0]))
        self.assertEqual(self.local(self.pane, FORMATS[1]), (False, None))
        self.assertEqual(self.tm("show-options", "-sqv", "@cctab_window_strip"), "")
        for name in SAVED:
            self.assertNotIn(name, self.tm("show-options", "-w", "-t", self.pane))

    def test_background_windows_and_split_panes_keep_their_own_cells(self):
        background = self.new_window("background")
        split = self.tm("split-window", "-d", "-t", background,
                        "-P", "-F", "#{pane_id}", "/bin/sh")
        plain = self.new_window("plain")
        plain_before = self.formats(plain)
        original_globals = self.globals()
        self.start()
        self.start(background)
        self.publish(self.pane, "working")
        self.publish(background, "waiting")
        self.publish(split, "idle")
        self.assertEqual(self.rendered(self.pane, True), "🔵 C:current")
        self.assertEqual(self.rendered(background), "🟠⚪ N:background")
        self.assertEqual(self.rendered(plain), "N:plain")
        self.assertEqual(self.globals(), original_globals)
        self.assertEqual(self.formats(plain), plain_before)
        # The outer terminal keeps its full cross-window strip and active label.
        outer = self.tm("display-message", "-p", "-t", self.pane, "#{T:@cctab_title}")
        self.assertIn("🔵🟠⚪", outer)

    def test_inherited_formats_restore_by_unsetting_locals(self):
        before = self.formats(self.pane)
        self.assertEqual(before, ((False, None), (False, None)))
        self.start()
        self.assertTrue(all(present for present, _ in self.formats(self.pane)))
        self.tm("set", "-gw", FORMATS[0], "NEW GLOBAL #W")
        self.uninstall()
        self.assertEqual(self.formats(self.pane), before)
        self.assertEqual(self.rendered(self.pane), "NEW GLOBAL current")

    def test_explicit_formats_round_trip_exactly_including_empty_and_newlines(self):
        raw = 'quote " slash \\ literal $HOME\n#{window_index}:#{window_name} #[bold]\n'
        self.tm("set", "-w", "-t", self.pane, FORMATS[0], raw)
        self.tm("set", "-w", "-t", self.pane, FORMATS[1], "")
        before = self.formats(self.pane)
        label = self.rendered(self.pane)
        self.start()
        self.publish(self.pane)
        self.assertEqual(self.rendered(self.pane), "🔵 " + label)
        self.uninstall()
        self.assertEqual(self.formats(self.pane), before)

    def test_explicit_format_equal_to_global_stays_explicit_after_restore(self):
        self.tm("set", "-w", "-t", self.pane, FORMATS[0], self.globals()[0])
        before = self.formats(self.pane)
        self.start()
        self.uninstall()
        self.assertEqual(self.formats(self.pane), before)

    def test_dollar_and_backslash_literals_survive_copy_render_and_restore(self):
        raw = r'$HOME ${HOME} \$HOME \${HOME} \\ ${CCTAB_UNSET_LITERAL} "quoted" #W'
        for option in FORMATS:
            self.tm("set", "-w", "-t", self.pane, option, raw)
        self.tm("set", "-w", "-t", self.pane, "@test_expected_format", raw)
        before = self.formats(self.pane)
        labels = tuple(self.rendered(self.pane, current) for current in (False, True))
        self.start()
        for index, saved in enumerate(("@cctab_prev_window_format", "@cctab_prev_window_current")):
            # tmux 3.4 escapes dollar signs in CLI output. Compare exact outputs
            # under the same version and also compare real values inside tmux;
            # never normalize away a slash that might be actual corruption.
            self.assertEqual(self.tm("show-options", "-wqv", "-t", self.pane, saved), before[index][1])
            self.assertEqual(self.tm("display-message", "-p", "-t", self.pane,
                                     "#{==:#{" + saved + "},#{@test_expected_format}}"), "1")
        self.publish(self.pane)
        for current in (False, True):
            self.assertEqual(self.rendered(self.pane, current), "🔵 " + labels[int(current)])
        self.uninstall()
        self.assertEqual(self.formats(self.pane), before)
        for option in FORMATS:
            self.assertEqual(self.tm("display-message", "-p", "-t", self.pane,
                                     "#{==:#{" + option + "},#{@test_expected_format}}"), "1")

    def test_outer_title_dollar_and_backslash_literals_restore_without_extra_escaping(self):
        raw = r'outer "$HOME" ${HOME} \$HOME \\ #{pane_title}'
        self.tm("set", "-g", "set-titles-string", raw)
        self.tm("set", "-s", "@test_expected_outer", raw)
        before = self.tm("show-options", "-gv", "set-titles-string")
        self.start()
        self.assertEqual(self.tm("show-options", "-sqv", "@cctab_prev_string"), before)
        self.assertEqual(self.tm("display-message", "-p", "-t", self.pane,
                                 "#{==:#{@cctab_prev_string},#{@test_expected_outer}}"), "1")
        self.uninstall()
        self.assertEqual(self.tm("show-options", "-gv", "set-titles-string"), before)
        self.assertEqual(self.tm("display-message", "-p", "-t", self.pane,
                                 "#{==:#{set-titles-string},#{@test_expected_outer}}"), "1")

    def test_outer_title_settings_restore_empty_and_nondefault_pairs(self):
        for enabled, title in (("on", ""), ("off", "original #{session_name}\nsecond line")):
            with self.subTest(enabled=enabled, title=title):
                self.tm("set", "-g", "set-titles", enabled)
                self.tm("set", "-g", "set-titles-string", title)
                before = tuple(self.tm("show-options", "-gv", name)
                               for name in ("set-titles", "set-titles-string"))
                self.start()
                self.publish(self.pane)
                self.uninstall()
                self.assertEqual(tuple(self.tm("show-options", "-gv", name)
                                       for name in ("set-titles", "set-titles-string")), before)

    def test_nested_formats_and_shell_literals_are_saved_without_interpretation(self):
        raw = ('100% #{?window_active,ACTIVE,#{?window_bell_flag,BELL,quiet}} '
               '#I:#W#F ; `` $() $HOME')
        self.tm("set", "-w", "-t", self.pane, FORMATS[0], raw)
        before = self.formats(self.pane)
        label = self.rendered(self.pane)
        self.start()
        self.assertEqual(self.tm("show-options", "-wqv", "-t", self.pane,
                                 "@cctab_prev_window_format"), before[0][1])
        self.publish(self.pane)
        self.assertEqual(self.rendered(self.pane), "🔵 " + label)
        self.uninstall()
        self.assertEqual(self.formats(self.pane), before)

    def test_repeated_starts_and_two_sessions_in_one_window_do_not_nest_wrappers(self):
        split = self.tm("split-window", "-d", "-t", self.pane,
                        "-P", "-F", "#{pane_id}", "/bin/sh")
        original = self.formats(self.pane)
        self.start()
        installed = self.formats(self.pane)
        self.start()
        self.start(split)
        self.assertEqual(self.formats(self.pane), installed)
        self.publish(self.pane)
        self.publish(split, "waiting")
        self.assertEqual(self.rendered(self.pane), "🔵🟠 N:current")
        # Exercise the real SessionEnd tty write into our disposable pane.
        pid = self.tm("display-message", "-p", "-t", self.pane, "#{pane_pid}")
        self.hook("session-end", self.pane, {"CLAUDE_PID": pid})
        self.wait_for(lambda: self.rendered(split), "🟠 N:current")
        self.assertEqual(self.formats(split), installed)
        self.uninstall(split)
        self.assertEqual(self.formats(split), original)

    def test_concurrent_starts_preserve_original_backups(self):
        self.tm("set", "-w", "-t", self.pane, FORMATS[0], "ORIGINAL #W")
        before = self.formats(self.pane)
        with ThreadPoolExecutor(max_workers=8) as pool:
            list(pool.map(lambda _: self.start(), range(8)))
        self.assertEqual(self.tm("show-options", "-wqv", "-t", self.pane,
                                 "@cctab_prev_window_format"), "ORIGINAL #W")
        self.publish(self.pane)
        self.assertEqual(self.rendered(self.pane), "🔵 ORIGINAL current")
        self.uninstall()
        self.assertEqual(self.formats(self.pane), before)

    def test_uninstall_restores_tracked_windows_across_sessions_and_deleted_windows(self):
        other = self.tm("new-session", "-d", "-s", "beta", "-n", "other",
                        "-P", "-F", "#{pane_id}", "/bin/sh")
        doomed = self.new_window("doomed")
        self.tm("set", "-w", "-t", other, FORMATS[1], "OTHER #W")
        before = {pane: self.formats(pane) for pane in (self.pane, other)}
        for pane in (self.pane, other, doomed):
            self.start(pane)
        self.tm("kill-window", "-t", doomed)
        self.uninstall()
        for pane in (self.pane, other):
            self.assertEqual(self.formats(pane), before[pane])
            options = self.tm("show-options", "-w", "-t", pane)
            for name in SAVED:
                self.assertNotIn(name, options)
        self.assertEqual(self.tm("show-options", "-sqv", "@cctab_window_strip"), "")

    def test_linked_window_is_decorated_once_and_restored_once(self):
        self.tm("new-session", "-d", "-s", "beta", "-n", "other", "/bin/sh")
        window = self.tm("display-message", "-p", "-t", self.pane, "#{window_id}")
        self.tm("link-window", "-s", window, "-t", "beta:1")
        listed = self.tm("list-windows", "-a", "-F", "#{window_id}").splitlines()
        self.assertEqual(listed.count(window), 2)
        self.tm("set", "-w", "-t", self.pane, FORMATS[0], "LINKED #W")
        before = self.formats(self.pane)
        self.start()
        installed = self.formats(self.pane)
        self.start()
        self.assertEqual(self.formats(self.pane), installed)
        self.publish(self.pane)
        self.assertEqual(self.rendered(self.pane), "🔵 LINKED current")
        self.uninstall()
        self.assertEqual(self.formats(self.pane), before)
        self.assertEqual(self.rendered(self.pane), "LINKED current")

    def test_user_edit_survives_later_starts_and_uninstall_independently_per_format(self):
        self.tm("set", "-w", "-t", self.pane, FORMATS[1], "ORIGINAL CURRENT #W")
        self.start()
        edited = "USER REPLACEMENT #{window_name}"
        self.tm("set", "-w", "-t", self.pane, FORMATS[0], edited)
        self.start()
        self.assertEqual(self.local(self.pane, FORMATS[0]), (True, edited))
        self.uninstall()
        self.assertEqual(self.local(self.pane, FORMATS[0]), (True, edited))
        self.assertEqual(self.local(self.pane, FORMATS[1]), (True, "ORIGINAL CURRENT #W"))

    def test_legacy_outer_title_ownership_does_not_skip_new_format_backups(self):
        self.tm("set", "-s", "@cctab_saved", "1")
        self.tm("set", "-s", "@cctab_prev_string", "ORIGINAL OUTER")
        self.tm("set", "-s", "@cctab_prev_titles", "0")
        self.tm("set", "-w", "-t", self.pane, FORMATS[0], "LEGACY WINDOW #W")
        before = self.formats(self.pane)
        self.start()
        self.publish(self.pane)
        self.assertEqual(self.rendered(self.pane), "🔵 LEGACY WINDOW current")
        self.uninstall()
        self.assertEqual(self.formats(self.pane), before)
        self.assertEqual(self.tm("show-options", "-gv", "set-titles-string"), "ORIGINAL OUTER")

    def test_lost_ownership_marker_does_not_save_a_self_referencing_original(self):
        before = self.formats(self.pane)
        self.start()
        self.publish(self.pane)
        installed = self.formats(self.pane)
        self.tm("set", "-wu", "-t", self.pane, "@cctab_window_format_saved")
        self.start()
        self.assertEqual(self.formats(self.pane), installed)
        self.assertEqual(self.tm("show-options", "-wqv", "-t", self.pane,
                                 "@cctab_prev_window_format"), "N:#W")
        self.assertEqual(self.rendered(self.pane), "🔵 N:current")
        self.assertIn("left intact", self.uninstall())
        self.assertEqual(self.rendered(self.pane), "🔵 N:current")
        # Restoring the missing ownership evidence makes clean removal possible.
        self.tm("set", "-w", "-t", self.pane, "@cctab_window_format_saved", "1")
        self.uninstall()
        self.assertEqual(self.formats(self.pane), before)

    def test_missing_original_or_inheritance_metadata_preserves_shared_rendering_options(self):
        for index, missing in enumerate(("@cctab_prev_window_format", "@cctab_prev_window_format_local")):
            with self.subTest(missing=missing):
                pane = self.new_window("missing" + str(index))
                before = self.formats(pane)
                self.start(pane)
                installed = self.formats(pane)
                value = self.tm("show-options", "-wqv", "-t", pane, missing)
                strip = self.tm("show-options", "-sqv", "@cctab_window_strip")
                self.tm("set", "-wu", "-t", pane, missing)
                self.assertIn("left intact", self.uninstall(pane))
                self.assertEqual(self.formats(pane), installed)
                self.assertEqual(self.tm("show-options", "-sqv", "@cctab_window_strip"), strip)
                self.tm("set", "-w", "-t", pane, missing, value)
                self.uninstall(pane)
                self.assertEqual(self.formats(pane), before)

    def test_missing_pane_target_does_not_decorate_an_arbitrary_window(self):
        other = self.new_window("other")
        before = {pane: self.formats(pane) for pane in (self.pane, other)}
        self.hook("session-start")
        for pane in before:
            self.assertEqual(self.formats(pane), before[pane])
        self.uninstall()
        for pane in before:
            self.assertEqual(self.formats(pane), before[pane])

    def test_names_automatic_rename_and_status_clock_are_unchanged(self):
        other = self.new_window("manual-name")
        self.tm("set", "-w", "-t", self.pane, "automatic-rename-format", "current")
        self.tm("set", "-w", "-t", self.pane, "automatic-rename", "on")
        self.tm("set", "-w", "-t", other, "automatic-rename", "off")
        self.tm("set", "-g", "status", "off")
        self.tm("set", "-g", "status-interval", "0")
        def identity():
            return self.tm("list-windows", "-a", "-F",
                           "#{window_id}|#{window_name}|#{automatic-rename}|#{automatic-rename-format}")
        before = identity()
        self.start()
        self.start(other)
        self.publish(self.pane, "waiting")
        self.assertEqual(identity(), before)
        self.assertEqual(self.tm("show-options", "-gv", "status"), "off")
        self.assertEqual(self.tm("show-options", "-gv", "status-interval"), "0")
        self.uninstall()
        self.assertEqual(identity(), before)

    def test_native_window_format_decays_without_hook_activity(self):
        self.start(CCTAB_TTL_WORKING="1", CCTAB_TTL_GONE="3")
        self.publish(self.pane)
        self.assertEqual(self.rendered(self.pane), "🔵 N:current")
        self.wait_for(lambda: self.rendered(self.pane), "⚪ N:current")
        self.wait_for(lambda: self.rendered(self.pane), "N:current")

    def test_glyphs_are_drawn_inside_the_actual_rounded_pills_with_the_label_background(self):
        for option, value in zip(FORMATS, MARKED_PILL_FORMATS):
            self.tm("set", "-gw", option, value)
        self.tm("set", "-g", "status-position", "top")
        self.tm("set", "-g", "window-status-separator", "")
        background = self.new_window("background")
        split = self.tm("split-window", "-d", "-t", background,
                        "-P", "-F", "#{pane_id}", "/bin/sh")
        self.start()
        self.start(background)
        self.publish(self.pane)
        self.publish(background, "waiting")
        self.publish(split, "idle")
        self.tm("select-window", "-t", self.pane)
        wanted = (" 🔵 0:current ", " 🟠⚪ 1:background ")
        with self.attached_client() as client:
            for fragment in wanted:
                self.assert_status(client, fragment)
            # OSC title text cannot satisfy these status-bar observations.
            clean, backgrounds = terminal_text_and_backgrounds(client.captured)
            for fragment in wanted:
                offset = clean.index(fragment)
                label = offset + fragment.index(":") + 1
                self.assertIsNotNone(backgrounds[offset + 2])
                self.assertEqual(backgrounds[offset + 2], backgrounds[label])
                self.assertNotEqual(backgrounds[offset], backgrounds[offset + 2])


if __name__ == "__main__":
    unittest.main()
