#!/usr/bin/env python3
"""Exercise kernel setup's OS banner without running package installation."""

import re
import shutil
import subprocess
import tempfile
import unittest
from pathlib import Path

SETUP = Path(__file__).resolve().parents[2] / "src/os-skills/devops/kernel-dev/scripts/setup.sh"


@unittest.skipUnless(shutil.which("bash"), "bash is required")
class KernelSetupPrettyNameTestCase(unittest.TestCase):
    def run_setup(self, release: str) -> str:
        with tempfile.TemporaryDirectory() as temp_dir:
            temp = Path(temp_dir)
            # Stop before privilege checks or package installation; only replace
            # the release-file path so the production OS-check code runs.
            source = SETUP.read_text(encoding="utf-8")
            prefix = source[: source.index("# Check root privileges")]
            script = temp / "setup-os-check.sh"
            script.write_bytes(prefix.replace("/etc/os-release", "os-release").encode("utf-8"))
            (temp / "os-release").write_bytes(release.encode("utf-8"))
            result = subprocess.run(
                ["bash", script.as_posix()],
                cwd=temp,
                capture_output=True,
                encoding="utf-8",
                timeout=10,
                check=False,
            )
        self.assertEqual(result.returncode, 0, result.stderr)
        return re.sub(r"\x1b\[[0-9;]*m", "", result.stdout)

    def test_pretty_name_formats(self) -> None:
        for value, expected in (
            ('"Alinux 4 Test"', "Alinux 4 Test"),
            ("'Alinux 4 Test'", "Alinux 4 Test"),
            ("Alinux", "Alinux"),
            ('"Alinux=4"', "Alinux=4"),
            ('"Alinux\'"', "Alinux'"),
        ):
            with self.subTest(value=value):
                output = self.run_setup(f"ID=alinux\nPRETTY_NAME={value}\n")
                self.assertIn(f"  ✓ {expected}", output.splitlines())
                self.assertNotIn("PRETTY_NAME=", output)

    def test_matches_exact_key_and_first_entry(self) -> None:
        output = self.run_setup(
            'ID=alinux\nOTHER_PRETTY_NAME="Ignore"\n'
            'PRETTY_NAME="First Name"\nPRETTY_NAME="Second Name"\n'
        )
        self.assertIn("  ✓ First Name", output.splitlines())
        self.assertNotIn("Second Name", output)
        self.assertNotIn("Ignore", output)

    def test_setup_script_has_valid_bash_syntax(self) -> None:
        result = subprocess.run(
            ["bash", "-n", SETUP.as_posix()],
            capture_output=True,
            encoding="utf-8",
            timeout=10,
            check=False,
        )
        self.assertEqual(result.returncode, 0, result.stderr)


if __name__ == "__main__":
    unittest.main(verbosity=2)
