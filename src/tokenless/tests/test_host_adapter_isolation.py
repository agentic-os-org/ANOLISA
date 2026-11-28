#!/usr/bin/env python3
"""Exercise host contracts and independent changes in a staged adapter tree."""

from __future__ import annotations

import json
import os
import shutil
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

from test_compress_response_hook import _create_mock_claude, _create_mock_tokenless

ROOT = Path(__file__).resolve().parents[1]
SOURCE = ROOT / "adapters/tokenless"
HOSTS = {
    "claude-code": "claude-code",
    "qoder-cli": "qoder",
    "qwenwork": "qwenwork",
    "opencode": "opencode",
    "cosh-ng": "cosh-ng",
    "copilot-shell": "copilot-shell",
    "codex": "codex",
    "qwencode": "qwencode",
    "workbuddy": "workbuddy",
    "unknown": "unknown",
}


class HostAdapterIsolation(unittest.TestCase):
    def setUp(self) -> None:
        temporary = tempfile.TemporaryDirectory(prefix="tokenless-host-adapters-")
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.adapters = self.root / "adapters"
        shutil.copytree(
            SOURCE / "common",
            self.adapters / "common",
            ignore=shutil.ignore_patterns("__pycache__", "*.pyc"),
        )
        shutil.copy2(SOURCE / "hook_dispatch.py", self.adapters)
        for host in HOSTS.values():
            (self.adapters / host).mkdir()
            shutil.copy2(SOURCE / host / "hook_adapter.py", self.adapters / host)
        binary_dir = self.root / "bin"
        binary_dir.mkdir()
        _create_mock_tokenless(str(binary_dir), "compress-text")
        _create_mock_claude(str(binary_dir))
        self.env = {
            key: value
            for key, value in os.environ.items()
            if key not in {"COSH_RUNTIME", "COSH_NG_VERSION", "QODER_PLUGIN_ROOT"}
        }
        self.env.update(
            HOME=str(self.root), PATH=f"{binary_dir}{os.pathsep}{os.environ['PATH']}"
        )

    def replay(self, host: str, hook: Path | None = None) -> dict:
        response = {"stdout": "build output\n" * 100, "stderr": "", "exit_code": 0}
        if host == "cosh-ng":
            response = {
                "llmContent": json.dumps(response),
                "returnDisplay": "display-only",
            }
        elif host == "opencode":
            response = response["stdout"]
        payload = {
            "tool_name": "Bash",
            "tool_input": {"command": "build"},
            "tool_response": response,
            "session_id": "session",
            "tool_use_id": "call",
        }
        env = {**self.env, "TOKENLESS_AGENT_ID": host}
        if host == "cosh-ng":
            env["COSH_NG_VERSION"] = "0.21.0"
        hook = hook or self.adapters / "common/hooks/compress_response_hook.py"
        process = subprocess.run(
            [sys.executable, str(hook), "--agent-id", host],
            input=json.dumps(payload),
            text=True,
            capture_output=True,
            env=env,
            timeout=10,
        )
        self.assertEqual(process.returncode, 0, process.stderr)
        return json.loads(process.stdout)

    def test_host_owned_replacement_types(self) -> None:
        for host in HOSTS:
            with self.subTest(host=host):
                output = self.replay(host)
                # workbuddy replaces only under a standalone CodeBuddy Code
                # CLI ancestor, which this replay never runs under.
                if host in {
                    "codex",
                    "qwencode",
                    "copilot-shell",
                    "workbuddy",
                    "unknown",
                }:
                    self.assertEqual(output, {})
                    continue
                specific = output["hookSpecificOutput"]
                self.assertNotIn("additionalContext", specific)
                if host == "cosh-ng":
                    self.assertIsInstance(specific["updatedToolResponse"], str)
                    value = json.loads(specific["updatedToolResponse"])
                else:
                    value = specific["updatedToolOutput"]
                if host in {"opencode", "qwenwork"}:
                    self.assertIsInstance(value, str)
                else:
                    self.assertIsInstance(value, dict)
                    self.assertEqual(value["stderr"], "")
                    self.assertEqual(value["exit_code"], 0)

    def test_cosh_replacement_change_is_isolated(self) -> None:
        baseline = {host: self.replay(host) for host in HOSTS}
        adapter = self.adapters / "cosh-ng/hook_adapter.py"
        source = adapter.read_text()
        self.assertIn('"updatedToolResponse": updated', source)
        adapter.write_text(
            source.replace(
                '"updatedToolResponse": updated', '"updatedToolResponse": "cosh-only"'
            )
        )
        for host in HOSTS:
            with self.subTest(host=host):
                output = self.replay(host)
                if host == "cosh-ng":
                    self.assertEqual(
                        output["hookSpecificOutput"]["updatedToolResponse"], "cosh-only"
                    )
                else:
                    self.assertEqual(output, baseline[host])

    def test_other_hosts_do_not_import_cosh_adapter(self) -> None:
        baseline = {host: self.replay(host) for host in HOSTS if host != "cosh-ng"}
        (self.adapters / "cosh-ng/hook_adapter.py").write_text(
            'raise RuntimeError("Cosh adapter unavailable")\n'
        )
        for host, expected in baseline.items():
            with self.subTest(host=host):
                self.assertEqual(self.replay(host), expected)

    def test_detached_extension_loads_its_own_adapter(self) -> None:
        expected = self.replay("cosh-ng")
        extension = self.root / "extension"
        subprocess.run(
            [
                "bash",
                str(ROOT / "packaging/payload.sh"),
                "extension",
                str(self.adapters),
                str(extension),
                "system",
            ],
            check=True,
            capture_output=True,
            text=True,
        )
        shutil.rmtree(self.adapters)
        (extension.parent / "hook_adapter.py").write_text(
            'raise RuntimeError("Loaded an adapter outside the extension")\n'
        )
        hook = extension / "hooks/compress_response_hook.py"
        self.assertEqual(self.replay("cosh-ng", hook), expected)
        self.assertEqual(self.replay("copilot-shell", hook), {})

    def test_missing_adapter_skips_each_compatibility_entrypoint(self) -> None:
        (self.adapters / "cosh-ng/hook_adapter.py").unlink()
        for script in (
            "compress_response_hook.py",
            "rewrite_hook.py",
            "compress_schema_hook.py",
        ):
            with self.subTest(script=script):
                process = subprocess.run(
                    [
                        sys.executable,
                        str(self.adapters / "common/hooks" / script),
                        "--agent-id",
                        "cosh-ng",
                    ],
                    input="{}",
                    text=True,
                    capture_output=True,
                    env=self.env,
                    timeout=10,
                )
                self.assertEqual(process.returncode, 0, process.stderr)
                self.assertEqual(json.loads(process.stdout), {})
                self.assertIn("Hook adapter unavailable", process.stderr)

    def test_before_model_entrypoint_for_every_host(self) -> None:
        for host in HOSTS:
            with self.subTest(host=host):
                process = subprocess.run(
                    [
                        sys.executable,
                        str(self.adapters / "common/hooks/compress_schema_hook.py"),
                        "--agent-id",
                        host,
                    ],
                    input=json.dumps({"llm_request": {"config": {"tools": []}}}),
                    text=True,
                    capture_output=True,
                    env=self.env,
                    timeout=10,
                )
                self.assertEqual(process.returncode, 0, process.stderr)
                self.assertEqual(json.loads(process.stdout), {})


if __name__ == "__main__":
    unittest.main()
