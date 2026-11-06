"""Native reply admission and bounded turn ownership for the AW factory."""

from __future__ import annotations

import asyncio
import os
import sys
from contextlib import aclosing, asynccontextmanager
from contextvars import ContextVar
from typing import Any, AsyncGenerator, Callable
from uuid import uuid4

from agentscope.agent import Agent
from agentscope.event import (
    ExternalExecutionResultEvent, UserConfirmResultEvent, UserInterruptEvent,
)
from agentscope.message import Msg, TextBlock
from agentscope.middleware import MiddlewareBase

from aw_scope_ledger import CallbackFailure, close_turn_scope, open_turn_scope, turn_scope, wait_owned


class SubmissionDenied(RuntimeError):
    """The submission did not enter native reasoning or memory."""


_turn: ContextVar[dict | None] = ContextVar("aw_agentscope_turn", default=None)


def submission_text(inputs: Any) -> str | None:
    """Only explicit user text is new input; context and resume events stay native."""
    if inputs is None or isinstance(inputs, (
        UserConfirmResultEvent, UserInterruptEvent, ExternalExecutionResultEvent,
    )):
        return None
    messages = inputs if isinstance(inputs, list) else [inputs]
    if any(not isinstance(message, Msg) for message in messages):
        raise ValueError("AW input.submit requires native Msg or list[Msg]")
    texts = []
    for message in messages:
        if message.role != "user":
            continue
        for block in message.content:
            if not isinstance(block, TextBlock):
                raise ValueError("AW input.submit requires user text blocks")
            texts.append(block.text)
    return "\n".join(texts) if texts else None


class AwSubmissionMiddleware(MiddlewareBase):
    """Check the effective reply inputs after the application's middleware."""

    def __init__(self, config: dict[str, Any]) -> None:
        self.config = config

    async def on_reply(
        self, agent: Agent, input_kwargs: dict[str, Any],
        next_handler: Callable[..., AsyncGenerator[Any, None]],
    ) -> AsyncGenerator[Any, None]:
        from aw_agentscope import AwToolMiddleware, _encode_payload

        owner = _turn.get()
        if owner is None or owner["agent"] is not agent:
            raise CallbackFailure("aw_reply_requires_factory_entrypoint")
        config = owner["scope"].config
        hooks = [hook for hook in config["hooks"] if hook["event"] == "input.submit"]
        # Unsupported native shapes are rejected even for report-only scans.
        text = submission_text(input_kwargs["inputs"]) if hooks else None
        if text is not None:
            payload = {"event": "input.submit", "session_id": owner["session"],
                       "aw_turn_id": owner["id"], "input_text": text}
            deadline = None
            for hook in hooks:
                if deadline is None:
                    deadline = asyncio.get_running_loop().time() + (hook["budget_ms"] + 3000) / 1000
                try:
                    _encode_payload(payload)
                    code, stdout, stderr = await owner["scope"].invoke(
                        hook, payload, max(0, deadline - asyncio.get_running_loop().time())
                    )
                    AwToolMiddleware._diagnostic(stderr)
                    if code == 2:
                        raise SubmissionDenied("Input blocked by AW policy")
                    if code != 0 or stdout.strip():
                        raise CallbackFailure("invalid_submission_response")
                except (OSError, TimeoutError, CallbackFailure) as error:
                    AwToolMiddleware(config, hook)._log_failure(type(error).__name__)
                    if hook["on_error"] == "block":
                        raise SubmissionDenied("AW input check failed; submission blocked") from error
        async with aclosing(next_handler()) as stream:
            async for item in stream:
                yield item


async def _owned_stream(stream: AsyncGenerator, scope: Any) -> AsyncGenerator:
    """Drive native iteration and cancellation in one context with one-item demand."""
    requested = asyncio.Event()
    output = asyncio.Queue(maxsize=1)
    stopping = advancing = cancelled = False

    async def drive() -> None:
        nonlocal advancing, cancelled
        try:
            while True:
                if not stopping:
                    await requested.wait()
                    requested.clear()
                if stopping and not cancelled:
                    # Resume the suspended chain with task cancellation. The SDK
                    # consumes interruption events while unwinding tool finally
                    # blocks; GeneratorExit alone does not close its nested streams.
                    asyncio.current_task().cancel()
                    cancelled = True
                advancing = True
                try:
                    item = await anext(stream)
                finally:
                    advancing = False
                if not stopping:
                    output.put_nowait((item, None))
        except BaseException as error:
            if not stopping:
                output.put_nowait((None, error))
            elif not isinstance(error, (StopAsyncIteration, asyncio.CancelledError)):
                raise
        finally:
            await stream.aclose()

    driver = asyncio.create_task(drive())
    try:
        while True:
            requested.set()
            item, error = await output.get()
            if isinstance(error, StopAsyncIteration):
                return
            if error is not None:
                raise error
            yield item
    finally:
        stopping = True
        if advancing:
            driver.cancel()
            cancelled = True
        requested.set()

        async def settle() -> None:
            _, pending = await asyncio.wait({driver}, timeout=6)
            if pending:
                # Keep the scope fenced if native cancellation fails to settle.
                scope.tasks.add(driver)
                driver.add_done_callback(
                    lambda task: None if task.cancelled() else task.exception()
                )
                raise CallbackFailure("aw_stream_cleanup_failed")
            if not driver.cancelled():
                driver.result()

        cleanup = asyncio.create_task(settle())
        interrupted = await wait_owned(cleanup)
        cleanup.result()
        if interrupted is not None:
            raise interrupted


class AwSubmissionAgent(Agent):
    """Keep public reply lifetime around the SDK's nested async generators."""

    def __init__(self, *args: Any, aw_config: dict[str, Any], aw_execution_factory=None, **kwargs: Any) -> None:
        super().__init__(*args, **kwargs)
        self._aw_config = aw_config
        self._aw_execution_factory = aw_execution_factory
        self._aw_busy = False
        self._aw_pid = os.getpid()

    @asynccontextmanager
    async def _aw_reply(self) -> AsyncGenerator[None, None]:
        if self._aw_pid != os.getpid():
            raise CallbackFailure("aw_reply_worker_changed")
        parent = _turn.get()
        checked_parent = parent is not None and any(
            hook["event"] == "input.submit" for hook in parent["scope"].config["hooks"]
        )
        if self._aw_busy or checked_parent:
            raise CallbackFailure("aw_concurrent_or_nested_reply_unsupported")
        session = self.state.session_id
        if not isinstance(session, str) or not session.strip() or len(session.encode()) > 512:
            raise ValueError("AW reply requires a bounded session_id")
        self._aw_busy = True
        scope = None
        from aw_private import current_execution

        token = scope_token = execution_token = None
        try:
            scope = await open_turn_scope(self._aw_config, self._aw_execution_factory)
            execution_token = current_execution.set(scope.execution.lease if scope.execution else None)
            scope_token = turn_scope.set(scope)
            token = _turn.set({"agent": self, "scope": scope, "session": session, "id": uuid4().hex})
            yield
        finally:
            try:
                if scope is not None:
                    cleanup = asyncio.create_task(close_turn_scope(scope))
                    interrupted = await wait_owned(cleanup)
                    cleanup.result()
                    if interrupted is not None:
                        raise interrupted
                    if not scope.closed or scope.reservation is not None:
                        error = sys.exc_info()[1]
                        if error is None or isinstance(error, GeneratorExit):
                            raise CallbackFailure("aw_scope_cleanup_failed")
                        from aw_agentscope import AwToolMiddleware

                        AwToolMiddleware._diagnostic(b"AW reply scope cleanup failed\n")
            finally:
                if execution_token is not None:
                    current_execution.reset(execution_token)
                if token is not None:
                    _turn.reset(token)
                if scope_token is not None:
                    turn_scope.reset(scope_token)
                self._aw_busy = False

    async def reply(self, inputs: Any = None, structured_schema: Any = None) -> Msg:
        async with self._aw_reply():
            return await super().reply(inputs, structured_schema)

    async def reply_stream(
        self, inputs: Any = None, structured_schema: Any = None, yield_final_msg: bool = False,
    ) -> AsyncGenerator[Any, None]:
        async with self._aw_reply():
            async with aclosing(_owned_stream(super().reply_stream(
                inputs, structured_schema, yield_final_msg
            ), turn_scope.get())) as stream:
                async for item in stream:
                    yield item
