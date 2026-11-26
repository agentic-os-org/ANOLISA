"""Adopt AW decisions at profile invocation and stream delivery boundaries."""

from __future__ import annotations

import asyncio
import functools
import inspect
from collections.abc import (
    AsyncIterable,
    AsyncIterator,
    Callable,
    Iterator,
)
from contextlib import ExitStack
from contextvars import ContextVar
from copy import deepcopy
from types import MethodType
from typing import TYPE_CHECKING, Any
from uuid import uuid4

from aw_sdk._profile_eval import CallFields
from aw_sdk._profile_patch import Seam
from aw_sdk._profile_stream import _RequestAsyncStream, _RequestStream
from aw_sdk._source_helpers import tool_executed, validate_identity
from aw_sdk._source_mutation import (
    _serialize_input_projection,
    apply_tool_result,
    isolate_strict_value,
    json_encoded_bytes,
)
from aw_sdk.sources import (
    DEFAULT_MAX_STREAM_BYTES,
    DEFAULT_MAX_STREAM_ITEMS,
    SourceDenied,
    SourceResultWithheld,
    SourceSession,
)
from aw_sdk.transport import AwError

if TYPE_CHECKING:
    from aw_sdk.profile_source import ProfileSource, _Request

SessionIdentity = ContextVar[str | None] | Callable[[], str | None]


def _reject_foreign_future(
    value: Any, source: ProfileSource, request: _Request, admission: ExitStack
) -> None:
    if not asyncio.isfuture(value) or value.get_loop() is source._loop:
        return
    try:
        caller_loop = asyncio.get_running_loop()
    except RuntimeError:  # A synchronous target may run outside an event loop.
        caller_loop = None
    owner_loop = value.get_loop()

    with tool_executed():
        if value.done():
            if not value.cancelled():
                value.result()
        elif caller_loop is not owner_loop or value is not asyncio.current_task():
            # A stopped loop may resume later. Transfer admission to the Task
            # before queueing cancellation, without making the caller wait.
            pending = admission.pop_all()

            def settled(future: asyncio.Future) -> None:
                with pending, source._lock:
                    request.cleanup_tasks.discard(future)
                    if not future.cancelled():
                        error = future.exception()
                        if request.cleanup_error is None:
                            request.cleanup_error = error

            def cancel() -> None:
                if value.done():
                    settled(value)
                else:
                    with source._lock:
                        request.cleanup_tasks.add(value)
                    value.add_done_callback(settled)
                    value.cancel()

            if caller_loop is owner_loop:
                cancel()
            else:
                try:
                    owner_loop.call_soon_threadsafe(cancel)
                except RuntimeError as error:
                    pending.close()
                    raise AwError("source_cleanup_unconfirmed") from error
        raise AwError("source_target_call_shape_mismatch")


async def _dispose_deferred(value: Any) -> None:
    if asyncio.isfuture(value):
        value.cancel()
        try:
            await value
        except asyncio.CancelledError:
            if not value.cancelled():
                raise
        return
    aclose = getattr(value, "aclose", None)
    if callable(aclose):
        await aclose()
    elif callable(close := getattr(value, "close", None)):
        close()


async def _reject_deferred(value: Any) -> None:
    # Preserve caller cancellation while the request still owns native cleanup.
    if value is not asyncio.current_task():
        cleanup = asyncio.create_task(_dispose_deferred(value))
        cancelled = None
        while not cleanup.done():
            try:
                await asyncio.wait((cleanup,))
            except asyncio.CancelledError as error:
                cancelled = error
        try:
            cleanup.result()
        finally:
            if cancelled is not None:
                raise cancelled
    raise AwError("source_target_call_shape_mismatch")


class Invocation:
    """One logical call, with immutable identity and a single tool correlation ID."""

    def __init__(
        self,
        session: SourceSession,
        target: dict[str, Any],
        fields: CallFields,
        identity_context: SessionIdentity | None,
    ) -> None:
        self.session, self.target, self.fields = session, target, fields
        self.bindings = target["fields"]
        self.hooks = target["hooks"]
        self.identity_context = identity_context
        self.settled: asyncio.Future[None] | None = None
        self.session_id = session.session_id
        self.call_id = None
        self.tool_name = None
        self.tool_input: dict[str, Any] = {}
        self.audit_after = self.strict_after = self.withhold_on_error = False

    def _field(self, name: str, *, optional: bool = False) -> Any:
        return (
            self.fields.read(self.bindings[name], nullable=optional)
            if name in self.bindings
            else None
        )

    def _input(self) -> dict[str, Any]:
        value = self._field("input")
        if type(value) is not dict:
            raise AwError("source_tool_input_object_required")
        return value

    async def before(self) -> None:
        self.settled = self.session._begin_call()
        await self.session.prepare()
        if self.session.fallback:
            return
        dynamic = self._field("session_id", optional=True)
        if dynamic is None:
            context = self.identity_context
            if isinstance(context, ContextVar):
                try:
                    dynamic = context.get()
                except LookupError:
                    pass
            elif context is not None:
                dynamic = context()
        if dynamic is not None:
            self.session_id = validate_identity(dynamic)

        if "session.start" in self.hooks:
            await self.session.session_start(session_id=self.session_id)
        if "input.submit" in self.hooks or "turn.stop" in self.hooks:
            turn_id = self._field("turn_id", optional=True)
            turn_id = str(uuid4()) if turn_id is None else validate_identity(turn_id)
            if "input.submit" in self.hooks:
                await self.session.submit(
                    self._field("prompt"), turn_id=turn_id, session_id=self.session_id
                )
            if "turn.stop" in self.hooks:
                decision = await self.session.turn_stop(
                    turn_id, "profile_target", session_id=self.session_id
                )
                if decision["decision"] == "block":
                    raise SourceDenied(
                        "turn.stop", session_id=self.session_id, turn_id=turn_id
                    )

        self.audit_after = "tool.after" in self.hooks and self.session._observe_after()
        self.strict_after = self.audit_after and self.session._strict_after()
        self.withhold_on_error = self.audit_after and any(
            route["event"] == "tool.after"
            and route.get("on_error") == "withhold_result"
            for route in self.session.task.routes["hooks"]
        )
        if "tool.before" not in self.hooks and not self.audit_after:
            return

        self.tool_name = validate_identity(self._field("tool_name"))
        mapped_id = self._field("call_id", optional=True)
        self.call_id = (
            str(uuid4()) if mapped_id is None else validate_identity(mapped_id)
        )
        if "tool.before" in self.hooks:
            self.fields.isolate_input(self.bindings["input"])
        if "input" in self.bindings:
            self.tool_input = self._input()
        if "tool.before" in self.hooks:
            _, effective, decision = await self.session.before_tool(
                self.tool_name,
                self.tool_input,
                call_id=self.call_id,
                session_id=self.session_id,
            )
            if _serialize_input_projection(
                self._input()
            ) != _serialize_input_projection(self.tool_input):
                raise AwError("source_input_mutated")
            if "updated_input" in decision:
                self.fields.write_input(self.bindings["input"], effective)
                if _serialize_input_projection(
                    self._input()
                ) != _serialize_input_projection(effective):
                    raise AwError("source_input_replace_mismatch")
                self.tool_input = effective

    async def before_stream(self) -> tuple[dict[str, Any] | None, bool, bool]:
        await self.before()
        config = self.target.get("stream")
        if self.session.fallback and not self.audit_after:
            return None, False, False
        if self.strict_after and not (config and config["buffer"]):
            raise AwError("source_stream_withhold_requires_buffering")
        if config is None and self.audit_after:
            config = {
                "buffer": False,
                "max_items": DEFAULT_MAX_STREAM_ITEMS,
                "max_bytes": DEFAULT_MAX_STREAM_BYTES,
            }
        return config, config is not None and config["buffer"], self.audit_after

    async def after(self, result: Any) -> Any:
        if (
            inspect.isawaitable(result)
            or isinstance(result, AsyncIterable)
            or hasattr(type(result), "__next__")
            or hasattr(type(result), "__anext__")
        ):
            with tool_executed():
                await _reject_deferred(result)
        if self.audit_after:
            with tool_executed():
                try:
                    result = await self._after_tool(result)
                except AwError as error:
                    if not self.withhold_on_error or isinstance(
                        error, (SourceDenied, SourceResultWithheld)
                    ):
                        raise
                    raise SourceResultWithheld(
                        session_id=self.session_id,
                        call_id=self.call_id,
                        tool_name=self.tool_name,
                    ) from error
        if "session.end" in self.hooks and not self.session.fallback:
            await self.session._emit_admitted_lifecycle(
                "session.end", self.session_id, reason="profile_target"
            )
        return result

    async def _after_tool(self, result: Any) -> Any:
        if self.session.fallback:
            if self.strict_after:
                raise AwError("source_unavailable")
            return result
        if self.strict_after:
            result = isolate_strict_value(
                result, snapshot_result=deepcopy, custom_extract=True
            )
        binding = self.bindings["result"]
        projected = self.fields.read(binding, result)
        if (
            "stream" in self.target
            and json_encoded_bytes(projected) > self.target["stream"]["max_bytes"]
        ):
            raise AwError("source_stream_result_too_large")
        decision = await self.session._after_admitted_tool(
            self.tool_name,
            self.tool_input,
            projected,
            call_id=self.call_id,
            session_id=self.session_id,
        )
        if self.strict_after and decision.get("coverage") != "checked":
            raise AwError("source_unavailable")
        if "replaced_result" in decision:
            return apply_tool_result(
                result,
                decision["replaced_result"],
                apply_result=lambda value, replacement: self.fields.write_result(
                    binding, value, replacement
                ),
                custom_extract=True,
            )
        return result


class _StreamItems:
    """Bound the compact JSON array size, including separators and brackets."""

    def __init__(self, config: dict[str, Any]) -> None:
        self.items: list[Any] = []
        self.count, self.size = 0, 2
        self.max_items, self.max_bytes = config["max_items"], config["max_bytes"]
        if self.size > self.max_bytes:
            raise AwError("source_stream_result_too_large")

    def append(self, item: Any, *, retain: bool = True) -> None:
        if self.count >= self.max_items:
            raise AwError("source_stream_result_too_large")
        snapshot = isolate_strict_value(item)
        self.size += json_encoded_bytes(snapshot) + int(self.count > 0)
        if self.size > self.max_bytes:
            raise AwError("source_stream_result_too_large")
        self.count += 1
        if retain:
            self.items.append(snapshot)


def _replay(result: Any, config: dict[str, Any]) -> list[Any]:
    if type(result) is not list:
        raise AwError("source_result_replace_unsupported")
    deliver = _StreamItems(config)
    for item in result:
        deliver.append(item)
    return deliver.items


def _sync_stream(
    source: ProfileSource,
    seam: Seam,
    request: _Request,
    args: tuple[Any, ...],
    kwargs: dict[str, Any],
) -> Iterator[Any]:
    with source._admit(request, seam, args, kwargs) as call:
        config = None
        buffer = retain = False
        if call is not None:
            config, buffer, retain = source._sync(call.before_stream())
            args, kwargs = call.fields.args, call.fields.kwargs
        with tool_executed():
            captured = _StreamItems(config) if config is not None else None
        stream = iter(seam.fn(*args, **kwargs))
        try:
            step: Callable[..., Any] = stream.__next__
            arguments: tuple[Any, ...] = ()
            while True:
                try:
                    with request.session._admitted_native_step(request):
                        item = step(*arguments)
                except StopIteration as stop:
                    # The checked buffered result consists only of the yielded items.
                    terminal = None if buffer else stop.value
                    break
                if captured is not None:
                    with tool_executed():
                        captured.append(item, retain=buffer or retain)
                step, arguments = stream.__next__, ()
                if not buffer:
                    try:
                        sent = yield item
                    except GeneratorExit:
                        raise
                    except BaseException as error:
                        throw = getattr(stream, "throw", None)
                        if throw is None:
                            raise
                        step, arguments = throw, (error,)
                    else:
                        if sent is not None:
                            step, arguments = stream.send, (sent,)
        finally:
            close = getattr(stream, "close", None)
            if close is not None:
                with request.session._admitted_native_step(request):
                    close()
        if call is None:
            return terminal
        with tool_executed():
            items = captured.items if captured is not None else None
            result = source._sync(call.after(items))
            deliver = (
                _replay(result, config) if buffer and result is not items else result
            )
        if buffer:
            yield from deliver
        return terminal


class _AsyncStream:
    """Keep async iterator send, throw and close on the native caller's loop."""

    def __init__(self, stream: AsyncIterator[Any]) -> None:
        self.stream = stream

    async def step(self, sent: Any, error: BaseException | None) -> Any:
        if error is not None:
            throw = getattr(self.stream, "athrow", None)
            if throw is None:
                raise error
            return await throw(error)
        if sent is not None:
            return await self.stream.asend(sent)
        return await anext(self.stream)

    async def close(self) -> None:
        close = getattr(self.stream, "aclose", None)
        if close is not None:
            await close()


async def _async_stream(
    source: ProfileSource,
    seam: Seam,
    request: _Request,
    args: tuple[Any, ...],
    kwargs: dict[str, Any],
) -> AsyncIterator[Any]:
    with source._admit(request, seam, args, kwargs) as call:
        config = None
        buffer = retain = False
        if call is not None:
            config, buffer, retain = await source._async(call.before_stream())
            args, kwargs = call.fields.args, call.fields.kwargs
        with tool_executed():
            captured = _StreamItems(config) if config is not None else None
        iterator = seam.fn(*args, **kwargs)
        stream = _AsyncStream(aiter(iterator))
        try:
            sent = error = None
            while True:
                try:
                    with request.session._admitted_native_step(request):
                        item = await stream.step(sent, error)
                except StopAsyncIteration:
                    break
                sent = error = None
                if captured is not None:
                    with tool_executed():
                        captured.append(item, retain=buffer or retain)
                if not buffer:
                    try:
                        sent = yield item
                    except GeneratorExit:
                        raise
                    except BaseException as thrown:  # noqa: BLE001
                        error = thrown  # Forward athrow to the native iterator.
        finally:
            with request.session._admitted_native_step(request):
                await stream.close()
        if call is None:
            return
        with tool_executed():
            items = None if captured is None else captured.items
            result = await source._async(call.after(items))
            if buffer and result is not items:
                result = _replay(result, config)
        if buffer:
            for item in result:
                yield item


class _LazyWrapper:
    """Expose native function metadata while binding the request at call time."""

    def __init__(self, fn: Callable[..., Any], dispatch: Callable[..., Any]) -> None:
        functools.update_wrapper(self, fn)
        self.__code__ = fn.__code__
        self.__defaults__ = fn.__defaults__
        self.__kwdefaults__ = fn.__kwdefaults__
        self._dispatch = dispatch

    def __get__(self, instance: Any, owner: type) -> Any:
        return self if instance is None else MethodType(self, instance)

    def __call__(self, *args: Any, **kwargs: Any) -> Any:
        return self._dispatch(*args, **kwargs)


def build_wrapper(seam: Seam, source: ProfileSource) -> Callable[..., Any]:
    fn = seam.fn
    shape = seam.target["call"]

    async def run_async(
        request: _Request, args: tuple[Any, ...], kwargs: dict[str, Any]
    ) -> Any:
        with ExitStack() as admission:
            call = admission.enter_context(source._admit(request, seam, args, kwargs))
            if call is None:
                return await fn(*args, **kwargs)
            await source._async(call.before())
            result = await fn(*call.fields.args, **call.fields.kwargs)
            _reject_foreign_future(result, source, request, admission)
            return await source._async(call.after(result))

    def dispatch(args: tuple[Any, ...], kwargs: dict[str, Any]) -> Any:
        request = source._call_request()
        if request is None:
            return fn(*args, **kwargs)
        if shape == "async":
            return run_async(request, args, kwargs)
        if shape == "sync_iter":
            return _RequestStream(
                _sync_stream(source, seam, request, args, kwargs), source, request
            )
        if shape == "async_iter":
            return _RequestAsyncStream(
                _async_stream(source, seam, request, args, kwargs), source, request
            )
        with ExitStack() as admission:
            call = admission.enter_context(source._admit(request, seam, args, kwargs))
            if call is None:
                return fn(*args, **kwargs)
            source._sync(call.before())
            result = fn(*call.fields.args, **call.fields.kwargs)
            _reject_foreign_future(result, source, request, admission)
            return source._sync(call.after(result))

    @functools.wraps(fn)
    def wrapped(*args: Any, **kwargs: Any) -> Any:
        request = source._request_context.get(None)
        if request is not None and request.cleanup_tasks:
            try:
                task = asyncio.current_task()
            except RuntimeError:
                task = None
            if task in request.cleanup_tasks:
                with request.session._admitted_native_step(request):
                    return dispatch(args, kwargs)
        return dispatch(args, kwargs)

    if any(
        test(fn)
        for test in (
            inspect.iscoroutinefunction,
            inspect.isasyncgenfunction,
            inspect.isgeneratorfunction,
        )
    ):
        return _LazyWrapper(fn, wrapped)
    return wrapped
