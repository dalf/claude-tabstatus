#!/usr/bin/env python3
"""Real tmux window-status and restoration regressions on private servers.

CCTAB_TEST_BIN=/path/to/tabstatus python3 tests/test_tmux_status.py
CCTAB_TEST_TMUX=/path/to/tmux selects another tmux build for every invocation.
Every test owns a unique socket, isolated HOME and disposable shell panes.
"""
import fcntl
from concurrent.futures import ThreadPoolExecutor
import json
import os
from pathlib import Path
import pty
import re
import select
import shutil
import struct
import subprocess
import tempfile
import termios
import time
import unittest


ROOT = Path(__file__).resolve().parents[1]
BIN = Path(os.environ.get("CCTAB_TEST_BIN", ROOT / "bin/tabstatus")).resolve()
TMUX_OVERRIDE = os.environ.get("CCTAB_TEST_TMUX")
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


def terminal_text_and_backgrounds(data, foreground=False):
    """Remove terminal controls while retaining each printed character's SGR background."""
    data = re.sub(rb"\x1b\].*?(?:\x07|\x1b\\)", b"", data, flags=re.S)
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


@unittest.skipUnless(TMUX_OVERRIDE or shutil.which("tmux"), "tmux is required for window-status integration tests")
class TmuxStatusTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory(prefix="cctab-tmux-status-")
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name)
        self.socket = self.root / "tmux.sock"
        self.env = {"PATH": "/usr/bin:/bin", "HOME": str(self.root),
                    "TERM": "xterm-256color", "LC_ALL": "C.UTF-8", "PS1": ""}
        tmux = "tmux"
        if TMUX_OVERRIDE:
            selected = Path(TMUX_OVERRIDE)
            self.assertTrue(selected.is_absolute(), "CCTAB_TEST_TMUX must be an absolute executable path")
            self.assertTrue(selected.is_file() and os.access(selected, os.X_OK),
                            "CCTAB_TEST_TMUX must name an executable file")
            tmux = str(selected)
            self.env["PATH"] = str(selected.parent) + os.pathsep + self.env["PATH"]
        self.base = [tmux, "-S", str(self.socket), "-f", "/dev/null"]
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
                   CLAUDE_CONFIG_DIR=str(self.root / "config"), CLAUDE_PID="0")
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
        self.assertEqual(actual, expected)

    def publish(self, pane, edge="working", age=0):
        # Invoke the real hook with the disposable pane shell as Claude's tty
        # owner. Replaying JSON ourselves would hide a broken hook transport.
        epoch = int(self.tm("display-message", "-p", "%s")) - age
        pid = self.tm("display-message", "-p", "-t", pane, "#{pane_pid}")
        output = self.hook(edge, pane, {"CCTAB_NOW": str(epoch), "CLAUDE_PID": pid})
        self.assertEqual(output, b"", "tmux hook updates must be delivered directly to the pane tty")
        state = {"working": "w", "waiting": "a", "idle": "i"}[edge]
        self.wait_for(lambda: self.tm("display-message", "-p", "-t", pane,
                                     "#{pane_title}").rsplit(" ct1 ", 1)[-1], f"{state} {epoch}")

    def test_real_hook_updates_need_no_stdout_protocol_consumer(self):
        self.start()
        for edge, glyph in (("working", "🔵"), ("waiting", "🟠"), ("idle", "⚪")):
            self.publish(self.pane, edge)
            self.assertEqual(self.rendered(self.pane, True), glyph + " C:current")

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
        master, slave = pty.openpty()
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 24, 160, 0, 0))
        client = subprocess.Popen(self.base + ["attach-session", "-t", "alpha"],
                                  env=self.env, stdin=slave, stdout=slave, stderr=slave,
                                  start_new_session=True)
        os.close(slave)
        captured = b""
        fragment = "   0:"
        try:
            deadline = time.monotonic() + 5
            while time.monotonic() < deadline:
                if select.select([master], [], [], 0.1)[0]:
                    try:
                        captured += os.read(master, 65536)
                    except OSError:
                        break
                clean, foregrounds = terminal_text_and_backgrounds(captured, foreground=True)
                if fragment in clean and "" in clean[clean.index(fragment):]:
                    break
            self.assertIn(fragment, clean)
            left = clean.index(fragment)
            right = clean.index("", left)
            _, backgrounds = terminal_text_and_backgrounds(captured)
            self.assertEqual(backgrounds[left + 1:left + 3], [(2, 251, 146, 60)] * 2)
            self.assertEqual(backgrounds[left + 3], (2, 229, 231, 235))
            self.assertEqual(foregrounds[left], (2, 251, 146, 60))
            self.assertEqual(foregrounds[right], (2, 229, 231, 235))
            self.assertNotRegex(clean[left:right], "[🔵🟠🟣⚪]")
        finally:
            client.terminate()
            try:
                client.wait(timeout=3)
            except subprocess.TimeoutExpired:
                client.kill()
                client.wait(timeout=3)
            os.close(master)

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

    def test_invalid_pid_and_headless_process_never_emit_json_or_change_the_pane(self):
        self.start()
        self.publish(self.pane)
        before = self.tm("display-message", "-p", "-t", self.pane, "#{pane_title}")
        headless = subprocess.Popen(["/bin/sleep", "60"], stdout=subprocess.PIPE,
                                    stderr=subprocess.DEVNULL, env=self.env)
        try:
            cases = (("working", {"hook_event_name": "PostToolUse"}),
                     ("waiting", {"hook_event_name": "PermissionRequest"}),
                     ("idle", {"hook_event_name": "Stop"}),
                     ("notify", {"hook_event_name": "Notification", "notification_type": "elicitation_dialog"}),
                     ("elicitation", {"hook_event_name": "Elicitation", "mcp_server_name": "mcp", "elicitation_id": "a"}))
            for pid in ("0", "999999999", str(headless.pid)):
                for edge, payload in cases:
                    with self.subTest(pid=pid, edge=edge):
                        self.assertEqual(self.hook(edge, self.pane, {"CLAUDE_PID": pid}, payload), b"")
                        self.assertEqual(self.tm("display-message", "-p", "-t", self.pane, "#{pane_title}"), before)
        finally:
            headless.terminate()
            headless.wait(timeout=3)
            headless.stdout.close()

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
        master, slave = pty.openpty()
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 24, 160, 0, 0))
        client = subprocess.Popen(self.base + ["attach-session", "-t", "alpha"],
                                  env=self.env, stdin=slave, stdout=slave, stderr=slave,
                                  start_new_session=True)
        os.close(slave)
        captured = b""
        clean = ""
        backgrounds = []
        wanted = (" 🔵 0:current ", " 🟠⚪ 1:background ")
        try:
            deadline = time.monotonic() + 5
            while time.monotonic() < deadline:
                if select.select([master], [], [], 0.1)[0]:
                    try:
                        captured += os.read(master, 65536)
                    except OSError:
                        break
                # Strip OSC titles first: the outer tab also contains the same
                # glyphs, and must not accidentally satisfy this status-bar test.
                clean, backgrounds = terminal_text_and_backgrounds(captured)
                if all(fragment in clean for fragment in wanted):
                    break
            for fragment in wanted:
                self.assertIn(fragment, clean)
                offset = clean.index(fragment)
                label = offset + fragment.index(":") + 1
                self.assertIsNotNone(backgrounds[offset + 2])
                self.assertEqual(backgrounds[offset + 2], backgrounds[label])
                self.assertNotEqual(backgrounds[offset], backgrounds[offset + 2])
        finally:
            client.terminate()
            try:
                client.wait(timeout=3)
            except subprocess.TimeoutExpired:
                client.kill()
                client.wait(timeout=3)
            os.close(master)


if __name__ == "__main__":
    unittest.main()
