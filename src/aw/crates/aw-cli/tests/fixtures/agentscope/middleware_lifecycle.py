"""AgentScope lifecycle contracts against the pinned public SDK."""

from __future__ import annotations
import asyncio
import errno
import io
import json
from contextlib import redirect_stderr
from types import SimpleNamespace
from typing import Any
from unittest.mock import AsyncMock, patch
from agentscope.agent import Agent
from agentscope.message import TextBlock, ToolCallBlock, ToolResultState
from agentscope.tool import ToolResponse


from middleware_support import MODULE, MiddlewareFixture


class LifecycleTests(MiddlewareFixture):
    async def test_finalization_drains_every_phase_before_repropagating_cancellation(self) -> None:
        original = ToolResponse(content=[TextBlock(text="original")])
        for phase in ["tool.before", "tool.after"]:
            for cancellations in [1, 2]:
                with self.subTest(phase=phase, cancellations=cancellations):
                    entered, release = asyncio.Event(), asyncio.Event()
                    closed = []

                    class Probe(MODULE.AwToolMiddleware):
                        async def _transport(
                            self, payload: Any, *, deadline: float, close_event: bool = False
                        ) -> Any:
                            if close_event:
                                if self.hook["event"] == phase:
                                    entered.set()
                                    await release.wait()
                                closed.append(self.hook["event"])
                            return 0, b"", b""

                    middleware = [
                        Probe({}, {"event": event, "step": "observe", "on_error": "report"})
                        for event in ["tool.after", "tool.before"]
                    ]
                    task = asyncio.create_task(self.execute(middleware, original))
                    try:
                        await asyncio.wait_for(entered.wait(), 1)
                        for number in range(cancellations):
                            task.cancel("finalization interrupted " + str(number))
                            await asyncio.sleep(0)
                        self.assertFalse(task.done(), "cancellation abandoned phase cleanup")
                        release.set()
                        with self.assertRaises(asyncio.CancelledError) as interrupted:
                            await asyncio.wait_for(task, 1)
                        self.assertEqual(
                            interrupted.exception.args, ("finalization interrupted 0",)
                        )
                        self.assertEqual(closed, ["tool.before", "tool.after"])
                        self.assertEqual(self.executed, ["call"])
                    finally:
                        release.set()
                        if not task.done():
                            task.cancel()
                        await asyncio.wait_for(asyncio.gather(task, return_exceptions=True), 1)


    async def test_close_deadline_includes_pending_subprocess_creation(self) -> None:
        stopped = asyncio.Event()

        async def pending_spawn(*args: Any, **kwargs: Any) -> Any:
            try:
                await asyncio.Event().wait()
            finally:
                stopped.set()

        middleware = MODULE.AwToolMiddleware(
            {"executable": "aw", "binding": "binding"},
            {"event": "tool.after", "step": "close", "on_error": "report", "budget_ms": 1000},
        )
        diagnostic = io.StringIO()
        with (
            patch.object(MODULE.asyncio, "create_subprocess_exec", side_effect=pending_spawn),
            redirect_stderr(diagnostic),
        ):
            await asyncio.wait_for(middleware._close_event({}), 4)
        self.assertTrue(stopped.is_set())
        self.assertIn("event_cleanup_failed", diagnostic.getvalue())


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
        self.assertEqual(transport.call_args_list[1].args, transport.call_args_list[2].args)
        for call in transport.call_args_list[1:]:
            self.assertEqual(set(call.kwargs), {"deadline", "close_event"})
            self.assertTrue(call.kwargs["close_event"])
        self.assertGreaterEqual(
            transport.call_args_list[2].kwargs["deadline"],
            transport.call_args_list[1].kwargs["deadline"],
        )
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

