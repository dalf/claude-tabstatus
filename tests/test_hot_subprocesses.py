"""Failure-mode tests for the required subprocess checker (no ptrace needed)."""
from pathlib import Path
import tempfile
import unittest

from check_hot_subprocesses import SYSCALLS, Violation, check_trace, traced


START = '123 execve("/candidate", ["/candidate", "working"], 0x123) = 0\n'
END = '123 +++ exited with 0 +++\n'


class CheckerTests(unittest.TestCase):
    def test_clean_completed_trace(self):
        check_trace(START + END, 0)

    def test_every_syscall_rejected_even_if_failed(self):
        for name in SYSCALLS:
            with self.subTest(name=name), self.assertRaises(Violation):
                check_trace(START + f'123 {name}(NULL) = -1 ENOSYS\n' + END, 0)

    def test_unfinished_and_resumed_call_rejected(self):
        with self.assertRaises(Violation):
            check_trace(START + '123 clone(flags=CLONE_VM <unfinished ...>\n'
                        '123 <... clone resumed>) = 124\n' + END, 0)

    def test_reexec_without_fork_rejected(self):
        with self.assertRaises(Violation):
            check_trace(START + START + END, 0)

    def test_missing_failed_or_incomplete_evidence_rejected(self):
        for trace, status in (("", 0), (END, 0), (START, 0), (START + END, 1),
                              (START.replace(' = 0', ' = -1 ENOENT') + END, 0),
                              (START + '124 +++ exited with 0 +++\n', 0),
                              (START + '123 +++ killed by SIGKILL +++\n', 0)):
            with self.subTest(trace=trace, status=status), self.assertRaises(RuntimeError):
                check_trace(trace, status)

    def test_nonworking_tracers_cannot_reuse_old_log(self):
        with tempfile.TemporaryDirectory() as tmp:
            log = Path(tmp) / "trace"
            for tracer in ("/bin/true", "/bin/false"):
                log.write_text(START + END)
                with self.subTest(tracer=tracer), self.assertRaises(RuntimeError):
                    traced(tracer, ["/bin/true"], {}, tmp, log)

    def test_missing_tracer_fails(self):
        with tempfile.TemporaryDirectory() as tmp, self.assertRaises(FileNotFoundError):
            traced(tmp + "/absent", ["/bin/true"], {}, tmp, Path(tmp) / "trace")


if __name__ == "__main__":
    unittest.main()
