"""Framework-free language-level Hook Source runtime over aw-hook-source/v1."""

from __future__ import annotations

import asyncio
import functools
import inspect
from typing import Any, AsyncIterator, Callable, Iterable, Mapping
from uuid import uuid4

from aw_sdk._source_helpers import (
    ActiveStream,
    Instrumentation,
    default_extract_input,
    default_extract_text,
    isolate_call_args,
    normalize_json,
    patch_method,
    snapshot_stream_chunk,
    snapshot_tool_input,
    tool_executed,
    validate_identity,
)
from aw_sdk._source_mutation import (
    DEFAULT_EVENTS,
    SOURCE_PROTOCOL,
    SUPPORTED_EVENTS,
    SourceDenied,
    SourceResultWithheld,
    apply_tool_input,
    apply_tool_result,
    confirm_source_routes,
    route_has_strict_after,
    run_tool_stream,
    source_capabilities,
)
from aw_sdk.admission import Admission
from aw_sdk.client import Instance, Task
from aw_sdk.transport import AwError

DEFAULT_MAX_STREAM_ITEMS = 1024
_INSTRUMENTED_ATTR = "_aw_source_wrapped"

__all__ = [
    "DEFAULT_EVENTS",
    "DEFAULT_MAX_STREAM_ITEMS",
    "Instrumentation",
    "LanguageSource",
    "SOURCE_PROTOCOL",
    "SourceDenied",
    "SourceResultWithheld",
    "SourceSession",
    "confirm_source_routes",
    "source_capabilities",
]


class SourceSession:
    """Per-request semantic session backed by one immutable Admission and Task."""

    def __init__(
        self,
        source: "LanguageSource",
        *,
        session_id: str | None = None,
        events: Iterable[str] | None = None,
        timeout_ms: int = 5000,
        cwd: str | None = None,
        environment: dict[str, str] | None = None,
    ) -> None:
        self._source = source
        self.session_id = validate_identity(str(uuid4()) if session_id is None else session_id)
        declared = list(source.instance.capabilities["events"])
        selected = declared if events is None else list(dict.fromkeys(events))
        if not selected or any(event not in declared for event in selected):
            raise AwError("admission_event_unavailable")
        self.events = tuple(selected)
        self.admission = Admission(
            source.instance,
            events=list(self.events),
            timeout_ms=timeout_ms,
            cwd=cwd,
            environment=environment,
        )
        self.closed = False
        self._instrumentations: list[Instrumentation] = []
        self._active_streams: set[ActiveStream] = set()
        self._active_calls: set[asyncio.Future[None]] = set()
        self._task_closed = False
        self._closing: asyncio.Task[BaseException | None] | None = None
        if source._closed:
            raise AwError("source_closed")
        source._sessions.add(self)

    @property
    def task(self) -> Task | None:
        return self.admission.task

    @property
    def fallback(self) -> bool:
        return self.admission.fallback

    def _observe_after(self) -> bool:
        if self.task is None or self.fallback:
            return False
        return any(hook["event"] == "tool.after" for hook in self.task.routes["hooks"])

    def _strict_after(self) -> bool:
        return not self.fallback and self.task is not None and route_has_strict_after(self.task.routes)

    def _require_open(self) -> None:
        if self.closed:
            raise AwError("source_session_closed")

    async def _run_admitted_call(self, fn: Callable[[], Any]) -> Any:
        self._require_open()
        settled = asyncio.get_running_loop().create_future()
        self._active_calls.add(settled)
        try:
            result = fn()
            return await result if inspect.isawaitable(result) else result
        finally:
            self._active_calls.discard(settled)
            settled.set_result(None)

    def _verify_route_coverage(self) -> None:
        """Ensure narrowed session events never skip an admitted Task route."""
        task = self.admission.task
        if self.fallback or task is None:
            return
        caps = self._source.instance.capabilities
        for hook in task.routes.get("hooks", []):
            if hook.get("event") not in self.events:
                raise AwError("source_route_coverage_incomplete")
            if not confirm_source_routes(caps, {"hooks": [hook]}):
                raise AwError("source_route_effect_unsupported")

    async def prepare(self) -> None:
        self._require_open()
        await self.admission.prepare()
        self._verify_route_coverage()
        self._require_open()

    async def submit(self, text: str, *, turn_id: str | None = None) -> dict[str, str]:
        """Emit input.submit and raise SourceDenied when AW blocks."""
        self._require_open()
        if "input.submit" not in self.events:
            raise AwError("semantic_event_unavailable")
        if not isinstance(text, str):
            raise AwError("source_input_text_required")
        resolved_turn = validate_identity(str(uuid4()) if turn_id is None else turn_id)
        await self.prepare()

        async def emit() -> dict[str, str]:
            assert self.task is not None
            decision = await self.task.emit(
                "input.submit",
                {"session_id": self.session_id, "turn_id": resolved_turn, "text": text},
            )
            if decision["decision"] == "block":
                raise SourceDenied("input.submit", session_id=self.session_id, turn_id=resolved_turn)
            self._require_open()
            return decision

        return await self._run_admitted_call(emit)

    async def before_tool(
        self, name: str, tool_input: Mapping[str, Any], *, call_id: str | None = None
    ) -> tuple[str, dict[str, Any], dict[str, Any]]:
        """Emit tool.before and raise SourceDenied before native tool execution."""
        self._require_open()
        tool_name = validate_identity(name)
        resolved_call = validate_identity(str(uuid4()) if call_id is None else call_id)
        snapshot = snapshot_tool_input(tool_input)
        await self.prepare()
        if "tool.before" in self.events:

            async def emit() -> tuple[str, dict[str, Any], dict[str, Any]]:
                assert self.task is not None
                decision = await self.task.emit(
                    "tool.before",
                    {
                        "session_id": self.session_id,
                        "call_id": resolved_call,
                        "name": tool_name,
                        "input": snapshot,
                    },
                )
                if decision["decision"] == "block":
                    raise SourceDenied(
                        "tool.before",
                        session_id=self.session_id,
                        call_id=resolved_call,
                        tool_name=tool_name,
                    )
                self._require_open()
                effective = snapshot
                if "updated_input" in decision:
                    effective = snapshot_tool_input(decision["updated_input"])
                    decision = {**decision, "updated_input": effective}
                return resolved_call, effective, decision

            return await self._run_admitted_call(emit)
        self._require_open()
        return resolved_call, snapshot, {"decision": "continue", "coverage": "not_configured"}

    async def after_tool(
        self, name: str, tool_input: Mapping[str, Any], result: Any, *, call_id: str
    ) -> dict[str, Any]:
        """Emit tool.after with original snapshot and normalized result; enforce withholding."""
        with tool_executed():
            self._require_open()
            if "tool.after" not in self.events:
                return {"decision": "continue", "coverage": "not_configured"}
            await self.prepare()
            return await self._run_admitted_call(
                lambda: self._after_admitted_tool(name, tool_input, result, call_id=call_id)
            )

    async def _after_admitted_tool(
        self, name: str, tool_input: Mapping[str, Any], result: Any, *, call_id: str
    ) -> dict[str, Any]:
        with tool_executed():
            if self._task_closed:
                raise AwError("source_session_closed")
            task = self.task
            if task is None or not self._observe_after():
                coverage = "unavailable" if self.fallback else "not_configured"
                return {"decision": "continue", "coverage": coverage}
            tool_name = validate_identity(name)
            resolved_call = validate_identity(call_id)
            snapshot = snapshot_tool_input(tool_input)
            decision = await task.emit(
                "tool.after",
                {
                    "session_id": self.session_id,
                    "call_id": resolved_call,
                    "name": tool_name,
                    "input": snapshot,
                    "result": normalize_json(result),
                },
            )
            if decision["decision"] == "withhold":
                raise SourceResultWithheld(
                    "tool.after",
                    session_id=self.session_id,
                    call_id=resolved_call,
                    tool_name=tool_name,
                )
            return decision

    def wrap_input(
        self,
        fn: Callable[..., Any],
        *,
        extract_text: Callable[..., str] | None = None,
        extract_turn_id: Callable[..., str | None] | None = None,
        snapshot_arg: Callable[[Any], Any] | None = None,
    ) -> Callable[..., Any]:
        """Wrap a non-blocking sync/async callable or async generator behind input.submit."""
        if not callable(fn):
            raise AwError("source_callable_required")
        text_extractor = default_extract_text if extract_text is None else extract_text

        async def admit_call(
            args: tuple[Any, ...], kwargs: dict[str, Any]
        ) -> tuple[tuple[Any, ...], dict[str, Any]]:
            args, kwargs = isolate_call_args(args, kwargs, snapshot_arg)
            text = text_extractor(*args, **kwargs)
            turn_id = extract_turn_id(*args, **kwargs) if extract_turn_id is not None else None
            await self.submit(text, turn_id=turn_id)
            self._require_open()
            if text_extractor(*args, **kwargs) != text:
                raise AwError("source_input_mutated")
            return args, kwargs

        if inspect.isasyncgenfunction(fn):

            @functools.wraps(fn)
            async def wrapped_stream(*args: Any, **kwargs: Any) -> AsyncIterator[Any]:
                call_args, call_kwargs = await admit_call(args, kwargs)
                self._require_open()
                stream = ActiveStream(fn(*call_args, **call_kwargs))
                self._active_streams.add(stream)
                try:
                    while True:
                        self._require_open()
                        try:
                            item = await stream.read()
                        except StopAsyncIteration:
                            self._require_open()
                            break
                        self._require_open()
                        yield item
                finally:
                    try:
                        await stream.close()
                    finally:
                        if stream.closing is not None and stream.closing.done():
                            self._active_streams.discard(stream)

            setattr(wrapped_stream, _INSTRUMENTED_ATTR, True)
            return wrapped_stream

        @functools.wraps(fn)
        async def wrapped(*args: Any, **kwargs: Any) -> Any:
            call_args, call_kwargs = await admit_call(args, kwargs)
            return await self._run_admitted_call(lambda: fn(*call_args, **call_kwargs))

        setattr(wrapped, _INSTRUMENTED_ATTR, True)
        return wrapped

    def wrap_tool(
        self,
        fn: Callable[..., Any],
        *,
        name: str | None = None,
        extract_input: Callable[..., Mapping[str, Any]] | None = None,
        apply_input: Callable[[dict[str, Any], tuple[Any, ...], dict[str, Any]], Any] | None = None,
        extract_call_id: Callable[..., str | None] | None = None,
        extract_result: Callable[[Any], Any] | None = None,
        apply_result: Callable[[Any, Any], Any] | None = None,
        snapshot_arg: Callable[[Any], Any] | None = None,
        max_stream_items: int = DEFAULT_MAX_STREAM_ITEMS,
        buffer_stream: bool = False,
    ) -> Callable[..., Any]:
        """Wrap a non-blocking sync/async tool or async generator behind tool.before/after."""
        if not callable(fn):
            raise AwError("source_callable_required")
        if type(max_stream_items) is not int or max_stream_items < 1:
            raise AwError("source_invalid_stream_limit")
        tool_name = validate_identity(getattr(fn, "__name__", "") if name is None else name)
        input_extractor = default_extract_input if extract_input is None else extract_input
        custom_extract_input = extract_input is not None
        custom_extract_result = extract_result is not None

        async def admit_call(args: tuple[Any, ...], kwargs: dict[str, Any]):
            call_args, call_kwargs = isolate_call_args(args, kwargs, snapshot_arg)
            raw_input = input_extractor(*call_args, **call_kwargs)
            original_snapshot = snapshot_tool_input(raw_input)
            call_id = (
                extract_call_id(*call_args, **call_kwargs) if extract_call_id is not None else None
            )
            resolved_call, snapshot, decision = await self.before_tool(
                tool_name, raw_input, call_id=call_id
            )
            self._require_open()
            if snapshot_tool_input(input_extractor(*call_args, **call_kwargs)) != original_snapshot:
                raise AwError("source_input_mutated")
            if "updated_input" in decision:
                call_args, call_kwargs = apply_tool_input(
                    decision["updated_input"],
                    call_args,
                    call_kwargs,
                    apply_input=apply_input,
                    custom_extract=custom_extract_input,
                    snapshot_arg=snapshot_arg,
                )
                if snapshot_tool_input(input_extractor(*call_args, **call_kwargs)) != snapshot:
                    raise AwError("source_input_replace_mismatch")
            return resolved_call, snapshot, call_args, call_kwargs

        async def observe_result(
            result: Any, snapshot: dict[str, Any], call_id: str
        ) -> dict[str, Any]:
            with tool_executed():
                payload = result if extract_result is None else extract_result(result)
                return await self._after_admitted_tool(tool_name, snapshot, payload, call_id=call_id)

        if inspect.isasyncgenfunction(fn):

            @functools.wraps(fn)
            async def wrapped_stream(*args: Any, **kwargs: Any) -> AsyncIterator[Any]:
                resolved_call, snapshot, call_args, call_kwargs = await admit_call(args, kwargs)
                self._require_open()
                inner = run_tool_stream(
                    self,
                    fn,
                    call_args,
                    call_kwargs,
                    snapshot=snapshot,
                    call_id=resolved_call,
                    observe_result=observe_result,
                    extract_result=extract_result,
                    apply_result=apply_result,
                    max_stream_items=max_stream_items,
                    buffer_stream=buffer_stream,
                )
                try:
                    async for item in inner:
                        yield item
                finally:
                    await inner.aclose()

            setattr(wrapped_stream, _INSTRUMENTED_ATTR, True)
            return wrapped_stream

        @functools.wraps(fn)
        async def wrapped(*args: Any, **kwargs: Any) -> Any:
            resolved_call, snapshot, call_args, call_kwargs = await admit_call(args, kwargs)

            async def invoke() -> Any:
                result = fn(*call_args, **call_kwargs)
                if inspect.isawaitable(result):
                    result = await result
                if self._observe_after():
                    decision = await observe_result(result, snapshot, resolved_call)
                    if "replaced_result" in decision:
                        with tool_executed():
                            return apply_tool_result(
                                result,
                                decision["replaced_result"],
                                apply_result=apply_result,
                                custom_extract=custom_extract_result,
                            )
                return result

            return await self._run_admitted_call(invoke)

        setattr(wrapped, _INSTRUMENTED_ATTR, True)
        return wrapped

    def instrument(
        self,
        target: Any,
        *,
        input_methods: Iterable[str] | Mapping[str, Mapping[str, Any]] = (),
        tool_methods: Iterable[str] | Mapping[str, str | Mapping[str, Any]] = (),
        snapshot_arg: Callable[[Any], Any] | None = None,
    ) -> Instrumentation:
        """Patch methods until restored, with a default snapshot callback for their arguments."""
        self._require_open()
        input_spec = (
            dict(input_methods)
            if isinstance(input_methods, Mapping)
            else {name: {} for name in input_methods}
        )
        tool_spec: dict[str, dict[str, Any]] = {}
        if isinstance(tool_methods, Mapping):
            for attr, spec in tool_methods.items():
                tool_spec[attr] = {"name": spec} if isinstance(spec, str) else dict(spec)
        else:
            for attr in tool_methods:
                tool_spec[attr] = {}
        if set(input_spec) & set(tool_spec):
            raise AwError("source_method_conflict")

        patched: list[tuple[Any, str, str, Any, Any]] = []
        try:
            for attr, opts in input_spec.items():
                patched.append(
                    patch_method(
                        target,
                        attr,
                        self.wrap_input,
                        {"snapshot_arg": snapshot_arg, **opts},
                        _INSTRUMENTED_ATTR,
                    )
                )
            for attr, opts in tool_spec.items():
                patched.append(
                    patch_method(
                        target,
                        attr,
                        self.wrap_tool,
                        {"name": attr, "snapshot_arg": snapshot_arg, **opts},
                        _INSTRUMENTED_ATTR,
                    )
                )
        except BaseException:
            Instrumentation(patched).restore()
            raise
        handle = Instrumentation(patched)
        self._instrumentations.append(handle)
        return handle

    async def close(self, *, timeout_ms: int | None = None) -> None:
        """Drain admitted work, restore patches and release the Task."""
        budget_ms = self.admission.timeout_ms if timeout_ms is None else timeout_ms
        if type(budget_ms) is not int or not 100 <= budget_ms <= 60000:
            raise AwError("invalid_attach_timeout")
        self.closed = True
        if self._closing is None:
            self._closing = asyncio.create_task(self._drain())
        done, _ = await asyncio.wait({self._closing}, timeout=budget_ms / 1000)
        if not done:
            raise AwError("source_close_pending")
        error = self._closing.result()
        if error is not None:
            raise error

    async def _drain(self) -> BaseException | None:
        errors: list[BaseException] = []
        while self._active_streams or self._active_calls:
            streams = list(self._active_streams)
            self._active_streams.clear()
            results = await asyncio.gather(
                *(stream.close() for stream in streams), return_exceptions=True
            )
            errors.extend(result for result in results if isinstance(result, BaseException))
            if self._active_calls:
                await asyncio.gather(*self._active_calls)
        handles = list(reversed(self._instrumentations))
        self._instrumentations.clear()
        for handle in handles:
            try:
                handle.restore()
            except BaseException as error:
                errors.append(error)
        self._task_closed = True
        try:
            await self.admission.close()
        except BaseException as error:
            errors.append(error)
        self._source._sessions.discard(self)
        if errors and self._source._closed and self._source._drain_error is None:
            self._source._drain_error = errors[0]
        return errors[0] if errors else None

    async def __aenter__(self) -> "SourceSession":
        return self

    async def __aexit__(self, *_: Any) -> None:
        await self.close()


class LanguageSource:
    """Framework-free Hook Source runtime for arbitrary Python agents."""

    def __init__(self, instance: Instance, *, owns_instance: bool = False) -> None:
        caps = instance.capabilities
        if caps.get("adapter") != "sdk" or caps.get("version") != SOURCE_PROTOCOL:
            raise AwError("semantic_profile_required")
        self.instance = instance
        self._owns_instance = owns_instance
        self._sessions: set[SourceSession] = set()
        self._closed = False
        self._drain_error: BaseException | None = None

    @classmethod
    def create(
        cls,
        *,
        endpoint: str,
        target: str,
        availability: str = "required",
        events: Iterable[str] = DEFAULT_EVENTS,
        replace_input: bool = False,
        replace_result: bool = False,
        withhold_result: bool = False,
        cwd: str | None = None,
        environment: dict[str, str] | None = None,
        config: str | None = None,
        timeout_ms: int = 1000,
        reconnect_ms: int = 1000,
    ) -> "LanguageSource":
        """Create an owned sdk/application Instance with canonical route confirmation."""
        capabilities = source_capabilities(
            events,
            replace_input=replace_input,
            replace_result=replace_result,
            withhold_result=withhold_result,
        )
        instance = Instance(
            endpoint=endpoint,
            target=target,
            capabilities=capabilities,
            availability=availability,
            install=lambda routes: confirm_source_routes(capabilities, routes),
            cwd=cwd,
            environment=environment,
            config=config,
            timeout_ms=timeout_ms,
            reconnect_ms=reconnect_ms,
        )
        return cls(instance, owns_instance=True)

    async def connect(self) -> bool:
        return await self.instance.connect()

    def start(self) -> None:
        self.instance.start()

    def listen(self, *, automatic: bool = False) -> None:
        self.instance.listen(automatic=automatic)

    async def stop(self) -> None:
        await self.instance.stop()

    def session(
        self,
        session_id: str | None = None,
        *,
        events: Iterable[str] | None = None,
        timeout_ms: int = 5000,
        cwd: str | None = None,
        environment: dict[str, str] | None = None,
    ) -> SourceSession:
        """Open a per-request SourceSession sharing this Instance."""
        return SourceSession(
            self,
            session_id=session_id,
            events=events,
            timeout_ms=timeout_ms,
            cwd=cwd,
            environment=environment,
        )

    async def close(
        self, *, close_instance: bool | None = None, timeout_ms: int | None = None
    ) -> None:
        """Close active sessions and optionally close the underlying Instance."""
        if timeout_ms is not None and (
            type(timeout_ms) is not int or not 100 <= timeout_ms <= 60000
        ):
            raise AwError("invalid_attach_timeout")
        self._closed = True
        errors: list[BaseException] = []
        active = list(self._sessions)
        for session in active:
            session.closed = True
        if active:
            results = await asyncio.gather(
                *(session.close(timeout_ms=timeout_ms) for session in active),
                return_exceptions=True,
            )
            errors.extend(result for result in results if isinstance(result, BaseException))
        should_close = self._owns_instance if close_instance is None else close_instance
        is_pending = lambda err: isinstance(err, AwError) and err.code == "source_close_pending"
        if self._sessions:
            for error in errors:
                if is_pending(error):
                    raise error
        else:
            errors = [error for error in errors if not is_pending(error)]
            if should_close:
                try:
                    await self.instance.close()
                except BaseException as error:
                    errors.append(error)
        if errors:
            raise errors[0]
        if self._drain_error is not None:
            raise self._drain_error

    async def __aenter__(self) -> "LanguageSource":
        await self.connect()
        return self

    async def __aexit__(self, *_: Any) -> None:
        await self.close()
