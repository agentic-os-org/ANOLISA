"""AgentScope task-scope ownership and cleanup contracts."""

from __future__ import annotations

import asyncio
import os
from types import SimpleNamespace
from typing import Any
from unittest.mock import AsyncMock, patch

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
