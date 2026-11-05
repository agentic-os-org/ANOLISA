"""Keep independent delivery in maintainer Make targets and test its CI command wiring."""

import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import textwrap
import unittest

from test_ci_checks import AW, GateFixture


class PackageSelectionTests(GateFixture):
    def test_delivery_uses_maintainer_make_targets(self) -> None:
        workflow = (AW.parents[1] / ".github/workflows/aw-integration.yml").read_text()
        step = workflow.split(
            "      - name: Test installer and build independent artifacts\n", 1
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
        make = binary / "make"
        make.write_text(
            "#!/bin/sh\n"
            "printf 'make\\000' >> \"$AW_PACKAGE_COMMANDS\"\n"
            "printf '%s\\000' \"$@\" >> \"$AW_PACKAGE_COMMANDS\"\n"
            "printf '\\000' >> \"$AW_PACKAGE_COMMANDS\"\n"
        )
        make.chmod(0o700)
        capture = self.root / "commands"
        output = self.root / "package output"
        subprocess.run(
            ["/bin/sh", "-eu", "-c", script],
            cwd=self.root,
            env={
                "PATH": str(binary),
                "GITHUB_WORKSPACE": str(self.root / "container workspace"),
                "AW_PACKAGE_DIR": str(output),
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
                ["make", "-C", "distribution", "test-aw-packages"],
                [
                    "make", "-C", "distribution", "aw-packages", "VERSION=0.1.0",
                    "OUTPUT=" + str(output / "packages"), "COMPONENTS=core sec-core tokenless",
                ],
                ["make", "-C", "distribution", "aw-integration-backends",
                 "BACKENDS=sec-core tokenless"],
            ],
        )
        self.assertFalse(any("packages_pty" in command for command in commands))

    def test_python_free_installer_precedes_python_enabled_make_builds(self) -> None:
        workflow = (AW.parents[1] / ".github/workflows/aw-integration.yml").read_text()
        name = "      - name: Test installer without Python\n"
        stage = workflow.split(name, 1)[1].split("      - name:", 1)[0]
        script = textwrap.dedent(stage.split("        run: |\n", 1)[1])
        self.assertLess(workflow.index(name), workflow.index("- name: Install packaging prerequisite"))
        self.assertLess(
            workflow.index("- name: Install packaging prerequisite"),
            workflow.index("- name: Test installer and build independent artifacts"),
        )
        prerequisites = workflow.split("- name: Install build prerequisites", 1)[1]
        self.assertNotIn("python3", prerequisites.split("- uses:", 1)[0])
        binary = self.root / "bin"
        binary.mkdir()
        (binary / "bash").symlink_to(shutil.which("bash"))
        capture = self.root / "commands"
        for command in ("git", "cargo"):
            executable = binary / command
            executable.write_text(
                "#!/bin/sh\nexit 0\n" if command == "git" else
                '#!/bin/sh\nprintf "%s\\n" "$*" >> "$AW_PACKAGE_COMMANDS"\n'
            )
            executable.chmod(0o700)
        helper = self.root / "src/aw/scripts/check_packages.sh"
        helper.parent.mkdir(parents=True)
        # The real bounded helper is exercised by PackageEvidenceTests on Linux.
        helper.write_text('#!/bin/sh\ncargo fixture-installer-gate "$1"\n')
        output = self.root / "package output"
        environment = {
            "PATH": str(binary), "GITHUB_WORKSPACE": str(self.root),
            "AW_PACKAGE_DIR": str(output), "AW_PACKAGE_COMMANDS": str(capture),
        }
        subprocess.run(
            ["/bin/sh", "-eu", "-c", script], cwd=self.root, env=environment,
            check=True, timeout=10,
        )
        self.assertIn("fixture-installer-gate " + str(output / "test-evidence"), capture.read_text())
        # Installing Python before this stage must fail before Cargo can run.
        python = binary / "python3"
        python.write_text("#!/bin/sh\nexit 0\n")
        python.chmod(0o700)
        before = capture.read_bytes()
        result = subprocess.run(
            ["/bin/sh", "-eu", "-c", script],
            cwd=self.root,
            env=environment,
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


@unittest.skipUnless(sys.platform == "linux", "cgroup and GNU timeout evidence requires Linux")
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
