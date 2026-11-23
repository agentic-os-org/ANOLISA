#!/usr/bin/env python3
"""Check the raw action's Cross environment without building release artifacts."""

import json
import os
import shlex
import subprocess
import tempfile
import unittest
from pathlib import Path

ACTION = Path(__file__).with_name("build.sh")
PRE_BUILD = "CROSS_TARGET_X86_64_UNKNOWN_LINUX_GNU_PRE_BUILD"
STUB = """#!/usr/bin/env python3
import json, os, pathlib, sys
tool = pathlib.Path(sys.argv[0]).name
if tool == "rustc":
    print(os.environ["TEST_SYSROOT"])
elif tool == "make":
    root = pathlib.Path(sys.argv[sys.argv.index("-C") + 1])
    assert (root / "v2/Cargo.toml").is_file()
    keys = ("CROSS_CONTAINER_OPTS", "CROSS_TARGET_X86_64_UNKNOWN_LINUX_GNU_PRE_BUILD", "DOCKER_BUILDKIT")
    captured = {key: os.environ[key] for key in keys if key in os.environ}
    pathlib.Path(os.environ["TEST_CAPTURE"]).write_text(json.dumps(captured))
    sys.exit(73)
elif tool == "rpm":
    if os.environ.get("TEST_RPM_FAIL"):
        sys.exit(9)
    print("2.28-251.el8_10.25.x86_64", end="")
elif tool == "dnf":
    with open(os.environ["TEST_DNF_LOG"], "a") as stream:
        stream.write(json.dumps(sys.argv[1:]) + "\\n")
    if sys.argv[1] == "install" and os.environ.get("TEST_DNF_FAIL"):
        sys.exit(8)
"""


class BuildEnvironmentTests(unittest.TestCase):
    def test_cross_source_mount_and_static_dependencies(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory).resolve()
            source = root / "source"
            component = source / "src/agent-sec-core"
            (component / "packaging/raw").mkdir(parents=True)
            (component / "v2").mkdir()
            (component / "v2/Cargo.toml").write_text("[workspace]\n")
            (component / "packaging/raw/verify_release.py").write_text(
                'print("0.0.0")\n'
            )
            for args in (
                ["init", "-q"],
                ["config", "core.hooksPath", "/dev/null"],
                ["add", "."],
                [
                    "-c",
                    "user.name=Test",
                    "-c",
                    "user.email=test@localhost",
                    "-c",
                    "commit.gpgsign=false",
                    "commit",
                    "-qm",
                    "Fixture",
                ],
            ):
                subprocess.run(
                    ["git", "-C", str(source), *args], check=True, timeout=15
                )

            fake_bin = root / "bin"
            fake_bin.mkdir()
            for name in (
                "cargo",
                "rustc",
                "make",
                "flock",
                "npm",
                "sha256sum",
                "rpm",
                "dnf",
            ):
                command = fake_bin / name
                command.write_text(STUB)
                command.chmod(0o755)
            capture = root / "environment.json"
            worktree = root / "worktree with space's"
            env = {
                **os.environ,
                "PATH": f"{fake_bin}:{os.environ['PATH']}",
                "SEC_CORE_SOURCE_WORKTREE": str(worktree),
                "TEST_CAPTURE": str(capture),
                "TEST_SYSROOT": str(root / "sysroot"),
                "TEST_DNF_LOG": str(root / "dnf.jsonl"),
                "CROSS_CONTAINER_OPTS": "--env='EXISTING=two words'",
                "DOCKER_BUILDKIT": "1",
            }
            result = subprocess.run(
                [
                    "bash",
                    str(ACTION),
                    "--source-repo",
                    str(source),
                    "--output-dir",
                    str(root / "out"),
                    "--version",
                    "0.0.0",
                    "--target-os",
                    "linux",
                    "--target-arch",
                    "x86_64",
                    "--profile",
                    "gnu2.28-x86_64",
                ],
                env=env,
                capture_output=True,
                text=True,
                check=False,
                timeout=30,
            )
            self.assertEqual(result.returncode, 73, result.stdout + result.stderr)
            captured = json.loads(capture.read_text())
            self.assertEqual(captured["DOCKER_BUILDKIT"], "0")
            mounted = worktree / "src/agent-sec-core"
            self.assertEqual(
                shlex.split(captured["CROSS_CONTAINER_OPTS"]),
                [
                    "--env=EXISTING=two words",
                    f"--volume={mounted}:{mounted}:ro",
                ],
            )
            self.assertFalse(
                worktree.exists(), "the action must remove its source worktree"
            )

            for failure, expected_code, expected_calls in (
                (
                    {},
                    0,
                    [
                        [
                            "install",
                            "-y",
                            "--enablerepo=powertools",
                            "glibc-static-2.28-251.el8_10.25.x86_64",
                        ],
                        ["clean", "all"],
                    ],
                ),
                ({"TEST_RPM_FAIL": "1"}, 9, []),
                (
                    {"TEST_DNF_FAIL": "1"},
                    8,
                    [
                        [
                            "install",
                            "-y",
                            "--enablerepo=powertools",
                            "glibc-static-2.28-251.el8_10.25.x86_64",
                        ]
                    ],
                ),
            ):
                with self.subTest(failure=failure):
                    log = Path(env["TEST_DNF_LOG"])
                    log.write_text("")
                    prepared = subprocess.run(
                        ["sh", "-c", captured[PRE_BUILD]],
                        env={**env, **failure},
                        capture_output=True,
                        text=True,
                        check=False,
                        timeout=10,
                    )
                    self.assertEqual(
                        prepared.returncode, expected_code, prepared.stderr
                    )
                    self.assertEqual(
                        [json.loads(line) for line in log.read_text().splitlines()],
                        expected_calls,
                    )


if __name__ == "__main__":
    unittest.main()
