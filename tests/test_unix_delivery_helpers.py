#!/usr/bin/env python3
"""Link the native probe against both panic strategies and release LTO bitcode.

The tiny stand-in dependency tests compiler compatibility, not terminal policy.
It exercises Cargo's bitcode-only dependency form without a Darwin host.
"""
from pathlib import Path
import subprocess
import tempfile
import unittest

import test_unix_delivery as delivery


class ProbeCompilationTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.tmp = tempfile.TemporaryDirectory(prefix="cctab-probe-compilation-")
        cls.addClassCleanup(cls.tmp.cleanup)
        cls.root = Path(cls.tmp.name)
        version = subprocess.run(["rustc", "-vV"], check=True, capture_output=True, text=True)
        cls.target = next(line.removeprefix("host: ") for line in version.stdout.splitlines()
                          if line.startswith("host: "))
        cls.dependency = cls.root / "libc.rs"
        cls.dependency.write_text('pub fn witness() -> u32 { 7 }\n')
        cls.consumer = cls.root / "probe.rs"
        cls.consumer.write_text('fn main() { println!("{}", libc::witness()); }\n')

    def library(self, strategy, bitcode_only=False):
        library = self.root / f"liblibc_{strategy}_{bitcode_only}.rlib"
        # Cargo uses linker-plugin-lto for release dependencies under Rust LTO:
        # these archives contain bitcode instead of native machine-code objects.
        flags = (["-C", "linker-plugin-lto", "-C", "embed-bitcode=no"]
                 if bitcode_only else ["-C", "embed-bitcode=yes"])
        delivery.compile_helper(["rustc", "--edition=2021", "--crate-name", "libc",
                                 "--crate-type", "rlib", "-C", f"panic={strategy}",
                                 *flags, str(self.dependency), "-o", str(library)])
        return library

    def test_probe_links_abort_and_unwind_libraries_with_release_bitcode(self):
        for strategy in ("abort", "unwind"):
            for bitcode_only in (False, True):
                with self.subTest(strategy=strategy, bitcode_only=bitcode_only):
                    output = self.root / f"probe-{strategy}-{bitcode_only}"
                    library = self.library(strategy, bitcode_only)
                    delivery.compile_contract_probe(self.target, library, output,
                                                    source=self.consumer)
                    result = subprocess.run([str(output)], capture_output=True, timeout=5)
                    self.assertEqual((result.returncode, result.stdout, result.stderr), (0, b"7\n", b""))

    def test_default_unwind_consumer_fails_with_visible_compiler_diagnostic(self):
        # Negative control: the old command must fail with the actual diagnostic.
        library = self.library("abort")
        with self.assertRaises(RuntimeError) as error:
            delivery.compile_helper(["rustc", "--edition=2021", "--target", self.target,
                                     str(self.consumer), "--extern", f"libc={library}",
                                     "-o", str(self.root / "bad-probe")])
        self.assertIn("requires panic strategy `abort`", str(error.exception))
        self.assertIn("strategy of `unwind`", str(error.exception))


if __name__ == "__main__":
    unittest.main()
