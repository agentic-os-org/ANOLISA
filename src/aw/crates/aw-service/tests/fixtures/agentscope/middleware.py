"""Exercise final-response adoption and failure behavior with the real SDK."""

from __future__ import annotations

import asyncio
import errno
import importlib.util
import io
import json
import logging
import os
import unittest
from contextlib import redirect_stderr
from pathlib import Path
from types import SimpleNamespace
from typing import Any
from unittest.mock import AsyncMock, patch

from agentscope.agent import Agent
from agentscope.message import TextBlock, ToolCallBlock, ToolResultState
from agentscope.tool import ToolChunk, ToolResponse

ROOT = Path(__file__).resolve().parents[5]
SPEC = importlib.util.spec_from_file_location(
    "aw_agentscope", ROOT / "adapters/agentscope/aw_agentscope.py"
)
MODULE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(MODULE)


class MiddlewareTests(unittest.IsolatedAsyncioTestCase):
    async def test_payload_encoding_recursion_follows_policy_and_cleanup(self) -> None:
        original = ToolResponse(content=[TextBlock(text="original")], metadata={"kept": True})
        for event, action, replace in [
            ("tool.before", "report", False),
            ("tool.before", "block", False),
            ("tool.after", "report", False),
            ("tool.after", "report", True),
        ]:
            with self.subTest(event=event, action=action, replace=replace):
                middleware = MODULE.AwToolMiddleware(
                    {"executable": "aw", "binding": "binding"},
                    {
                        "event": event,
                        "step": "encoding",
                        "on_error": action,
                        "budget_ms": 1000,
                        "replace_result": replace,
                    },
                )

                def encode(payload: dict[str, Any], **kwargs: Any) -> str:
                    self.assertEqual(payload["effective_input"], {"command": "echo allowed"})
                    raise RecursionError("fixture payload encoding limit")

                diagnostic = io.StringIO()
                with (
                    patch.object(MODULE.json, "dumps", side_effect=encode) as encoding,
                    patch.object(MODULE.asyncio, "create_subprocess_exec", AsyncMock()) as spawn,
                    redirect_stderr(diagnostic),
                ):
                    result = await self.execute(middleware, original, '{"command":"echo allowed"}')
                self.assertEqual(encoding.call_count, 2)
                spawn.assert_not_awaited()
                self.assertIn("native_input_too_deep", diagnostic.getvalue())
                self.assertIn("event_cleanup_failed", diagnostic.getvalue())
                if event == "tool.before" and action == "block":
                    self.assertEqual(self.executed, [])
                    self.assertEqual(result[-1].state, ToolResultState.DENIED)
                else:
                    self.assertEqual(self.executed, ["call"])
                    self.assertIs(result[0], self.chunk)
                    self.assertIs(result[-1], original)
                with self.assertRaises(LookupError):
                    MODULE._pending_closes.get()

    async def test_deep_directive_follows_failure_policy_and_preserves_native_results(self) -> None:
        stdout = b"[" * 10000 + b"0" + b"]" * 10000
        with self.assertRaises(RecursionError):
            json.loads(stdout)
        original = ToolResponse(content=[TextBlock(text="original")], metadata={"kept": True})
        for event, action, replace in [
            ("tool.before", "report", False),
            ("tool.before", "block", False),
            ("tool.after", "report", False),
            ("tool.after", "report", True),
        ]:
            with self.subTest(event=event, action=action, replace=replace):
                middleware = MODULE.AwToolMiddleware(
                    {},
                    {"event": event, "step": "deep", "on_error": action, "replace_result": replace},
                )
                diagnostic = io.StringIO()
                with (
                    patch.object(middleware, "_invoke", AsyncMock(return_value=(0, stdout, b""))),
                    redirect_stderr(diagnostic),
                ):
                    result = await self.execute(middleware, original)
                self.assertIn("native_output_too_deep", diagnostic.getvalue())
                self.assertIn(f"on_error={action}", diagnostic.getvalue())
                if event == "tool.before" and action == "block":
                    self.assertEqual(self.executed, [])
                    self.assertEqual(result[-1].state, ToolResultState.DENIED)
                else:
                    self.assertEqual(self.executed, ["call"])
                    self.assertIs(result[0], self.chunk)
                    self.assertIs(result[-1], original)

    async def test_recursive_arguments_follow_policy_before_and_after_execution(self) -> None:
        raw = '{"options":' + "[" * 10000 + "0" + "]" * 10000 + "}"
        with self.assertRaises(RecursionError):
            MODULE._json_loads_with_repair(raw)
        original = ToolResponse(content=[TextBlock(text="original")], metadata={"kept": True})
        for event, action, replace in [
            ("tool.before", "report", False),
            ("tool.before", "block", False),
            ("tool.after", "report", False),
            ("tool.after", "report", True),
        ]:
            with self.subTest(event=event, action=action, replace=replace):
                middleware = MODULE.AwToolMiddleware(
                    {},
                    {
                        "event": event,
                        "step": "arguments",
                        "on_error": action,
                        "replace_result": replace,
                    },
                )
                diagnostic = io.StringIO()
                with (
                    patch.object(middleware, "_invoke", AsyncMock()) as invoke,
                    redirect_stderr(diagnostic),
                ):
                    result = await self.execute(middleware, original, raw)
                invoke.assert_not_awaited()
                self.assertIn("native_arguments_invalid", diagnostic.getvalue())
                if event == "tool.before" and action == "block":
                    self.assertEqual(self.executed, [])
                    self.assertEqual(result[-1].state, ToolResultState.DENIED)
                else:
                    self.assertEqual(self.executed, ["call"])
                    self.assertIs(result[0], self.chunk)
                    self.assertIs(result[-1], original)

    async def test_cancelled_before_close_remains_owned_by_invocation(self) -> None:
        middleware = MODULE.AwToolMiddleware(
            {},
            {"event": "tool.before", "step": "last", "on_error": "report"},
            last_before=True,
        )
        with (
            patch.object(
                middleware,
                "_transport",
                side_effect=[(0, b"", b""), asyncio.CancelledError(), (0, b"", b"")],
            ) as transport,
            self.assertRaises(asyncio.CancelledError),
        ):
            await self.execute(middleware, ToolResponse(content=[TextBlock(text="done")]))
        self.assertEqual(self.executed, [])
        self.assertEqual(transport.await_count, 3)
        self.assertEqual(transport.call_args_list[1], transport.call_args_list[2])
        self.assertEqual(transport.call_args.kwargs, {"close_event": True})
        with self.assertRaises(LookupError):
            MODULE._pending_closes.get()

    async def test_cancellation_closes_the_same_correlation_before_propagating(self) -> None:
        for event in ["tool.before", "tool.after"]:
            for failure in [asyncio.CancelledError, TimeoutError]:
                with self.subTest(event=event, failure=failure):
                    child = SimpleNamespace(
                        pid=1234,
                        returncode=None,
                        communicate=AsyncMock(side_effect=failure()),
                        wait=AsyncMock(return_value=-9),
                    )
                    cleanup = SimpleNamespace(
                        returncode=0, communicate=AsyncMock(return_value=(b"", b""))
                    )
                    middleware = MODULE.AwToolMiddleware(
                        {"executable": "aw", "binding": "binding"},
                        {"event": event, "step": "first", "on_error": "report", "budget_ms": 1000},
                    )
                    payload = {
                        "event": event,
                        "invocation_id": "invocation",
                        "session_id": "session",
                    }

                    async def invoke() -> Any:
                        await middleware._invoke(payload)
                        yield  # Keep the SDK's async-generator contract.

                    with (
                        patch.object(
                            MODULE.asyncio,
                            "create_subprocess_exec",
                            side_effect=[child, cleanup],
                        ) as spawn,
                        patch.object(MODULE.os, "killpg") as kill,
                        self.assertRaises(failure),
                    ):
                        async for _ in MODULE.AwInvocationMiddleware().on_acting(None, {}, invoke):
                            self.fail("interrupted callback yielded a result")
                    self.assertEqual(spawn.await_count, 2)
                    self.assertNotIn("--close-event", spawn.call_args_list[0].args)
                    self.assertEqual(spawn.call_args_list[1].args[-2:], ("--close-event", "true"))
                    self.assertEqual(child.communicate.call_args, cleanup.communicate.call_args)
                    self.assertEqual(json.loads(cleanup.communicate.call_args.args[0]), payload)
                    kill.assert_called_once_with(child.pid, MODULE.signal.SIGKILL)
                    child.wait.assert_awaited_once()
                    with self.assertRaises(LookupError):
                        MODULE._pending_closes.get()

    async def test_report_spawn_failure_leaves_later_steps_runnable(self) -> None:
        original = ToolResponse(content=[TextBlock(text="original")])
        replacement = original.model_dump(mode="json")
        replacement["content"][0]["text"] = "compact"
        for event in ["tool.before", "tool.after"]:
            with self.subTest(event=event):
                hooks = [
                    {
                        "event": event,
                        "step": step,
                        "on_error": "report" if step == "broken" else "block",
                        "budget_ms": 1000,
                        "replace_result": step == "healthy" and event == "tool.after",
                    }
                    for step in ["broken", "healthy"]
                ]
                if event == "tool.after":
                    hooks.reverse()
                middlewares = [
                    MODULE.AwToolMiddleware({"executable": "aw", "binding": "binding"}, hook)
                    for hook in hooks
                ]
                healthy = SimpleNamespace(
                    returncode=2 if event == "tool.before" else 0,
                    communicate=AsyncMock(
                        return_value=(json.dumps({"aw_replacement": replacement}).encode(), b"")
                    ),
                )
                cleanup = SimpleNamespace(
                    returncode=0, communicate=AsyncMock(return_value=(b"", b""))
                )
                diagnostic = io.StringIO()
                with (
                    patch.object(
                        MODULE.asyncio,
                        "create_subprocess_exec",
                        side_effect=[
                            OSError(errno.EMFILE, "fixture spawn failure"),
                            healthy,
                            cleanup,
                        ],
                    ) as spawn,
                    patch.object(MODULE.os, "killpg") as kill,
                    redirect_stderr(diagnostic),
                ):
                    result = await self.execute(middlewares, original)
                self.assertEqual(spawn.await_count, 3)
                self.assertTrue(
                    all("--close-event" not in call.args for call in spawn.call_args_list[:2])
                )
                self.assertEqual(spawn.call_args_list[2].args[-2:], ("--close-event", "true"))
                self.assertEqual(healthy.communicate.call_args, cleanup.communicate.call_args)
                kill.assert_not_called()
                self.assertIn("failed: OSError", diagnostic.getvalue())
                if event == "tool.before":
                    self.assertEqual(self.executed, [])
                    self.assertEqual(result[0].state, ToolResultState.DENIED)
                else:
                    self.assertEqual(self.executed, ["call"])
                    self.assertEqual(result[-1].model_dump(mode="json"), replacement)

    async def test_deferred_close_preserves_cancellation_from_the_remaining_chain(self) -> None:
        async def abort(tool_call: Any) -> Any:
            raise asyncio.CancelledError()
            yield  # Keep the SDK's async-generator contract.

        hook = {"event": "tool.before", "on_error": "report", "budget_ms": 1000}
        agent = SimpleNamespace(
            _acting_middlewares=[
                MODULE.AwInvocationMiddleware(),
                *[
                    MODULE.AwToolMiddleware(
                        {"executable": "aw", "binding": "binding"}, {**hook, "step": step}
                    )
                    for step in ["broken", "healthy"]
                ],
            ],
            _acting_impl=abort,
            state=SimpleNamespace(session_id="session"),
        )
        healthy = SimpleNamespace(returncode=0, communicate=AsyncMock(return_value=(b"", b"")))
        cleanup = SimpleNamespace(returncode=0, communicate=AsyncMock(return_value=(b"", b"")))
        with (
            patch.object(
                MODULE.asyncio,
                "create_subprocess_exec",
                side_effect=[OSError(errno.EMFILE, "fixture spawn failure"), healthy, cleanup],
            ) as spawn,
            redirect_stderr(io.StringIO()),
            self.assertRaises(asyncio.CancelledError),
        ):
            async for _ in Agent._acting(agent, ToolCallBlock(id="call", name="tool", input="{}")):
                self.fail("cancelled tool yielded a response")
        self.assertEqual(spawn.await_count, 3)
        self.assertEqual(spawn.call_args_list[2].args[-2:], ("--close-event", "true"))
        self.assertEqual(healthy.communicate.call_args, cleanup.communicate.call_args)

    async def execute(
        self, middleware: Any, response: ToolResponse, raw_input: str = "{}"
    ) -> list[Any]:
        self.executed = []
        self.chunk = ToolChunk(content=[TextBlock(text="partial")], is_last=False)

        async def tool(tool_call: Any) -> Any:
            self.executed.append(tool_call.id)
            yield self.chunk
            yield response

        agent = SimpleNamespace(
            _acting_middlewares=[
                MODULE.AwInvocationMiddleware(),
                *(middleware if isinstance(middleware, list) else [middleware]),
            ],
            _acting_impl=tool,
            state=SimpleNamespace(session_id="session"),
        )
        return [
            item
            async for item in Agent._acting(
                agent, ToolCallBlock(id="call", name="tool", input=raw_input)
            )
        ]

    async def test_callbacks_receive_repaired_arguments_without_mutating_native_input(self) -> None:
        seen = []

        class Probe(MODULE.AwToolMiddleware):
            async def _invoke(self, payload: dict[str, Any]) -> Any:
                seen.append(payload)
                return 0, b"", b""

        raw = '{"command":"echo allowed"'
        middleware = [
            Probe({}, {"event": event, "step": "check", "on_error": "block"})
            for event in ["tool.before", "tool.after"]
        ]
        await self.execute(middleware, ToolResponse(content=[TextBlock(text="done")]), raw)
        self.assertEqual(len(seen), 2)
        self.assertEqual([p["effective_input"] for p in seen], [{"command": "echo allowed"}] * 2)
        self.assertEqual([p["tool_call"]["input"] for p in seen], [raw] * 2)

    async def test_only_final_response_changes_and_preserves_its_native_envelope(self) -> None:
        original = ToolResponse(
            id="native-id", content=[TextBlock(text="original")], metadata={"kept": [None, True]}
        )
        seen = []

        class Probe(MODULE.AwToolMiddleware):
            async def _invoke(self, payload: dict[str, Any]) -> Any:
                seen.append(payload)
                result = json.loads(json.dumps(payload["tool_response"]))
                result["content"][0]["text"] = "compact"
                return 0, json.dumps({"aw_replacement": result}).encode(), b""

        result = await self.execute(
            Probe(
                {},
                {
                    "event": "tool.after",
                    "step": "compress",
                    "on_error": "report",
                    "replace_result": True,
                },
            ),
            original,
        )
        self.assertIs(result[0], self.chunk)
        self.assertEqual(len(seen), 1)
        expected = original.model_dump(mode="json")
        expected["content"][0]["text"] = "compact"
        self.assertEqual(result[1].model_dump(mode="json"), expected)
        self.assertEqual(original.content[0].text, "original")

    async def test_only_the_final_admitted_step_adopts_a_replacement(self) -> None:
        original = ToolResponse(content=[TextBlock(text="original")], metadata={"kept": True})
        seen = []

        class Probe(MODULE.AwToolMiddleware):
            async def _invoke(self, payload: dict[str, Any]) -> Any:
                seen.append((self.hook["step"], payload["tool_response"]))
                response = json.loads(json.dumps(payload["tool_response"]))
                response["content"][0]["text"] = self.hook["step"]
                return 0, json.dumps({"aw_replacement": response}).encode(), b""

        registrations = [
            Probe(
                {},
                {
                    "event": "tool.after",
                    "step": step,
                    "on_error": "report",
                    "replace_result": step == "compact",
                },
            )
            for step in ["compact", "late-observe", "native", "observe"]
        ]
        result = await self.execute(registrations, original)
        self.assertEqual(
            [step for step, _ in seen], ["observe", "native", "late-observe", "compact"]
        )
        self.assertTrue(all(response == original.model_dump(mode="json") for _, response in seen))
        self.assertIs(result[0], self.chunk)
        expected = original.model_dump(mode="json")
        expected["content"][0]["text"] = "compact"
        self.assertEqual(result[-1].model_dump(mode="json"), expected)
        self.assertEqual(original.content[0].text, "original")

    async def test_broken_diagnostic_pipe_cannot_undo_a_policy_denial(self) -> None:
        reason = b'[tokenless:rewrite] Retry this tool with arguments: {"command":"rtk git status"}'

        class Probe(MODULE.AwToolMiddleware):
            async def _invoke(self, payload: dict[str, Any]) -> Any:
                return 2, b"", reason

        for buffered in [False, True]:
            for action in ["report", "block"]:
                with self.subTest(buffered=buffered, on_error=action):
                    reader, writer = os.pipe()
                    os.close(reader)
                    raw = os.fdopen(writer, "wb", buffering=0)
                    # Unbuffered write fails immediately; buffered output fails on flush.
                    stream = io.BufferedWriter(raw) if buffered else raw
                    try:
                        with (
                            patch.object(MODULE.sys, "stderr", SimpleNamespace(buffer=stream)),
                            patch.object(logging.getLogger("aw_agentscope"), "error") as log,
                        ):
                            result = await self.execute(
                                Probe(
                                    {},
                                    {"event": "tool.before", "step": "retry", "on_error": action},
                                ),
                                ToolResponse(content=[TextBlock(text="tool executed")]),
                            )
                        self.assertEqual(self.executed, [])
                        self.assertEqual(len(result), 1)
                        self.assertEqual(result[0].state, ToolResultState.DENIED)
                        self.assertEqual(result[0].content[0].text, reason.decode())
                        log.assert_not_called()
                    finally:
                        try:
                            stream.close()
                        except OSError:
                            pass
                        self.assertTrue(raw.closed)

    async def test_full_diagnostic_pipe_preserves_denial_failure_and_replacement(self) -> None:
        original = ToolResponse(content=[TextBlock(text="original")])
        replacement = original.model_dump(mode="json")
        replacement["content"][0]["text"] = "compact"
        diagnostic = b"native diagnostic\n" * 1024
        reader, writer = os.pipe()
        stream = os.fdopen(writer, "wb", buffering=0)
        try:
            os.set_blocking(writer, False)
            for _ in range(4096):
                try:
                    os.write(writer, b"x" * 4096)
                except BlockingIOError:
                    break
            else:
                self.fail("fixture could not fill its bounded diagnostic pipe")
            os.set_blocking(writer, True)
            for event, code, action in [
                ("tool.before", 2, "report"),
                ("tool.before", 2, "block"),
                ("tool.before", 7, "report"),
                ("tool.before", 7, "block"),
                ("tool.after", 0, "report"),
            ]:
                with self.subTest(event=event, code=code, action=action):

                    class Probe(MODULE.AwToolMiddleware):
                        async def _invoke(self, payload: dict[str, Any]) -> Any:
                            return (
                                code,
                                json.dumps({"aw_replacement": replacement}).encode(),
                                diagnostic,
                            )

                    with patch.object(MODULE.sys, "stderr", SimpleNamespace(buffer=stream)):
                        result = await asyncio.wait_for(
                            self.execute(
                                Probe(
                                    {},
                                    {
                                        "event": event,
                                        "step": "fixture",
                                        "on_error": action,
                                        "replace_result": event == "tool.after",
                                    },
                                ),
                                original,
                            ),
                            timeout=1,
                        )
                    self.assertTrue(os.get_blocking(writer))
                    if code == 2 or (code == 7 and action == "block"):
                        self.assertEqual(self.executed, [])
                        self.assertEqual(result[0].state, ToolResultState.DENIED)
                        if code == 2:
                            self.assertEqual(result[0].content[0].text, diagnostic.decode().strip())
                    else:
                        self.assertEqual(self.executed, ["call"])
                        if event == "tool.after":
                            self.assertEqual(result[-1].model_dump(mode="json"), replacement)
                        else:
                            self.assertIs(result[-1], original)
        finally:
            stream.close()
            os.close(reader)
        self.assertTrue(stream.closed)

    async def test_diagnostic_streams_preserve_callback_outcomes(self) -> None:
        diagnostic = b"native diagnostic: \xe4\xbd\xa0\xe5\xa5\xbd \xff"
        original = ToolResponse(content=[TextBlock(text="original")], metadata={"kept": True})
        replacement = original.model_dump(mode="json")
        replacement["content"][0]["text"] = "compact"

        for kind in ["binary", "text", "closed-text", "closed-binary", "suppressed"]:
            for event, code, action in [
                ("tool.before", 2, "report"),
                ("tool.before", 2, "block"),
                ("tool.before", 0, "report"),
                ("tool.before", 0, "block"),
                ("tool.before", 7, "report"),
                ("tool.before", 7, "block"),
                ("tool.after", 0, "report"),
            ]:
                with self.subTest(stream=kind, event=event, code=code, on_error=action):
                    if kind in {"binary", "closed-binary"}:
                        stream = io.TextIOWrapper(io.BytesIO(), encoding="ascii")
                    elif kind in {"text", "closed-text"}:
                        stream = io.StringIO()
                    else:
                        stream = None
                    if kind.startswith("closed-"):
                        stream.close()

                    class Probe(MODULE.AwToolMiddleware):
                        async def _invoke(self, payload: dict[str, Any]) -> Any:
                            return (
                                code,
                                json.dumps({"aw_replacement": replacement}).encode(),
                                diagnostic,
                            )

                    try:
                        with (
                            redirect_stderr(stream),
                            patch.object(logging.getLogger("aw_agentscope"), "error") as log,
                        ):
                            result = await self.execute(
                                Probe(
                                    {},
                                    {
                                        "event": event,
                                        "step": "fixture",
                                        "on_error": action,
                                        "replace_result": event == "tool.after",
                                    },
                                ),
                                original,
                            )
                        if code == 2 or (code == 7 and action == "block"):
                            self.assertEqual(self.executed, [])
                            self.assertEqual(len(result), 1)
                            self.assertEqual(result[0].state, ToolResultState.DENIED)
                            if code == 2:
                                self.assertEqual(
                                    result[0].content[0].text, diagnostic.decode(errors="replace")
                                )
                        else:
                            self.assertEqual(self.executed, ["call"])
                            self.assertIs(result[0], self.chunk)
                            if event == "tool.after":
                                self.assertEqual(result[-1].model_dump(mode="json"), replacement)
                            else:
                                self.assertIs(result[-1], original)
                        log.assert_not_called()
                        expected = diagnostic
                        if code == 7:
                            expected += (
                                f"AW {event} step fixture failed: native_exit_7; "
                                f"on_error={action}\n"
                            ).encode()
                        if kind == "binary":
                            self.assertEqual(stream.buffer.getvalue(), expected)
                        elif kind == "text":
                            self.assertEqual(stream.getvalue(), expected.decode(errors="replace"))
                    finally:
                        if stream is not None:
                            stream.close()

    async def test_closed_logging_stream_preserves_the_failure_action(self) -> None:
        logger = logging.getLogger("aw_agentscope")
        original = ToolResponse(content=[TextBlock(text="original")])
        for failure in ["native", "transport"]:
            for event, action in [
                ("tool.before", "report"),
                ("tool.before", "block"),
                ("tool.after", "report"),
            ]:
                with self.subTest(failure=failure, event=event, on_error=action):
                    stream = io.StringIO()
                    stream.close()
                    handler = logging.StreamHandler(stream)

                    class Probe(MODULE.AwToolMiddleware):
                        async def _invoke(self, payload: dict[str, Any]) -> Any:
                            if failure == "transport":
                                raise FileNotFoundError("fixture transport failure")
                            return 7, b"", b"native failure"

                    try:
                        with (
                            redirect_stderr(stream),
                            patch.object(logger, "handlers", [handler]),
                            patch.object(logger, "propagate", False),
                            patch.object(logging, "raiseExceptions", True),
                        ):
                            result = await self.execute(
                                Probe(
                                    {},
                                    {"event": event, "step": "fixture", "on_error": action},
                                ),
                                original,
                            )
                        if action == "block":
                            self.assertEqual(self.executed, [])
                            self.assertEqual(result[0].state, ToolResultState.DENIED)
                        else:
                            self.assertEqual(self.executed, ["call"])
                            self.assertIs(result[-1], original)
                    finally:
                        handler.close()

    async def test_malformed_replacement_reports_and_retains_original_response(self) -> None:
        original = ToolResponse(content=[TextBlock(text="original")])
        for value in [None, "invalid", {"state": "running"}, {"content": [{"type": "invalid"}]}]:

            class Probe(MODULE.AwToolMiddleware):
                async def _invoke(self, payload: dict[str, Any]) -> Any:
                    return 0, json.dumps({"aw_replacement": value}).encode(), b""

            diagnostic = io.StringIO()
            with redirect_stderr(diagnostic):
                result = await self.execute(
                    Probe(
                        {},
                        {
                            "event": "tool.after",
                            "step": "compress",
                            "on_error": "report",
                            "replace_result": True,
                        },
                    ),
                    original,
                )
            self.assertIs(result[-1], original)
            self.assertIn("invalid_result_replacement", diagnostic.getvalue())
        for stdout in [b"arbitrary native stdout", b"\xff", b"{}"]:

            class Neutral(MODULE.AwToolMiddleware):
                async def _invoke(self, payload: dict[str, Any]) -> Any:
                    return 0, stdout, b""

            result = await self.execute(
                Neutral({}, {"event": "tool.after", "step": "native", "on_error": "report"}),
                original,
            )
            self.assertIs(result[-1], original)


if __name__ == "__main__":
    unittest.main()
