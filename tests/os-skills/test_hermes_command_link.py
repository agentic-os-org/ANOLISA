"""Check Hermes command installation with real POSIX files and links."""

import os
import re
import shlex
import shutil
import subprocess
import tempfile
import unittest
from pathlib import Path

SCRIPT = Path(__file__).resolve().parents[2] / "src/os-skills/ai/install-hermes/scripts/install.sh"


@unittest.skipUnless(os.name == "posix" and shutil.which("bash"), "POSIX tools")
class HermesCommandLinkTests(unittest.TestCase):
    def setUp(self) -> None:
        temporary = tempfile.TemporaryDirectory(prefix="hermes-command-")
        self.addCleanup(temporary.cleanup)
        self.base = Path(temporary.name)
        self.bin = self.base / "prefix with spaces" / "bin"
        self.bin.mkdir(parents=True)
        self.target = self.bin / "hermes"
        self.install = self.base / "checkout"

    def executable(self, path: Path) -> Path:
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text("#!/bin/sh\nprintf 'hermes-ready\\n'\n", encoding="utf-8")
        path.chmod(0o755)
        return path

    def run_setup(
        self, executable: Path | None, use_venv: bool = False
    ) -> subprocess.CompletedProcess[str]:
        source = SCRIPT.read_text(encoding="utf-8")
        function = re.search(r"^setup_path\(\) \{.*?^\}", source, re.M | re.S)
        self.assertIsNotNone(function)
        driver = "\n".join(
            [
                "log_info() { printf '%s\\n' \"$*\"; }",
                "log_success() { printf '%s\\n' \"$*\"; }",
                "log_warn() { printf '%s\\n' \"$*\"; }",
                f"get_command_link_dir() {{ printf '%s\\n' {shlex.quote(str(self.bin))}; }}",
                "get_command_link_display_dir() { printf '%s\\n' test-bin; }",
                f"which() {{ printf '%s\\n' {shlex.quote(str(executable) if executable else '')}; }}",
                f"INSTALL_DIR={shlex.quote(str(self.install))}",
                f"USE_VENV={'true' if use_venv else 'false'}",
                "DISTRO=termux",
                function.group(0),
                "setup_path",
                "printf '%s\\n' setup-completed",
            ]
        )
        return subprocess.run(
            ["bash", "-e"], input=driver, capture_output=True, text=True, timeout=10
        )

    def assert_success(self, result: subprocess.CompletedProcess[str]) -> None:
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("setup-completed", result.stdout)
        result = subprocess.run([str(self.target)], capture_output=True, text=True, check=True)
        self.assertEqual(result.stdout, "hermes-ready\n")

    def test_existing_command_in_destination_remains_a_regular_file(self) -> None:
        self.executable(self.target)
        original = self.target.read_bytes()
        result = self.run_setup(self.target)
        self.assert_success(result)
        self.assertFalse(self.target.is_symlink())
        self.assertEqual(self.target.read_bytes(), original)

    def test_hardlink_alias_does_not_replace_existing_file(self) -> None:
        source = self.executable(self.base / "source" / "hermes")
        os.link(source, self.target)
        inode = self.target.stat().st_ino
        result = self.run_setup(source)
        self.assert_success(result)
        self.assertFalse(self.target.is_symlink())
        self.assertEqual(self.target.stat().st_ino, inode)

    def test_existing_correct_symlink_stays_usable(self) -> None:
        source = self.executable(self.base / "source" / "hermes")
        self.target.symlink_to(source)
        result = self.run_setup(self.target)
        self.assert_success(result)
        self.assertEqual(self.target.readlink(), source)

    def test_distinct_existing_target_is_relinked(self) -> None:
        source = self.executable(self.base / "source" / "hermes")
        self.executable(self.target)
        result = self.run_setup(source)
        self.assert_success(result)
        self.assertTrue(self.target.is_symlink())
        self.assertEqual(self.target.readlink(), source)

    def test_venv_command_is_linked_normally(self) -> None:
        source = self.executable(self.install / "venv/bin/hermes")
        result = self.run_setup(None, use_venv=True)
        self.assert_success(result)
        self.assertEqual(self.target.readlink(), source)

    def test_missing_no_venv_command_does_not_create_a_link(self) -> None:
        result = self.run_setup(None)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("hermes not found", result.stdout)
        self.assertFalse(self.target.exists())


if __name__ == "__main__":
    unittest.main()
