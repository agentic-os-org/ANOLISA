"""Installed-wheel tests for the four Tokenless lifecycle SDK operations."""

from __future__ import annotations

import contextlib
import csv
import io
import json
import os
import re
import subprocess
import sys
import tempfile
import time
import unittest
from dataclasses import fields
from pathlib import Path
from unittest.mock import Mock, patch

from anolisa_tokenless import (
    AppliedOperation,
    Attribution,
    BeforeModelCapabilities,
    BeforeModelRequest,
    ContentOrigin,
    ContentType,
    OutputOptimization,
    PostToolCapabilities,
    PostToolRequest,
    PreToolAction,
    PreToolCapabilities,
    PreToolRequest,
    RecoveryMethod,
    ResultKind,
    RetrieveRequest,
    TokenlessConfig,
    TokenlessError,
    TokenlessSdk,
    ToolResultStatus,
)
from anolisa_tokenless.sdk import _kill_process_tree

SDK_LOGGER = "anolisa_tokenless.sdk"
REQUIRES_POSIX_STUBS = unittest.skipIf(sys.platform == "win32", "uses POSIX shell stubs")


def wait_until_gone(pid: int) -> bool:
    deadline = time.monotonic() + 5
    while time.monotonic() < deadline:
        try:
            os.kill(pid, 0)
        except ProcessLookupError:
            return True
        # A killed descendant stays a zombie until PID 1 reaps it.
        try:
            state = Path(f"/proc/{pid}/stat").read_text().rsplit(")", 1)[1].split()[0]
        except OSError:
            state = ""
        if state == "Z":
            return True
        time.sleep(0.05)
    return False


class TokenlessSdkTests(unittest.IsolatedAsyncioTestCase):
    """Exercise all four lifecycle boundaries against the native runtime."""

    def test_recovery_method_wire_and_validation(self) -> None:
        self.assertEqual(RecoveryMethod().as_dict(), {"kind": "none"})
        self.assertEqual(RecoveryMethod.shell().as_dict(), {"kind": "shell"})
        self.assertEqual(
            RecoveryMethod.tool("tenant-retrieve_2").as_dict(),
            {"kind": "tool", "name": "tenant-retrieve_2"},
        )
        for name in ("", "x" * 65, "tool name", "tool\nname", "工具", "tool;command"):
            with self.subTest(name=name), self.assertRaises(ValueError):
                RecoveryMethod.tool(name)
        for kind, name in (
            ("unknown", None),
            ("shell", "unexpected"),
            ("none", "unexpected"),
            ("tool", None),
        ):
            with self.subTest(kind=kind, name=name), self.assertRaises(ValueError):
                RecoveryMethod(kind, name)

    def setUp(self) -> None:
        self.temporary_directory = tempfile.TemporaryDirectory(prefix="tokenless-sdk-test-")
        self.attribution = Attribution("sdk-test", "session-a")

    def tearDown(self) -> None:
        self.temporary_directory.cleanup()

    def sdk(self, **overrides: object) -> TokenlessSdk:
        return TokenlessSdk(
            TokenlessConfig(
                data_dir=Path(self.temporary_directory.name),
                **overrides,
            )
        )

    def pre_tool_request(self, command: str = "grep needle file.txt") -> PreToolRequest:
        return PreToolRequest(
            tool_name="shell",
            arguments={"command": command, "other": [1]},
            command_field="command",
            capabilities=PreToolCapabilities(replace_arguments=True, block_and_suggest=False),
            attribution=Attribution("sdk-agent", "sdk-session", "call-7"),
        )

    def stub_launcher(self, body: str) -> Path:
        stub = Path(self.temporary_directory.name) / "stub-tokenless"
        stub.write_text(f"#!/bin/sh\n{body}\n", encoding="utf-8")
        stub.chmod(0o755)
        return stub

    def test_record_reduction_operation_uses_the_core_wire_value(self) -> None:
        self.assertEqual(AppliedOperation("tabular_compaction"), AppliedOperation.TABULAR_COMPACTION)
        self.assertEqual(
            AppliedOperation("tabular_row_reduction"), AppliedOperation.TABULAR_ROW_REDUCTION
        )
        self.assertEqual(
            AppliedOperation("json_record_reduction"),
            AppliedOperation.JSON_RECORD_REDUCTION,
        )
        self.assertEqual(
            AppliedOperation("terminal_cleanup"),
            AppliedOperation.TERMINAL_CLEANUP,
        )
        self.assertEqual(
            AppliedOperation("build_log_reduction"),
            AppliedOperation.BUILD_LOG_REDUCTION,
        )

    async def test_before_model_compresses_schema_and_scopes_retrieve(self) -> None:
        sdk = self.sdk(rtk_enabled=False)
        description = "SCHEMA_SENTINEL " + "details " * 500
        tool = {
            "type": "function",
            "function": {
                "name": "lookup",
                "description": description,
                "parameters": {"type": "object", "properties": {}},
            },
        }
        request = BeforeModelRequest(
            tools=(tool, {"type": "web_search"}),
            visible_context="",
            capabilities=BeforeModelCapabilities(
                replace_tools=True,
                recovery=RecoveryMethod.tool("tokenless_retrieve"),
            ),
            attribution=self.attribution,
        )
        result = await sdk.before_model(request)

        self.assertEqual(tool["function"]["description"], description)
        self.assertEqual(result.tools[1], {"type": "web_search"})
        marker = re.search(
            r"If needed, call tool tokenless_retrieve with hash_or_marker=([0-9a-f]{24})",
            result.tools[0]["function"]["description"],
        )
        self.assertIsNotNone(marker)
        assert marker is not None
        self.assertIn(marker.group(1), result.visible_markers)

        recovered = await sdk.retrieve(
            RetrieveRequest(
                marker.group(1).upper(),
                result.visible_markers,
                self.attribution,
            )
        )
        self.assertEqual(recovered.payload, description)
        with self.assertRaisesRegex(TokenlessError, "not authorized"):
            await sdk.retrieve(RetrieveRequest(marker.group(1), frozenset(), self.attribution))

    async def test_html_extraction_is_on_by_default_and_recovers_the_page(self) -> None:
        original = (
            "<!DOCTYPE html><html><head><title>Guide</title></head><body>"
            "<nav><a href='/'>Home</a></nav><main><h1>Guide</h1>"
            + "".join(f"<p>Section {i}: {'explanatory prose ' * 10}</p>" for i in range(8))
            + "</main><footer>footer</footer><script>" + "window.x = 1;" * 40 + "</script>"
            "</body></html>"
        )
        default_sdk = self.sdk(rtk_enabled=False)
        disabled_sdk = self.sdk(rtk_enabled=False, html_extraction_enabled=False)
        for sdk, origin, command, applied in (
            (disabled_sdk, ContentOrigin.COMMAND_OUTPUT, None, False),
            (default_sdk, ContentOrigin.COMMAND_OUTPUT, None, True),
            (default_sdk, ContentOrigin.COMMAND_OUTPUT, "cat page.html", False),
            (default_sdk, ContentOrigin.COMMAND_OUTPUT, "cat page.html | head -c 9000", True),
            (default_sdk, ContentOrigin.API_RESPONSE, "cat page.html", True),
            (default_sdk, ContentOrigin.FILE_CONTENT, None, False),
            (default_sdk, ContentOrigin.FILE_READ, None, False),
        ):
            with self.subTest(
                enabled=sdk.config.html_extraction_enabled, origin=origin, command=command
            ):
                attribution = Attribution("sdk-agent", "sdk-session", "html-1")
                result = await sdk.post_tool(
                    PostToolRequest(
                        result_kind=ResultKind.TOOL,
                        tool_name="Bash",
                        content=original,
                        status=ToolResultStatus.SUCCESS,
                        content_origin=origin,
                        output_optimization=OutputOptimization.NONE,
                        capabilities=PostToolCapabilities(
                            True, RecoveryMethod.tool("tokenless_retrieve"), True
                        ),
                        attribution=attribution,
                        command=command,
                    )
                )
                self.assertEqual(result.content_type, ContentType.HTML)
                if not applied:
                    self.assertEqual(result.applied_operations, ())
                    self.assertEqual(result.output, original)
                    continue
                self.assertEqual(result.applied_operations, (AppliedOperation.HTML_EXTRACTION,))
                self.assertEqual(result.recoverability.value, "retrievable")
                self.assertLess(len(result.output), len(original))
                self.assertIn("Title: Guide\n# Guide\n\nSection 0: explanatory prose", result.output)
                self.assertNotIn("Home", result.output)
                self.assertEqual(len(result.stash_keys), 1)
                retrieved = await sdk.retrieve(
                    RetrieveRequest(result.stash_keys[0], frozenset(result.stash_keys), attribution)
                )
                self.assertEqual(retrieved.payload, original)

    def test_config_contains_runtime_resources_and_search_control(self) -> None:
        with self.assertRaisesRegex(ValueError, "absolute path"):
            TokenlessConfig(data_dir="relative")
        self.assertEqual(
            {field.name for field in fields(TokenlessConfig)},
            {
                "data_dir",
                "retrieve_tool_name",
                "rtk_enabled",
                "search_path_sharing_enabled",
                "diff_compression_enabled",
                "mixed_content_enabled",
                "html_extraction_enabled",
            },
        )

    def test_config_identifies_invalid_retrieve_tool_names(self) -> None:
        for name in ("", "x" * 65, "tool.name", "tool:name", "tool name", "工具"):
            with self.subTest(name=name), self.assertRaisesRegex(
                ValueError, r"^retrieve_tool_name: recovery tool name"
            ):
                TokenlessConfig(retrieve_tool_name=name)
        self.assertEqual(
            TokenlessConfig(retrieve_tool_name="tenant-retrieve_2").retrieve_tool_name,
            "tenant-retrieve_2",
        )

    def test_packaged_launcher_requires_a_stable_filesystem_resource(self) -> None:
        with patch("anolisa_tokenless.sdk.files") as package_files:
            package_files.return_value.joinpath.return_value = object()
            with self.assertRaisesRegex(RuntimeError, "unpacked wheel"):
                self.sdk()

    def test_packaged_launcher_uses_the_platform_executable_name(self) -> None:
        package = Path(self.temporary_directory.name) / "package"
        (package / "_bin").mkdir(parents=True)
        for platform, name in (
            ("win32", "tokenless.exe"),
            ("linux", "tokenless"),
            ("darwin", "tokenless"),
        ):
            with self.subTest(platform=platform):
                executable = package / "_bin" / name
                executable.touch(mode=0o755)
                with patch("sys.platform", platform), patch(
                    "anolisa_tokenless.sdk.files", return_value=package
                ):
                    self.assertEqual(self.sdk()._rtk_launcher, executable)
                executable.unlink()

    async def test_pre_tool_uses_core_rewrite_and_preserves_input(self) -> None:
        sdk = self.sdk()
        original_arguments = {"command": "git status", "other": [1]}
        result = await sdk.pre_tool(
            PreToolRequest(
                tool_name="shell",
                arguments=original_arguments,
                command_field="command",
                capabilities=PreToolCapabilities(
                    replace_arguments=True,
                    block_and_suggest=False,
                ),
                attribution=Attribution("sdk-agent", "sdk-session", "call-7"),
            )
        )
        self.assertEqual(result.action, PreToolAction.REPLACE_ARGUMENTS)
        self.assertEqual(result.output_optimization, OutputOptimization.RTK)
        self.assertEqual(original_arguments["command"], "git status")
        # The CLI names itself by its resolved path.
        launcher = os.path.realpath(sdk._rtk_launcher)
        self.assertIn(f"{launcher} rtk git status", result.arguments["command"])
        self.assertIn("TOKENLESS_AGENT_ID=sdk-agent", result.arguments["command"])
        self.assertIn("TOKENLESS_SESSION_ID=sdk-session", result.arguments["command"])
        self.assertIn("TOKENLESS_TOOL_USE_ID=call-7", result.arguments["command"])
        self.assertIn(
            f"TOKENLESS_DATA_DIR={self.temporary_directory.name}",
            result.arguments["command"],
        )

    async def test_pre_tool_leaves_search_commands_for_post_tool(self) -> None:
        sdk = self.sdk()
        for command in ("grep needle file.txt", "rg 'foo|bar' src"):
            with self.subTest(command=command):
                request = self.pre_tool_request(command)
                result = await sdk.pre_tool(request)
                self.assertEqual(result.action, PreToolAction.PASSTHROUGH)
                self.assertEqual(result.output_optimization, OutputOptimization.NONE)
                self.assertEqual(request.arguments, {"command": command, "other": [1]})
                self.assertEqual(result.arguments, request.arguments)

    async def test_pre_tool_leaves_build_log_commands_for_post_tool(self) -> None:
        sdk = self.sdk()
        original_arguments = {"command": "cargo test --workspace", "timeout": 120}
        result = await sdk.pre_tool(
            PreToolRequest(
                tool_name="shell",
                arguments=original_arguments,
                command_field="command",
                capabilities=PreToolCapabilities(
                    replace_arguments=True,
                    block_and_suggest=False,
                ),
                attribution=Attribution("sdk-agent", "sdk-session", "call-build"),
            )
        )
        self.assertEqual(result.action, PreToolAction.PASSTHROUGH)
        self.assertEqual(result.output_optimization, OutputOptimization.NONE)
        self.assertEqual(result.arguments, original_arguments)

    @REQUIRES_POSIX_STUBS
    async def test_pre_tool_timeout_kills_the_process_tree_and_passes_through(self) -> None:
        sdk = self.sdk()
        pids = Path(self.temporary_directory.name) / "pids"
        # The background sleep keeps stdout open, as a stalled `git` would.
        sdk._rtk_launcher = self.stub_launcher(f'sleep 60 &\necho "$! $$" > "{pids}"\nwait')
        request = self.pre_tool_request()
        started = time.monotonic()
        with (
            patch("anolisa_tokenless.sdk._PRE_TOOL_TIMEOUT_SECONDS", 1.0),
            self.assertLogs(SDK_LOGGER, "WARNING") as logs,
        ):
            result = await sdk.pre_tool(request)
        self.assertLess(time.monotonic() - started, 5)
        self.assertIn("exceeded", logs.output[0])
        self.assertEqual(result.action, PreToolAction.PASSTHROUGH)
        self.assertEqual(result.output_optimization, OutputOptimization.NONE)
        self.assertEqual(result.arguments, request.arguments)
        for pid in pids.read_text(encoding="utf-8").split():
            self.assertTrue(wait_until_gone(int(pid)), pid)

    @REQUIRES_POSIX_STUBS
    async def test_pre_tool_failures_pass_through_and_warn_once(self) -> None:
        missing = Path(self.temporary_directory.name) / "missing"
        action = 'result":{"arguments":{},"action":"unknown","output_optimization":"none"}'
        for name, body, detail in (
            ("missing launcher", None, "cannot start"),
            ("exit status", "echo boom >&2\nexit 1", "boom"),
            ("killed", "kill -9 $$", "exited with -9"),
            ("not JSON", "cat >/dev/null\necho not-json", "malformed"),
            ("no result", "cat >/dev/null\necho '{}'", "malformed"),
            ("unknown action", f"cat >/dev/null\necho '{{\"{action}}}'", "malformed"),
        ):
            with self.subTest(name):
                sdk = self.sdk()
                sdk._rtk_launcher = missing if body is None else self.stub_launcher(body)
                request = self.pre_tool_request()
                with self.assertLogs(SDK_LOGGER, "WARNING") as logs:
                    result = await sdk.pre_tool(request)
                self.assertIn(detail, logs.output[0])
                self.assertEqual(result.action, PreToolAction.PASSTHROUGH)
                self.assertEqual(result.output_optimization, OutputOptimization.NONE)
                self.assertEqual(result.arguments, request.arguments)
                self.assertIsNot(result.arguments, request.arguments)
                with self.assertNoLogs(SDK_LOGGER, "WARNING"):
                    await sdk.pre_tool(request)

    async def test_pre_tool_raises_for_a_request_the_cli_cannot_decode(self) -> None:
        request = PreToolRequest(
            tool_name=1,  # type: ignore[arg-type]
            arguments={"command": "grep needle file.txt"},
            command_field="command",
            capabilities=PreToolCapabilities(replace_arguments=True, block_and_suggest=False),
            attribution=Attribution("sdk-agent", "sdk-session", "call-7"),
        )
        with self.assertRaisesRegex(ValueError, "invalid PreTool request"):
            await self.sdk().pre_tool(request)

    async def test_pre_tool_applies_a_newly_trusted_filter_on_the_next_call(self) -> None:
        root = Path(self.temporary_directory.name)
        project = root / "project"
        (project / ".rtk").mkdir(parents=True)
        (project / ".rtk" / "filters.toml").write_text(
            'schema_version = 1\n[filters.review]\nmatch_command = "^reviewtool "\n'
            "max_lines = 5\n",
            encoding="utf-8",
        )
        sdk = self.sdk()
        request = self.pre_tool_request("reviewtool list")
        with (
            contextlib.chdir(project),
            patch.dict(
                os.environ,
                {
                    "HOME": str(root / "home"),
                    "XDG_CONFIG_HOME": str(root / "config"),
                    "XDG_DATA_HOME": str(root / "data"),
                },
            ),
        ):
            untrusted = await sdk.pre_tool(request)
            subprocess.run(
                [sdk._rtk_launcher, "rtk", "trust", "--yes"], check=True, capture_output=True
            )
            trusted = await sdk.pre_tool(request)
        self.assertEqual(untrusted.action, PreToolAction.PASSTHROUGH)
        self.assertEqual(trusted.action, PreToolAction.REPLACE_ARGUMENTS)
        self.assertIn("rtk reviewtool list", trusted.arguments["command"])

    @REQUIRES_POSIX_STUBS
    async def test_pre_tool_keeps_child_diagnostics_off_the_host_stderr(self) -> None:
        response = '{"result":{"arguments":{"command":"x"},"action":"passthrough",'
        response += '"output_optimization":"none"}}'
        sdk = self.sdk()
        sdk._rtk_launcher = self.stub_launcher(
            f"cat >/dev/null\necho '[rtk] noise' >&2\necho '{response}'"
        )
        with tempfile.TemporaryFile() as captured:
            host_stderr = os.dup(2)
            os.dup2(captured.fileno(), 2)
            try:
                result = await sdk.pre_tool(self.pre_tool_request())
            finally:
                os.dup2(host_stderr, 2)
                os.close(host_stderr)
            captured.seek(0)
            self.assertEqual(captured.read(), b"")
        self.assertEqual(result.arguments, {"command": "x"})

    def test_timeout_kills_the_windows_tree_with_taskkill(self) -> None:
        process = Mock(pid=42)
        with (
            patch("sys.platform", "win32"),
            patch("subprocess.CREATE_NO_WINDOW", 0x08000000, create=True),
            patch.dict(os.environ, {"SystemRoot": "C:\\Windows"}),
            patch("subprocess.run") as run,
        ):
            _kill_process_tree(process)
        command = run.call_args.args[0]
        self.assertTrue(command[0].endswith("taskkill.exe"), command)
        self.assertEqual(command[1:], ["/T", "/F", "/PID", "42"])
        process.kill.assert_not_called()
        process.wait.assert_called_once()
        # Reader threads may still hold the pipes on Windows.
        process.stdout.close.assert_not_called()

    def test_timeout_falls_back_to_the_child_when_the_tree_kill_fails(self) -> None:
        for platform, kill, patched in (
            ("win32", "subprocess.run", subprocess.TimeoutExpired("taskkill", 1)),
            ("linux", "os.killpg", PermissionError()),
        ):
            with self.subTest(platform):
                process = Mock(pid=42)
                process.wait.side_effect = subprocess.TimeoutExpired("tokenless", 1)
                with (
                    patch("sys.platform", platform),
                    patch("subprocess.CREATE_NO_WINDOW", 0x08000000, create=True),
                    patch(kill, side_effect=patched),
                ):
                    _kill_process_tree(process)
                process.kill.assert_called_once()

    def test_post_tool_tool_kind_requires_call_identity_for_wire_strings(self) -> None:
        with self.assertRaisesRegex(ValueError, "tool_use_id"):
            PostToolRequest(
                result_kind="tool",  # type: ignore[arg-type]
                tool_name="api",
                content="result",
                status=ToolResultStatus.SUCCESS,
                content_origin=ContentOrigin.API_RESPONSE,
                output_optimization=OutputOptimization.NONE,
                capabilities=PostToolCapabilities(True, RecoveryMethod.shell(), True),
                attribution=self.attribution,
            )

    async def test_post_tool_routes_rtk_error_and_retrieve_in_core(self) -> None:
        sdk = self.sdk(rtk_enabled=False)
        capabilities = PostToolCapabilities(
            replace_output=True,
            recovery=RecoveryMethod.tool("tokenless_retrieve"),
            replace_with_text=True,
        )
        attribution = Attribution("sdk-agent", "sdk-session", "call-8")

        optimized_content = json.dumps({"items": list(range(100))})
        optimized = await sdk.post_tool(
            PostToolRequest(
                result_kind=ResultKind.TOOL,
                tool_name="shell",
                content=optimized_content,
                status=ToolResultStatus.SUCCESS,
                content_origin=ContentOrigin.COMMAND_OUTPUT,
                output_optimization=OutputOptimization.RTK,
                capabilities=capabilities,
                attribution=attribution,
            )
        )
        self.assertEqual(optimized.output, optimized_content)
        self.assertEqual(optimized.applied_operations, ())

        error = await sdk.post_tool(
            PostToolRequest(
                result_kind=ResultKind.TOOL,
                tool_name="shell",
                content="/bin/sh: jq: command not found",
                status=ToolResultStatus.ERROR,
                content_origin=ContentOrigin.COMMAND_OUTPUT,
                output_optimization=OutputOptimization.NONE,
                capabilities=capabilities,
                attribution=attribution,
            )
        )
        self.assertIn("ENV_DEPENDENCY_MISSING", error.additional_context or "")

        retrieved = await sdk.post_tool(
            PostToolRequest(
                result_kind=ResultKind.RETRIEVE,
                tool_name="tokenless_retrieve",
                content="restored payload",
                status=ToolResultStatus.SUCCESS,
                content_origin=ContentOrigin.API_RESPONSE,
                output_optimization=OutputOptimization.NONE,
                capabilities=capabilities,
                attribution=self.attribution,
            )
        )
        self.assertEqual(retrieved.output, "restored payload")
        self.assertEqual(retrieved.applied_operations, ())

    async def test_post_tool_uses_core_json_pipeline(self) -> None:
        sdk = self.sdk(rtk_enabled=False)
        records = [{"name": "same", "value": index, "message": "x" * 80} for index in range(300)]
        original = json.dumps(
            {"items": records},
            separators=(",", ":"),
        )
        result = await sdk.post_tool(
            PostToolRequest(
                result_kind=ResultKind.TOOL,
                tool_name="api",
                content=original,
                status=ToolResultStatus.SUCCESS,
                content_origin=ContentOrigin.API_RESPONSE,
                output_optimization=OutputOptimization.NONE,
                capabilities=PostToolCapabilities(
                    replace_output=True,
                    recovery=RecoveryMethod.tool("tokenless_retrieve"),
                    replace_with_text=False,
                ),
                attribution=Attribution("sdk-agent", "sdk-session", "call-9"),
            )
        )
        self.assertLess(len(result.output.encode()), len(original.encode()))
        self.assertIn(
            AppliedOperation.JSON_RECORD_REDUCTION,
            result.applied_operations,
        )
        self.assertEqual(result.recoverability.value, "retrievable")
        self.assertEqual(len(result.stash_keys), 1)

        restored = await sdk.retrieve(
            RetrieveRequest(
                result.stash_keys[0],
                frozenset(result.stash_keys),
                Attribution("sdk-agent", "sdk-session", "call-9"),
            )
        )
        self.assertEqual(json.loads(restored.payload), records)

    async def test_search_path_sharing_exposes_the_typed_operation(self) -> None:
        sdk = self.sdk(rtk_enabled=False)
        self.assertTrue(sdk.config.search_path_sharing_enabled)
        original = "crates/long_directory/src/file.rs:42:  matching text  \r\n" * 12
        result = await sdk.post_tool(
            PostToolRequest(
                result_kind=ResultKind.TOOL,
                tool_name="Grep",
                content=original,
                status=ToolResultStatus.SUCCESS,
                content_origin=ContentOrigin.API_RESPONSE,
                output_optimization=OutputOptimization.NONE,
                capabilities=PostToolCapabilities(
                    replace_output=True, recovery=RecoveryMethod(), replace_with_text=True
                ),
                attribution=Attribution("sdk-agent", "sdk-session", "search-1"),
            )
        )
        self.assertEqual(result.applied_operations, (AppliedOperation.SEARCH_PATH_SHARING,))
        self.assertEqual(result.content_type.value, "search_results")
        self.assertEqual(result.recoverability.value, "lossless")
        self.assertEqual(result.stash_keys, ())
        self.assertEqual(result.output.count("42:  matching text  \r\n"), 12)
        self.assertLess(len(result.output), len(original))

    async def test_search_path_sharing_can_be_disabled(self) -> None:
        sdk = self.sdk(rtk_enabled=False, search_path_sharing_enabled=False)
        original = "crates/long_directory/src/file.rs:42:matching text\n" * 12
        result = await sdk.post_tool(
            PostToolRequest(
                result_kind=ResultKind.TOOL,
                tool_name="SearchFiles",
                content=original,
                status=ToolResultStatus.SUCCESS,
                content_origin=ContentOrigin.API_RESPONSE,
                output_optimization=OutputOptimization.NONE,
                capabilities=PostToolCapabilities(
                    replace_output=True, recovery=RecoveryMethod(), replace_with_text=True
                ),
                attribution=Attribution("sdk-agent", "sdk-session", "search-disabled"),
            )
        )
        self.assertFalse(sdk.config.search_path_sharing_enabled)
        self.assertEqual(result.output, original)
        self.assertEqual(result.applied_operations, ())
        self.assertEqual(result.stash_keys, ())

    async def test_diff_opt_in_recovers_original_and_respects_rtk_ownership(self) -> None:
        original = (
            "diff --git a/f b/f\nindex 123..456 100644\n--- a/f\n+++ b/f\n"
            "@@ -1,12 +1,12 @@\n"
            + (" " + "context" * 20 + "\n") * 10
            + "-old\n+new\n tail\n"
        )
        default_sdk = self.sdk(rtk_enabled=False)
        self.assertFalse(default_sdk.config.diff_compression_enabled)
        self.assertTrue(default_sdk.config.html_extraction_enabled)
        enabled_sdk = self.sdk(rtk_enabled=False, diff_compression_enabled=True)
        for sdk, optimization, applied in (
            (default_sdk, OutputOptimization.NONE, False),
            (enabled_sdk, OutputOptimization.NONE, True),
            (enabled_sdk, OutputOptimization.RTK, False),
        ):
            with self.subTest(enabled=sdk.config.diff_compression_enabled, optimization=optimization):
                attribution = Attribution("sdk-agent", "sdk-session", "diff-1")
                result = await sdk.post_tool(
                    PostToolRequest(
                        result_kind=ResultKind.TOOL,
                        tool_name="Bash",
                        content=original,
                        status=ToolResultStatus.SUCCESS,
                        content_origin=ContentOrigin.COMMAND_OUTPUT,
                        output_optimization=optimization,
                        capabilities=PostToolCapabilities(
                            True, RecoveryMethod.tool("tokenless_retrieve"), True
                        ),
                        attribution=attribution,
                    )
                )
                if applied:
                    self.assertEqual(result.applied_operations, (AppliedOperation.DIFF_REDUCTION,))
                    self.assertEqual(result.recoverability.value, "retrievable")
                    self.assertLess(len(result.output), len(original))
                    self.assertIn("-old\n+new\n tail\n", result.output)
                    self.assertEqual(len(result.stash_keys), 1)
                    recovered = await sdk.retrieve(
                        RetrieveRequest(result.stash_keys[0], frozenset(result.stash_keys), attribution)
                    )
                    self.assertEqual(recovered.payload, original)
                else:
                    self.assertEqual(result.output, original)
                    self.assertEqual(result.applied_operations, ())
                    self.assertEqual(result.stash_keys, ())

    def test_stats_client_is_lazy_and_uses_runtime_data_dir(self) -> None:
        sdk = self.sdk(rtk_enabled=False)
        self.assertIsNone(sdk._stats)

        stats = sdk.stats
        self.assertIs(stats, sdk.stats)
        self.assertEqual(stats.status.data_dir, sdk.runtime.data_dir)

    async def test_tabular_pipeline_preserves_all_cells_without_recovery(self) -> None:
        sdk = self.sdk(rtk_enabled=False)
        for delimiter in (",", "\t"):
            with self.subTest(delimiter=delimiter):
                original = f"id{delimiter}message\r\n" + "".join(
                    f"{index:04}{delimiter}record-{index}-{'payload ' * 12}\r\n"
                    for index in range(100)
                )
                capabilities = PostToolCapabilities(
                    replace_output=True,
                    recovery=RecoveryMethod.tool("tokenless_retrieve"),
                    replace_with_text=True,
                )
                result = await sdk.post_tool(
                    PostToolRequest(
                        result_kind=ResultKind.TOOL,
                        tool_name="table_query",
                        content=original,
                        status=ToolResultStatus.SUCCESS,
                        content_origin=ContentOrigin.API_RESPONSE,
                        output_optimization=OutputOptimization.NONE,
                        capabilities=capabilities,
                        attribution=Attribution("sdk-agent", "table-session", "table-call"),
                    )
                )
                self.assertEqual(result.applied_operations, (AppliedOperation.TABULAR_COMPACTION,))
                self.assertEqual(result.content_type.value, "tabular")
                self.assertEqual(result.recoverability.value, "lossless")
                self.assertEqual(result.stash_keys, ())
                self.assertNotIn("Incomplete table", result.output)
                self.assertEqual(
                    list(csv.reader(io.StringIO(result.output), delimiter=delimiter)),
                    list(csv.reader(io.StringIO(original), delimiter=delimiter)),
                )
                for kind, optimization in (
                    (ResultKind.RETRIEVE, OutputOptimization.NONE),
                    (ResultKind.TOOL, OutputOptimization.RTK),
                ):
                    bypass = await sdk.post_tool(
                        PostToolRequest(
                            result_kind=kind,
                            tool_name="tokenless_retrieve",
                            content=original,
                            status=ToolResultStatus.SUCCESS,
                            content_origin=ContentOrigin.API_RESPONSE,
                            output_optimization=optimization,
                            capabilities=capabilities,
                            attribution=Attribution("sdk-agent", "table-session", "table-bypass"),
                        )
                    )
                    self.assertEqual(bypass.output, original)
                    self.assertEqual(bypass.applied_operations, ())


if __name__ == "__main__":
    unittest.main()
