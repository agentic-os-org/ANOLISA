"""Check that source installs remain discoverable without changing package paths."""

from __future__ import annotations

import os
import shutil
import subprocess
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]


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
        **variables: object,
    ) -> subprocess.CompletedProcess[str]:
        env = os.environ.copy()
        for name in (
            "INSTALL_PROFILE",
            "PREFIX",
            "AWPRJ",
            "DESTDIR",
            "DATADIR",
            "SKILLS_DIR",
            "COMPONENT_DIR",
            "XDG_DATA_HOME",
            "MAKEFLAGS",
            "MFLAGS",
        ):
            env.pop(name, None)
        env["HOME"] = str(self.home)
        env["PATH"] = str(self.tools) + os.pathsep + env["PATH"]
        if xdg_data_home is not None:
            env["XDG_DATA_HOME"] = str(xdg_data_home)
        (self.tools / "id").write_text(f"#!/bin/sh\nprintf '%s\\n' '{uid}'\n")
        (self.tools / "id").chmod(0o755)
        return subprocess.run(
            ["make", "--no-print-directory", target, f"DESTDIR={self.stage}"]
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

    def test_root_defaults_to_local_anolisa_collection(self) -> None:
        self.assert_install("/usr/local/share/anolisa/skills")
        self.assertTrue(
            self.installed("/usr/local/share/anolisa/components/os-skills/component.toml").is_file()
        )
        self.assertFalse(self.installed("/usr/share").exists())

    def test_user_reinstall_and_uninstall_use_existing_cosh_collection(self) -> None:
        directory = self.home / ".copilot-shell/skills"
        previous = self.installed(directory) / "example"
        previous.mkdir(parents=True)
        (previous / "SKILL.md").write_text("old version\n")
        skill = self.assert_install(directory, uid=1000)
        self.assertEqual(skill, previous)
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
        self.assertFalse(directory.exists())

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
