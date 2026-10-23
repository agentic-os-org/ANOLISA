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
from unittest.mock import patch

from test_hermes_lifecycle import _load_plugin

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

    def protocol(self, operation: str, value: dict) -> dict:
        result = subprocess.run(
            [str(TOKENLESS), "compress"],
            input=json.dumps({
                "protocol_version": 2, "operation": operation,
                "attribution": {"agent_id": "rtk-integration", "session_id": "session-49", "tool_use_id": "tool-49"},
                "input": value,
            }),
            cwd=self.root, env=self.env, text=True, capture_output=True, timeout=30,
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        return json.loads(result.stdout)["result"]

    def rewrite_command(self, command: str, tracked: bool = True) -> str:
        if not (self.bin_dir / "rtk").exists():
            (self.bin_dir / "rtk").symlink_to(RTK)
        rewritten = self.protocol("pre_tool", {
            "tool_name": "Bash", "arguments": {"command": command}, "command_field": "command",
            "capabilities": {"replace_arguments": True, "block_and_suggest": False},
        })
        self.assertEqual(rewritten["action"], "replace_arguments")
        result = rewritten["arguments"]["command"]
        self.assertEqual("TOKENLESS_RTK_RUN_ID=" in result, tracked)
        return result

    def execute(self, command: str) -> str:
        executed = subprocess.run(
            ["/bin/sh", "-c", command],
            cwd=self.root, env=self.env, text=True, capture_output=True, timeout=30,
        )
        self.assertEqual(executed.returncode, 0, executed.stderr)
        return executed.stdout

    def execute_rewrite(self, command: str) -> str:
        return self.execute(self.rewrite_command(command))

    def post_rtk(self, content: str, command: str | None = None, recovery: str = "none") -> dict:
        return self.protocol("post_tool", {
            "result_kind": "tool", "tool_name": "Bash", "content": content,
            "command": command,
            "status": "success", "content_origin": "command_output", "output_optimization": "rtk",
            "capabilities": {"replace_output": True, "replace_with_text": True, "recovery": {"kind": recovery}},
        })

    def test_unchanged_local_html_keeps_file_origin_after_rewrite(self) -> None:
        content = "<html><body><main>" + "<p>Editable source markup.</p>" * 100 + "</main></body></html>"
        (self.root / "page.html").write_text(content)
        self.env["TOKENLESS_HTML_EXTRACTION_ENABLED"] = "1"
        command = self.rewrite_command("cat page.html")
        output = self.execute(command)
        self.assertEqual(output, content)
        result = self.post_rtk(output, command, recovery="shell")
        self.assertEqual(result["output"], content)
        self.assertEqual(result["applied_operations"], [])

    def test_untracked_retry_invalidates_prior_receipts_and_late_reports(self) -> None:
        (self.root / "clean.txt").write_text("original unchanged output")
        content = json.dumps([{"name": f"item-{index}", "value": index} for index in range(30)], indent=2)
        (self.root / "filtered.js").write_text("// stripped by RTK\n" + content)
        (self.root / "empty.txt").write_text("")
        old_command = self.rewrite_command("cat clean.txt")
        self.execute(old_command)
        command = self.rewrite_command("rtk read -l minimal filtered.js && cat empty.txt", tracked=False)
        # A delayed producer from the previous attempt cannot recreate its receipt.
        self.execute(old_command)
        output = self.execute(command)
        self.assertEqual(output, content)
        result = self.post_rtk(output, command)
        self.assertEqual(result["disposition"], "passthrough")
        self.assertEqual(result["output"], output)

    def test_locked_retry_cannot_consume_an_older_receipt(self) -> None:
        (self.root / "clean.txt").write_text("original unchanged output")
        content = json.dumps([{"name": f"item-{index}", "value": index} for index in range(30)], indent=2)
        (self.root / "filtered.js").write_text("// stripped by RTK\n" + content)
        (self.root / "empty.txt").write_text("")
        for retry in ["cat filtered.js", "rtk read -l minimal filtered.js && cat empty.txt"]:
            with self.subTest(retry=retry):
                self.execute_rewrite("cat clean.txt")
                database = sqlite3.connect(self.root / "tokenless/rtk-outcomes.db")
                try:
                    database.execute("BEGIN EXCLUSIVE")
                    command = self.rewrite_command(retry, tracked=False)
                finally:
                    database.rollback()
                    database.close()
                if retry.startswith("cat"):
                    command += " -l minimal"
                output = self.execute(command)
                self.assertEqual(output, content)
                for executed_command in [None, command]:
                    result = self.post_rtk(output, executed_command)
                    self.assertEqual(result["disposition"], "passthrough")
                    self.assertEqual(result["output"], output)

    def test_fallback_requires_the_exact_executed_command(self) -> None:
        content = json.dumps([{"name": f"item-{index}", "value": index} for index in range(30)], indent=2)
        (self.root / "clean.txt").write_text(content)
        command = self.rewrite_command("cat clean.txt")
        output = self.execute(command)
        for supplied in [None, "cat clean.txt", command + " && printf extra"]:
            with self.subTest(command=supplied):
                result = self.post_rtk(output, supplied)
                self.assertEqual(result["disposition"], "passthrough")
        self.assertEqual(self.post_rtk(output, command)["disposition"], "applied")

    def test_hermes_recognizes_real_tracked_wrappers(self) -> None:
        plugin = _load_plugin(str(ROOT / "adapters/tokenless/hermes/__init__.py"), "hermes_real_rtk")
        (self.bin_dir / "rtk").symlink_to(RTK)
        self.env["TOKENLESS_HTML_EXTRACTION_ENABLED"] = "1"
        html = "<html><body><main>" + "<p>Editable source markup.</p>" * 100 + "</main></body></html>"
        data = json.dumps([{"name": f"item-{index}", "value": index} for index in range(30)], indent=2)
        requests = []

        def run_core(tokenless_bin, request, timeout, operation):
            requests.append(request)
            result = subprocess.run([tokenless_bin, "compress"], input=json.dumps(request),
                                    cwd=self.root, env=self.env, text=True, capture_output=True, timeout=timeout)
            self.assertEqual(result.returncode, 0, result.stderr)
            return json.loads(result.stdout)["result"]

        with patch.object(plugin, "_resolve_binary", return_value=str(TOKENLESS)), \
             patch.object(plugin, "run_compress", side_effect=run_core), \
             patch.object(plugin, "tokenless_retrieve_command_available", return_value=True):
            for filename, source, expected, suffix in [
                ("page.html", html, html, ""),
                ("filtered.js", "// stripped by RTK\n" + data, data, " -l minimal"),
                ("clean.json", data, data, ""),
            ]:
                with self.subTest(filename=filename):
                    (self.root / filename).write_text(source)
                    rewritten = plugin.on_pre_tool_call(tool_name="terminal", args={"command": f"cat {filename}"},
                                                        session_id="hermes-session", tool_call_id="hermes-call")
                    self.assertEqual(rewritten["action"], "block")
                    command = rewritten["message"].removeprefix("[tokenless:rewrite] Re-execute as: ") + suffix
                    self.assertIn("TOKENLESS_RTK_RUN_ID=", command)
                    output = self.execute(command)
                    self.assertEqual(output, expected)
                    result = plugin.on_transform_tool_result(tool_name="terminal", args={"command": command},
                                                             result=json.dumps({"output": output, "exit_code": 0}),
                                                             session_id="hermes-session", tool_call_id="hermes-call", status="ok")
                    if filename == "clean.json":
                        self.assertIsNotNone(result)
                        self.assertLess(len(json.loads(result)["output"]), len(output))
                    else:
                        self.assertIsNone(result)
                    self.assertEqual(requests[-1]["input"]["output_optimization"], "rtk")
                    self.assertEqual(requests[-1]["input"]["capabilities"]["recovery"]["kind"], "none")

    def test_rtk_post_tool_ignores_unusable_state_directory(self) -> None:
        content = json.dumps([{"name": f"item-{index}", "value": index} for index in range(30)], indent=2)
        invalid = self.root / "not-a-directory"
        invalid.write_text("file")
        for directory in ["relative/data", str(invalid)]:
            for stats in ["0", "1"]:
                with self.subTest(directory=directory, stats=stats):
                    self.env["TOKENLESS_DATA_DIR"] = directory
                    self.env["TOKENLESS_STATS_ENABLED"] = stats
                    result = self.post_rtk(content)
                    self.assertEqual(result["disposition"], "passthrough")
                    self.assertEqual(result["output"], content)

    def test_rtk_receipt_failure_does_not_pollute_tool_output(self) -> None:
        content = "unchanged tool output"
        (self.root / "clean.txt").write_text(content)
        command = self.rewrite_command("cat clean.txt")
        database = self.root / "tokenless/rtk-outcomes.db"
        database.unlink()
        database.mkdir()
        result = subprocess.run(["/bin/sh", "-c", command], cwd=self.root, env=self.env,
                                text=True, capture_output=True, timeout=30)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout, content)
        self.assertEqual(result.stderr, "")

    def test_changed_bytes_with_equal_token_estimate_keep_rtk_ownership(self) -> None:
        records = [{"name": f"item-{index}", "value": index} for index in range(30)]
        filtered = json.dumps(records, indent=2)
        # Three removed bytes must remain in the same ceil(bytes / 4) bucket.
        records[0]["name"] += "x" * ((1 - len(filtered)) % 4)
        filtered = json.dumps(records, indent=2)
        content = "//\n" + filtered
        (self.root / "tied.js").write_text(content)
        command = self.rewrite_command("cat tied.js")
        # Exercise the producer's explicit filtering mode with a tracked generation.
        output = self.execute(command + " -l minimal")
        self.assertEqual(output, filtered)
        self.assertNotEqual(output, content)
        self.assertEqual((len(output) + 3) // 4, (len(content) + 3) // 4)
        result = self.post_rtk(output, command + " -l minimal")
        self.assertEqual(result["disposition"], "passthrough")
        self.assertEqual(result["output"], output)

    def test_receipt_fallback_is_independent_of_statistics(self) -> None:
        content = json.dumps([{"name": f"item-{index}", "value": index} for index in range(30)], indent=2)
        git = self.bin_dir / "git"
        git.write_text("#!/bin/sh\nprintf '%s\\n' " + shlex.quote(content) + "\n")
        git.chmod(0o755)
        self.env["TOKENLESS_STATS_ENABLED"] = "0"
        for command, unchanged in [
            ("git show --format=%B --stat", True),
            ("git log -20 --numstat", True),
            # Oneline tracking trims the trailing newline even without token savings.
            ("git log -20 --oneline", False),
        ]:
            with self.subTest(command=command):
                executed_command = self.rewrite_command(command)
                output = self.execute(executed_command)
                self.assertEqual(output.strip(), content)
                fallback = self.post_rtk(output, executed_command)
                self.assertEqual(fallback["disposition"], "applied" if unchanged else "passthrough")
                if unchanged:
                    self.assertEqual(fallback["recoverability"], "lossless")
                    self.assertLess(fallback["after_tokens"], fallback["before_tokens"])
                else:
                    self.assertEqual(fallback["output"], output)
                repeated = self.post_rtk(output, executed_command)
                self.assertEqual(repeated["disposition"], "passthrough")
                self.assertEqual(repeated["output"], output)
        self.assertFalse((self.root / "tokenless/stats.db").exists())

    def test_rtk_savings_still_skip_post_processing(self) -> None:
        content = "abc1234 subject (1 day ago) <author>\n" + "commit body detail\n" * 100 + "---END---\n"
        git = self.bin_dir / "git"
        git.write_text("#!/bin/sh\nprintf '%s' " + shlex.quote(content) + "\n")
        git.chmod(0o755)
        output = self.execute_rewrite("git log -20")
        self.assertLess(len(output), len(content))
        result = self.post_rtk(output)
        self.assertEqual(result["disposition"], "passthrough")
        self.assertEqual(result["output"], output)

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
