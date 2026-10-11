"""Verify the selected Hermes sync interpreter and existing copy fallback."""

import os
import re
import shlex
import shutil
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

SCRIPT = Path(__file__).resolve().parents[2] / "src/os-skills/ai/install-hermes/scripts/install.sh"


@unittest.skipUnless(os.name == "posix" and shutil.which("bash"), "POSIX tools")
class HermesSkillSyncTests(unittest.TestCase):
    def setUp(self) -> None:
        temporary = tempfile.TemporaryDirectory(prefix="hermes-skills-")
        self.addCleanup(temporary.cleanup)
        self.base = Path(temporary.name)
        self.install = self.base / "checkout with spaces"
        self.home = self.base / "data"
        self.home.mkdir()
        self.skills = self.home / "skills"
        self.skills.mkdir()
        (self.install / "tools").mkdir(parents=True)
        (self.install / "skills").mkdir()
        (self.install / "skills/bundled.txt").write_text("bundled\n", encoding="utf-8")
        self.marker = self.base / "interpreter"
        self.python = self.base / "selected interpreter" / "python"
        (self.install / "tools/skills_sync.py").write_text(
            "from pathlib import Path\n"
            f"home = Path({str(self.home)!r})\n"
            "(home / 'skills/bundled.txt').write_text('synced\\n', encoding='utf-8')\n"
            "(home / 'skills/.bundled_manifest').write_text('manifest\\n', encoding='utf-8')\n",
            encoding="utf-8",
        )

    def interpreter(self, path: Path, label: str, fail: bool = False) -> None:
        path.parent.mkdir(parents=True, exist_ok=True)
        command = "exit 1" if fail else f'exec {shlex.quote(sys.executable)} "$@"'
        path.write_text(
            "#!/bin/sh\n"
            f"printf '%s\\n' {shlex.quote(label)} > {shlex.quote(str(self.marker))}\n"
            + command
            + "\n",
            encoding="utf-8",
        )
        path.chmod(0o755)

    def personal_skill(self) -> None:
        (self.skills / "personal.txt").write_text("personal customization\n", encoding="utf-8")

    def run_sync(self, use_venv: bool) -> subprocess.CompletedProcess[str]:
        source = SCRIPT.read_text(encoding="utf-8")
        function = re.search(r"^copy_config_templates\(\) \{.*?^\}", source, re.M | re.S)
        self.assertIsNotNone(function)
        driver = "\n".join(
            [
                "log_info() { printf '%s\\n' \"$*\"; }",
                "log_success() { printf '%s\\n' \"$*\"; }",
                f"INSTALL_DIR={shlex.quote(str(self.install))}",
                f"HERMES_HOME={shlex.quote(str(self.home))}",
                f"PYTHON_PATH={shlex.quote(str(self.python))}",
                f"USE_VENV={'true' if use_venv else 'false'}",
                function.group(0),
                "copy_config_templates",
            ]
        )
        return subprocess.run(
            ["bash", "-e"], input=driver, capture_output=True, text=True, timeout=10
        )

    def assert_synced(self, label: str) -> None:
        self.assertEqual((self.skills / "bundled.txt").read_text(encoding="utf-8"), "synced\n")
        self.assertEqual(
            (self.skills / ".bundled_manifest").read_text(encoding="utf-8"), "manifest\n"
        )
        self.assertEqual(self.marker.read_text(encoding="utf-8"), label + "\n")

    def test_no_venv_syncs_bundled_skills_beside_personal_skills(self) -> None:
        self.interpreter(self.python, "selected")
        self.personal_skill()
        result = self.run_sync(False)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assert_synced("selected")
        self.assertEqual(
            (self.skills / "personal.txt").read_text(encoding="utf-8"), "personal customization\n"
        )

    def test_no_venv_initial_install_uses_manifest_sync(self) -> None:
        self.interpreter(self.python, "selected")
        result = self.run_sync(False)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assert_synced("selected")

    def test_venv_mode_keeps_using_venv_interpreter(self) -> None:
        self.interpreter(self.python, "unused", fail=True)
        self.interpreter(self.install / "venv/bin/python", "venv")
        result = self.run_sync(True)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assert_synced("venv")

    def test_failed_sync_falls_back_for_empty_skills(self) -> None:
        self.interpreter(self.install / "venv/bin/python", "failed", fail=True)
        result = self.run_sync(True)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual((self.skills / "bundled.txt").read_text(encoding="utf-8"), "bundled\n")
        self.assertFalse((self.skills / ".bundled_manifest").exists())

    def test_failed_sync_preserves_existing_personal_skills(self) -> None:
        self.interpreter(self.install / "venv/bin/python", "failed", fail=True)
        self.personal_skill()
        result = self.run_sync(True)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertFalse((self.skills / "bundled.txt").exists())
        self.assertEqual(
            (self.skills / "personal.txt").read_text(encoding="utf-8"), "personal customization\n"
        )


if __name__ == "__main__":
    unittest.main()
