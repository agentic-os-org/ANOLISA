#!/usr/bin/env python3
"""Lifecycle contract tests for the Hermes adapter."""

import importlib.util
import json
import os
import sys
import unittest
from unittest.mock import patch

_REPO_ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
_PLUGIN_SRC = os.path.join(_REPO_ROOT, "adapters", "tokenless", "hermes", "__init__.py")


def _load_plugin(path: str, name: str):
    """Load the Hermes plugin module under a unique name."""
    sys.modules.pop("hook_utils", None)
    spec = importlib.util.spec_from_file_location(name, path)
    module = importlib.util.module_from_spec(spec)
    pre_path = sys.path[:]
    try:
        spec.loader.exec_module(module)
    finally:
        sys.path[:] = pre_path
    return module


class HermesLifecycleTest(unittest.TestCase):
    """Pin the host-to-Core translation without duplicating Core policy."""

    @classmethod
    def setUpClass(cls):
        cls.plugin = _load_plugin(_PLUGIN_SRC, "hermes_plugin_lifecycle")

    def setUp(self):
        plugin = self.plugin
        self.requests = []
        self.response = None
        self.original_resolve = plugin._resolve_binary
        self.original_run = plugin.run_compress
        self.original_retrieve_available = plugin.tokenless_retrieve_command_available

        def resolve(name, fallback):
            del fallback
            return f"/mock/{name}"

        def run(tokenless_bin, request, timeout, operation):
            self.requests.append((tokenless_bin, request, timeout, operation))
            return self.response

        plugin._resolve_binary = resolve
        plugin.run_compress = run
        plugin.tokenless_retrieve_command_available = lambda: True

    def tearDown(self):
        self.plugin._resolve_binary = self.original_resolve
        self.plugin.run_compress = self.original_run
        self.plugin.tokenless_retrieve_command_available = self.original_retrieve_available

    def test_default_does_not_resolve_binaries_or_block(self):
        with patch.dict(os.environ), patch.object(self.plugin, "_resolve_binary") as resolve:
            os.environ.pop("TOKENLESS_RTK_ENABLED", None)
            self.assertIsNone(self.plugin.on_pre_tool_call(
                tool_name="terminal", args={"command": "git status"}
            ))
        resolve.assert_not_called()
        self.assertEqual(self.requests, [])

    @patch.dict(os.environ, {"TOKENLESS_RTK_ENABLED": "1"})
    def test_pre_tool_blocks_with_core_rewrite(self):
        rewritten = (
            "env TOKENLESS_AGENT_ID=hermes-agent TOKENLESS_SESSION_ID=session-1 "
            "TOKENLESS_TOOL_USE_ID=call-1 TOKENLESS_DATA_DIR=/tmp/tokenless "
            "/mock/rtk git status"
        )
        self.response = {
            "arguments": {"command": rewritten},
            "action": "block_and_suggest",
            "output_optimization": "rtk",
        }

        result = self.plugin.on_pre_tool_call(
            tool_name="terminal",
            args={"command": "git status"},
            session_id="session-1",
            tool_call_id="call-1",
        )

        self.assertEqual(
            result,
            {
                "action": "block",
                "message": f"[tokenless:rewrite] Re-execute as: {rewritten}",
            },
        )
        tokenless_bin, request, timeout, operation = self.requests[0]
        self.assertEqual(tokenless_bin, "/mock/tokenless")
        self.assertEqual(timeout, 8)
        self.assertEqual(operation, "pre_tool")
        self.assertEqual(request["protocol_version"], 2)
        self.assertEqual(request["operation"], "pre_tool")
        self.assertEqual(
            request["attribution"],
            {
                "agent_id": "hermes-agent",
                "session_id": "session-1",
                "tool_use_id": "call-1",
            },
        )
        self.assertEqual(
            request["input"],
            {
                "tool_name": "terminal",
                "arguments": {"command": "git status"},
                "command_field": "command",
                "capabilities": {
                    "replace_arguments": False,
                    "block_and_suggest": True,
                },
            },
        )

    @patch.dict(os.environ, {"TOKENLESS_RTK_ENABLED": "1"})
    def test_pre_tool_fail_open_paths(self):
        cases = [
            None,
            {
                "arguments": {"command": "git status"},
                "action": "passthrough",
                "output_optimization": "none",
            },
            {
                "arguments": [],
                "action": "block_and_suggest",
                "output_optimization": "rtk",
            },
            {
                "arguments": {"command": "git status"},
                "action": "block_and_suggest",
                "output_optimization": "rtk",
            },
        ]
        for response in cases:
            with self.subTest(response=response):
                self.response = response
                self.assertIsNone(
                    self.plugin.on_pre_tool_call(
                        tool_name="terminal", args={"command": "git status"}
                    )
                )

        self.requests.clear()
        self.assertIsNone(
            self.plugin.on_pre_tool_call(tool_name="web_search", args={"query": "tokenless"})
        )
        self.assertEqual(self.requests, [])

    def test_post_tool_applies_core_output(self):
        self.response = {
            "output": "compact",
            "disposition": "applied",
            "recoverability": "lossless",
        }

        result = self.plugin.on_transform_tool_result(
            tool_name="web_search",
            args={"query": "tokenless"},
            result='{"debug":true,"items":[]}',
            session_id="session-1",
            tool_call_id="call-1",
            status="ok",
        )

        self.assertEqual(result, "compact")
        _, request, timeout, operation = self.requests[0]
        self.assertEqual(timeout, 8)
        self.assertEqual(operation, "post_tool")
        self.assertEqual(request["operation"], "post_tool")
        self.assertEqual(request["input"]["status"], "success")
        self.assertEqual(request["input"]["content_origin"], "api_response")
        self.assertEqual(request["input"]["output_optimization"], "none")
        self.assertEqual(
            request["input"]["capabilities"],
            {
                "replace_output": True,
                "recovery": {"kind": "shell"},
                "replace_with_text": True,
            },
        )

    def test_post_tool_disables_recovery_when_marker_command_is_unavailable(self):
        self.plugin.tokenless_retrieve_command_available = lambda: False
        self.response = {"output": "unchanged", "disposition": "passthrough"}

        self.plugin.on_transform_tool_result(
            tool_name="web_search",
            args={"query": "tokenless"},
            result='{"items":[]}',
            status="ok",
        )

        request = self.requests[0][1]["input"]
        self.assertEqual(request["capabilities"]["recovery"]["kind"], "none")

    def test_post_tool_compresses_terminal_output_field(self):
        self.response = {
            "output": "shortened <<tokenless:0123456789abcdef01234567>>",
            "disposition": "applied",
            "recoverability": "retrievable",
        }
        envelope = {
            "output": '{"results":["entry-000","entry-150"]}',
            "exit_code": 0,
            "status": "completed",
        }

        result = self.plugin.on_transform_tool_result(
            tool_name="terminal",
            args={"command": "python3 emit.py"},
            result=json.dumps(envelope),
            status="ok",
        )

        self.assertEqual(self.requests[0][1]["input"]["content"], envelope["output"])
        self.assertEqual(
            json.loads(result),
            {
                "output": "shortened <<tokenless:0123456789abcdef01234567>>",
                "exit_code": 0,
                "status": "completed",
            },
        )

    def test_post_tool_bypasses_successful_retrieve_command(self):
        self.response = {"output": "full payload", "disposition": "passthrough"}
        marker = "<<tokenless:0123456789abcdef01234567>>"

        result = self.plugin.on_transform_tool_result(
            tool_name="terminal",
            args={"command": f"tokenless retrieve '{marker}'"},
            result="full payload",
            status="ok",
        )

        self.assertIsNone(result)
        request = self.requests[0][1]["input"]
        self.assertEqual(request["result_kind"], "retrieve")
        self.assertEqual(request["capabilities"]["recovery"]["kind"], "none")

    def test_post_tool_does_not_misclassify_retrieve_like_commands(self):
        self.response = {"output": "unchanged", "disposition": "passthrough"}
        marker = "<<tokenless:0123456789abcdef01234567>>"

        for command in (
            f"tokenless retrieve '{marker}' | jq .",
            f"tokenless retrieve '{marker}' extra",
            "relative/tokenless retrieve 0123456789abcdef01234567",
        ):
            with self.subTest(command=command):
                self.requests.clear()
                self.plugin.on_transform_tool_result(
                    tool_name="terminal",
                    args={"command": command},
                    result="unchanged",
                    status="ok",
                )
                request = self.requests[0][1]["input"]
                self.assertEqual(request["result_kind"], "tool")
                self.assertEqual(request["capabilities"]["recovery"]["kind"], "shell")

        self.requests.clear()
        self.plugin.on_transform_tool_result(
            tool_name="web_search",
            args={"command": f"tokenless retrieve '{marker}'"},
            result="unchanged",
            status="ok",
        )
        request = self.requests[0][1]["input"]
        self.assertEqual(request["result_kind"], "tool")
        self.assertEqual(request["capabilities"]["recovery"]["kind"], "shell")

    def test_failed_retrieve_command_remains_a_tool_error(self):
        self.response = {"output": "missing", "disposition": "passthrough"}
        marker = "<<tokenless:0123456789abcdef01234567>>"

        self.plugin.on_transform_tool_result(
            tool_name="terminal",
            args={"command": f"tokenless retrieve '{marker}'"},
            result="stash entry not found",
            status="error",
        )

        request = self.requests[0][1]["input"]
        self.assertEqual(request["status"], "error")
        self.assertEqual(request["result_kind"], "tool")
        self.assertEqual(request["capabilities"]["recovery"]["kind"], "none")

    def test_post_tool_maps_host_status_and_content_origin(self):
        self.response = {"output": "unchanged", "disposition": "passthrough"}
        cases = [
            ("read_file", "ok", "file_content", "success"),
            ("terminal", "error", "command_output", "error"),
            ("web_search", "blocked", "api_response", "denied"),
            ("web_search", "interrupted", "api_response", "interrupted"),
        ]

        for tool_name, status, origin, expected_status in cases:
            with self.subTest(tool_name=tool_name, status=status):
                self.requests.clear()
                result = self.plugin.on_transform_tool_result(
                    tool_name=tool_name,
                    args={},
                    result="unchanged",
                    status=status,
                )
                self.assertIsNone(result)
                request = self.requests[0][1]
                self.assertEqual(request["input"]["content_origin"], origin)
                self.assertEqual(request["input"]["status"], expected_status)

    def test_post_tool_reports_plain_shell_file_reads_as_file_read(self):
        self.response = {"output": "unchanged", "disposition": "passthrough"}
        for command, origin in (
            ("cat page.html", "file_read"),
            ("cat page.html | head -c 100", "command_output"),
        ):
            with self.subTest(command=command):
                self.requests.clear()
                self.plugin.on_transform_tool_result(
                    tool_name="terminal",
                    args={"command": command},
                    result="unchanged",
                    status="ok",
                )
                request = self.requests[0][1]
                self.assertEqual(request["input"]["content_origin"], origin)

    def test_post_tool_infers_status_when_host_omits_it(self):
        self.response = {"output": "unchanged", "disposition": "passthrough"}

        for result, expected_status in (
            ('{"items":[]}', "success"),
            ('{"error":"missing command"}', "error"),
            ("plain output", "success"),
        ):
            with self.subTest(result=result):
                self.requests.clear()
                self.assertIsNone(
                    self.plugin.on_transform_tool_result(
                        tool_name="web_search",
                        args={},
                        result=result,
                    )
                )
                request = self.requests[0][1]
                self.assertEqual(request["input"]["status"], expected_status)

    def test_post_tool_marks_only_attributed_rtk_wrapper(self):
        self.response = {"output": "unchanged", "disposition": "passthrough"}
        attributed = (
            "env TOKENLESS_AGENT_ID=hermes-agent TOKENLESS_SESSION_ID=session-1 "
            "TOKENLESS_TOOL_USE_ID=call-1 TOKENLESS_DATA_DIR=/tmp/tokenless "
            "/usr/libexec/anolisa/tokenless/rtk git status"
        )

        self.plugin.on_transform_tool_result(
            tool_name="terminal",
            args={"command": attributed},
            result="rtk output",
            status="ok",
        )
        self.assertEqual(self.requests[-1][1]["input"]["output_optimization"], "rtk")
        self.assertEqual(self.requests[-1][1]["input"]["capabilities"]["recovery"]["kind"], "none")

        self.plugin.on_transform_tool_result(
            tool_name="terminal",
            args={"command": f"echo $({attributed})"},
            result="nested rtk output",
            status="ok",
        )
        self.assertEqual(self.requests[-1][1]["input"]["output_optimization"], "rtk")

        self.plugin.on_transform_tool_result(
            tool_name="terminal",
            args={"command": "/mock/rtk git status"},
            result="manual rtk output",
            status="ok",
        )
        self.assertEqual(self.requests[-1][1]["input"]["output_optimization"], "none")

        self.plugin.on_transform_tool_result(
            tool_name="terminal",
            args={
                "command": (
                    "env TOKENLESS_AGENT_ID=hermes-agent "
                    "TOKENLESS_DATA_DIR=/tmp/tokenless /mock/rtk git status"
                )
            },
            result="partial wrapper output",
            status="ok",
        )
        self.assertEqual(self.requests[-1][1]["input"]["output_optimization"], "none")

    def test_post_tool_appends_core_error_context(self):
        self.response = {
            "output": "command not found",
            "disposition": "tool_error",
            "additional_context": "Install the missing command and retry.",
        }

        result = self.plugin.on_transform_tool_result(
            tool_name="terminal",
            args={"command": "missing-command"},
            result="command not found",
            status="error",
        )

        self.assertEqual(
            result,
            "command not found\n\nInstall the missing command and retry.",
        )

    def test_post_tool_fail_open_paths(self):
        for response in (
            None,
            {"output": 42, "disposition": "applied"},
            {"output": "unchanged", "disposition": "recoverability_unavailable"},
            {"output": "unchanged", "disposition": "no_savings"},
        ):
            with self.subTest(response=response):
                self.response = response
                self.assertIsNone(
                    self.plugin.on_transform_tool_result(
                        tool_name="web_search",
                        args={},
                        result="unchanged",
                        status="ok",
                    )
                )

        self.requests.clear()
        self.assertIsNone(
            self.plugin.on_transform_tool_result(
                tool_name="web_search",
                args={},
                result="unchanged",
                status="future_status",
            )
        )
        self.assertEqual(self.requests, [])


if __name__ == "__main__":
    unittest.main(verbosity=2)
