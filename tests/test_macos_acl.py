#!/usr/bin/env python3
"""Native Darwin settings protection: chmod/ownership fixtures and SDK faults.

These tests deliberately do not use sys::security_of to observe preservation.
Every CLI run isolates all four installer/state locations and emits no titles.
Linux discovery skips this suite; the native macOS CI job requires it.
"""
import json
import os
from pathlib import Path
import re
import stat
import subprocess
import sys
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[1]
BIN = Path(os.environ.get("CCTAB_TEST_BIN", ROOT / "bin/tabstatus")).resolve()
KEY = "CLAUDE_CODE_DISABLE_TERMINAL_TITLE"
ORIGINAL = b'{"env":{"ANTHROPIC_API_KEY":"test-secret"},"other":1}\n'


def tool(*args):
    return subprocess.run(args, check=True, capture_output=True, text=True).stdout


def acl(path):
    # Ignore ls's mode/owner/timestamp header, retain ACE order and ALL flags.
    return tuple(line.strip() for line in tool("/bin/ls", "-lde", str(path)).splitlines()
                 if re.match(r"\s*\d+:", line))


def protection(path):
    # The independent text observer additionally includes ACL-level flags, which
    # ls's entry listing omits. Compare both to keep human-readable ACE evidence.
    text = tool(str(DarwinACL.observer), "show", str(path))
    observed = path.stat()
    return (stat.S_IMODE(observed.st_mode), acl(path), text,
            observed.st_uid, observed.st_gid)


def add(path, entry):
    tool("/bin/chmod", "+a", entry, str(path))


@unittest.skipUnless(sys.platform == "darwin", "requires native Darwin ACLs")
class DarwinACL(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.native = tempfile.TemporaryDirectory(prefix="cctab-acl-native-")
        cls.dylib = Path(cls.native.name) / "fault.dylib"
        tool("cc", "-Wall", "-Wextra", "-Werror", "-dynamiclib",
             str(ROOT / "tests/fixtures/darwin_acl_fault.c"), "-o", str(cls.dylib))
        cls.observer = Path(cls.native.name) / "observer"
        tool("cc", "-Wall", "-Wextra", "-Werror",
             str(ROOT / "tests/fixtures/darwin_acl_observer.c"), "-o", str(cls.observer))

    @classmethod
    def tearDownClass(cls):
        cls.native.cleanup()

    def setUp(self):
        self.scratch = tempfile.TemporaryDirectory(prefix="cctab-acl-")
        self.root = Path(self.scratch.name)
        self.home = self.root / "home"
        self.config = self.root / "config"
        self.data = self.root / "data"
        self.state = self.root / "state"
        for path in (self.home, self.config, self.data, self.state):
            path.mkdir()
        self.settings = self.config / "settings.json"
        self.settings.write_bytes(ORIGINAL)
        self.env = os.environ.copy()
        for name in ("CLAUDE_PID", "TMUX", "STY", "DYLD_INSERT_LIBRARIES",
                     "CCTAB_TEST_ACL_FAULT", "CCTAB_TEST_ACL_LOG", "CCTAB_TEST_COPY_GID"):
            self.env.pop(name, None)
        self.env.update(HOME=str(self.home), CLAUDE_CONFIG_DIR=str(self.config),
                        XDG_DATA_HOME=str(self.data), CCTAB_STATE_DIR=str(self.state),
                        CCTAB_DRY_RUN="1")
        self.root_owned = False

    def tearDown(self):
        if self.root_owned:
            # Privileged tests own only this freshly allocated fixture. Restore
            # ownership without following symlinks before ordinary cleanup.
            tool("sudo", "-n", "/usr/sbin/chown", "-R", "-P",
                 f"{os.getuid()}:{os.getgid()}", str(self.root))
        self.scratch.cleanup()

    def run_cli(self, *args, umask=0o022, fault=None, ok=True, privileged=False):
        env = self.env.copy()
        if fault:
            env.update(DYLD_INSERT_LIBRARIES=str(self.dylib),
                       CCTAB_TEST_ACL_FAULT=fault,
                       CCTAB_TEST_ACL_LOG=str(self.root / "fault.log"),
                       CCTAB_TEST_COPY_GID=str(self.config.stat().st_gid))
        command = [str(BIN), *args]
        if privileged:
            self.root_owned = True
            # sudo's default HOME must never reach install. Use only the isolated
            # locations and a minimal environment; no ambient session or terminal.
            isolated = {name: env[name] for name in
                        ("HOME", "CLAUDE_CONFIG_DIR", "XDG_DATA_HOME", "CCTAB_STATE_DIR",
                         "CCTAB_DRY_RUN")}
            command = ["sudo", "-n", "/usr/bin/env", "-i", "PATH=/usr/bin:/bin:/usr/sbin:/sbin",
                       *(f"{name}={value}" for name, value in isolated.items()), *command]
        result = subprocess.run(command, env=env, cwd=self.root,
                                umask=umask, capture_output=True, text=True, timeout=30)
        self.assertEqual(result.returncode, 0 if ok else 1, result.stdout + result.stderr)
        self.assertFalse(list(self.root.rglob("*.cctab-tmp.*")), "temporary output left behind")
        self.assertFalse(list(self.root.rglob("*.cctab-owner-probe.*")), "ownership probe left behind")
        return result

    def alternate_group(self, optional=False):
        current = self.config.stat().st_gid
        groups = [gid for gid in os.getgroups() if gid != current]
        if not groups:
            if os.environ.get("CCTAB_TEST_REQUIRE_SUDO") == "1":
                self.fail("native CI requires a supplementary group fixture")
            if optional:
                return current
            self.skipTest("requires a second group membership")
        return groups[0]

    def require_sudo(self):
        available = subprocess.run(["sudo", "-n", "/usr/bin/true"],
                                   capture_output=True, timeout=10).returncode == 0
        if os.geteuid() == 0 or not available:
            if os.environ.get("CCTAB_TEST_REQUIRE_SUDO") == "1":
                self.fail("native CI requires a non-root runner with passwordless sudo")
            self.skipTest("requires a non-root runner with passwordless sudo")

    def explicit(self, path=None):
        path = path or self.settings
        tool("/bin/chmod", "-N", str(path))
        # Principal nobody is distinct from the test process, so deny read/write
        # does not disable the fixture. Test both tags and inheritance control bits.
        add(path, "user:nobody deny read,write,execute")
        add(path, "user:nobody deny append,file_inherit,directory_inherit,limit_inherit,only_inherit")
        add(path, "group:everyone allow read,readattr,readsecurity")
        self.assertIn("deny", " ".join(acl(path)))
        self.assertIn("allow", " ".join(acl(path)))
        self.assertIn("limit_inherit", " ".join(acl(path)))

    def inherit_directory(self):
        add(self.config, "group:everyone allow read,file_inherit,directory_inherit")

    def lifecycle(self, umask):
        wanted = protection(self.settings)
        backup = Path(str(self.settings) + ".cctab-preinstall")
        safety = Path(str(self.settings) + ".cctab-preuninstall")
        self.run_cli("install", umask=umask)
        self.assertEqual(protection(self.settings), wanted)
        self.assertEqual(protection(backup), wanted)
        self.assertEqual(backup.read_bytes(), ORIGINAL)
        self.assertIn(KEY, json.loads(self.settings.read_bytes())["env"])

        # A refresh, including a real rewrite after the key was removed externally.
        self.run_cli("install", umask=umask)
        self.settings.write_bytes(ORIGINAL)
        self.run_cli("install", umask=umask)
        self.assertEqual(protection(self.settings), wanted)
        self.assertEqual(protection(backup), wanted)
        before = self.settings.read_bytes()
        self.run_cli("uninstall", umask=umask)
        self.assertEqual(protection(self.settings), wanted)
        self.assertEqual(protection(safety), wanted)
        self.assertEqual(safety.read_bytes(), before)
        self.assertNotIn(KEY, json.loads(self.settings.read_bytes())["env"])

        self.run_cli("install", umask=umask)
        # Restore over a live file uses the live protection, even when the backup
        # has subsequently acquired different mode bits and a different ACL.
        tool("/bin/chmod", "-N", str(backup))
        add(backup, "user:nobody deny execute")
        os.chmod(backup, 0o664)
        # Give the backup a different group too when the source was moved from
        # elsewhere. A live restore must still select the live file's ownership.
        os.chown(backup, -1, self.config.stat().st_gid)
        backup_protection = protection(backup)
        self.assertNotEqual(backup_protection, wanted)
        self.run_cli("uninstall", "--restore-backup", umask=umask)
        self.assertEqual(self.settings.read_bytes(), backup.read_bytes())
        self.assertEqual(protection(self.settings), wanted)
        self.assertEqual(protection(safety), wanted)

        # Restore a missing file from the backup's protection.
        self.settings.unlink()
        self.run_cli("uninstall", "--restore-backup", umask=umask)
        self.assertEqual(protection(self.settings), backup_protection)

    def test_explicit_acl_and_modes_through_every_settings_operation(self):
        self.explicit()
        for umask, mode in ((0o022, 0o640), (0o777, 0o600)):
            with self.subTest(umask=umask, mode=mode):
                self.settings.write_bytes(ORIGINAL)
                self.explicit()
                os.chmod(self.settings, mode)
                self.lifecycle(umask)

    def test_source_group_and_special_mode_survive_every_settings_operation(self):
        self.explicit()
        group = self.alternate_group()
        os.chown(self.settings, -1, group)
        control = self.config / "control"
        control.touch()
        self.assertNotEqual(control.stat().st_gid, self.settings.stat().st_gid)
        control.unlink()
        for umask in (0o022, 0o777):
            with self.subTest(umask=umask):
                self.settings.write_bytes(ORIGINAL)
                self.explicit()
                os.chown(self.settings, -1, group)
                os.chmod(self.settings, 0o2640)
                self.lifecycle(umask)

    def test_inherited_acl_moved_from_another_directory(self):
        other = self.root / "elsewhere"
        other.mkdir()
        add(other, "user:nobody deny read,write,file_inherit,directory_inherit")
        moved = other / "moved"
        moved.write_bytes(ORIGINAL)
        os.chown(moved, -1, self.alternate_group(optional=True))
        moved.replace(self.settings)
        self.assertIn("inherited", " ".join(acl(self.settings)))
        self.inherit_directory()
        os.chmod(self.settings, 0o640)
        self.lifecycle(0o077)

    def test_acl_level_no_inherit_flag_is_preserved(self):
        self.explicit()
        tool(str(self.observer), "no-inherit", str(self.settings))
        self.assertIn("no_inherit", protection(self.settings)[2])
        self.inherit_directory()
        self.lifecycle(0o077)

    def test_existing_no_acl_in_an_inheriting_directory(self):
        self.inherit_directory()
        tool("/bin/chmod", "-N", str(self.settings))
        os.chown(self.settings, -1, self.alternate_group(optional=True))
        self.assertEqual(acl(self.settings), ())
        self.lifecycle(0o777)

    def test_missing_settings_takes_normal_directory_inheritance(self):
        self.inherit_directory()
        self.settings.unlink()
        self.run_cli("install")
        self.assertIn("inherited", " ".join(acl(self.settings)))

    def test_settings_symlink_updates_target_and_preserves_link(self):
        target = self.root / "target.json"
        self.settings.replace(target)
        self.settings.symlink_to(target)
        self.explicit(target)
        os.chown(target, -1, self.alternate_group(optional=True))
        wanted = protection(target)
        link = os.readlink(self.settings)
        for args in (("install",), ("install",), ("uninstall",),
                     ("install",), ("uninstall", "--restore-backup")):
            self.run_cli(*args)
            self.assertTrue(self.settings.is_symlink())
            self.assertEqual(os.readlink(self.settings), link)
            self.assertEqual(protection(target), wanted)
        for suffix in (".cctab-preinstall", ".cctab-preuninstall"):
            self.assertEqual(protection(Path(str(target) + suffix)), wanted)

    def test_backups_retain_non_acl_metadata(self):
        self.explicit()
        os.setxattr(self.settings, "user.cctab-test", b"metadata")
        os.utime(self.settings, ns=(1_600_000_000_000_000_000,) * 2)
        original_stat = self.settings.stat()
        self.run_cli("install")
        backup = Path(str(self.settings) + ".cctab-preinstall")
        self.assertEqual(os.getxattr(backup, "user.cctab-test"), b"metadata")
        self.assertEqual(backup.stat().st_mtime_ns, original_stat.st_mtime_ns)
        self.assertEqual(backup.stat().st_birthtime, original_stat.st_birthtime)
        # The byte rewrite's xattr policy is unchanged. Give the live file an
        # attribute and check that the pre-uninstall copy also retains it.
        os.setxattr(self.settings, "user.cctab-test", b"safety")
        live_stat = self.settings.stat()
        self.run_cli("uninstall")
        safety = Path(str(self.settings) + ".cctab-preuninstall")
        self.assertEqual(os.getxattr(safety, "user.cctab-test"), b"safety")
        self.assertEqual(safety.stat().st_mtime_ns, live_stat.st_mtime_ns)
        self.assertEqual(safety.stat().st_birthtime, live_stat.st_birthtime)

    def test_inspection_failures_are_preflighted_even_with_force(self):
        self.explicit()
        wanted = protection(self.settings)
        for fault in ("get", "get_enoent", "get_unsupported", "unsupported"):
            for command in ("install", "uninstall"):
                for force in ((), ("--force",)):
                    with self.subTest(fault=fault, command=command, force=force):
                        result = self.run_cli(command, *force, fault=fault, ok=False)
                        self.assertIn("access control list", result.stderr)
                        self.assertIn("Nothing has been changed", result.stderr)
                        self.assertEqual(self.settings.read_bytes(), ORIGINAL)
                        self.assertEqual(protection(self.settings), wanted)
                        self.assertEqual(set(p.name for p in self.config.iterdir()), {"settings.json"})
                        self.assertEqual(list(self.data.iterdir()), [])

    def test_application_and_verification_failures_keep_original_and_cleanup(self):
        self.explicit()
        self.run_cli("install")
        wanted = protection(self.settings)
        before = self.settings.read_bytes()
        # Keep the existing safety copy intact too, even when its replacement fails.
        safety = Path(str(self.settings) + ".cctab-preuninstall")
        safety.write_bytes(b"old safety copy")
        safety_wanted = protection(safety)
        for fault in ("set", "lost_acl", "open", "final_mode", "copy"):
            with self.subTest(fault=fault):
                self.run_cli("uninstall", "--force", fault=fault, ok=False)
                self.assertEqual(self.settings.read_bytes(), before)
                self.assertEqual(protection(self.settings), wanted)
                self.assertEqual(safety.read_bytes(), b"old safety copy")
                self.assertEqual(protection(safety), safety_wanted)
                # The existing pre-install backup means a refresh reaches the
                # SETTINGS replacement itself, rather than failing on a copy.
                if fault != "copy":
                    self.settings.write_bytes(ORIGINAL)
                    self.run_cli("install", "--force", fault=fault, ok=False)
                    self.assertEqual(self.settings.read_bytes(), ORIGINAL)
                    self.assertEqual(protection(self.settings), wanted)
                    self.settings.write_bytes(before)
        self.assertIn("private empty staging", (self.root / "fault.log").read_text())

    def test_ownership_failures_refuse_preflight_even_with_force(self):
        self.explicit()
        os.chown(self.settings, -1, self.alternate_group())
        wanted = protection(self.settings)
        for fault in ("chown", "lost_owner"):
            for command in ("install", "uninstall"):
                for force in ((), ("--force",)):
                    with self.subTest(fault=fault, command=command, force=force):
                        result = self.run_cli(command, *force, fault=fault, ok=False)
                        self.assertIn("owner/group", result.stderr)
                        self.assertIn("Nothing has been changed", result.stderr)
                        self.assertEqual(self.settings.read_bytes(), ORIGINAL)
                        self.assertEqual(protection(self.settings), wanted)
                        self.assertEqual(set(p.name for p in self.config.iterdir()), {"settings.json"})
                        self.assertEqual(list(self.data.iterdir()), [])
        self.assertIn("private empty staging", (self.root / "fault.log").read_text())

    def test_late_ownership_and_copy_verification_failures_keep_original(self):
        self.explicit()
        os.chown(self.settings, -1, self.alternate_group())
        self.run_cli("install")
        wanted = protection(self.settings)
        before = self.settings.read_bytes()
        safety = Path(str(self.settings) + ".cctab-preuninstall")
        safety.write_bytes(b"old safety copy")
        safety_wanted = protection(safety)
        for fault in ("late_chown", "late_lost_owner", "copy_group"):
            with self.subTest(fault=fault):
                result = self.run_cli("uninstall", "--force", fault=fault, ok=False)
                self.assertIn("owner/group", result.stderr)
                self.assertEqual(self.settings.read_bytes(), before)
                self.assertEqual(protection(self.settings), wanted)
                self.assertEqual(safety.read_bytes(), b"old safety copy")
                self.assertEqual(protection(safety), safety_wanted)
                if fault != "copy_group":
                    self.settings.write_bytes(ORIGINAL)
                    self.run_cli("install", "--force", fault=fault, ok=False)
                    self.assertEqual(self.settings.read_bytes(), ORIGINAL)
                    self.assertEqual(protection(self.settings), wanted)
                    self.settings.write_bytes(before)

    def test_foreign_owner_is_refused_without_privileges(self):
        self.require_sudo()
        self.explicit()
        os.chmod(self.settings, 0o666)  # writable, but the caller cannot recreate its UID
        self.root_owned = True
        tool("sudo", "-n", "/usr/sbin/chown", "0", str(self.settings))
        wanted = protection(self.settings)
        self.assertEqual(wanted[3], 0)
        for command in ("install", "uninstall"):
            for force in ((), ("--force",)):
                result = self.run_cli(command, *force, ok=False)
                self.assertIn("owner/group", result.stderr)
                self.assertIn("Nothing has been changed", result.stderr)
                self.assertEqual(self.settings.read_bytes(), ORIGINAL)
                self.assertEqual(protection(self.settings), wanted)
                self.assertEqual(set(p.name for p in self.config.iterdir()), {"settings.json"})
                self.assertEqual(list(self.data.iterdir()), [])

    def test_privileged_writer_preserves_a_different_owner(self):
        self.require_sudo()
        self.explicit()
        os.chown(self.settings, -1, self.alternate_group())
        os.chmod(self.settings, 0o2640)
        wanted = protection(self.settings)
        self.assertNotEqual(wanted[3], 0)
        backup = Path(str(self.settings) + ".cctab-preinstall")
        safety = Path(str(self.settings) + ".cctab-preuninstall")
        for args in (("install",), ("install",), ("uninstall",),
                     ("install",), ("uninstall", "--restore-backup")):
            self.run_cli(*args, privileged=True, umask=0o077)
            self.assertEqual(protection(self.settings), wanted)
        self.assertEqual(protection(backup), wanted)
        self.assertEqual(protection(safety), wanted)
        self.settings.unlink()
        self.run_cli("uninstall", "--restore-backup", privileged=True)
        self.assertEqual(protection(self.settings), wanted)

    def test_missing_live_restore_preflights_the_backups_acl(self):
        self.explicit()
        self.run_cli("install")
        backup = Path(str(self.settings) + ".cctab-preinstall")
        wanted = protection(backup)
        self.settings.unlink()
        result = self.run_cli("uninstall", "--restore-backup", "--force", fault="get", ok=False)
        self.assertIn("Nothing has been changed", result.stderr)
        self.assertFalse(self.settings.exists())
        self.assertEqual(protection(backup), wanted)
        self.assertTrue((self.config / "skills/claude-tabstatus").is_symlink())

    def test_missing_live_restore_preflights_the_backups_ownership(self):
        self.explicit()
        os.chown(self.settings, -1, self.alternate_group())
        self.run_cli("install")
        backup = Path(str(self.settings) + ".cctab-preinstall")
        wanted = protection(backup)
        self.settings.unlink()
        result = self.run_cli("uninstall", "--restore-backup", "--force", fault="chown", ok=False)
        self.assertIn("owner/group", result.stderr)
        self.assertIn("Nothing has been changed", result.stderr)
        self.assertFalse(self.settings.exists())
        self.assertEqual(backup.read_bytes(), ORIGINAL)
        self.assertEqual(protection(backup), wanted)
        self.assertTrue((self.config / "skills/claude-tabstatus").is_symlink())

    def test_failure_to_remove_staging_acl_refuses_no_acl_replacement(self):
        self.inherit_directory()
        tool("/bin/chmod", "-N", str(self.settings))
        wanted = protection(self.settings)
        self.run_cli("install", "--force", fault="clear", ok=False)
        self.assertEqual(self.settings.read_bytes(), ORIGINAL)
        self.assertEqual(protection(self.settings), wanted)
        self.assertIn("private empty staging", (self.root / "fault.log").read_text())


if __name__ == "__main__":
    unittest.main(verbosity=2)
