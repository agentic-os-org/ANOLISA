"""Check independent staging and legacy layout without building Linux binaries."""

import os
import shutil
import subprocess
import tarfile
import tempfile
import unittest
from pathlib import Path

COMPONENT = Path(__file__).resolve().parents[1]


class PackagingLayout(unittest.TestCase):
    def setUp(self) -> None:
        temporary = tempfile.TemporaryDirectory(prefix="skillfs-layout-")
        self.addCleanup(temporary.cleanup)
        self.work = Path(temporary.name).resolve()
        self.source = self.work / "source"
        shutil.copytree(COMPONENT / "docs/skills", self.source / "docs/skills")
        for name in ("LICENSE", "README.md"):
            shutil.copyfile(COMPONENT / name, self.source / name)
        shutil.copyfile(COMPONENT / "Makefile", self.source / "Makefile")
        shutil.copytree(
            COMPONENT / "packaging",
            self.source / "packaging",
            ignore=shutil.ignore_patterns("__pycache__"),
        )
        self.binary = self.work / "renamed-input"
        self.binary.write_text("fixture; never execute\n")
        self.binary.chmod(0o755)
        self.destination = self.work / "stage"
        self.traps = self.work / "traps"
        self.traps.mkdir()
        for command in ("cargo", "npm", "systemctl"):
            trap = self.traps / command
            trap.write_text("#!/bin/sh\necho unexpected-activation-or-build >&2\nexit 91\n")
            trap.chmod(0o755)

    def make(self, target: str, **values: str) -> subprocess.CompletedProcess[str]:
        # Stage these ownership fixtures in the explicit RPM-style /usr layout.
        if any(
            goal in {"install", "install-core", "install-plugins", "install-skills"}
            for goal in target.split()
        ):
            values = {"PREFIX": "/usr", **values}
        settings = {
            "DESTDIR": str(self.destination),
            "BINARY": str(self.binary),
            **values,
        }
        return subprocess.run(
            [
                "make",
                "-s",
                "-j2",
                "-C",
                str(self.source),
                *target.split(),
                *(f"{key}={value}" for key, value in settings.items()),
            ],
            env={**os.environ, "PATH": f"{self.traps}:{os.environ['PATH']}"},
            text=True,
            capture_output=True,
        )

    def test_core_install_does_not_require_or_copy_skills(self) -> None:
        shutil.rmtree(self.source / "docs/skills")
        result = self.make("install-core", AWPRJ="owner")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(
            (self.destination / "usr/bin/skillfs").read_bytes(), self.binary.read_bytes()
        )
        self.assertTrue((self.destination / "usr/share/doc/skillfs/LICENSE").is_file())
        self.assertTrue((self.destination / "usr/share/doc/skillfs/README.md").is_file())
        self.assertFalse((self.destination / "usr/share/owner/skillfs").exists())

    def test_skills_install_does_not_require_or_copy_core_inputs(self) -> None:
        self.binary.unlink()
        (self.source / "LICENSE").unlink()
        (self.source / "README.md").unlink()
        result = self.make("install-skills", AWPRJ="owner")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertTrue(
            (self.destination / "usr/share/owner/skills/skillfs-mount/SKILL.md").is_file()
        )
        self.assertFalse((self.destination / "usr/bin").exists())
        self.assertFalse((self.destination / "usr/share/doc").exists())

    def test_complete_install_preflights_skills_before_writing_core(self) -> None:
        (self.source / "docs/skills/skillfs-mount/SKILL.md").unlink()
        result = self.make("install")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("skillfs-mount/SKILL.md", result.stderr)
        self.assertNotIn("run make build first", result.stderr)
        self.assertFalse(self.destination.exists())
        self.assertNotIn("unexpected-activation-or-build", result.stderr)

    def test_core_ignores_unselected_skill_destination_links(self) -> None:
        skill = self.destination / "usr/share/skillfs/skills/skillfs-mount"
        skill.parent.mkdir(parents=True)
        outside = self.work / "outside-skills"
        outside.mkdir()
        skill.symlink_to(outside, target_is_directory=True)
        result = self.make("install-core")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertTrue((self.destination / "usr/bin/skillfs").is_file())
        self.assertTrue(skill.is_symlink())
        self.assertEqual(list(outside.iterdir()), [])

    def test_skills_ignore_unselected_core_destination_links(self) -> None:
        binary = self.destination / "usr/bin/skillfs"
        binary.parent.mkdir(parents=True)
        outside = self.work / "outside-binary"
        outside.write_text("preserve")
        binary.symlink_to(outside)
        result = self.make("install-skills")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertTrue(binary.is_symlink())
        self.assertEqual(outside.read_text(), "preserve")
        self.assertTrue(
            (self.destination / "usr/share/skillfs/skills/skillfs-mount/SKILL.md").is_file()
        )

    def test_complete_install_rejects_skill_destination_before_copying_core(self) -> None:
        skill = self.destination / "usr/share/skillfs/skills/skillfs-mount"
        skill.parent.mkdir(parents=True)
        outside = self.work / "outside-skills"
        outside.mkdir()
        skill.symlink_to(outside, target_is_directory=True)
        result = self.make("install")
        self.assertNotEqual(result.returncode, 0)
        self.assertFalse((self.destination / "usr/bin").exists())
        self.assertEqual(list(outside.iterdir()), [])

    def test_group_install_rejects_its_missing_input_before_writing(self) -> None:
        self.binary.unlink()
        (self.source / "docs/skills/skillfs-mount/SKILL.md").unlink()
        for target in ("install-core", "install-skills"):
            with self.subTest(target=target):
                result = self.make(target)
                self.assertNotEqual(result.returncode, 0)
                self.assertFalse(self.destination.exists())

    def test_selected_groups_preserve_destination_validation_and_omit_empty_plugins(self) -> None:
        for target in ("install-core", "install-skills"):
            for destination in ("/", "relative", f"{self.work}/../escape"):
                with self.subTest(target=target, destination=destination):
                    self.assertNotEqual(self.make(target, DESTDIR=destination).returncode, 0)
                    self.assertFalse(self.destination.exists())
        self.assertNotEqual(self.make("install-plugins").returncode, 0)

    def test_install_copies_complete_prebuilt_payload_and_overrides(self) -> None:
        result = self.make("install", AWPRJ="owner", SKILLDIR="/opt/skills/skillfs")
        self.assertEqual(result.returncode, 0, result.stderr)
        binary = self.destination / "usr/bin/skillfs"
        self.assertEqual(binary.read_bytes(), self.binary.read_bytes())
        self.assertTrue(os.access(binary, os.X_OK))
        self.assertTrue((self.destination / "opt/skills/skillfs/SKILL.md").is_file())
        self.assertTrue((self.destination / "usr/share/doc/skillfs/LICENSE").is_file())
        self.assertFalse((self.destination / "usr/share/anolisa/components").exists())

    def test_namespaced_install_honors_datadir_override(self) -> None:
        result = self.make("install", AWPRJ="anolisa", DATADIR="/opt/skills-data")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertTrue(
            (self.destination / "opt/skills-data/skills/skillfs-mount/SKILL.md").is_file()
        )
        self.assertFalse((self.destination / "usr/share/anolisa/skills").exists())

    def test_explicit_legacy_layout_retains_raw_paths(self) -> None:
        result = self.make("install", PROFILE="legacy-anolisa")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertTrue((self.destination / "usr/local/bin/skillfs").is_file())
        self.assertTrue(
            (self.destination / "usr/share/anolisa/skills/skillfs-mount/SKILL.md").is_file()
        )
        self.assertTrue(
            (self.destination / "usr/share/anolisa/components/skillfs/component.toml").is_file()
        )

    def test_direct_install_uses_prefix_and_namespace_without_scanning_root(self) -> None:
        prefix = self.work / "direct"
        result = self.make("install", DESTDIR="", PREFIX=str(prefix), AWPRJ="owner")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual((prefix / "bin" / COMPONENT.name).read_bytes(), self.binary.read_bytes())
        self.assertTrue((prefix / "share/owner/skills/skillfs-mount").is_dir())
        self.assertIn("no build, framework registration, or service activation", result.stdout)
        self.assertFalse(self.destination.exists())

    def test_missing_prebuilt_inputs_fail_before_any_installation(self) -> None:
        self.binary.unlink()
        result = self.make("install")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("run make build first", result.stderr)
        self.assertNotIn("unexpected-activation-or-build", result.stderr)
        self.assertFalse(self.destination.exists())

    def test_removed_stage_targets_are_not_public_entrypoints(self) -> None:
        for target in (
            "stage-package",
            "stage-package-core",
            "stage-package-plugin",
            "stage-package-skills",
        ):
            with self.subTest(target=target):
                result = self.make(target)
                self.assertNotEqual(result.returncode, 0)
                self.assertIn("No rule to make target", result.stderr)
        help_result = self.make("help")
        self.assertEqual(help_result.returncode, 0, help_result.stderr)
        self.assertNotIn("stage-package", help_result.stdout)
        self.assertIn("install", help_result.stdout)

    def test_rejects_unsafe_destinations_and_layouts_before_writing(self) -> None:
        for value in ("/", "//", "///", "relative", f"{self.work}/../escape"):
            with self.subTest(destination=value):
                self.assertNotEqual(self.make("install", DESTDIR=value).returncode, 0)
        self.assertNotEqual(self.make("install", BINDIR="/usr/../escape").returncode, 0)
        self.assertNotEqual(self.make("layout", AWPRJ="../../escape").returncode, 0)
        self.assertFalse(self.destination.exists())

    def test_rejects_symlinked_destination_ancestor(self) -> None:
        outside = self.work / "outside"
        outside.mkdir()
        self.destination.mkdir()
        (self.destination / "usr").symlink_to(outside, target_is_directory=True)
        result = self.make("install")
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(list(outside.iterdir()), [])
        (self.destination / "usr").unlink()
        linked = self.work / "linked-parent"
        linked.symlink_to(outside, target_is_directory=True)
        result = self.make("install", DESTDIR=str(linked / "stage"))
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(list(outside.iterdir()), [])

    def test_shared_destination_ignores_other_components_links(self) -> None:
        binary_directory = self.destination / "usr/bin"
        binary_directory.mkdir(parents=True)
        outside = self.work / "outside-file"
        outside.write_text("unrelated component data")
        link = binary_directory / "another-component"
        link.symlink_to(outside)
        (self.destination / "another-component-tree").symlink_to(
            self.work, target_is_directory=True
        )
        result = self.make("install")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertTrue(link.is_symlink())
        self.assertEqual(outside.read_text(), "unrelated component data")

    def test_direct_install_rejects_linked_own_file_without_overwriting_it(self) -> None:
        prefix = self.work / "direct"
        (prefix / "bin").mkdir(parents=True)
        outside = self.work / "outside-file"
        outside.write_text("preserve")
        (prefix / "bin" / COMPONENT.name).symlink_to(outside)
        result = self.make("install", DESTDIR="", PREFIX=str(prefix))
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(outside.read_text(), "preserve")
        self.assertFalse((prefix / "share").exists())

    def test_rejects_paths_that_cannot_form_a_systemd_executable(self) -> None:
        for value in (
            "/opt/owner package/bin",
            "/opt/unit%name/bin",
            "/opt/x\nExecStart=/bin/x",
            "/opt/owner'package/bin",
        ):
            with self.subTest(path=value):
                result = self.make("install", BINDIR=value)
                self.assertNotEqual(result.returncode, 0)
        self.assertFalse(self.destination.exists())

    def test_user_install_profile_requires_explicit_legacy_entrypoint(self) -> None:
        result = self.make("install", INSTALL_PROFILE="user")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("INSTALL_PROFILE=system only", result.stderr)
        self.assertFalse(self.destination.exists())

    def test_explicit_environment_overrides_and_empty_namespace(self) -> None:
        environment = {**os.environ, "AWPRJ": "owner", "PREFIX": "/opt/owner"}
        result = subprocess.run(
            ["make", "-s", "-C", str(self.source), "layout"],
            env=environment,
            text=True,
            capture_output=True,
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("DATADIR=/opt/owner/share/owner/" + COMPONENT.name, result.stdout)
        result = subprocess.run(
            ["make", "-s", "-C", str(self.source), "layout", "AWPRJ=", "PREFIX=/usr"],
            env=environment,
            text=True,
            capture_output=True,
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("DATADIR=/usr/share/" + COMPONENT.name, result.stdout)
        environment["BINDIR"] = ""
        result = subprocess.run(
            ["make", "-s", "-C", str(self.source), "layout"],
            env=environment,
            text=True,
            capture_output=True,
        )
        self.assertNotEqual(result.returncode, 0)
        self.assertNotEqual(self.make("layout", PREFIX="").returncode, 0)

    def test_mixed_goals_keep_legacy_file_helpers_separate(self) -> None:
        environment = {
            **os.environ,
            "HOME": str(self.work / "home"),
            "PATH": f"{self.traps}:{os.environ['PATH']}",
        }
        result = subprocess.run(
            [
                "make",
                "-s",
                "-C",
                str(self.source),
                "install",
                "install-core-files",
                "layout",
                f"DESTDIR={self.destination}",
                f"BINARY={self.binary}",
            ],
            env=environment,
            text=True,
            capture_output=True,
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertTrue((self.destination / "usr/local/bin" / COMPONENT.name).is_file())
        legacy_bin = (
            self.work / "home/.local/bin"
            if COMPONENT.name == "agent-memory"
            else Path("/usr/local/bin")
        )
        self.assertTrue((self.destination / str(legacy_bin).lstrip("/") / COMPONENT.name).is_file())
        self.assertIn("DATADIR=/usr/local/share/" + COMPONENT.name, result.stdout)

    def test_root_entry_works_from_archived_source_without_monorepo(self) -> None:
        archive = self.work / "source.tar"
        with tarfile.open(archive, "w") as output:
            output.add(self.source, arcname="component")
        unpacked = self.work / "unpacked"
        with tarfile.open(archive) as source:
            source.extractall(unpacked, filter="data")
        staged = self.work / "unpacked-stage"
        result = subprocess.run(
            [
                "make",
                "-s",
                "-C",
                str(unpacked / "component"),
                "install",
                f"DESTDIR={staged}",
                f"BINARY={self.binary}",
            ],
            env={**os.environ, "PATH": f"{self.traps}:{os.environ['PATH']}"},
            text=True,
            capture_output=True,
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertTrue((staged / "usr/local/bin" / COMPONENT.name).is_file())
        self.assertTrue((unpacked / "component/packaging/check-install.py").is_file())
        self.assertFalse((unpacked / "component/packaging/Makefile").exists())

    def test_raw_archive_rejects_independent_namespace(self) -> None:
        result = subprocess.run(
            ["bash", str(COMPONENT / "packaging/raw/package.sh"), "package"],
            env={**os.environ, "AWPRJ": ""},
            capture_output=True,
            text=True,
        )
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("raw payload requires AWPRJ=anolisa", result.stderr)


if __name__ == "__main__":
    unittest.main()
