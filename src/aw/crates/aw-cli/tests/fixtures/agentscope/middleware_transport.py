"""AgentScope transport contracts against the pinned public SDK."""

from __future__ import annotations
import asyncio
import io
import json
import tracemalloc
from contextlib import redirect_stderr
from types import SimpleNamespace
from typing import Any
from unittest.mock import AsyncMock, patch
from agentscope.message import TextBlock, ToolResultState
from agentscope.tool import ToolResponse


from middleware_support import MODULE, MiddlewareFixture


class TransportTests(MiddlewareFixture):
    async def test_oversized_strings_do_not_allocate_a_complete_encoding(self) -> None:
        limit = 1024 * 1024
        middleware = MODULE.AwToolMiddleware({}, {})
        for payload in [{"text": "中" * (2 * limit)}, {"😀" * (2 * limit): None}]:
            with self.subTest(field=next(iter(payload))[:10]):
                with patch.object(MODULE.asyncio, "create_subprocess_exec", AsyncMock()) as spawn:
                    tracemalloc.start()
                    try:
                        with self.assertRaisesRegex(MODULE.CallbackFailure, "native_input_limit"):
                            await middleware._transport(
                                payload, deadline=asyncio.get_running_loop().time() + 3
                            )
                        _, peak = tracemalloc.get_traced_memory()
                    finally:
                        tracemalloc.stop()
                spawn.assert_not_awaited()
                self.assertLess(
                    peak, 4 * limit, "encoding allocated in proportion to rejected input"
                )


    async def test_payload_limit_preserves_exact_boundary_and_json_semantics(self) -> None:
        limit = 1024 * 1024
        overhead = len(json.dumps({"text": ""}).encode())
        payloads = [
            {"z": '中😀\n"', "a": [None, True, False, 3.5, {}]},
            *[{"text": "x" * (limit - overhead + extra)} for extra in [-1, 0, 1]],
            {"text": "😀" * (limit // 12)},
            {"text": "\n" * (limit // 2)},
            {"parts": ["x" * 16384] * 80},
        ]
        middleware = MODULE.AwToolMiddleware(
            {"executable": "aw", "binding": "binding"},
            {"event": "tool.before", "step": "check", "on_error": "report", "budget_ms": 1},
        )
        for payload in payloads:
            expected = json.dumps(payload, ensure_ascii=True, sort_keys=True).encode()
            with self.subTest(size=len(expected)):
                child = SimpleNamespace(
                    returncode=0, communicate=AsyncMock(return_value=(b"", b""))
                )
                with patch.object(
                    MODULE.asyncio, "create_subprocess_exec", AsyncMock(return_value=child)
                ) as spawn:
                    if len(expected) > limit:
                        with self.assertRaisesRegex(MODULE.CallbackFailure, "native_input_limit"):
                            await middleware._transport(
                                payload, deadline=asyncio.get_running_loop().time() + 3
                            )
                        spawn.assert_not_awaited()
                    else:
                        self.assertEqual(
                            await middleware._transport(
                                payload, deadline=asyncio.get_running_loop().time() + 3
                            ),
                            (0, b"", b""),
                        )
                        child.communicate.assert_awaited_once_with(expected)


    async def test_oversized_payload_follows_failure_policy(self) -> None:
        large = "中" * (2 * 1024 * 1024)
        for event, action, replace in [
            ("tool.before", "report", False),
            ("tool.before", "block", False),
            ("tool.after", "report", False),
            ("tool.after", "report", True),
        ]:
            with self.subTest(event=event, action=action, replace=replace):
                original = ToolResponse(
                    content=[TextBlock(text=large if event == "tool.after" else "original")],
                    metadata={"kept": True},
                )
                middleware = MODULE.AwToolMiddleware(
                    {},
                    {
                        "event": event,
                        "step": "check",
                        "on_error": action,
                        "replace_result": replace,
                    },
                )
                diagnostic = io.StringIO()
                with (
                    patch.object(MODULE.asyncio, "create_subprocess_exec", AsyncMock()) as spawn,
                    redirect_stderr(diagnostic),
                ):
                    result = await self.execute(
                        middleware,
                        original,
                        (
                            json.dumps({"text": large}, ensure_ascii=False)
                            if event == "tool.before"
                            else "{}"
                        ),
                    )
                spawn.assert_not_awaited()
                self.assertIn("native_input_limit", diagnostic.getvalue())
                if event == "tool.before" and action == "block":
                    self.assertEqual(self.executed, [])
                    self.assertEqual(result[-1].state, ToolResultState.DENIED)
                else:
                    self.assertEqual(self.executed, ["call"])
                    self.assertIs(result[-1], original)


    async def test_pending_spawn_times_out_through_failure_policy(self) -> None:
        for event, action, replace in [
            ("tool.before", "report", False),
            ("tool.before", "block", False),
            ("tool.after", "report", False),
            ("tool.after", "report", True),
        ]:
            with self.subTest(event=event, action=action, replace=replace):
                stopped = asyncio.Event()
                cleanup = SimpleNamespace(
                    returncode=0, communicate=AsyncMock(return_value=(b"", b""))
                )

                async def pending_spawn(*args: Any, **kwargs: Any) -> Any:
                    if "--close-event" in args:
                        return cleanup
                    try:
                        await asyncio.Event().wait()
                    finally:
                        stopped.set()

                middleware = MODULE.AwToolMiddleware(
                    {"executable": "aw", "binding": "binding"},
                    {
                        "event": event,
                        "step": "check",
                        "on_error": action,
                        "budget_ms": 1,
                        "replace_result": replace,
                    },
                )
                original = ToolResponse(content=[TextBlock(text="original")])
                diagnostic = io.StringIO()
                with (
                    patch.object(
                        MODULE.asyncio, "create_subprocess_exec", side_effect=pending_spawn
                    ),
                    redirect_stderr(diagnostic),
                ):
                    result = await asyncio.wait_for(self.execute(middleware, original), 4)
                self.assertTrue(stopped.is_set())
                self.assertIn("failed: TimeoutError", diagnostic.getvalue())
                cleanup.communicate.assert_awaited_once()
                if event == "tool.before" and action == "block":
                    self.assertEqual(self.executed, [])
                    self.assertEqual(result[-1].state, ToolResultState.DENIED)
                else:
                    self.assertEqual(self.executed, ["call"])
                    self.assertIs(result[-1], original)


    async def test_spawn_and_communication_share_one_deadline_and_reap(self) -> None:
        stopped = asyncio.Event()

        async def communicate(data: bytes) -> Any:
            try:
                await asyncio.Event().wait()
            finally:
                stopped.set()

        child = SimpleNamespace(
            pid=1234, returncode=None, communicate=communicate, wait=AsyncMock(return_value=-9)
        )

        async def delayed_spawn(*args: Any, **kwargs: Any) -> Any:
            await asyncio.sleep(1.5)
            return child

        middleware = MODULE.AwToolMiddleware(
            {"executable": "aw", "binding": "binding"},
            {"event": "tool.before", "step": "check", "on_error": "report", "budget_ms": 1},
        )
        started = asyncio.get_running_loop().time()
        with (
            patch.object(MODULE.asyncio, "create_subprocess_exec", side_effect=delayed_spawn),
            patch.object(MODULE.os, "killpg") as kill,
            self.assertRaises(TimeoutError),
        ):
            await asyncio.wait_for(middleware._transport({}, deadline=started + 3.001), 6)
        self.assertLess(asyncio.get_running_loop().time() - started, 4)
        self.assertTrue(stopped.is_set())
        kill.assert_called_once_with(child.pid, MODULE.signal.SIGKILL)
        child.wait.assert_awaited_once()


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

                encode_raw = json.JSONEncoder().iterencode

                def encode(payload: dict[str, Any], **kwargs: Any) -> Any:
                    if "input" in payload:
                        return encode_raw(payload)
                    self.assertEqual(payload["effective_input"], {"command": "echo allowed"})
                    raise RecursionError("fixture payload encoding limit")

                diagnostic = io.StringIO()
                with (
                    patch.object(
                        MODULE.json.JSONEncoder, "iterencode", side_effect=encode
                    ) as encoding,
                    patch.object(MODULE.asyncio, "create_subprocess_exec", AsyncMock()) as spawn,
                    redirect_stderr(diagnostic),
                ):
                    result = await self.execute(middleware, original, '{"command":"echo allowed"}')
                self.assertEqual(encoding.call_count, 3)
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


    async def test_argument_decoder_limits_follow_policy_before_and_after_execution(self) -> None:
        cases = [
            ('{"options":' + "[" * 10000 + "0" + "]" * 10000 + "}", RecursionError),
            ('{"options":{"value":' + "9" * 10000 + "}}", ValueError),
        ]
        original = ToolResponse(content=[TextBlock(text="original")], metadata={"kept": True})
        for raw, failure in cases:
            with self.assertRaises(failure):
                MODULE._json_loads_with_repair(raw)
            for event, action, replace in [
                ("tool.before", "report", False),
                ("tool.before", "block", False),
                ("tool.after", "report", False),
                ("tool.after", "report", True),
            ]:
                with self.subTest(failure=failure, event=event, action=action, replace=replace):
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


    async def test_out_of_range_integers_follow_failure_policy_without_transport(self) -> None:
        original = ToolResponse(content=[TextBlock(text="original")], metadata={"kept": True})
        for value in [-(2**63) - 1, 2**64 + 1]:
            raw = json.dumps({"options": {"values": [value]}})
            for event, action, replace in [
                ("tool.before", "report", False),
                ("tool.before", "block", False),
                ("tool.after", "report", False),
                ("tool.after", "report", True),
            ]:
                with self.subTest(value=value, event=event, action=action, replace=replace):
                    middleware = MODULE.AwToolMiddleware(
                        {"executable": "aw", "binding": "binding"},
                        {
                            "event": event,
                            "step": "check",
                            "on_error": action,
                            "replace_result": replace,
                            "budget_ms": 1000,
                        },
                    )
                    child = SimpleNamespace(
                        returncode=0, communicate=AsyncMock(return_value=(b"", b""))
                    )
                    diagnostic = io.StringIO()
                    with (
                        patch.object(
                            MODULE.asyncio, "create_subprocess_exec", AsyncMock(return_value=child)
                        ) as spawn,
                        redirect_stderr(diagnostic),
                    ):
                        result = await self.execute(middleware, original, raw)
                    spawn.assert_not_awaited()
                    self.assertIn("native_integer_out_of_range", diagnostic.getvalue())
                    if event == "tool.before" and action == "block":
                        self.assertEqual(self.executed, [])
                        self.assertEqual(result[-1].state, ToolResultState.DENIED)
                    else:
                        self.assertEqual(self.executed, ["call"])
                        self.assertIs(result[-1], original)


    async def test_oversized_raw_arguments_are_rejected_before_repair(self) -> None:
        limit = 1024 * 1024
        original = ToolResponse(content=[TextBlock(text="original")], metadata={"kept": True})
        for raw in [
            json.dumps({"text": "x" * (2 * limit)}),
            json.dumps({"text": "中" * (limit // 6)}, ensure_ascii=False),
            '{"text":"' + "x" * (2 * limit),
        ]:
            for event, action, replace in [
                ("tool.before", "report", False),
                ("tool.before", "block", False),
                ("tool.after", "report", False),
                ("tool.after", "report", True),
            ]:
                with self.subTest(size=len(raw), event=event, action=action, replace=replace):
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
                        patch.object(MODULE, "_json_loads_with_repair", return_value={}) as decode,
                        patch.object(
                            MODULE.asyncio, "create_subprocess_exec", AsyncMock()
                        ) as spawn,
                        redirect_stderr(diagnostic),
                    ):
                        tracemalloc.start()
                        try:
                            result = await self.execute(middleware, original, raw)
                            _, peak = tracemalloc.get_traced_memory()
                        finally:
                            tracemalloc.stop()
                    self.assertEqual(decode.call_count, 0)
                    spawn.assert_not_awaited()
                    self.assertLess(peak, 4 * limit)
                    self.assertIn("native_input_limit", diagnostic.getvalue())
                    if event == "tool.before" and action == "block":
                        self.assertEqual(self.executed, [])
                        self.assertEqual(result[-1].state, ToolResultState.DENIED)
                    else:
                        self.assertEqual(self.executed, ["call"])
                        self.assertIs(result[-1], original)


    async def test_surrogate_code_points_follow_failure_policy_without_transport(self) -> None:
        original = ToolResponse(content=[TextBlock(text="original")], metadata={"kept": True})
        for text in ["\ud800", "\udfff", "\ud83d\ude00", "x" * 4095 + "\ud83d\ude00"]:
            for payload in [{"text": text}, {text: None}, {"nested": [text]}]:
                with self.assertRaisesRegex(MODULE.CallbackFailure, "native_input_invalid_unicode"):
                    MODULE._encode_payload(payload)
            raw = json.dumps({"command": text}, ensure_ascii=False)
            for event, action, replace in [
                ("tool.before", "report", False),
                ("tool.before", "block", False),
                ("tool.after", "report", False),
                ("tool.after", "report", True),
            ]:
                with self.subTest(points=[ord(c) for c in text[-2:]], event=event, action=action):
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
                        patch.object(
                            MODULE.asyncio, "create_subprocess_exec", AsyncMock()
                        ) as spawn,
                        redirect_stderr(diagnostic),
                    ):
                        result = await self.execute(middleware, original, raw)
                    spawn.assert_not_awaited()
                    self.assertIn("native_input_invalid_unicode", diagnostic.getvalue())
                    if event == "tool.before" and action == "block":
                        self.assertEqual(self.executed, [])
                        self.assertEqual(result[-1].state, ToolResultState.DENIED)
                    else:
                        self.assertEqual(self.executed, ["call"])
                        self.assertIs(result[-1], original)


    async def test_phase_transports_share_deadline_before_service_acceptance(self) -> None:
        original = ToolResponse(content=[TextBlock(text="original")])
        for event, last_action in [
            ("tool.before", "report"),
            ("tool.before", "block"),
            ("tool.after", "report"),
        ]:
            with self.subTest(event=event, last_action=last_action):
                first_invocation = None
                children, closes = [], []

                async def spawn(*args: Any, **kwargs: Any) -> Any:
                    if "--close-event" in args:

                        async def close(data: bytes) -> Any:
                            closes.append(json.loads(data)["invocation_id"])
                            return b"", b""

                        return SimpleNamespace(returncode=0, communicate=close)
                    child = SimpleNamespace(pid=1234, returncode=None, wait=AsyncMock())
                    children.append(child)

                    async def communicate(data: bytes) -> Any:
                        nonlocal first_invocation
                        invocation = json.loads(data)["invocation_id"]
                        if first_invocation is None:
                            first_invocation = invocation
                        if invocation == first_invocation:
                            await asyncio.Event().wait()
                        child.returncode = 0
                        return b"", b""

                    child.communicate = communicate
                    return child

                hooks = [
                    MODULE.AwToolMiddleware(
                        {"executable": "aw", "binding": "binding"},
                        {
                            "event": event,
                            "step": str(number),
                            "budget_ms": 1,
                            "on_error": last_action if number == 3 else "report",
                        },
                        last_before=event == "tool.before" and number == 3,
                    )
                    for number in range(4)
                ]
                if event == "tool.after":
                    hooks.reverse()
                with (
                    patch.object(MODULE.asyncio, "create_subprocess_exec", side_effect=spawn),
                    patch.object(MODULE.os, "killpg") as kill,
                    redirect_stderr(io.StringIO()),
                ):
                    result = await asyncio.wait_for(self.execute(hooks, original), 5)
                    self.assertEqual(len(children), 1, "a later hook renewed the phase wait")
                    children[0].wait.assert_awaited_once()
                    kill.assert_called_once_with(1234, MODULE.signal.SIGKILL)
                    self.assertEqual(closes, [first_invocation])
                    if event == "tool.before" and last_action == "block":
                        self.assertEqual(self.executed, [])
                        self.assertEqual(result[-1].state, ToolResultState.DENIED)
                    else:
                        self.assertEqual(self.executed, ["call"])
                        self.assertIs(result[-1], original)
                    # A fresh invocation receives its own budget; cleanup retains
                    # an independent wait even after the phase has expired.
                    result = await asyncio.wait_for(self.execute(hooks, original), 1)
                    self.assertEqual(len(children), 5)
                    self.assertEqual(len(closes), 2)
                    self.assertNotEqual(closes[0], closes[1])
                    self.assertEqual(self.executed, ["call"])
                    self.assertIs(result[-1], original)

