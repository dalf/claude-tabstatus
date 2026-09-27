"""Regression tests for the corpus's process and terminal fixtures."""
import os
import tempfile
import unittest
from unittest import mock

import runner


class PtyHelpersTest(unittest.TestCase):
    def test_low_numbered_ptys_are_accepted(self):
        # The host may already occupy these numbers. Substitute only the name
        # reported for a real, freshly allocated pair to reproduce a clean CI
        # runner without opening or writing any existing terminal by name.
        for number in range(6):
            with self.subTest(number=number), tempfile.TemporaryDirectory() as tmp:
                helper = runner.Helpers(tmp)
                try:
                    with mock.patch.object(os, "ttyname", return_value=f"/dev/pts/{number}"):
                        pid = helper.pty_pid()
                    self.assertIsNone(helper.pty_proc.poll())
                    self.assertEqual(helper.pty_pid(), pid)
                finally:
                    helper.close()

    def test_two_helpers_have_isolated_raw_output(self):
        with tempfile.TemporaryDirectory() as tmp:
            first, second = runner.Helpers(tmp), runner.Helpers(tmp)
            try:
                first.pty_pid()
                second.pty_pid()
                self.assertNotEqual(first.pty_path, second.pty_path)
                data = b"\x1b]0;fixture\x07\n"
                with open(first.pty_path, "wb", buffering=0) as slave:
                    slave.write(data)
                self.assertEqual(first.drain_pty(), data)
                self.assertEqual(second.drain_pty(), b"")
            finally:
                first.close()
                second.close()


if __name__ == "__main__":
    unittest.main()
