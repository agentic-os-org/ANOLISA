#!/usr/bin/env python3
# SPDX-License-Identifier: MIT
"""Help and invalid installer arguments must exit before installation commands."""

import os
import shutil
import subprocess
import tempfile
import unittest
from pathlib import Path

SCRIPT = (
    Path(__file__).resolve().parents[2]
    / "src/os-skills/ai/install-claude-code/scripts/install-claude-code.sh"
)


@unittest.skipUnless(os.name == "posix" and shutil.which("bash"), "requires POSIX Bash")
class InstallerArgumentTests(unittest.TestCase):
    def invoke(self, arguments: list[str]) -> tuple[subprocess.CompletedProcess[str], str]:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            script = root / "install-claude-code.sh"
            # Also support checking a Windows checkout through WSL.
            script.write_text(SCRIPT.read_text(encoding="utf-8"), encoding="utf-8", newline="\n")
            commands = root / "bin"
            commands.mkdir()
            for name in ("rpm", "curl", "node", "npm", "claude", "dnf", "sudo"):
                behavior = {
                    "curl": "exit 1\n",
                    "node": "printf 'v20.0.0\\n'\n",
                    "npm": 'if [ "$*" = "config get prefix" ]; then printf "%s\\n" "$TASK_INSTALLER_PREFIX"; fi\n',
                    "claude": "printf 'test version\\n'\n",
                }.get(name, "exit 0\n")
                command = commands / name
                command.write_text(
                    f'#!/bin/bash\nprintf "%s\\n" "{name}" >> "$TASK_INSTALLER_LOG"\n{behavior}',
                    encoding="utf-8",
                )
                command.chmod(0o700)
            trace = root / "commands.log"
            environment = os.environ.copy()
            environment.pop("CLAUDE_API_KEY", None)
            environment.update(
                PATH=f"{commands}:{environment.get('PATH', '')}",
                TASK_INSTALLER_LOG=str(trace),
                TASK_INSTALLER_PREFIX=str(root / "prefix"),
            )
            result = subprocess.run(
                [shutil.which("bash"), str(script), *arguments, "--skip-tokenless"],
                capture_output=True,
                stdin=subprocess.DEVNULL,
                encoding="utf-8",
                env=environment,
                check=False,
                timeout=15,
            )
            return result, trace.read_text(encoding="utf-8") if trace.exists() else ""

    def test_help_describes_flags_without_running_installer_commands(self) -> None:
        for arguments in (["--help"], ["-h"], ["--config", "--help"]):
            with self.subTest(arguments=arguments):
                result, commands = self.invoke(arguments)
                self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
                self.assertIn("Usage:", result.stdout)
                self.assertIn("--config", result.stdout)
                self.assertIn("--skip-tokenless", result.stdout)
                self.assertEqual(result.stderr, "")
                self.assertEqual(commands, "")

    def test_invalid_arguments_fail_before_any_installation(self) -> None:
        for arguments in (
            ["--skip-tokenles"],
            ["--config=true"],
            ["unexpected"],
            ["--help", "--unknown"],
            ["--unknown", "--help"],
        ):
            with self.subTest(arguments=arguments):
                result, commands = self.invoke(arguments)
                self.assertEqual(result.returncode, 2, result.stdout + result.stderr)
                self.assertIn("Unknown argument:", result.stderr)
                self.assertIn("Usage:", result.stderr)
                self.assertEqual(commands, "")

    def test_supported_arguments_still_enter_the_installation_workflow(self) -> None:
        result, commands = self.invoke(["--skip-tokenless"])
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("Done!", result.stdout)
        self.assertIn("Skipping tokenless plugin", result.stdout)
        self.assertIn("rpm\n", commands)
        self.assertIn("curl\n", commands)
        self.assertIn("npm\n", commands)
        self.assertNotIn("dnf\n", commands)
        self.assertNotIn("sudo\n", commands)


if __name__ == "__main__":
    unittest.main()
