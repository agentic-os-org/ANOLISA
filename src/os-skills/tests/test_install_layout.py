"""Check that source installs remain discoverable without changing package paths."""

from __future__ import annotations

import os
import shutil
import subprocess
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
_DEFAULT_HOME = object()


class InstallLayoutTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.source = self.root / "source"
        self.source.mkdir()
        shutil.copyfile(ROOT / "Makefile", self.source / "Makefile")
        (self.source / "component.toml").write_text("fixture contract\n")
        skill = self.source / "category/example"
        skill.mkdir(parents=True)
        (skill / "SKILL.md").write_text("---\nname: example\n---\nExample\n")
        (skill / "run.sh").write_text("#!/bin/sh\nexit 0\n")
        (skill / "run.sh").chmod(0o755)
        self.home = self.root / "home"
        self.home.mkdir()
        self.stage = self.root / "stage"
        self.tools = self.root / "bin"
        self.tools.mkdir()

    def make(
        self,
        target: str = "install",
        uid: int = 0,
        xdg_data_home: str | Path | None = None,
        destdir: str | Path | None = None,
        home: object = _DEFAULT_HOME,
        **variables: object,
    ) -> subprocess.CompletedProcess[str]:
        env = os.environ.copy()
        for name in (
            "HOME",
            "INSTALL_PROFILE",
            "PREFIX",
            "AWPRJ",
            "DESTDIR",
            "DATADIR",
            "SKILLS_DIR",
            "COMPONENT_DIR",
            "MANIFEST",
            "REPLACE_UNOWNED",
            "XDG_DATA_HOME",
            "MAKEFLAGS",
            "MFLAGS",
        ):
            env.pop(name, None)
        if home is _DEFAULT_HOME:
            env["HOME"] = str(self.home)
        elif home is not None:
            env["HOME"] = str(home)
        env["PATH"] = str(self.tools) + os.pathsep + env["PATH"]
        if xdg_data_home is not None:
            env["XDG_DATA_HOME"] = str(xdg_data_home)
        (self.tools / "id").write_text(f"#!/bin/sh\nprintf '%s\\n' '{uid}'\n")
        (self.tools / "id").chmod(0o755)
        return subprocess.run(
            [
                "make",
                "--no-print-directory",
                target,
                f"DESTDIR={self.stage if destdir is None else destdir}",
            ]
            + [f"{name}={value}" for name, value in variables.items()],
            cwd=self.source,
            env=env,
            text=True,
            capture_output=True,
        )

    def installed(self, absolute: str | Path) -> Path:
        return self.stage / str(absolute).lstrip("/")

    def assert_install(self, directory: str | Path, **variables: object) -> Path:
        result = self.make(**variables)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        skill = self.installed(directory) / "example"
        self.assertTrue((skill / "SKILL.md").is_file())
        self.assertEqual((skill / "run.sh").stat().st_mode & 0o777, 0o755)
        return skill

    def manifest(self) -> Path:
        return self.installed("/usr/local/share/anolisa/components/os-skills/install-manifest")

    def assert_no_staging_leftovers(self, directory: str | Path) -> None:
        self.assertEqual(
            [
                entry.name
                for entry in self.installed(directory).iterdir()
                if entry.name.startswith(".os-skills.")
            ],
            [],
        )

    def test_root_defaults_to_local_anolisa_collection(self) -> None:
        self.assert_install("/usr/local/share/anolisa/skills")
        self.assertTrue(
            self.installed("/usr/local/share/anolisa/components/os-skills/component.toml").is_file()
        )
        self.assertFalse(self.installed("/usr/share").exists())

    def test_user_reinstall_takes_over_an_existing_cosh_collection(self) -> None:
        directory = self.home / ".copilot-shell/skills"
        previous = self.installed(directory) / "example"
        previous.mkdir(parents=True)
        (previous / "SKILL.md").write_text("old version\n")
        result = self.make(uid=1000, REPLACE_UNOWNED=1)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("which os-skills did not install", result.stderr)
        result = self.make(uid=1000)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        skill = previous
        self.assertEqual((skill / "SKILL.md").read_text(), "---\nname: example\n---\nExample\n")
        result = self.make("uninstall", uid=1000)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertFalse(skill.exists())
        self.assertFalse(self.installed(self.home / ".local/share/anolisa/skills").exists())

    def test_user_data_override_preserves_skill_directory(self) -> None:
        data = self.root / "user data"
        self.assert_install(self.home / ".copilot-shell/skills", uid=1000, xdg_data_home=data)
        self.assertTrue(
            self.installed(data / "anolisa/components/os-skills/component.toml").is_file()
        )
        self.assertFalse(self.installed(self.home / ".local/share").exists())
        self.assertEqual(
            sorted(entry.name for entry in self.installed(self.root).iterdir()),
            ["home", "user data"],
        )

    def test_user_profile_without_home_is_rejected(self) -> None:
        result = self.make(uid=1000, home=None)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("HOME", result.stderr)
        self.assertFalse(self.installed("/.copilot-shell").exists())
        self.assertFalse(self.installed("/.local").exists())

    def test_user_profile_with_root_home_is_rejected(self) -> None:
        result = self.make(uid=1000, home="/")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("HOME", result.stderr)
        self.assertFalse(self.installed("/.copilot-shell").exists())

    def test_user_profile_without_home_accepts_explicit_paths(self) -> None:
        directory = self.root / "custom skills"
        result = self.make(
            uid=1000,
            home=None,
            PREFIX=directory,
            DATADIR=directory,
            SKILLS_DIR=directory,
        )
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertTrue((self.installed(directory) / "example/SKILL.md").is_file())

    def test_rejects_root_as_staging_root(self) -> None:
        result = self.make("check-layout", destdir="/")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("other than /", result.stderr)

    def test_empty_namespace_selects_native_system_collection(self) -> None:
        self.assert_install("/usr/local/share/cosh-ng/skills", AWPRJ="")

    def test_empty_prefix_supports_build_all_staging(self) -> None:
        self.assert_install("/share/anolisa/skills", INSTALL_PROFILE="system", PREFIX="")

    def test_user_can_select_an_xdg_skill_directory(self) -> None:
        data = self.root / "user data"
        self.assert_install(
            data / "anolisa/skills",
            uid=1000,
            xdg_data_home=data,
            SKILLS_DIR=data / "anolisa/skills",
        )

    def test_system_override_ignores_user_data_home(self) -> None:
        self.assert_install(
            "/opt/os-skills/share/team/skills",
            uid=1000,
            xdg_data_home=self.root / "user data",
            INSTALL_PROFILE="system",
            PREFIX="/opt/os-skills",
            AWPRJ="team",
        )

    def test_explicit_skill_directory_is_used_for_install_and_uninstall(self) -> None:
        directory = self.root / "custom skills"
        skill = self.assert_install(directory, SKILLS_DIR=directory)
        neighbor = skill.parent / "another-package"
        neighbor.mkdir()
        result = self.make("uninstall", SKILLS_DIR=directory)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertFalse(skill.exists())
        self.assertTrue(neighbor.is_dir())
        self.assertFalse(
            self.installed("/usr/local/share/anolisa/components/os-skills").exists()
        )

    def test_uninstall_without_install_removes_nothing(self) -> None:
        directory = self.home / ".copilot-shell/skills"
        foreign = self.installed(directory) / "example"
        foreign.mkdir(parents=True)
        (foreign / "SKILL.md").write_text("---\nname: example\n---\nUser copy\n")
        (foreign / "MY_OWN.md").write_text("user owned\n")
        result = self.make("uninstall", uid=1000)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("nothing removed", result.stderr)
        self.assertEqual((foreign / "MY_OWN.md").read_text(), "user owned\n")
        self.assertTrue((foreign / "SKILL.md").is_file())

    def test_uninstall_ignores_skill_directories_it_did_not_record(self) -> None:
        directory = self.root / "custom skills"
        skill = self.assert_install(directory, SKILLS_DIR=directory)
        foreign = skill.parent / "example-extra"
        foreign.mkdir(parents=True)
        (foreign / "MY_OWN.md").write_text("user owned\n")
        (foreign / "SKILL.md").write_text("---\nname: example-extra\n---\n")
        result = self.make("uninstall", SKILLS_DIR=directory)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertFalse(skill.exists())
        self.assertEqual((foreign / "MY_OWN.md").read_text(), "user owned\n")

    def test_uninstall_removes_skills_dropped_from_the_source_tree(self) -> None:
        directory = self.root / "custom skills"
        installed = self.installed(directory)
        result = self.make(SKILLS_DIR=directory)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertTrue((installed / "example/SKILL.md").is_file())
        shutil.rmtree(self.source / "category/example")
        replacement = self.source / "category/replacement"
        replacement.mkdir(parents=True)
        (replacement / "SKILL.md").write_text("---\nname: replacement\n---\n")
        result = self.make(SKILLS_DIR=directory)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertTrue((installed / "replacement/SKILL.md").is_file())
        result = self.make("uninstall", SKILLS_DIR=directory)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertFalse((installed / "example").exists())
        self.assertFalse((installed / "replacement").exists())

    def test_uninstall_refuses_a_different_skill_collection(self) -> None:
        directory = self.root / "custom skills"
        skill = self.assert_install(directory, SKILLS_DIR=directory)
        other = self.root / "other skills"
        foreign = self.installed(other) / "example"
        foreign.mkdir(parents=True)
        result = self.make("uninstall", SKILLS_DIR=other)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("different skill collection", result.stderr)
        self.assertTrue(skill.is_dir())
        self.assertTrue(foreign.is_dir())

    def test_install_refuses_a_different_skill_collection(self) -> None:
        first = self.root / "first skills"
        second = self.root / "second skills"
        self.assert_install(first, SKILLS_DIR=first)
        result = self.make(SKILLS_DIR=second)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("different skill collection", result.stderr)
        self.assertFalse((self.installed(second) / "example").exists())

    def test_failed_copy_keeps_the_previous_payload_and_receipt(self) -> None:
        directory = self.root / "custom skills"
        skill = self.assert_install(directory, SKILLS_DIR=directory)
        manifest = self.installed(
            "/usr/local/share/anolisa/components/os-skills/install-manifest"
        )
        recorded = manifest.read_text()
        (self.source / "category/example/SKILL.md").write_text("new version\n")
        (self.tools / "cp").write_text("#!/bin/sh\nexit 1\n")
        (self.tools / "cp").chmod(0o755)
        result = self.make(SKILLS_DIR=directory)
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(
            (skill / "SKILL.md").read_text(), "---\nname: example\n---\nExample\n"
        )
        self.assertEqual(manifest.read_text(), recorded)
        self.assertEqual([entry.name for entry in skill.parent.iterdir()], ["example"])

    def test_uninstall_removes_a_receipt_outside_the_component_directory(self) -> None:
        directory = self.root / "custom skills"
        receipt = self.installed("/usr/local/share/anolisa/components/receipt")
        receipt.parent.mkdir(parents=True)
        skill = self.assert_install(
            directory,
            SKILLS_DIR=directory,
            MANIFEST="/usr/local/share/anolisa/components/receipt",
        )
        result = self.make(
            "uninstall",
            SKILLS_DIR=directory,
            MANIFEST="/usr/local/share/anolisa/components/receipt",
        )
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertFalse(skill.exists())
        self.assertFalse(receipt.exists())
        foreign = skill.parent / "example"
        foreign.mkdir()
        (foreign / "MY_OWN.md").write_text("user owned\n")
        result = self.make(
            "uninstall",
            SKILLS_DIR=directory,
            MANIFEST="/usr/local/share/anolisa/components/receipt",
        )
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("nothing removed", result.stderr)
        self.assertEqual((foreign / "MY_OWN.md").read_text(), "user owned\n")

    def test_install_refuses_an_unowned_same_name_directory(self) -> None:
        directory = self.home / ".copilot-shell/skills"
        foreign = self.installed(directory) / "example"
        foreign.mkdir(parents=True)
        (foreign / "MY_OWN.md").write_text("user owned\n")
        result = self.make(uid=1000)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("not installed by os-skills", result.stderr)
        self.assertEqual((foreign / "MY_OWN.md").read_text(), "user owned\n")
        self.assertFalse(self.manifest().exists())
        self.assert_no_staging_leftovers(directory)

    def test_replace_unowned_requires_the_documented_value(self) -> None:
        directory = self.home / ".copilot-shell/skills"
        foreign = self.installed(directory) / "example"
        foreign.mkdir(parents=True)
        (foreign / "MY_OWN.md").write_text("user owned\n")
        for value, message in (
            (None, "not installed by os-skills"),
            ("0", "not installed by os-skills"),
            ("false", "REPLACE_UNOWNED must be 1"),
        ):
            with self.subTest(value=value):
                variables = {} if value is None else {"REPLACE_UNOWNED": value}
                result = self.make(uid=1000, **variables)
                self.assertNotEqual(result.returncode, 0)
                self.assertIn(message, result.stderr)
                self.assertEqual((foreign / "MY_OWN.md").read_text(), "user owned\n")
                self.assertFalse(self.manifest().exists())

    def test_replace_unowned_takes_over_a_same_name_directory(self) -> None:
        directory = self.home / ".copilot-shell/skills"
        foreign = self.installed(directory) / "example"
        foreign.mkdir(parents=True)
        (foreign / "MY_OWN.md").write_text("user owned\n")
        result = self.make(uid=1000, REPLACE_UNOWNED=1)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("which os-skills did not install", result.stderr)
        self.assertFalse((foreign / "MY_OWN.md").exists())
        self.assertTrue((foreign / "SKILL.md").is_file())
        result = self.make("uninstall", uid=1000)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertFalse(foreign.exists())

    def test_install_keeps_an_unrelated_hidden_directory(self) -> None:
        directory = self.root / "custom skills"
        neighbour = self.installed(directory) / ".example.os-skills-new"
        neighbour.mkdir(parents=True)
        (neighbour / "personal.txt").write_text("user owned\n")
        skill = self.assert_install(directory, SKILLS_DIR=directory)
        self.assertEqual((neighbour / "personal.txt").read_text(), "user owned\n")
        self.assertEqual(
            sorted(entry.name for entry in skill.parent.iterdir()),
            [".example.os-skills-new", "example"],
        )

    def test_failed_publish_restores_the_previous_payload(self) -> None:
        directory = self.root / "custom skills"
        skill = self.assert_install(directory, SKILLS_DIR=directory)
        recorded = self.manifest().read_text()
        (self.source / "category/example/SKILL.md").write_text("new version\n")
        (self.tools / "mv").write_text(
            "#!/bin/sh\n"
            'case "$1" in */.old/*) exec /bin/mv "$@";; esac\n'
            'case "$1" in *.os-skills.*/example) exit 1;; esac\n'
            'exec /bin/mv "$@"\n'
        )
        (self.tools / "mv").chmod(0o755)
        result = self.make(SKILLS_DIR=directory)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("install failed", result.stderr)
        self.assertEqual(
            (skill / "SKILL.md").read_text(), "---\nname: example\n---\nExample\n"
        )
        self.assertEqual(self.manifest().read_text(), recorded)
        self.assert_no_staging_leftovers(directory)

    def test_failed_publish_restores_a_dangling_link(self) -> None:
        directory = self.root / "custom skills"
        skill = self.assert_install(directory, SKILLS_DIR=directory)
        shutil.rmtree(skill)
        skill.symlink_to("missing-skill")
        (self.tools / "mv").write_text(
            "#!/bin/sh\n"
            'case "$1" in */.old/*) exec /bin/mv "$@";; esac\n'
            'case "$1" in *.os-skills.*/example) exit 1;; esac\n'
            'exec /bin/mv "$@"\n'
        )
        (self.tools / "mv").chmod(0o755)
        result = self.make(SKILLS_DIR=directory)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("unchanged", result.stderr)
        self.assertTrue(skill.is_symlink())
        self.assertEqual(os.readlink(skill), "missing-skill")
        self.assert_no_staging_leftovers(directory)

    def test_failed_restore_keeps_the_backup(self) -> None:
        directory = self.root / "custom skills"
        skill = self.assert_install(directory, SKILLS_DIR=directory)
        recorded = self.manifest().read_text()
        (self.source / "category/example/SKILL.md").write_text("new version\n")
        (self.tools / "mv").write_text(
            "#!/bin/sh\n"
            'case "$1" in *.os-skills.*/example) exit 1;; esac\n'
            'exec /bin/mv "$@"\n'
        )
        (self.tools / "mv").chmod(0o755)
        result = self.make(SKILLS_DIR=directory)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("could not be restored", result.stderr)
        self.assertNotIn("unchanged", result.stderr)
        self.assertFalse(skill.exists())
        self.assertEqual(self.manifest().read_text(), recorded)
        stages = [
            entry for entry in skill.parent.iterdir() if entry.name.startswith(".os-skills.")
        ]
        self.assertEqual(len(stages), 1)
        self.assertIn(str(stages[0] / ".old"), result.stderr)
        self.assertEqual([entry.name for entry in stages[0].iterdir()], [".old"])
        self.assertEqual(
            (stages[0] / ".old/example/SKILL.md").read_text(),
            "---\nname: example\n---\nExample\n",
        )

    def test_failed_rollback_reports_a_new_copy_it_cannot_remove(self) -> None:
        directory = self.root / "custom skills"
        skill = self.assert_install(directory, SKILLS_DIR=directory)
        recorded = self.manifest().read_text()
        second = self.source / "category/second"
        second.mkdir()
        (second / "SKILL.md").write_text("---\nname: second\n---\n")
        (self.tools / "mv").write_text(
            "#!/bin/sh\n"
            'case "$*" in *install-manifest.tmp*) exit 1;; esac\n'
            'exec /bin/mv "$@"\n'
        )
        (self.tools / "mv").chmod(0o755)
        (self.tools / "rm").write_text(
            "#!/bin/sh\n"
            "for last; do :; done\n"
            'case "$last" in *.os-skills.*) ;; */second) exit 1;; esac\n'
            'exec /bin/rm "$@"\n'
        )
        (self.tools / "rm").chmod(0o755)
        result = self.make(SKILLS_DIR=directory)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("cannot remove the new copy", result.stderr)
        self.assertNotIn("unchanged", result.stderr.replace("the receipt is unchanged", ""))
        self.assertEqual(
            (skill / "SKILL.md").read_text(), "---\nname: example\n---\nExample\n"
        )
        self.assertTrue((self.installed(directory) / "second").is_dir())
        self.assertEqual(self.manifest().read_text(), recorded)

    def test_interrupted_install_rolls_back(self) -> None:
        directory = self.root / "custom skills"
        skill = self.assert_install(directory, SKILLS_DIR=directory)
        recorded = self.manifest().read_text()
        (self.source / "category/example/SKILL.md").write_text("new version\n")
        (self.tools / "mv").write_text(
            "#!/bin/sh\n"
            'case "$1" in *.os-skills.*/example) kill -TERM "$PPID";; esac\n'
            'exec /bin/mv "$@"\n'
        )
        (self.tools / "mv").chmod(0o755)
        result = self.make(SKILLS_DIR=directory)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("unchanged", result.stderr)
        self.assertEqual(
            (skill / "SKILL.md").read_text(), "---\nname: example\n---\nExample\n"
        )
        self.assertEqual(self.manifest().read_text(), recorded)
        self.assert_no_staging_leftovers(directory)

    def test_install_interrupted_after_the_receipt_is_published_stays_committed(
        self,
    ) -> None:
        directory = self.root / "custom skills"
        skill = self.assert_install(directory, SKILLS_DIR=directory)
        (self.source / "category/example/SKILL.md").write_text("new version\n")
        second = self.source / "category/second"
        second.mkdir()
        (second / "SKILL.md").write_text("---\nname: second\n---\n")
        (self.tools / "mv").write_text(
            "#!/bin/sh\n"
            '/bin/mv "$@" || exit\n'
            'case "$*" in *install-manifest.tmp*) kill -TERM "$PPID";; esac\n'
        )
        (self.tools / "mv").chmod(0o755)
        result = self.make(SKILLS_DIR=directory)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("run make install again", result.stderr)
        self.assertNotIn("unchanged", result.stderr)
        recorded = [
            line for line in self.manifest().read_text().splitlines()
            if not line.startswith("#")
        ]
        self.assertEqual(recorded, ["example", "second"])
        installed = sorted(
            path.name for path in self.installed(directory).iterdir()
            if not path.name.startswith(".")
        )
        self.assertEqual(installed, recorded)
        self.assertEqual((skill / "SKILL.md").read_text(), "new version\n")
        self.assert_no_staging_leftovers(directory)
        (self.tools / "mv").unlink()
        unowned = self.installed(directory) / "third"
        unowned.mkdir()
        (unowned / "MY_OWN.md").write_text("mine\n")
        result = self.make(SKILLS_DIR=directory)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertTrue((self.manifest().parent / "component.toml").is_file())
        result = self.make("uninstall", SKILLS_DIR=directory)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertFalse(skill.exists())
        self.assertFalse((self.installed(directory) / "second").exists())
        self.assertEqual((unowned / "MY_OWN.md").read_text(), "mine\n")

    def test_install_rejects_a_receipt_inside_a_skill_directory(self) -> None:
        directory = self.root / "custom skills"
        result = self.make(SKILLS_DIR=directory, MANIFEST=directory / "example/receipt")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("inside the skill directory", result.stderr)
        self.assertFalse(self.installed(directory).exists())

    def test_install_rejects_a_receipt_that_is_not_a_file(self) -> None:
        directory = self.root / "custom skills"
        self.manifest().mkdir(parents=True)
        result = self.make(SKILLS_DIR=directory)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("not a regular file", result.stderr)
        self.assertFalse(self.installed(directory).exists())

    def test_receipt_directory_may_contain_spaces(self) -> None:
        directory = self.root / "custom skills"
        receipt = "/custom receipts/receipt-a"
        skill = self.assert_install(directory, SKILLS_DIR=directory, MANIFEST=receipt)
        self.assertTrue(self.installed(receipt).is_file())
        self.assertFalse(self.installed("/ receipts").exists())
        result = self.make("uninstall", SKILLS_DIR=directory, MANIFEST=receipt)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertFalse(skill.exists())
        self.assertFalse(self.installed(receipt).exists())

    def test_failed_copy_publishes_nothing(self) -> None:
        second = self.source / "category/second"
        second.mkdir()
        (second / "SKILL.md").write_text("---\nname: second\n---\n")
        (self.tools / "cp").write_text(
            "#!/bin/sh\n"
            "for last; do :; done\n"
            'case "$last" in */second) exit 1;; esac\n'
            'exec /bin/cp "$@"\n'
        )
        (self.tools / "cp").chmod(0o755)
        directory = self.root / "custom skills"
        result = self.make(SKILLS_DIR=directory)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("install failed", result.stderr)
        self.assertFalse((self.installed(directory) / "example").exists())
        self.assertFalse((self.installed(directory) / "second").exists())
        self.assertFalse(self.manifest().exists())
        self.assert_no_staging_leftovers(directory)

    def test_install_rejects_an_unsupported_skill_name(self) -> None:
        odd = self.source / "category/my+skill"
        odd.mkdir()
        (odd / "SKILL.md").write_text("---\nname: my+skill\n---\n")
        directory = self.root / "custom skills"
        result = self.make(SKILLS_DIR=directory)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("unsupported skill directory name", result.stderr)
        self.assertFalse((self.installed(directory) / "my+skill").exists())

    def test_receipt_is_sorted_and_unique(self) -> None:
        second = self.source / "category/second"
        second.mkdir()
        (second / "SKILL.md").write_text("---\nname: second\n---\n")
        directory = self.root / "custom skills"
        self.assert_install(directory, SKILLS_DIR=directory)
        self.assert_install(directory, SKILLS_DIR=directory)
        entries = self.manifest().read_text().splitlines()
        self.assertEqual(entries[0], f"# skills_dir={directory}")
        self.assertEqual(entries[1:], ["example", "second"])

    def test_uninstall_fails_on_an_entry_it_cannot_remove(self) -> None:
        directory = self.root / "custom skills"
        skill = self.assert_install(directory, SKILLS_DIR=directory)
        with self.manifest().open("a") as handle:
            handle.write("my+skill\n")
        result = self.make("uninstall", SKILLS_DIR=directory)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("unreadable receipt entry", result.stderr)
        self.assertTrue(skill.is_dir())
        self.assertTrue(self.manifest().is_file())

    def test_uninstall_keeps_another_collections_receipt(self) -> None:
        first_dir = self.root / "first skills"
        second_dir = self.root / "second skills"
        first = self.assert_install(first_dir, SKILLS_DIR=first_dir, MANIFEST="/receipts/first")
        second = self.assert_install(second_dir, SKILLS_DIR=second_dir, MANIFEST="/receipts/second")
        result = self.make("uninstall", SKILLS_DIR=second_dir, MANIFEST="/receipts/second")
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertFalse(second.exists())
        self.assertTrue(self.installed("/receipts/first").is_file())
        result = self.make("uninstall", SKILLS_DIR=first_dir, MANIFEST="/receipts/first")
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertFalse(first.exists())
        self.assertEqual(list(self.installed("/receipts").iterdir()), [])

    def test_trailing_slash_names_the_same_collection(self) -> None:
        directory = self.root / "custom  skills"
        self.assert_install(directory, SKILLS_DIR=str(directory) + "//")
        result = self.make("uninstall", SKILLS_DIR=directory)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertFalse((self.installed(directory) / "example").exists())

    def test_rejects_invalid_layout_before_copying(self) -> None:
        for values in (
            {"INSTALL_PROFILE": "unknown"},
            {"AWPRJ": "../other"},
            {"SKILLS_DIR": "relative"},
            {"uid": 1000, "xdg_data_home": "relative"},
        ):
            with self.subTest(values=values):
                result = self.make(**values)
                self.assertNotEqual(result.returncode, 0)
                self.assertFalse(self.stage.exists())


if __name__ == "__main__":
    unittest.main()
