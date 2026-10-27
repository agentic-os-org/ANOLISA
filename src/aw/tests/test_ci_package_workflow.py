"""Lock Preview selection; command stubs do not certify Python-free execution."""

import json
import os
from pathlib import Path
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
        for name in ("bash", "mkdir", "date", "uname", "getconf", "cat", "df", "timeout", "tee"):
            (binary / name).symlink_to(shutil.which(name))
        helper = self.root / "src/aw/scripts/check_packages.sh"
        helper.parent.mkdir(parents=True)
        shutil.copyfile(AW / "scripts/check_packages.sh", helper)
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
                    "-p", "aw-package", "--", "--test-threads=2",
                ],
                [
                    "test", "--locked", "--manifest-path", "src/aw/Cargo.toml",
                    "-p", "aw-cli", "--test", "installation", "--test",
                    "config_edit_cli", "--test", "standalone", "--test", "packages",
                    "--", "--test-threads=2", "--show-output",
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
        # Installing Python before this stage must fail before Cargo can run.
        python = binary / "python3"
        python.write_text("#!/bin/sh\nexit 0\n")
        python.chmod(0o700)
        before = capture.read_bytes()
        result = subprocess.run(
            ["/bin/sh", "-eu", "-c", script],
            cwd=self.root,
            env={
                "PATH": str(binary), "GITHUB_WORKSPACE": str(self.root),
                "AW_PREVIEW_DIR": str(preview), "AW_PACKAGE_COMMANDS": str(capture),
            },
            capture_output=True, text=True, timeout=10,
        )
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("Python-free environment", result.stderr)
        self.assertEqual(capture.read_bytes(), before)

    def test_terminal_target_remains_required_in_the_python_enabled_gate(self) -> None:
        matrix = json.loads((AW / "tests/regression-matrix.json").read_text())
        target = "aw-cli/test/packages_pty"
        self.assertIn(target, matrix["required_targets"])
        self.assertNotIn(target, matrix["opt_in_targets"])


class PackageEvidenceTests(GateFixture):
    def setUp(self) -> None:
        super().setUp()
        self.bin = self.root / "bin"
        self.bin.mkdir()
        self.output = self.root / "evidence"
        self.calls = self.root / "calls"
        self.environment = {
            **os.environ, "PATH": str(self.bin) + ":" + os.environ["PATH"],
            "PACKAGE_CALLS": str(self.calls),
        }

    def executable(self, name: str, body: str) -> None:
        path = self.bin / name
        path.write_text("#!/bin/bash\nset -eu\n" + body)
        path.chmod(0o700)

    def run_gate(self) -> subprocess.CompletedProcess[str]:
        return subprocess.run(
            ["bash", str(AW / "scripts/check_packages.sh"), str(self.output)],
            cwd=AW.parents[1], env=self.environment, capture_output=True, text=True, timeout=15,
        )

    def test_failed_cargo_retains_diagnostics_and_stops_following_suite(self) -> None:
        self.executable("cargo", 'echo "$*" >> "$PACKAGE_CALLS"\necho fixture-failure\nexit 37\n')
        result = self.run_gate()
        self.assertEqual(result.returncode, 37, result.stderr)
        self.assertEqual(len(self.calls.read_text().splitlines()), 1)
        self.assertIn("fixture-failure", (self.output / "package.log").read_text())
        self.assertIn("exit_code=37", (self.output / "package-command.log").read_text())
        for phase in ("before", "after"):
            self.assertTrue((self.output / f"package-{phase}.log").stat().st_size)
        self.assertFalse((self.output / "cli-command.log").exists())

    def test_success_runs_both_suites_and_log_failure_remains_failure(self) -> None:
        self.executable("cargo", 'echo "$*" >> "$PACKAGE_CALLS"\necho fixture-success\n')
        result = self.run_gate()
        self.assertEqual(result.returncode, 0, result.stderr)
        calls = self.calls.read_text().splitlines()
        self.assertEqual(len(calls), 2)
        self.assertTrue(all("--test-threads=2" in call for call in calls))
        self.assertIn("--test packages -- --test-threads=2 --show-output", calls[1])
        self.assertIn("exit_code=0", (self.output / "cli-command.log").read_text())
        (self.output / "package.log").unlink()
        (self.output / "package.log").mkdir()
        result = self.run_gate()
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(len(self.calls.read_text().splitlines()), 3)

    def test_expired_deadline_fails_and_reaps_the_owned_command(self) -> None:
        pid = self.root / "pid"
        self.environment["PACKAGE_PID"] = str(pid)
        self.executable("cargo", 'echo "$*" >> "$PACKAGE_CALLS"\necho $$ > "$PACKAGE_PID"\nexec sleep 60\n')
        self.executable(
            "timeout",
            'test "$1" = --signal=INT\ntest "$2" = --kill-after=15s\ntest "$3" = 600\n'
            'shift 3\nexec /usr/bin/timeout --signal=INT --kill-after=1s 1 "$@"\n',
        )
        result = self.run_gate()
        self.assertEqual(result.returncode, 124, result.stderr)
        self.assertIn("exit_code=124", (self.output / "package-command.log").read_text())
        self.assertFalse(Path("/proc", pid.read_text().strip()).exists())
        self.assertEqual(len(self.calls.read_text().splitlines()), 1)


class UnavailableMetricTests(PackageEvidenceTests):
    def setUp(self) -> None:
        super().setUp()
        self.executable(
            "cat",
            'if test "$1" = /sys/fs/cgroup/cpu.pressure; then\n'
            '  echo "cat: $1: Operation not supported" >&2\n'
            '  exit 1\nfi\nexec /bin/cat "$@"\n',
        )

    def run_gate(self) -> subprocess.CompletedProcess[str]:
        result = super().run_gate()
        for phase in ("before", "after"):
            snapshot = (self.output / f"package-{phase}.log").read_text()
            self.assertIn("unavailable (exit_code=1)", snapshot)
            self.assertIn("/sys/fs/cgroup/cpu.pressure: Operation not supported", snapshot)
        return result


if __name__ == "__main__":
    unittest.main()
