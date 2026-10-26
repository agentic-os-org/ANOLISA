"""Lock Preview selection; command stubs do not certify Python-free execution."""

import json
import shutil
import subprocess
import textwrap
import unittest

from test_ci_checks import AW, GateFixture


class PackageSelectionTests(GateFixture):
    def test_preview_keeps_exact_python_free_package_targets(self) -> None:
        workflow = (AW.parents[1] / ".github/workflows/aw-preview.yml").read_text()
        step = workflow.split(
            "      - name: Test installer and build the four artifacts\n", 1
        )[1]
        step = step.split("      - name: Stage the same build for native acceptance\n", 1)[0]
        script = textwrap.dedent(step.split("        run: |\n", 1)[1])
        binary = self.root / "bin"
        binary.mkdir()
        git = binary / "git"
        git.write_text("#!/bin/sh\nexit 0\n")
        git.chmod(0o700)
        cargo = binary / "cargo"
        cargo.write_text(
            "#!/bin/sh\n"
            'printf \'%s\\000\' "$@" >> "$AW_PACKAGE_COMMANDS"\n'
            'printf \'\\000\' >> "$AW_PACKAGE_COMMANDS"\n'
        )
        cargo.chmod(0o700)
        self.assertIsNone(shutil.which("python3", path=str(binary)))
        capture = self.root / "commands"
        preview = self.root / "preview output"
        subprocess.run(
            ["/bin/sh", "-eu", "-c", script],
            cwd=self.root,
            env={
                "PATH": str(binary),
                "GITHUB_WORKSPACE": str(self.root / "container workspace"),
                "AW_PREVIEW_DIR": str(preview),
                "AW_PACKAGE_COMMANDS": str(capture),
            },
            check=True,
            timeout=10,
        )
        commands = [
            row.decode().split("\0")
            for row in capture.read_bytes().split(b"\0\0")
            if row
        ]
        self.assertEqual(
            commands,
            [
                [
                    "test", "--locked", "--manifest-path", "src/aw/Cargo.toml",
                    "-p", "aw-package",
                ],
                [
                    "test", "--locked", "--manifest-path", "src/aw/Cargo.toml",
                    "-p", "aw-cli", "--test", "installation", "--test",
                    "config_edit_cli", "--test", "standalone", "--test", "packages",
                ],
                [
                    "run", "--locked", "--release", "--manifest-path",
                    "src/aw/Cargo.toml", "-p", "aw-package", "--bin", "aw-build",
                    "--", "--version", "0.1.0-preview.1", "--output",
                    str(preview / "packages"), "--component", "all",
                ],
            ],
        )
        self.assertFalse(any("packages_pty" in command for command in commands))

    def test_terminal_target_remains_required_in_the_python_enabled_gate(self) -> None:
        matrix = json.loads((AW / "tests/regression-matrix.json").read_text())
        target = "aw-cli/test/packages_pty"
        self.assertIn(target, matrix["required_targets"])
        self.assertNotIn(target, matrix["opt_in_targets"])


if __name__ == "__main__":
    unittest.main()
