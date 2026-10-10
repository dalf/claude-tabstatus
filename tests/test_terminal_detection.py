#!/usr/bin/env python3
"""Compiled-binary family/evidence contract; macOS cases require a native binary.

Fresh environments exclude every ambient terminal/mux signal and explicitly leave
CCTAB_TERMINAL unset for automatic detection. Doctor, dry-run and captured protocol
output only: no terminal application or developer session is a target.
"""
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[1]
BIN = Path(os.environ.get("CCTAB_TEST_BIN", ROOT / "bin/tabstatus")).resolve()


class DetectionFixture(unittest.TestCase):
    def setUp(self):
        tmp = tempfile.TemporaryDirectory(prefix="cctab-detection-")
        self.addCleanup(tmp.cleanup)
        self.root = Path(tmp.name).resolve()
        self.cwd = self.root / "project"
        self.cwd.mkdir()
        self.env = {
            "PATH": os.defpath,
            "HOME": str(self.root),
            "CLAUDE_CONFIG_DIR": str(self.root / "config"),
            "XDG_DATA_HOME": str(self.root / "data"),
            "CCTAB_STATE_DIR": str(self.root / "state"),
            "CCTAB_GLYPH_WORKING": "WORK",
            "CCTAB_DRY_RUN": "1",
        }
        # Windows process APIs need the system directory; terminal variables are
        # still absent. Neither doctor nor protocol output opens a session console.
        if sys.platform == "win32":
            self.env["SystemRoot"] = os.environ["SystemRoot"]
        self.env.pop("CCTAB_TERMINAL", None)

    def run_binary(self, edge="doctor", values=None):
        env = self.env.copy()
        env.update(values or {})
        result = subprocess.run(
            [str(BIN), edge], cwd=self.cwd, env=env, input=b"{}",
            capture_output=True, timeout=10)
        self.assertEqual((result.returncode, result.stderr), (0, b""))
        return result.stdout

    def assert_detection(self, values, family, evidence):
        output = self.run_binary(values=values).decode()
        surface = next(line for line in output.splitlines() if line.startswith("surface "))
        self.assertEqual(surface.split()[1], family, output)
        row = next(line for line in output.splitlines() if line.startswith("  evidence "))
        label, verdict, reason = row.split(maxsplit=2)
        self.assertEqual(label, "evidence")
        self.assertEqual((verdict, reason), evidence, output)

    def surface_report(self, values):
        # Platform diagnostics include this subprocess's PID and start time.
        # Keep the selected family, evidence and every surface capability row.
        lines = self.run_binary(values=values).splitlines(keepends=True)
        start = next(i for i, line in enumerate(lines) if line.startswith(b"surface "))
        end = next(i for i, line in enumerate(lines)
                   if i > start and line.startswith(b"multiplexer "))
        self.assertTrue(any(line.startswith(b"  evidence ") for line in lines[start:end]))
        return b"".join(lines[start:end])


class TerminalDetectionTests(DetectionFixture):
    def test_override_stops_automatic_detection_including_unknown(self):
        signals = {"ITERM_SESSION_ID": "session", "LC_TERMINAL": "iTerm2",
                   "TERM_PROGRAM": "Apple_Terminal", "KONSOLE_VERSION": "260400",
                   "WT_SESSION": "session"}
        for value, family in [("ITeRm2", "iterm2"), ("Apple-Terminal", "apple-terminal"),
                              ("unknown-terminal", "unknown"), (" iterm2", "unknown")]:
            with self.subTest(value=value):
                self.assert_detection({**signals, "CCTAB_TERMINAL": value}, family,
                                      ("ok", "CCTAB_TERMINAL=" + value))

    def test_empty_override_is_unset_and_presence_evidence_is_unchanged(self):
        if sys.platform == "darwin":
            variable, family = "ITERM_SESSION_ID", "iterm2"
        elif sys.platform == "win32":
            variable, family = "WT_SESSION", "windows-terminal"
        else:
            variable, family = "KONSOLE_VERSION", "konsole"
        for value in ["0", " ", "arbitrary"]:
            with self.subTest(value=value):
                self.assert_detection({variable: value, "CCTAB_TERMINAL": ""}, family,
                                      ("ok", "$" + variable))
        for values in [{}, {variable: ""}]:
            self.assert_detection(values, "unknown",
                                  ("n/a", "nothing in the environment named it"))

    def test_explicit_overrides_remain_effective_under_mux_vetoes(self):
        for mux in [{"TMUX": "malformed"}, {"STY": "screen"},
                    {"TMUX": "malformed", "CCTAB_NO_TMUX": "1"},
                    {"TMUX": "malformed", "STY": "screen"}]:
            for value, family in [("ITERM2", "iterm2"), ("unsupported", "unknown")]:
                with self.subTest(mux=mux, value=value):
                    self.assert_detection({**mux, "CCTAB_TERMINAL": value}, family,
                                          ("ok", "CCTAB_TERMINAL=" + value))

    def test_family_hints_and_vendor_version_variables_do_not_promote_capabilities(self):
        for values, family in [({"LC_TERMINAL": "iTerm2"}, "iterm2"),
                               ({"TERM_PROGRAM": "iTerm.app"}, "iterm2"),
                               ({"TERM_PROGRAM": "Apple_Terminal"}, "apple-terminal")]:
            # macOS exercises automatic detection; other hosts reach the same
            # capability rows through overrides and check report stability too.
            if sys.platform != "darwin":
                values = {**values, "CCTAB_TERMINAL": family}
            baseline = self.surface_report(values)
            for version in ["999999", "3.5.0", "invalid"]:
                with self.subTest(values=values, version=version):
                    reported = self.surface_report({**values, "LC_TERMINAL_VERSION": version,
                                                    "TERM_PROGRAM_VERSION": version})
                    self.assertEqual(reported, baseline)


@unittest.skipUnless(sys.platform == "darwin", "requires native macOS binary execution")
class MacTerminalDetectionTests(DetectionFixture):
    def test_every_verified_mapping_reports_the_matched_value(self):
        for values, family, evidence in [
            ({"LC_TERMINAL": "iTerm2"}, "iterm2", "$LC_TERMINAL=iTerm2"),
            ({"TERM_PROGRAM": "iTerm.app"}, "iterm2", "$TERM_PROGRAM=iTerm.app"),
            ({"TERM_PROGRAM": "Apple_Terminal"}, "apple-terminal", "$TERM_PROGRAM=Apple_Terminal"),
        ]:
            with self.subTest(values=values):
                self.assert_detection(values, family, ("ok", evidence))

    def test_arbitrary_lc_terminal_and_unsupported_term_program_do_not_match(self):
        for var in ["LC_TERMINAL", "TERM_PROGRAM"]:
            for value in ["", "some-other-terminal", "tmux", "WezTerm", "vscode",
                          "ghostty", "iterm2", "apple-terminal", "iTerm.app-suffix"]:
                with self.subTest(var=var, value=value):
                    self.assert_detection({var: value}, "unknown",
                                          ("n/a", "nothing in the environment named it"))

    def test_case_whitespace_prefixes_suffixes_and_invalid_bytes_are_rejected(self):
        for var, value in [("LC_TERMINAL", "iTerm2"), ("TERM_PROGRAM", "iTerm.app"),
                           ("TERM_PROGRAM", "Apple_Terminal")]:
            for miss in [value.lower(), value.upper(), " " + value, value + " ",
                         value + "\n", value + "\t", "x" + value, value + "x",
                         value[:-1], value + "\udcff"]:
                with self.subTest(var=var, miss=miss):
                    self.assert_detection({var: miss}, "unknown",
                                          ("n/a", "nothing in the environment named it"))

    def test_later_valid_probe_is_not_blocked_by_an_earlier_invalid_value(self):
        for value in ["some-other-terminal", "iTerm2 ", "\udcff", ""]:
            self.assert_detection({"ITERM_SESSION_ID": "", "LC_TERMINAL": value,
                                   "TERM_PROGRAM": "Apple_Terminal"}, "apple-terminal",
                                  ("ok", "$TERM_PROGRAM=Apple_Terminal"))

    def test_disagreement_preserves_dedicated_then_lc_then_term_program_precedence(self):
        self.assert_detection({"ITERM_SESSION_ID": "session", "LC_TERMINAL": "iTerm2",
                               "TERM_PROGRAM": "Apple_Terminal"}, "iterm2",
                              ("ok", "$ITERM_SESSION_ID"))
        self.assert_detection({"ITERM_SESSION_ID": "session", "LC_TERMINAL": "unrelated",
                               "TERM_PROGRAM": "Apple_Terminal"}, "iterm2",
                              ("ok", "$ITERM_SESSION_ID"))
        self.assert_detection({"LC_TERMINAL": "iTerm2", "TERM_PROGRAM": "Apple_Terminal"},
                              "iterm2", ("ok", "$LC_TERMINAL=iTerm2"))

    def test_mux_claims_veto_every_inherited_probe_even_if_malformed_or_disabled(self):
        for signal in [{"ITERM_SESSION_ID": "session"}, {"LC_TERMINAL": "iTerm2"},
                       {"TERM_PROGRAM": "iTerm.app"}, {"TERM_PROGRAM": "Apple_Terminal"}]:
            for mux in [{"TMUX": str(self.root / "absent-socket") + ",1,0"},
                        {"TMUX": "malformed"}, {"STY": "screen"},
                        {"TMUX": "malformed", "STY": "screen"},
                        {"TMUX": "malformed", "CCTAB_NO_TMUX": "1"}]:
                with self.subTest(signal=signal, mux=mux):
                    self.assert_detection({**signal, **mux}, "unknown",
                        ("n/a", "a multiplexer swallowed the environment's evidence"))
        self.assert_detection({"TMUX": "", "STY": "", "CCTAB_TERMINAL": "",
                               "TERM_PROGRAM": "Apple_Terminal"}, "apple-terminal",
                              ("ok", "$TERM_PROGRAM=Apple_Terminal"))

    def test_recognised_families_keep_exact_ordinary_title_protocol_bytes(self):
        for values in [{"LC_TERMINAL": "iTerm2"}, {"TERM_PROGRAM": "iTerm.app"},
                       {"TERM_PROGRAM": "Apple_Terminal"}]:
            with self.subTest(values=values):
                self.assertEqual(self.run_binary("working", values), b"WORK ~/project\n")
                protocol = self.run_binary("working", {**values, "CCTAB_DRY_RUN": ""})
                self.assertEqual(json.loads(protocol), {
                    "terminalSequence": "\x1b]0;WORK ~/project\x07", "suppressOutput": True})


if __name__ == "__main__":
    unittest.main(verbosity=2)
