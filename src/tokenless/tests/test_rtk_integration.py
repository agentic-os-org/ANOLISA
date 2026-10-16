#!/usr/bin/env python3
"""Verify Tokenless patches through the built RTK executable."""

from __future__ import annotations

import json
import os
import shlex
import sqlite3
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
RTK = Path(
    os.environ.get("TOKENLESS_TEST_RTK_BINARY", ROOT / "third_party/rtk/target/release/rtk")
).resolve()
TOKENLESS = Path(
    os.environ.get("TOKENLESS_TEST_TOKENLESS_BINARY", ROOT / "target/debug/tokenless")
).resolve()


class RtkIntegrationTest(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.bin_dir = self.root / "bin"
        self.bin_dir.mkdir()
        self.env = {
            **os.environ,
            "PATH": f"{self.bin_dir}:{os.environ['PATH']}",
            "XDG_CONFIG_HOME": str(self.root / "config"),
            "XDG_DATA_HOME": str(self.root / "data"),
            "RTK_DB_PATH": str(self.root / "rtk.db"),
            "TOKENLESS_DATA_DIR": str(self.root / "tokenless"),
            "TOKENLESS_STATS_DB": str(self.root / "tokenless/stats.db"),
            "TOKENLESS_STATS_ENABLED": "1",
            "TOKENLESS_AGENT_ID": "rtk-integration",
            "TOKENLESS_SESSION_ID": "session-49",
            "TOKENLESS_TOOL_USE_ID": "tool-49",
        }

    def run_rtk(self, *args: str) -> subprocess.CompletedProcess[str]:
        return subprocess.run(
            [str(RTK), *args],
            cwd=self.root,
            env=self.env,
            capture_output=True,
            text=True,
            timeout=15,
        )

    def fake_pytest(self, stdout: str, stderr: str = "", exit_code: int = 0) -> None:
        executable = self.bin_dir / "pytest"
        executable.write_text(
            "#!/bin/sh\n"
            f"printf '%s' {shlex.quote(stdout)}\n"
            f"printf '%s' {shlex.quote(stderr)} >&2\n"
            f"exit {exit_code}\n"
        )
        executable.chmod(0o755)

    def test_rewrite_returns_command_without_executing_it(self) -> None:
        result = self.run_rtk("rewrite", "git status")
        self.assertIn(result.returncode, (0, 3), result.stderr)
        self.assertEqual(result.stdout.strip(), "rtk git status")

    def test_native_find_hooks_keep_paths_when_pinned_rtk_would_rewrite(self) -> None:
        (self.bin_dir / "rtk").symlink_to(RTK)
        (self.bin_dir / "tokenless").symlink_to(TOKENLESS)
        self.env["TOKENLESS_AGENT_ID"] = "qwenwork"
        self.env["HOME"] = str(self.root / "home")
        Path(self.env["HOME"]).mkdir()
        hooks = ROOT / "adapters/tokenless/common/hooks"
        target = self.root / "project with spaces/long_directory_for_read_regression/records"
        target.mkdir(parents=True)
        for index in range(60):
            (target / f"document {index}.json").write_text(str(index), encoding="utf-8")
        quoted = shlex.quote(str(target))

        def run_hook(script: str, payload: dict) -> dict:
            result = subprocess.run(
                [sys.executable, str(hooks / script)],
                input=json.dumps(payload), env=self.env, cwd=self.root,
                capture_output=True, text=True, timeout=30,
            )
            self.assertEqual(result.returncode, 0, result.stderr)
            return json.loads(result.stdout)

        def pre_tool(command: str, call_id: str) -> dict:
            return run_hook("rewrite_hook.py", {
                "session_id": "find-counterfactual", "tool_use_id": call_id,
                "tool_name": "Bash", "tool_input": {"command": command},
            })

        # This must rewrite: otherwise fail-open hooks could make every native
        # assertion below pass without running the CLI or resolving RTK.
        control = pre_tool("git status", "control")
        self.assertIn("rtk", control["hookSpecificOutput"]["updatedInput"]["command"])
        commands = [
            f"{prefix} {quoted} -type f" for prefix in (
                "find", "timeout 30 find", "timeout -k 5 -s TERM 30 find",
                "timeout --signal=TERM 30 find", "time -p find", "nice -n10 find",
                "time -f '' find", "timeout -s '' 30 find", "nice -n '' find",
                "nice --adjustment=10 find", "nohup find", "/usr/bin/timeout 30 find",
                "env FOO=1 timeout 20 find", "nohup nice -n 5 timeout 30 find", "uv run find",
                "command \\\n  find", "noglob \\\n  find", "env \\\n  FOO=1 \\\n  find", "timeout 30 \\\n  find",
            )
        ]
        for index, command in enumerate(commands):
            with self.subTest(command=command):
                counterfactual = self.run_rtk("rewrite", command)
                self.assertEqual(counterfactual.returncode, 3, counterfactual.stderr)
                self.assertIn("rtk find", counterfactual.stdout)
                self.assertEqual(pre_tool(command, f"native-{index}"), {})

        for index, prefix in enumerate(("timeout 30", "nice -n 10", "nohup", "command \\\n")):
            command = f"{prefix} find {quoted} -type f"
            call_id = f"read-{index}"
            self.assertEqual(pre_tool(command, call_id), {})
            executed = subprocess.run(
                ["/bin/sh", "-c", command], env=self.env, cwd=self.root,
                capture_output=True, text=True, timeout=15,
            )
            self.assertEqual(executed.returncode, 0, executed.stderr)
            result = run_hook("compress_response_hook.py", {
                "session_id": "find-counterfactual", "tool_use_id": call_id,
                "tool_name": "Bash", "tool_input": {"command": command},
                "tool_response": executed.stdout,
            })
            slot = result["hookSpecificOutput"]
            view = slot.get("updatedToolOutput", slot.get("updatedToolResponse"))
            data = json.loads(view.split("\n", 1)[1])
            paths = [data["base"] + path for path in data["paths"]]
            restored = data["line_ending"].join(paths)
            if data["trailing_newline"]:
                restored += data["line_ending"]
            self.assertEqual(restored, executed.stdout)
            self.assertEqual(len(paths), 60)
            for path in paths:
                self.assertEqual(Path(path).read_text(), Path(path).stem.split()[-1])

    def test_pytest_error_count_survives_color_and_failure_exit(self) -> None:
        self.fake_pytest(
            "\x1b[31m=== ERRORS ===\x1b[0m\n"
            "_______ ERROR collecting test_sample.py _______\n"
            "E   ImportError: missing_dependency\n"
            "=== short test summary info ===\n"
            "ERROR test_sample.py - ImportError: missing_dependency\n"
            "\x1b[31m=== 1 error in 0.10s ===\x1b[0m\n",
            exit_code=2,
        )
        result = self.run_rtk("pytest")
        self.assertEqual(result.returncode, 2, result.stderr)
        self.assertIn("1 error", result.stdout)
        self.assertIn("missing_dependency", result.stdout)
        self.assertNotIn("No tests collected", result.stdout)

    def test_pytest_stderr_only_failure_remains_visible(self) -> None:
        self.fake_pytest("", "ImportError: missing_pytest_plugin\n", exit_code=4)
        result = self.run_rtk("pytest")
        self.assertEqual(result.returncode, 4)
        self.assertIn("ImportError: missing_pytest_plugin", result.stdout + result.stderr)

    def test_pytest_startup_traceback_remains_visible(self) -> None:
        self.fake_pytest(
            "Traceback (most recent call last):\nImportError: startup_failure\n", exit_code=3
        )
        result = self.run_rtk("pytest")
        self.assertEqual(result.returncode, 3)
        self.assertIn("ImportError: startup_failure", result.stdout)

    def test_pytest_short_multiline_stderr_preserves_last_error(self) -> None:
        stderr = "warning\n" * 48 + "ImportError: missing_plugin\n"
        self.assertLess(len(stderr.encode()), 500)
        self.fake_pytest("", stderr, exit_code=4)
        result = self.run_rtk("pytest")
        self.assertEqual(result.returncode, 4)
        self.assertIn(stderr, result.stdout)
        self.assertNotIn("full output available via tee", result.stdout)

    def test_pytest_stderr_survives_disabled_recovery(self) -> None:
        self.env["RTK_RECALL"] = "0"
        stderr = "plugin initialization warning\n" * 48 + "ImportError: missing_plugin\n"
        self.fake_pytest("", stderr, exit_code=4)
        result = self.run_rtk("pytest")
        self.assertEqual(result.returncode, 4)
        self.assertIn(stderr, result.stdout)

    def test_pytest_no_tests_signal_survives_stderr_warning(self) -> None:
        self.fake_pytest(
            "============================= test session starts ==============================\n"
            "collected 0 items\n"
            "============================ no tests ran in 0.01s =============================\n",
            "DeprecationWarning: plugin xyz is deprecated\n",
            exit_code=5,
        )
        result = self.run_rtk("pytest")
        self.assertEqual(result.returncode, 5)
        self.assertIn("No tests collected", result.stdout)
        self.assertIn("DeprecationWarning: plugin xyz is deprecated", result.stdout)

    def test_sudo_command_is_not_rewritten(self) -> None:
        result = self.run_rtk("rewrite", "sudo git status")
        self.assertEqual(result.returncode, 1, result.stderr)
        self.assertEqual(result.stdout, "")

    def test_compression_records_tokenless_attribution(self) -> None:
        self.fake_pytest(
            "=== test session starts ===\ncollected 100 items\n"
            + "tests/test_sample.py . [ 50%]\n" * 100
            + "=== 100 passed in 0.10s ===\n"
        )
        result = self.run_rtk("pytest")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("100 passed", result.stdout)
        database = self.root / "tokenless/stats.db"
        self.assertTrue(database.is_file(), result.stdout + result.stderr)
        with sqlite3.connect(database) as connection:
            rows = connection.execute(
                "SELECT agent_id, session_id, tool_use_id, before_tokens, after_tokens FROM stats"
            ).fetchall()
        self.assertEqual(len(rows), 1)
        self.assertEqual(rows[0][:3], ("rtk-integration", "session-49", "tool-49"))
        self.assertGreater(rows[0][3], rows[0][4])


if __name__ == "__main__":
    unittest.main()
