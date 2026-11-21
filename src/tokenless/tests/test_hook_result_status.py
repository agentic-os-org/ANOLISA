"""Explicit host failures must reach the shared PostTool request as failures."""

import contextlib
import io
import json
import os
import sys
import unittest
from pathlib import Path
from unittest.mock import patch

ADAPTERS = Path(__file__).resolve().parents[1] / "adapters/tokenless"
sys.path[:0] = [str(ADAPTERS / "common/hooks"), str(ADAPTERS)]

import hook_runtime
from hook_dispatch import load_adapter
from result_utils import result_status


class HookResultStatusTests(unittest.TestCase):
    def test_explicit_error_is_preserved_by_host_adapters(self):
        for host in (
            "qoder-cli",
            "qwenwork",
            "opencode",
            "codex",
            "qwencode",
            "unknown",
            "copilot-shell",
            "claude-code",
            "cosh-ng",
        ):
            for status in ("error", "ERROR"):
                with self.subTest(
                    host=host, status=status
                ), contextlib.ExitStack() as stack:
                    adapter = load_adapter(host)
                    if host == "claude-code":
                        stack.enter_context(
                            patch.object(
                                adapter,
                                "_claude_supports_replacement",
                                return_value=False,
                            )
                        )
                        stack.enter_context(contextlib.redirect_stderr(io.StringIO()))
                    if host == "cosh-ng":
                        stack.enter_context(
                            patch.dict(os.environ, {"COSH_NG_VERSION": "0.5.0"})
                        )
                    result = adapter.response(
                        {
                            "tool_name": "Bash",
                            "tool_input": {"command": "build"},
                            "tool_response": "explicit tool failure\n" * 20,
                            "status": status,
                        }
                    )
                    self.assertEqual(result.status, "error")

    def test_explicit_error_does_not_need_a_nonzero_process_code(self):
        self.assertEqual(result_status("error", False, {"exit_code": 0}), "error")

    def test_existing_inference_and_terminal_priorities_are_preserved(self):
        for status, flag, process, expected in (
            ("", False, None, "success"),
            ("success", False, {"exit_code": 0}, "success"),
            ("success", True, None, "error"),
            ("", False, {"exit_code": 1}, "error"),
            ("", False, {"exitCode": 127}, "error"),
            ("", False, {"error": "failure"}, "error"),
            ("interrupted", True, {"exit_code": 1}, "interrupted"),
            ("denied", True, {"exit_code": 1}, "denied"),
        ):
            with self.subTest(status=status, flag=flag, process=process):
                self.assertEqual(result_status(status, flag, process), expected)

    def test_failed_retrieve_is_not_advertised_as_successful_recovery(self):
        adapter = load_adapter("qoder-cli")
        marker = "0123456789abcdef01234567"
        payload = {
            "tool_name": "Bash",
            "tool_input": {"command": f"tokenless retrieve {marker}"},
            "tool_response": "recovery failed\n" * 20,
            "status": "error",
        }
        requests = []

        def core_boundary(binary, request, timeout, operation):
            requests.append(request)
            return None

        with (
            patch.object(sys, "stdin", io.StringIO(json.dumps(payload))),
            contextlib.redirect_stdout(io.StringIO()),
            patch.object(
                hook_runtime, "consume_output_optimization", return_value="none"
            ),
            patch.object(hook_runtime, "resolve_binary", return_value="isolated-core"),
            patch.object(adapter, "recovery_available", return_value=True),
            patch.object(hook_runtime, "run_compress", side_effect=core_boundary),
        ):
            hook_runtime.post_tool(adapter, "qoder-cli")
        self.assertEqual(len(requests), 1)
        operation_input = requests[0]["input"]
        self.assertEqual(operation_input["status"], "error")
        self.assertEqual(operation_input["result_kind"], "tool")
        self.assertEqual(operation_input["capabilities"]["recovery"], {"kind": "none"})
        self.assertEqual(operation_input["content"], payload["tool_response"])


if __name__ == "__main__":
    unittest.main()
