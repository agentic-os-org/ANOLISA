"""Installed-wheel tests for the four Tokenless lifecycle SDK operations."""

from __future__ import annotations

import json
import re
import tempfile
import unittest
from dataclasses import fields
from pathlib import Path
from unittest.mock import patch

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

    async def test_html_extraction_is_opt_in_and_recovers_the_page(self) -> None:
        original = (
            "<!DOCTYPE html><html><head><title>Guide</title></head><body>"
            "<nav><a href='/'>Home</a></nav><main><h1>Guide</h1>"
            + "".join(f"<p>Section {i}: {'explanatory prose ' * 10}</p>" for i in range(8))
            + "</main><footer>footer</footer><script>" + "window.x = 1;" * 40 + "</script>"
            "</body></html>"
        )
        default_sdk = self.sdk(rtk_enabled=False)
        enabled_sdk = self.sdk(rtk_enabled=False, html_extraction_enabled=True)
        for sdk, origin, applied in (
            (default_sdk, ContentOrigin.COMMAND_OUTPUT, False),
            (enabled_sdk, ContentOrigin.COMMAND_OUTPUT, True),
            (enabled_sdk, ContentOrigin.API_RESPONSE, True),
            (enabled_sdk, ContentOrigin.FILE_CONTENT, False),
            (enabled_sdk, ContentOrigin.FILE_READ, False),
        ):
            with self.subTest(enabled=sdk.config.html_extraction_enabled, origin=origin):
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

    def test_packaged_rtk_requires_a_stable_filesystem_resource(self) -> None:
        with patch("anolisa_tokenless.sdk.files") as package_files:
            package_files.return_value.joinpath.return_value = object()
            with self.assertRaisesRegex(RuntimeError, "unpacked wheel"):
                self.sdk()

    async def test_pre_tool_uses_core_rewrite_and_preserves_input(self) -> None:
        sdk = self.sdk()
        original_arguments = {"command": "grep needle file.txt", "other": [1]}
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
        self.assertEqual(original_arguments["command"], "grep needle file.txt")
        self.assertIn(str(sdk._rtk_path), result.arguments["command"])
        self.assertIn("TOKENLESS_AGENT_ID=sdk-agent", result.arguments["command"])
        self.assertIn("TOKENLESS_SESSION_ID=sdk-session", result.arguments["command"])
        self.assertIn("TOKENLESS_TOOL_USE_ID=call-7", result.arguments["command"])
        self.assertIn(
            f"TOKENLESS_DATA_DIR={self.temporary_directory.name}",
            result.arguments["command"],
        )

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
        self.assertFalse(default_sdk.config.html_extraction_enabled)
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

    async def test_tabular_pipeline_preserves_raw_recovery_and_bypasses_retrieved_output(self) -> None:
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
                self.assertEqual(result.applied_operations, (AppliedOperation.TABULAR_ROW_REDUCTION,))
                self.assertEqual(result.content_type.value, "tabular")
                self.assertEqual(result.recoverability.value, "retrievable")
                self.assertIn("Incomplete table", result.output)
                restored = await sdk.retrieve(
                    RetrieveRequest(result.stash_keys[0], frozenset(result.stash_keys), self.attribution)
                )
                self.assertEqual(restored.payload.encode(), original.encode())
                for kind, optimization in (
                    (ResultKind.RETRIEVE, OutputOptimization.NONE),
                    (ResultKind.TOOL, OutputOptimization.RTK),
                ):
                    bypass = await sdk.post_tool(
                        PostToolRequest(
                            result_kind=kind,
                            tool_name="tokenless_retrieve",
                            content=restored.payload,
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
