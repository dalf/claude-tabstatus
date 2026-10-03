"""Installer destination guards; fixtures never live in a checkout.

Native reproducer (run before changing the binary):
  CCTAB_TEST_BIN=/path/to/old/tabstatus python3 tests/test_install_paths.py --reproduce-old
It requires and proves case-insensitive lookup, then asserts the old late failure
has written settings, the installation record and the generated tree. Normal
execution requires refusal with an unchanged fixture instead.

The missing-Unicode limitation has a separate native reproducer:
  CCTAB_TEST_BIN=/path/to/f4b9fff/tabstatus python3 tests/test_install_paths.py --reproduce-missing-unicode
It proves Unicode lookup equivalence on disposable APFS, then asserts that the
pre-probe binary refuses a safe tree with both Unicode parents missing.
"""
import os
import errno
import hashlib
import pwd
from pathlib import Path
import stat
import subprocess
import sys
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[1]
BIN = Path(os.environ.get("CCTAB_TEST_BIN", ROOT / "bin/tabstatus")).resolve()
ORIGINAL = b'{"env":{"kept":"value"},"other":1}\n'


def isolated(root):
    env = os.environ.copy()
    for key in ("CLAUDE_PID", "TMUX", "STY", "XDG_RUNTIME_DIR", "LOCALAPPDATA"):
        env.pop(key, None)
    env.update(HOME=str(root / "home"), CLAUDE_CONFIG_DIR=str(root / "config"),
               XDG_DATA_HOME=str(root / "data"), CCTAB_STATE_DIR=str(root / "state"),
               CCTAB_DRY_RUN="1")
    return env


def invoke(root, env, *args):
    return subprocess.run([str(BIN), *map(str, args)], cwd=root, env=env,
                          capture_output=True, text=True, timeout=30)


def case_insensitive(root):
    probe = root / "CaseProbe"
    probe.mkdir()
    try:
        return (root / "caseprobe").exists() and os.path.samefile(probe, root / "caseprobe")
    finally:
        probe.rmdir()


def reproduce_old():
    if sys.platform != "darwin":
        raise SystemExit("Native reproducer requires macOS; cross-compilation is insufficient")
    with tempfile.TemporaryDirectory(prefix="cctab-path-repro-") as tmp:
        root = Path(tmp)
        assert case_insensitive(root), "reproducer needs a case-insensitive filesystem"
        env = isolated(root)
        config = root / "config"
        (config / "skills").mkdir(parents=True)
        (config / "settings.json").write_bytes(ORIGINAL)
        target = config / "SKILLS/claude-tabstatus"
        result = invoke(root, env, "install", "--tree", target)
        assert result.returncode == 1, result.stdout + result.stderr
        assert (target / ".tabstatus-generated").is_file(), result.stdout + result.stderr
        assert (config / "claude-tabstatus.state").is_file()
        assert (config / "settings.json").read_bytes() != ORIGINAL
        assert not (config / "skills/claude-tabstatus").is_symlink()
        print("REPRODUCED: late link failure after tree, record and settings writes")


def reproduce_missing_unicode():
    if sys.platform != "darwin":
        raise SystemExit("Native reproducer requires macOS; cross-compilation is insufficient")
    volume = NativeVolume(sensitive=False)
    try:
        with tempfile.TemporaryDirectory(prefix="unicode-repro-", dir=volume.mount) as tmp:
            root = Path(tmp)
            reference = root / "reference"
            reference.mkdir()
            (reference / "caf\u00e9").mkdir()
            assert os.path.samefile(reference / "caf\u00e9", reference / "cafe\u0301")
            env = isolated(root)
            env["CLAUDE_CONFIG_DIR"] = str(root / "caf\u00e9/config")
            target = root / "cafe\u0301/data/claude-tabstatus"
            before = snapshot(root)
            result = invoke(root, env, "install", "--tree", target)
            assert result.returncode == 1, result.stdout + result.stderr
            assert "cannot establish filesystem equivalence" in result.stderr, result.stderr
            assert snapshot(root) == before
            print("REPRODUCED: safe tree refused because both Unicode ancestor spellings are missing")
    finally:
        volume.close()


def snapshot(root):
    """No links followed; includes file identity, bytes, mode, owner/group and ACLs."""
    result = {}

    def visit(path):
        md = path.lstat()
        kind = stat.S_IFMT(md.st_mode)
        protection = (md.st_mode, md.st_uid, md.st_gid, md.st_dev, md.st_ino)
        if sys.platform == "darwin":
            ls = subprocess.run(["/bin/ls", "-lde", str(path)], capture_output=True, text=True)
            # Only ACL entries: ls's timestamp/name header is irrelevant.
            protection += (tuple(line.strip() for line in ls.stdout.splitlines()[1:]), ls.returncode)
        value = None
        if stat.S_ISLNK(kind):
            value = os.readlink(path)
        elif stat.S_ISREG(kind):
            value = (hashlib.sha256(path.read_bytes()).hexdigest(), md.st_mtime_ns)
        result[str(path.relative_to(root))] = (protection, value)
        if stat.S_ISDIR(kind):
            try:
                children = list(path.iterdir())
            except PermissionError:
                result[str(path.relative_to(root))] += ("inaccessible",)
                return
            for child in children:
                visit(child)

    visit(root)
    return result


class InstallerPaths(unittest.TestCase):
    fixture_parent = None

    def setUp(self):
        self.scratch = tempfile.TemporaryDirectory(prefix="cctab-paths-", dir=self.fixture_parent)
        self.root = Path(self.scratch.name)
        self.env = isolated(self.root)
        self.home = self.root / "home"
        self.config = self.root / "config"
        self.skills = self.config / "skills"
        self.data = self.root / "data"
        self.tree = self.data / "claude-tabstatus"
        self.link = self.skills / "claude-tabstatus"
        for path in (self.home, self.config, self.data, self.root / "state"):
            path.mkdir()
        self.settings = self.config / "settings.json"
        self.settings.write_bytes(ORIGINAL)
        self.settings.chmod(0o640)
        if sys.platform == "darwin":
            user = pwd.getpwuid(os.getuid()).pw_name
            subprocess.run(["/bin/chmod", "+a", f"user:{user} allow read,write,readattr,readsecurity",
                            str(self.settings)], check=True, capture_output=True)

    def tearDown(self):
        self.scratch.cleanup()

    def cli(self, *args, ok=True):
        result = invoke(self.root, self.env, *args)
        self.assertEqual(result.returncode, 0 if ok else 1, result.stdout + result.stderr)
        return result.stdout + result.stderr

    def refuse(self, *args):
        return self.unchanged_refusal("install", *args)

    def unchanged_refusal(self, verb, *args):
        before = snapshot(self.root)
        output = self.cli(verb, *args, ok=False)
        self.assertIn("Nothing has been changed", output)
        self.assertEqual(snapshot(self.root), before, output)
        return output

    def test_skills_existing_missing_and_prefix_boundaries(self):
        for exists in (False, True):
            if exists:
                self.skills.mkdir()
            for target in (self.skills, self.link, self.skills / "missing/a/tree"):
                with self.subTest(exists=exists, target=target):
                    self.refuse("--tree", target)
        allowed = self.config / "skills2/claude-tabstatus"
        self.cli("install", "--tree", allowed)
        self.assertTrue((allowed / ".tabstatus-generated").is_file())
        self.cli("uninstall")
        self.assertFalse(allowed.exists())

    def test_default_environment_destination_is_guarded(self):
        for exists in (False, True):
            if exists:
                self.skills.mkdir()
            self.env["XDG_DATA_HOME"] = str(self.skills / "missing/a")
            self.refuse()

    def test_ancestor_link_into_skills_with_multiple_missing_components(self):
        self.skills.mkdir()
        alias = self.root / "alias"
        alias.symlink_to(self.skills, target_is_directory=True)
        self.refuse("--tree", alias / "one/two/tree")
        self.env["XDG_DATA_HOME"] = str(alias / "one/two")
        self.refuse()

    def test_skills_link_points_at_actual_destination(self):
        actual = self.root / "actual-skills"
        actual.mkdir()
        self.skills.symlink_to(actual, target_is_directory=True)
        self.refuse("--tree", actual / "missing/tree")

    def test_alias_into_nested_checkout_and_deep_checkout(self):
        checkout = self.home / "code/project"
        (checkout / ".git").mkdir(parents=True)
        (checkout / ".claude-plugin").mkdir()
        (checkout / ".claude-plugin/plugin.json").write_text("{}")
        nested = checkout.joinpath(*(["deep"] * 28))
        nested.mkdir(parents=True)
        alias = self.root / "alias"
        alias.symlink_to(nested, target_is_directory=True)
        self.refuse("--tree", alias / "missing/tree")
        self.refuse("--tree", nested / "missing/tree")
        self.env["XDG_DATA_HOME"] = str(alias / "missing")
        self.refuse()

    def test_unrelated_home_repository_allows_default(self):
        (self.home / ".git").mkdir()
        self.env.pop("XDG_DATA_HOME")
        self.cli("install")
        tree = self.home / ".local/share/claude-tabstatus"
        self.assertTrue((tree / ".tabstatus-generated").is_file())
        self.cli("uninstall")
        self.assertFalse(tree.exists())

    def test_relative_dots_and_symlink_parent_resolution(self):
        self.skills.mkdir()
        self.refuse("--tree", "config/./skills/../skills/missing/tree")
        child = self.skills / "child"
        child.mkdir()
        alias = self.root / "alias"
        alias.symlink_to(child, target_is_directory=True)
        # Lexically alias/.. is root; the kernel reaches skills instead.
        self.refuse("--tree", "alias/../claude-tabstatus")
        self.cli("install", "--tree", "data/../data/./missing/tree")
        self.cli("doctor")
        self.cli("uninstall")
        self.assertFalse((self.data / "missing/tree").exists())

    def test_missing_component_before_dotdot_is_unresolvable(self):
        self.refuse("--tree", "absent/../data/tree")

    def test_existing_and_dangling_root_links_remain_refused(self):
        for name, target in (("live", self.data), ("dangling", self.root / "absent")):
            link = self.root / name
            link.symlink_to(target, target_is_directory=True)
            for spelling in (str(link), str(link) + "/", str(link) + "/."):
                with self.subTest(spelling=spelling):
                    self.refuse("--tree", spelling)

    def test_dangling_and_looping_ancestors_are_not_missing(self):
        dangling = self.root / "dangling"
        dangling.symlink_to(self.root / "absent", target_is_directory=True)
        loop = self.root / "loop"
        loop.symlink_to(loop, target_is_directory=True)
        for ancestor in (dangling, loop):
            self.refuse("--tree", ancestor / "one/two/tree")
            self.env["XDG_DATA_HOME"] = str(ancestor / "one/two")
            self.refuse()
        self.env["XDG_DATA_HOME"] = str(self.data)
        self.skills.symlink_to(dangling, target_is_directory=True)
        self.refuse()
        self.skills.unlink()
        self.skills.symlink_to(loop, target_is_directory=True)
        self.refuse()

    def test_existing_plugin_link_to_file_can_be_repointed(self):
        file = self.root / "file"
        file.write_text("kept")
        self.skills.mkdir()
        self.link.symlink_to(file)
        self.cli("install")
        self.cli("uninstall")
        self.assertTrue(self.link.is_symlink())
        self.assertTrue(os.path.samefile(self.link, file))
        self.assertEqual(file.read_text(), "kept")

    def test_relative_config_and_default_environment(self):
        self.env["CLAUDE_CONFIG_DIR"] = "config/."
        self.env["XDG_DATA_HOME"] = "data/missing"
        self.cli("install")
        self.assertTrue(os.path.samefile(self.link, self.data / "missing/claude-tabstatus"))
        self.assertNotIn("orphan", self.cli("doctor").lower())
        self.cli("uninstall")
        self.assertFalse((self.data / "missing/claude-tabstatus").exists())

    def test_non_directory_ancestor_is_refused(self):
        file = self.root / "file"
        file.write_text("kept")
        self.refuse("--tree", file / "one/two/tree")

    def test_inaccessible_ancestor_refuses_without_changes(self):
        if os.getuid() == 0:
            self.skipTest("permission denial requires an unprivileged process")
        blocked = self.root / "blocked"
        blocked.mkdir()
        blocked.chmod(0)
        try:
            self.refuse("--tree", blocked / "one/two/tree")
            self.env["CLAUDE_CONFIG_DIR"] = str(blocked / "config")
            self.refuse()
        finally:
            blocked.chmod(0o700)

    def test_allowed_ancestor_alias_missing_tail_and_live_identity(self):
        alias = self.root / "data-alias"
        alias.symlink_to(self.data, target_is_directory=True)
        tree = self.data / "one/two/tree"
        self.cli("install", "--tree", alias / "one/two/tree")
        link_before = self.link.lstat().st_ino
        output = self.cli("install", "--tree", tree)
        self.assertIn("already correct", output)
        self.assertNotIn("orphan", output.lower())
        self.assertEqual(self.link.lstat().st_ino, link_before)
        self.assertNotIn("orphan", self.cli("doctor").lower())
        self.assertNotIn("orphan", self.cli("uninstall").lower())
        self.assertFalse(tree.exists())

    def test_equivalent_default_record_and_relative_plugin_target(self):
        self.cli("install")
        alias = self.root / "data-alias"
        alias.symlink_to(self.data, target_is_directory=True)
        self.env["XDG_DATA_HOME"] = str(alias)
        self.link.unlink()
        self.link.symlink_to("../../data-alias/claude-tabstatus", target_is_directory=True)
        for verb in ("install", "doctor"):
            output = self.cli(verb)
            self.assertNotIn("orphan", output.lower())
            if verb == "install":
                self.assertIn("already correct", output)
        output = self.cli("uninstall")
        self.assertNotIn("orphan", output.lower())
        self.assertFalse(self.tree.exists())

    def test_symlinked_generated_components_refused_before_marker_write(self):
        self.cli("install")
        hooks = self.tree / "hooks"
        hooks.rename(self.root / "saved-hooks")
        hooks.symlink_to(self.root / "saved-hooks", target_is_directory=True)
        self.refuse()
        foreign_before = snapshot(self.root / "saved-hooks")
        self.cli("uninstall")
        self.assertEqual(snapshot(self.root / "saved-hooks"), foreign_before)

    def test_settings_symlink_survives_destination_refusal(self):
        real = self.root / "actual-settings.json"
        self.settings.rename(real)
        self.settings.symlink_to(real)
        self.refuse("--tree", self.link)

    def test_case_sensitive_skills_sibling_is_allowed(self):
        if case_insensitive(self.root):
            self.skipTest("case-sensitive coverage runs separately on APFSX in macOS CI")
        self.skills.mkdir()
        other = self.config / "SKILLS"
        other.mkdir()
        self.assertFalse(os.path.samefile(other, self.skills))
        self.cli("install", "--tree", other / "claude-tabstatus")
        self.assertNotIn("orphan", self.cli("doctor").lower())
        self.cli("uninstall")
        self.assertTrue(self.skills.is_dir())


class NativeVolume:
    def __init__(self, sensitive):
        self.scratch = tempfile.TemporaryDirectory(prefix="cctab-path-volume-", dir="/private/var/tmp")
        root = Path(self.scratch.name)
        self.mount = root / "mount"
        self.mount.mkdir()
        self.attached = False
        try:
            subprocess.run(["hdiutil", "create", "-size", "256m", "-type", "SPARSE",
                            "-fs", "Case-sensitive APFS" if sensitive else "APFS", "-volname", "cctab-paths",
                            str(root / "fixture.sparseimage")], check=True, capture_output=True, text=True)
            subprocess.run(["hdiutil", "attach", "-nobrowse", "-mountpoint", str(self.mount),
                            str(root / "fixture.sparseimage")], check=True, capture_output=True, text=True)
            self.attached = True
            assert case_insensitive(self.mount) != sensitive, "volume lookup semantics do not match requested coverage"
            print(f"Native fixture: {'case-sensitive APFSX' if sensitive else 'case-insensitive APFS'}; lookup verified", flush=True)
        except BaseException as error:
            if isinstance(error, subprocess.CalledProcessError):
                print(error.stdout + error.stderr, file=sys.stderr)
            self.close()
            raise

    def close(self):
        if self.attached:
            subprocess.run(["hdiutil", "detach", str(self.mount)], check=True, capture_output=True, text=True)
        self.scratch.cleanup()


def compile_path_faults(cls):
    cls.fault_dylib = Path(cls.volume.scratch.name) / "fault.dylib"
    subprocess.run(["cc", "-Wall", "-Wextra", "-Werror", "-dynamiclib",
                    str(ROOT / "tests/fixtures/darwin_path_fault.c"), "-o", str(cls.fault_dylib)],
                   check=True, capture_output=True, text=True)


class MissingUnicodePaths:
    """Shared native tests: every compared destination still has missing parents."""
    def prove_unicode_lookup(self):
        reference = self.root / "unicode-reference"
        reference.mkdir()
        composed = reference / "caf\u00e9"
        composed.mkdir()
        self.assertTrue(os.path.samefile(composed, reference / "cafe\u0301"),
                        "native normalisation-insensitive lookup is required")
        uppercase = reference / "CAF\u00c9"
        if case_insensitive(self.root):
            self.assertTrue(os.path.samefile(composed, uppercase), "native Unicode case lookup is required")
        else:
            uppercase.mkdir()
            self.assertFalse(os.path.samefile(composed, uppercase), "native Unicode case distinction is required")
            uppercase.rmdir()
        different = reference / "caf\u00e8"
        different.mkdir()
        self.assertFalse(os.path.samefile(composed, different))
        different.rmdir()
        composed.rmdir()
        reference.rmdir()

    def unicode_config(self):
        self.prove_unicode_lookup()
        self.env["CLAUDE_CONFIG_DIR"] = str(self.root / "caf\u00e9/config")

    def test_missing_unicode_allowed_tree_and_live_lifecycle(self):
        self.unicode_config()
        target = self.root / "cafe\u0301/data/one/two/claude-tabstatus"
        self.assertFalse(target.parent.exists())
        original_config = snapshot(self.config)
        self.cli("install", "--tree", target)
        self.assertEqual(snapshot(self.config), original_config)
        self.env["XDG_DATA_HOME"] = str(self.root / "caf\u00e9/data/one/two")
        self.assertIn("already correct", self.cli("install"))
        self.assertNotIn("orphan", self.cli("doctor").lower())
        self.assertNotIn("orphan", self.cli("uninstall").lower())
        self.assertFalse(target.exists())
        self.assertFalse(list(self.root.glob(".cctab-name-probe-*")))

    def test_missing_unicode_aliases_beneath_skills_are_refused(self):
        self.unicode_config()
        target = self.root / "cafe\u0301/config/skills/one/two/tree"
        self.assertFalse((self.root / "caf\u00e9").exists())
        output = self.refuse("--tree", target)
        self.assertIn("skills", output)
        self.assertNotIn("cannot establish", output)
        # The default tree/environment must use the same evidence, including an
        # ancestor link; neither the Unicode parent nor skills exists yet.
        alias = self.root / "alias"
        alias.symlink_to(self.root, target_is_directory=True)
        self.env["XDG_DATA_HOME"] = str(alias / "cafe\u0301/config/skills/one/two")
        self.refuse()

    def test_multiple_missing_unicode_components(self):
        self.unicode_config()
        self.env["CLAUDE_CONFIG_DIR"] = str(self.root / "caf\u00e9/c\u00f4t\u00e9/config")
        self.refuse("--tree", self.root / "cafe\u0301/co\u0302te\u0301/config/skills/new/tree")

    def test_distinct_missing_unicode_names_and_prefix_lookalikes(self):
        self.unicode_config()
        self.cli("install", "--tree", self.root / "caf\u00e8/data/claude-tabstatus")
        self.cli("uninstall")
        self.cli("install", "--tree", self.root / "cafe\u0301/config/skills2/claude-tabstatus")
        self.cli("uninstall")
        self.assertFalse(list(self.root.glob(".cctab-name-probe-*")))

    def test_unicode_probe_failures_refuse_without_installation_writes(self):
        self.unicode_config()
        target = self.root / "cafe\u0301/data/tree"
        self.env["DYLD_INSERT_LIBRARIES"] = str(self.fault_dylib)
        faults = (
            ("probe-create", "cannot probe missing Unicode names", errno.EACCES),
            ("probe-child", "cannot inspect missing Unicode names", errno.EIO),
            ("probe-lookup", "cannot inspect missing Unicode names", errno.EIO),
            ("probe-cleanup", "cannot remove filename probe", errno.EIO),
            ("probe-filesystem", "Unicode filename probes require APFS or HFS", None),
            ("probe-acl-inspect", "cannot probe missing Unicode names", errno.EIO),
            ("probe-acl-iterate", "cannot probe missing Unicode names", errno.EIO),
        )
        for fault, reason, error in faults:
            for force in (False, True):
                with self.subTest(fault=fault, force=force):
                    self.env["CCTAB_TEST_PATH_FAULT"] = fault
                    output = self.refuse(*(["--force"] if force else []), "--tree", target)
                    self.assertIn(reason, output)
                    if error is not None:
                        self.assertIn(f"(os error {error})", output)

    def test_persistent_probe_cleanup_failure_reports_private_remainder(self):
        self.unicode_config()
        before = snapshot(self.root)
        self.env["DYLD_INSERT_LIBRARIES"] = str(self.fault_dylib)
        self.env["CCTAB_TEST_PATH_FAULT"] = "probe-cleanup-persistent"
        output = self.cli("install", "--tree", self.root / "cafe\u0301/data/tree", ok=False)
        remainders = list(self.root.glob(".cctab-name-probe-*"))
        self.assertEqual(len(remainders), 1)
        self.assertIn(str(remainders[0]), output)
        self.assertIn("comparison remains uncertain", output)
        self.assertEqual(remainders[0].stat().st_mode & 0o777, 0o700)
        self.assertEqual(remainders[0].stat().st_uid, os.getuid())
        # A failed cleanup may retain our private probe, never installation
        # content. Fixture teardown removes it; production reports it for inspection.
        after = {name: value for name, value in snapshot(self.root).items()
                 if not name.startswith(".cctab-name-probe-")}
        self.assertEqual(after, before)

    def test_unicode_probe_private_birth_with_inherited_acl(self):
        self.unicode_config()
        subprocess.run(["/bin/chmod", "+a",
                        "everyone allow list,search,add_file,add_subdirectory,file_inherit,directory_inherit",
                        str(self.root)], check=True, capture_output=True)
        self.env["DYLD_INSERT_LIBRARIES"] = str(self.fault_dylib)
        self.env["CCTAB_TEST_PATH_FAULT"] = "probe-observe"
        output = self.refuse("--tree", self.root / "cafe\u0301/config/skills/tree")
        self.assertIn("observed private filename probe", output)


@unittest.skipUnless(sys.platform == "darwin", "native case-insensitive APFS coverage pending on this host")
class DarwinInsensitive(MissingUnicodePaths, InstallerPaths):
    @classmethod
    def setUpClass(cls):
        cls.volume = NativeVolume(sensitive=False)
        cls.addClassCleanup(cls.volume.close)
        cls.fixture_parent = cls.volume.mount
        compile_path_faults(cls)

    def test_native_inspection_failures_are_uncertain(self):
        self.env["DYLD_INSERT_LIBRARIES"] = str(self.fault_dylib)
        self.env["CCTAB_TEST_PATH_FAULT"] = "case"
        self.refuse("--tree", self.config / "SKILLS/new/tree")
        self.env["CCTAB_TEST_PATH_FAULT"] = "resolve"
        self.refuse("--tree", self.data / "one/two/tree")

    def test_native_uninstall_inspection_failure_preserves_installation(self):
        self.cli("install")
        self.env["DYLD_INSERT_LIBRARIES"] = str(self.fault_dylib)
        self.env["CCTAB_TEST_PATH_FAULT"] = "resolve"
        self.unchanged_refusal("uninstall")
        output = self.cli("doctor")
        self.assertIn("cannot identify", output)
        self.assertNotIn("is also a generated tree", output)

    def test_native_reproducer_existing_and_missing_skills(self):
        self.assertTrue(case_insensitive(self.root))
        for exists in (False, True):
            if exists:
                self.skills.mkdir()
            for target in (self.config / "SKILLS/claude-tabstatus", self.config / "SKILLS/one/two/tree"):
                self.refuse("--tree", target)
            self.env["XDG_DATA_HOME"] = str(self.config / "SKILLS/one/two")
            self.refuse()
            alias = self.root / "config-alias"
            if not alias.is_symlink():
                alias.symlink_to(self.config, target_is_directory=True)
            self.refuse("--tree", alias / "SKILLS/one/two/tree")

    def test_missing_config_and_skills_case_aliases(self):
        self.env["CLAUDE_CONFIG_DIR"] = str(self.root / "CLAUDE")
        self.refuse("--tree", self.root / "claude/SKILLS/one/two/tree")

    def test_existing_live_tree_case_and_unicode_aliases(self):
        parent = self.data / "caf\u00e9"
        parent.mkdir()
        tree = parent / "Tree"
        self.cli("install", "--tree", tree)
        alias = self.data / "cafe\u0301/tREE"
        self.assertTrue(os.path.samefile(tree, alias), "APFS Unicode/case lookup coverage must be present")
        self.env["XDG_DATA_HOME"] = str(self.data / "cafe\u0301")
        # Default candidate must also be the same live tree.
        tree.rename(parent / "claude-tabstatus")
        self.link.unlink()
        self.link.symlink_to(parent / "claude-tabstatus")
        output = self.cli("install", "--tree", self.data / "cafe\u0301/CLAUDE-TABSTATUS")
        self.assertIn("already correct", output)
        self.assertNotIn("orphan", self.cli("doctor").lower())
        self.assertNotIn("orphan", self.cli("uninstall").lower())
        self.assertFalse((parent / "claude-tabstatus").exists())

    def test_unicode_alias_into_skills_and_checkout(self):
        parent = self.root / "caf\u00e9"
        parent.mkdir()
        self.env["CLAUDE_CONFIG_DIR"] = str(parent / "config")
        (parent / "config/skills").mkdir(parents=True)
        self.refuse("--tree", self.root / "cafe\u0301/config/SKILLS/new/tree")
        checkout = parent / "checkout"
        (checkout / ".git").mkdir(parents=True)
        (checkout / ".claude-plugin").mkdir()
        (checkout / ".claude-plugin/plugin.json").write_text("{}")
        self.refuse("--tree", self.root / "cafe\u0301/checkout/new/tree")

    def test_missing_unicode_case_aliases_are_refused(self):
        self.unicode_config()
        self.refuse("--tree", self.root / "CAF\u00c9/config/skills/tree")

    def test_var_private_var_live_alias(self):
        real = self.root.resolve()
        self.assertTrue(str(real).startswith("/private/var/"), "fixture must exercise the Darwin /var alias")
        alternate = Path(str(real).removeprefix("/private"))
        self.assertTrue(os.path.samefile(real, alternate))
        self.skills.mkdir()
        self.refuse("--tree", alternate / "config/skills/new/tree")
        self.cli("install")
        output = self.cli("install", "--tree", alternate / "data/claude-tabstatus")
        self.assertIn("already correct", output)
        self.env["XDG_DATA_HOME"] = str(alternate / "data")
        self.assertNotIn("orphan", self.cli("doctor").lower())
        self.assertNotIn("orphan", self.cli("uninstall").lower())


@unittest.skipUnless(sys.platform == "darwin", "native case-sensitive APFSX coverage pending on this host")
class DarwinSensitive(MissingUnicodePaths, InstallerPaths):
    @classmethod
    def setUpClass(cls):
        cls.volume = NativeVolume(sensitive=True)
        cls.addClassCleanup(cls.volume.close)
        cls.fixture_parent = cls.volume.mount
        compile_path_faults(cls)

    def test_distinct_missing_unicode_case_names_are_allowed(self):
        self.unicode_config()
        target = self.root / "CAF\u00c9/config/skills/claude-tabstatus"
        self.cli("install", "--tree", target)
        self.assertFalse(os.path.samefile(self.root / "caf\u00e9", self.root / "CAF\u00c9"))
        self.cli("uninstall")

    def test_missing_case_sensitive_sibling_is_allowed(self):
        self.assertFalse(case_insensitive(self.root))
        self.cli("install", "--tree", self.config / "SKILLS/claude-tabstatus")
        self.assertTrue(self.link.is_symlink())
        self.assertFalse(os.path.samefile(self.skills, self.config / "SKILLS"))
        self.cli("uninstall")

    def test_unicode_normalisation_on_case_sensitive_apfs(self):
        parent = self.root / "caf\u00e9"
        parent.mkdir()
        alias = self.root / "cafe\u0301"
        self.assertTrue(os.path.samefile(parent, alias), "normalisation-insensitive APFSX lookup must be present")
        self.env["CLAUDE_CONFIG_DIR"] = str(parent / "config")
        (parent / "config/skills").mkdir(parents=True)
        self.refuse("--tree", alias / "config/skills/new/tree")
        tree = parent / "allowed/claude-tabstatus"
        self.cli("install", "--tree", tree)
        self.env["XDG_DATA_HOME"] = str(alias / "allowed")
        self.assertIn("already correct", self.cli("install", "--tree", alias / "allowed/claude-tabstatus"))
        self.assertNotIn("orphan", self.cli("doctor").lower())
        self.assertNotIn("orphan", self.cli("uninstall").lower())
        self.assertFalse(tree.exists())



if __name__ == "__main__":
    if "--reproduce-old" in sys.argv:
        reproduce_old()
    elif "--reproduce-missing-unicode" in sys.argv:
        reproduce_missing_unicode()
    else:
        unittest.main(verbosity=2)
