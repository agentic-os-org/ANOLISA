"""AgentScope task-scope ownership and cleanup contracts."""

from __future__ import annotations

import asyncio
import os
from types import SimpleNamespace
from typing import Any
from unittest.mock import AsyncMock, patch

from agentscope.agent import Agent
from agentscope.message import TextBlock, ToolCallBlock
from agentscope.tool import ToolResponse

import aw_scope_ledger as LEDGER
from middleware_support import MODULE, MiddlewareFixture


class ScopeTests(MiddlewareFixture):
    def setUp(self) -> None:
        super().setUp()
        self.globals = (
            LEDGER._task_scope,
            LEDGER._task_scope_pid,
            LEDGER._task_scope_lock,
            LEDGER._task_scope_lock_pid,
            set(LEDGER._unclosed_scopes),
            LEDGER._atexit_registered,
        )
        LEDGER._task_scope = None
        LEDGER._task_scope_pid = None
        LEDGER._task_scope_lock = None
        LEDGER._task_scope_lock_pid = None
        LEDGER._unclosed_scopes.clear()
        LEDGER._atexit_registered = True

    def tearDown(self) -> None:
        (
            LEDGER._task_scope,
            LEDGER._task_scope_pid,
            LEDGER._task_scope_lock,
            LEDGER._task_scope_lock_pid,
            unclosed,
            LEDGER._atexit_registered,
        ) = self.globals
        LEDGER._unclosed_scopes.clear()
        LEDGER._unclosed_scopes.update(unclosed)
        super().tearDown()

    async def test_abandoned_child_drains_tools_before_releasing_reservation(self) -> None:
        for drain_timeout in [False, True]:
            with self.subTest(drain_timeout=drain_timeout):
                scope = LEDGER.AwTaskScope({"resident_policy_reload": True})
                scope.handle = {"id": "child"}
                scope.reservation = "reservation"
                scope.invocation_limit = 16
                entered, cleaning, release = (asyncio.Event() for _ in range(3))
                commands = []

                async def command(action, *args):
                    commands.append(action)
                    return (1 if action == "finish" else 0), b"", b""

                async def tool(tool_call):
                    if tool_call.id == "first":
                        await entered.wait()
                        yield ToolResponse(content=[TextBlock(text="first")])
                    else:
                        entered.set()
                        try:
                            await asyncio.Future()
                        finally:
                            cleaning.set()
                            await release.wait()

                scope._command = command
                agent = SimpleNamespace(
                    _acting_middlewares=[MODULE.AwInvocationMiddleware(scope.config)],
                    _acting_impl=tool, state=SimpleNamespace(session_id="session"),
                )

                async def consume(call_id):
                    return [item async for item in Agent._acting(
                        agent, ToolCallBlock(id=call_id, name="tool", input="{}"),
                    )]

                token = LEDGER.turn_scope.set(scope)
                first = asyncio.create_task(consume("first"))
                second = asyncio.create_task(consume("second"))
                closing = None
                try:
                    with self.assertRaisesRegex(LEDGER.CallbackFailure, "aw_invocation_finish_failed"):
                        await asyncio.wait_for(first, 1)
                    self.assertTrue(scope.closed)
                    wait = asyncio.wait

                    async def bounded_wait(tasks, **kwargs):
                        return await wait(tasks, timeout=.02)

                    with patch.object(LEDGER.asyncio, "wait", bounded_wait if drain_timeout else wait):
                        closing = asyncio.create_task(LEDGER.close_turn_scope(scope))
                        await asyncio.sleep(0)
                        self.assertNotIn("release", commands)
                        await asyncio.wait_for(cleaning.wait(), 1)
                        await asyncio.sleep(.03)
                        self.assertNotIn("release", commands)
                        self.assertEqual(scope.reservation, "reservation")
                        if drain_timeout:
                            await closing
                            self.assertIn(scope, LEDGER._unclosed_scopes)
                            with self.assertRaisesRegex(LEDGER.CallbackFailure, "aw_scope_cleanup_failed"):
                                await LEDGER._retry_unclosed_scopes()
                        else:
                            self.assertFalse(closing.done())
                        release.set()
                        await asyncio.wait_for(asyncio.gather(second, return_exceptions=True), 1)
                        await closing
                    await LEDGER._retry_unclosed_scopes()
                    self.assertIsNone(scope.reservation)
                    self.assertEqual(commands.count("release"), 1)
                    self.assertNotIn(scope, LEDGER._unclosed_scopes)
                finally:
                    release.set()
                    for task in [first, second]:
                        if not task.done():
                            task.cancel()
                    await asyncio.gather(first, second, return_exceptions=True)
                    if closing is not None:
                        await closing
                    LEDGER.turn_scope.reset(token)
                    LEDGER._unclosed_scopes.discard(scope)

    async def test_failed_open_keeps_owner_until_retry_closes_it(self) -> None:
        scope = LEDGER.AwTaskScope({})
        scope.handle = {"handle": "old"}
        scope.failed = True
        scope.open = AsyncMock(side_effect=LEDGER.CallbackFailure("enable failed"))
        scope._command = AsyncMock(return_value=(0, b"", b""))
        healthy = LEDGER.AwTaskScope({})
        healthy.open = AsyncMock()

        with patch.object(LEDGER, "AwTaskScope", side_effect=[scope, healthy]):
            with self.assertRaisesRegex(LEDGER.CallbackFailure, "enable failed"):
                await LEDGER._ensure_task_scope({})
            self.assertIn(scope, LEDGER._unclosed_scopes)
            self.assertIs(await LEDGER._ensure_task_scope({}), healthy)

        scope._command.assert_awaited_once()
        close_call = scope._command.await_args
        self.assertEqual(close_call.args[:2], ("close", scope.handle))
        self.assertGreater(close_call.args[2], 0)
        self.assertLessEqual(close_call.args[2], 6)
        self.assertTrue(scope.closed)
        self.assertNotIn(scope, LEDGER._unclosed_scopes)

    async def test_failed_cleanup_blocks_another_binding_allocation(self) -> None:
        scope = LEDGER.AwTaskScope({})
        scope.handle = {"handle": "old"}
        scope.failed = True
        scope._command = AsyncMock(return_value=(1, b"", b""))
        LEDGER._unclosed_scopes.add(scope)

        with patch.object(LEDGER, "AwTaskScope") as constructor:
            with self.assertRaisesRegex(LEDGER.CallbackFailure, "aw_scope_cleanup_failed"):
                await LEDGER._ensure_task_scope({})
        constructor.assert_not_called()
        scope._command.assert_awaited_once()
        close_call = scope._command.await_args
        self.assertEqual(close_call.args[:2], ("close", scope.handle))
        self.assertGreater(close_call.args[2], 0)
        self.assertLessEqual(close_call.args[2], 6)
        self.assertIn(scope, LEDGER._unclosed_scopes)

    async def test_cleanup_retry_has_one_total_deadline(self) -> None:
        scopes = []
        for _ in range(3):
            scope = LEDGER.AwTaskScope({})
            scope.handle = {"handle": "old"}
            scope._close_binding = AsyncMock()
            scopes.append(scope)
        LEDGER._unclosed_scopes.update(scopes)
        loop = SimpleNamespace(time=lambda: next(times))
        times = iter([0.0, 0.0, 5.0, 6.0])

        with patch.object(LEDGER.asyncio, "get_running_loop", return_value=loop):
            with self.assertRaisesRegex(LEDGER.CallbackFailure, "aw_scope_cleanup_failed"):
                await LEDGER._retry_unclosed_scopes()

        timeouts = [
            call.args[0]
            for scope in scopes
            for call in scope._close_binding.await_args_list
        ]
        self.assertEqual(sorted(timeouts), [1, 6])
        self.assertEqual(sum(scope._close_binding.await_count for scope in scopes), 2)

    async def test_cleanup_ignores_scopes_owned_by_parent(self) -> None:
        scope = LEDGER.AwTaskScope({})
        scope.handle = {"handle": "parent"}
        scope.pid = os.getpid() + 1
        scope._close_binding = AsyncMock()
        LEDGER._unclosed_scopes.add(scope)

        await LEDGER._retry_unclosed_scopes()

        scope._close_binding.assert_not_awaited()
        self.assertNotIn(scope, LEDGER._unclosed_scopes)

    async def test_failed_invocation_owner_never_reroutes_transport(self) -> None:
        owner = SimpleNamespace(
            invoke=AsyncMock(side_effect=LEDGER.CallbackFailure("aw_scope_unavailable"))
        )
        replacement = SimpleNamespace(invoke=AsyncMock())
        middleware = MODULE.AwToolMiddleware(
            {"executable": "aw", "binding": "binding"},
            {"event": "tool.after", "step": "check", "on_error": "report"},
        )
        token = MODULE._invocation_scope.set(owner)
        try:
            with patch.object(MODULE.asyncio, "create_subprocess_exec", AsyncMock()) as spawn:
                with self.assertRaisesRegex(LEDGER.CallbackFailure, "aw_scope_unavailable"):
                    await middleware._transport(
                        {}, deadline=asyncio.get_running_loop().time() + 1
                    )
        finally:
            MODULE._invocation_scope.reset(token)

        owner.invoke.assert_awaited_once()
        replacement.invoke.assert_not_awaited()
        spawn.assert_not_awaited()

    async def test_drain_uses_registration_owner_after_global_replacement(self) -> None:
        owner = LEDGER.AwTaskScope({})
        replacement = LEDGER.AwTaskScope({})
        owner.register = AsyncMock(
            side_effect=lambda call_id, *_: owner.invocations.__setitem__(call_id, 1)
        )
        owner.finish = AsyncMock(
            side_effect=lambda call_id: owner.invocations.pop(call_id, None)
        )
        replacement.finish = AsyncMock()

        async def next_handler() -> Any:
            owner.failed = True
            LEDGER._task_scope = replacement
            if False:
                yield None

        call = SimpleNamespace(name="tool", input="{}")
        middleware = MODULE.AwInvocationMiddleware({})
        with patch.object(MODULE, "_ensure_task_scope", AsyncMock(return_value=owner)):
            async for _ in middleware.on_acting(None, {"tool_call": call}, next_handler):
                self.fail("empty fixture yielded a response")

        owner.finish.assert_awaited_once()
        replacement.finish.assert_not_awaited()
        self.assertEqual(owner.invocations, {})

    async def test_resident_policy_is_installed_before_ack_and_release_follows_close(self):
        import json

        config = {"resident_policy_reload": True, "hooks": []}
        hook = {"event": "tool.before", "step": "new", "on_error": "block",
                "replace_result": False, "budget_ms": 1000}
        scope = LEDGER.AwTaskScope(config)
        scope._command = AsyncMock(side_effect=[
            (0, b"", b""), (0, b'{"binding":"task"}', b""),
            (0, json.dumps({"hooks": [hook]}).encode(), b""),
            (0, b"", b""), (0, b'{"invocations_per_binding":512}', b""),
            (0, b"", b""), (0, b"", b""),
        ])
        await scope.open()
        self.assertEqual(scope.config["hooks"], [hook])
        self.assertEqual(config["hooks"], [])
        self.assertNotIn(scope, LEDGER._unclosed_scopes)
        await scope.close()
        self.assertIsNotNone(scope.reservation)
        await scope.release()
        self.assertIsNone(scope.reservation)
        self.assertEqual([call.args[0] for call in scope._command.await_args_list],
                         ["reserve", "open", "policy", "acknowledge", "enable", "close", "release"])

    async def test_lost_open_reply_keeps_reservation_and_blocks_next_reply(self):
        scope = LEDGER.AwTaskScope({"resident_policy_reload": True})
        scope._command = AsyncMock(side_effect=[(0, b"", b""), TimeoutError()])
        with patch.object(LEDGER, "AwTaskScope", return_value=scope):
            with self.assertRaises(TimeoutError):
                await LEDGER.open_turn_scope(scope.config)
        self.assertIsNotNone(scope.reservation)
        self.assertFalse(scope.closed)
        self.assertIn(scope, LEDGER._unclosed_scopes)
        with patch.object(LEDGER, "AwTaskScope") as constructor:
            with self.assertRaisesRegex(LEDGER.CallbackFailure, "cleanup_failed"):
                await LEDGER.open_turn_scope(scope.config)
            constructor.assert_not_called()
        self.assertEqual([call.args[0] for call in scope._command.await_args_list], ["reserve", "open"])

    async def test_lost_release_confirmation_retries_without_reopening_child(self):
        scope = LEDGER.AwTaskScope({"resident_policy_reload": True})
        scope.handle = {"binding": "task"}
        scope.reservation = "reserved"
        scope._command = AsyncMock(side_effect=[(0, b"", b""), TimeoutError(), (0, b"", b"")])
        await LEDGER.close_turn_scope(scope)
        self.assertTrue(scope.closed)
        self.assertIsNotNone(scope.reservation)
        self.assertIn(scope, LEDGER._unclosed_scopes)
        await LEDGER._retry_unclosed_scopes()
        self.assertIsNone(scope.reservation)
        self.assertNotIn(scope, LEDGER._unclosed_scopes)
        self.assertEqual([call.args[0] for call in scope._command.await_args_list], ["close", "release", "release"])
