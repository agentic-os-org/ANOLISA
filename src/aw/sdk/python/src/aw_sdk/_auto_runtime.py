"""Runtime session pools, spec validation and sync execution for aw_sdk.auto_instrument."""

from __future__ import annotations

import asyncio
from collections.abc import AsyncIterator as AsyncIteratorABC
from copy import deepcopy
from dataclasses import dataclass
import inspect
import threading
from typing import Any, Callable, Iterator, Mapping

from aw_sdk._source_helpers import (
    default_extract_input,
    default_extract_text,
    is_plain_json,
    isolate_call_args,
    normalize_json,
    snapshot_tool_input,
    tool_executed,
    validate_identity,
)
from aw_sdk._source_mutation import (
    DEFAULT_EVENTS,
    DEFAULT_MAX_STREAM_BYTES,
    SUPPORTED_EVENTS,
    apply_stream_result,
    apply_tool_input,
    apply_tool_result,
    buffer_sync_iterator,
    close_lazy_result,
    is_lazy_result,
    isolate_strict_value,
)
from aw_sdk.sources import DEFAULT_MAX_STREAM_ITEMS, LanguageSource, SourceSession
from aw_sdk.transport import AwError

_TOP_KEYS = frozenset(
    {
        "endpoint",
        "target",
        "availability",
        "session_id",
        "events",
        "replace_input",
        "replace_result",
        "withhold_result",
        "timeout_ms",
        "cwd",
        "eager_import",
        "targets",
    }
)
_TARGET_KEYS = frozenset(
    {
        "module",
        "qualname",
        "kind",
        "name",
        "buffer_stream",
        "max_stream_items",
        "max_stream_bytes",
    }
)


@dataclass(frozen=True)
class AutoTarget:
    """Validated allowlist entry naming one module attribute or class method."""

    module: str
    qualname: str
    kind: str
    name: str
    buffer_stream: bool = False
    max_stream_items: int = DEFAULT_MAX_STREAM_ITEMS
    max_stream_bytes: int = DEFAULT_MAX_STREAM_BYTES


def _valid_dotted(value: Any, *, allow_dunder: bool = False) -> bool:
    if not isinstance(value, str) or not value or "*" in value:
        return False
    for part in value.split("."):
        if not part.isidentifier():
            return False
        if not allow_dunder and part.startswith("__") and part.endswith("__"):
            return False
    return True


def validate_spec(raw: Mapping[str, Any]) -> dict[str, Any]:
    """Validate an explicit allowlist auto-instrumentation spec, rejecting wildcards."""
    if not isinstance(raw, Mapping) or any(k not in _TOP_KEYS for k in raw):
        raise AwError("source_auto_config_invalid")
    endpoint = raw.get("endpoint")
    target = raw.get("target")
    if not isinstance(endpoint, str) or not endpoint.strip():
        raise AwError("source_auto_config_invalid")
    if not isinstance(target, str) or not target.strip():
        raise AwError("source_auto_config_invalid")
    availability = raw.get("availability", "required")
    if availability not in {"required", "native_fallback"}:
        raise AwError("source_auto_config_invalid")
    events = raw.get("events", list(DEFAULT_EVENTS))
    if (
        not isinstance(events, list)
        or not events
        or any(not isinstance(e, str) or e not in SUPPORTED_EVENTS for e in events)
    ):
        raise AwError("source_auto_config_invalid")
    for flag in ("replace_input", "replace_result", "withhold_result", "eager_import"):
        if not isinstance(raw.get(flag, False), bool):
            raise AwError("source_auto_config_invalid")
    timeout_ms = raw.get("timeout_ms", 5000)
    if type(timeout_ms) is not int or not 100 <= timeout_ms <= 60000:
        raise AwError("source_auto_config_invalid")
    session_id = raw.get("session_id")
    if session_id is not None:
        validate_identity(session_id)
    cwd = raw.get("cwd")
    if cwd is not None and (not isinstance(cwd, str) or not cwd.strip()):
        raise AwError("source_auto_config_invalid")
    raw_targets = raw.get("targets")
    if not isinstance(raw_targets, list) or not raw_targets:
        raise AwError("source_auto_config_invalid")
    parsed_targets: list[AutoTarget] = []
    seen: set[tuple[str, str]] = set()
    for entry in raw_targets:
        if not isinstance(entry, Mapping) or any(k not in _TARGET_KEYS for k in entry):
            raise AwError("source_auto_config_invalid")
        mod = entry.get("module")
        qual = entry.get("qualname")
        kind = entry.get("kind", "tool")
        if not _valid_dotted(mod) or not _valid_dotted(qual):
            raise AwError("source_auto_config_invalid")
        if kind not in {"tool", "input"}:
            raise AwError("source_auto_config_invalid")
        key = (mod, qual)
        if key in seen:
            raise AwError("source_auto_config_invalid")
        seen.add(key)
        tool_name = entry.get("name", qual.rsplit(".", 1)[-1])
        validate_identity(tool_name)
        buf = entry.get("buffer_stream", False)
        max_items = entry.get("max_stream_items", DEFAULT_MAX_STREAM_ITEMS)
        max_bytes = entry.get("max_stream_bytes", DEFAULT_MAX_STREAM_BYTES)
        if (
            not isinstance(buf, bool)
            or type(max_items) is not int
            or max_items < 1
            or type(max_bytes) is not int
            or max_bytes < 1
        ):
            raise AwError("source_auto_config_invalid")
        parsed_targets.append(
            AutoTarget(
                module=mod,
                qualname=qual,
                kind=kind,
                name=tool_name,
                buffer_stream=buf,
                max_stream_items=max_items,
                max_stream_bytes=max_bytes,
            )
        )
    return {
        "endpoint": endpoint,
        "target": target,
        "availability": availability,
        "session_id": session_id,
        "events": list(dict.fromkeys(events)),
        "replace_input": raw.get("replace_input", False),
        "replace_result": raw.get("replace_result", False),
        "withhold_result": raw.get("withhold_result", False),
        "timeout_ms": timeout_ms,
        "cwd": cwd,
        "eager_import": raw.get("eager_import", False),
        "targets": parsed_targets,
    }


def create_source(spec: dict[str, Any]) -> LanguageSource:
    """Create a LanguageSource from a validated auto-instrumentation spec."""
    return LanguageSource.create(
        endpoint=spec["endpoint"],
        target=spec["target"],
        availability=spec["availability"],
        events=spec["events"],
        replace_input=spec["replace_input"],
        replace_result=spec["replace_result"],
        withhold_result=spec["withhold_result"],
        cwd=spec["cwd"],
    )


class _LoopSlot:
    """Single-flight LanguageSource + SourceSession bound to one owning event loop."""

    def __init__(self, loop: asyncio.AbstractEventLoop, spec: dict[str, Any]) -> None:
        self.loop = loop
        self._spec = spec
        self._source: LanguageSource | None = None
        self._session: SourceSession | None = None
        self._connect_task: asyncio.Task[SourceSession] | None = None
        self._closed = False

    async def _connect(self) -> SourceSession:
        source = create_source(self._spec)
        try:
            await source.connect()
            if self._closed:
                await source.close()
                raise AwError("source_closed")
            session = source.session(
                session_id=self._spec["session_id"],
                timeout_ms=self._spec["timeout_ms"],
            )
            self._source = source
            self._session = session
            return session
        except BaseException:
            try:
                await source.close()
            except BaseException:
                pass
            raise

    async def get_session(self) -> SourceSession:
        if self._closed:
            raise AwError("source_closed")
        if self._session is not None:
            return self._session
        task = self._connect_task
        if task is None:
            task = self.loop.create_task(self._connect())
            self._connect_task = task

            def _clear(done: asyncio.Task[SourceSession]) -> None:
                if self._connect_task is done:
                    self._connect_task = None

            task.add_done_callback(_clear)
        return await asyncio.shield(task)

    async def aclose(self) -> None:
        self._closed = True
        task = self._connect_task
        self._connect_task = None
        if task is not None and not task.done():
            task.cancel()
            await asyncio.gather(task, return_exceptions=True)
        source = self._source
        self._session = None
        self._source = None
        if source is not None:
            await source.close()


def _running_loop_or_none() -> asyncio.AbstractEventLoop | None:
    try:
        return asyncio.get_running_loop()
    except RuntimeError:
        return None


class AsyncSessionPool:
    """Thread-safe map of per-loop single-flight sessions with owning-loop shutdown."""

    def __init__(self, spec: dict[str, Any]) -> None:
        self._spec = spec
        self._lock = threading.Lock()
        self._slots: dict[int, _LoopSlot] = {}
        self._closed = False

    async def get_session(self) -> SourceSession:
        loop = asyncio.get_running_loop()
        key = id(loop)
        with self._lock:
            if self._closed:
                raise AwError("source_closed")
            slot = self._slots.get(key)
            if slot is None or slot.loop is not loop:
                slot = _LoopSlot(loop, self._spec)
                self._slots[key] = slot
        return await slot.get_session()

    def _drain_slots(self) -> list[_LoopSlot]:
        with self._lock:
            self._closed = True
            slots = list(self._slots.values())
            self._slots.clear()
        return slots

    async def aclose(self) -> None:
        slots = self._drain_slots()
        current = asyncio.get_running_loop()
        errors: list[BaseException] = []
        for slot in slots:
            try:
                if slot.loop is current:
                    await slot.aclose()
                elif slot.loop.is_closed():
                    continue
                elif slot.loop.is_running():
                    fut = asyncio.run_coroutine_threadsafe(slot.aclose(), slot.loop)
                    await asyncio.wait_for(asyncio.wrap_future(fut), timeout=5.0)
                else:
                    await asyncio.to_thread(slot.loop.run_until_complete, slot.aclose())
            except BaseException as error:
                errors.append(error)
        if errors:
            raise errors[0]

    def close_sync(self) -> None:
        slots = self._drain_slots()
        current = _running_loop_or_none()
        errors: list[BaseException] = []
        for slot in slots:
            try:
                if slot.loop.is_closed():
                    continue
                if slot.loop.is_running():
                    if slot.loop is current:
                        slot.loop.create_task(slot.aclose())
                    else:
                        asyncio.run_coroutine_threadsafe(slot.aclose(), slot.loop).result(
                            timeout=5.0
                        )
                else:
                    slot.loop.run_until_complete(slot.aclose())
            except BaseException as error:
                errors.append(error)
        if errors:
            raise errors[0]


class SyncBridge:
    """Dedicated background event loop thread with single-flight connection."""

    def __init__(self, spec: dict[str, Any]) -> None:
        self._spec = spec
        self._lock = threading.Lock()
        self._loop: asyncio.AbstractEventLoop | None = None
        self._thread: threading.Thread | None = None
        self._slot: _LoopSlot | None = None
        self._closed = False

    def _ensure_loop(self) -> tuple[asyncio.AbstractEventLoop, _LoopSlot]:
        with self._lock:
            if self._closed:
                raise AwError("source_closed")
            if self._loop is not None and self._slot is not None:
                return self._loop, self._slot
            ready = threading.Event()
            loop_box: list[asyncio.AbstractEventLoop] = []

            def runner() -> None:
                loop = asyncio.new_event_loop()
                asyncio.set_event_loop(loop)
                loop_box.append(loop)
                ready.set()
                loop.run_forever()
                loop.close()

            self._thread = threading.Thread(target=runner, name="aw-auto-bridge", daemon=True)
            self._thread.start()
            ready.wait()
            self._loop = loop_box[0]
            self._slot = _LoopSlot(self._loop, self._spec)
            return self._loop, self._slot

    def run(self, coro_fn: Callable[[SourceSession], Any]) -> Any:
        loop, slot = self._ensure_loop()

        async def execute() -> Any:
            session = await slot.get_session()
            return await coro_fn(session)

        return asyncio.run_coroutine_threadsafe(execute(), loop).result()

    def close(self) -> None:
        with self._lock:
            self._closed = True
            loop = self._loop
            thread = self._thread
            slot = self._slot
            self._loop = None
            self._thread = None
            self._slot = None
        if loop is None:
            return
        try:
            if slot is not None:
                asyncio.run_coroutine_threadsafe(slot.aclose(), loop).result(timeout=5.0)
        finally:
            loop.call_soon_threadsafe(loop.stop)
            if thread is not None:
                thread.join(timeout=5.0)


def _admit_sync_tool(
    sync: SyncBridge,
    entry: AutoTarget,
    user_args: tuple[Any, ...],
    kwargs: dict[str, Any],
) -> tuple[str, dict[str, Any], tuple[Any, ...], dict[str, Any], bool, bool]:
    call_args, call_kwargs = isolate_call_args(user_args, kwargs, None)
    raw_input = default_extract_input(*call_args, **call_kwargs)
    orig_snapshot = snapshot_tool_input(raw_input)

    async def before(session: SourceSession) -> tuple[Any, ...]:
        call_id, snapshot, decision = await session.before_tool(entry.name, raw_input)
        return call_id, snapshot, decision, session._strict_after(), session._observe_after()

    call_id, snapshot, decision, strict_after, observe_after = sync.run(before)
    if snapshot_tool_input(default_extract_input(*call_args, **call_kwargs)) != orig_snapshot:
        raise AwError("source_input_mutated")
    if "updated_input" in decision:
        call_args, call_kwargs = apply_tool_input(
            decision["updated_input"],
            call_args,
            call_kwargs,
            apply_input=None,
            custom_extract=False,
            snapshot_arg=None,
        )
        if snapshot_tool_input(default_extract_input(*call_args, **call_kwargs)) != snapshot:
            raise AwError("source_input_replace_mismatch")
    return call_id, snapshot, call_args, call_kwargs, strict_after, observe_after


def run_sync_tool(
    sync: SyncBridge,
    original: Callable[..., Any],
    entry: AutoTarget,
    receiver: tuple[Any, ...],
    user_args: tuple[Any, ...],
    kwargs: dict[str, Any],
) -> Any:
    """Execute a synchronous input or tool target with strict output isolation."""
    if entry.kind == "input":
        call_args, call_kwargs = isolate_call_args(user_args, kwargs, None)
        text = default_extract_text(*call_args, **call_kwargs)
        sync.run(lambda session: session.submit(text))
        if default_extract_text(*call_args, **call_kwargs) != text:
            raise AwError("source_input_mutated")
        return original(*receiver, *call_args, **call_kwargs)

    call_id, snapshot, call_args, call_kwargs, strict_after, observe_after = _admit_sync_tool(
        sync, entry, user_args, kwargs
    )
    result = original(*receiver, *call_args, **call_kwargs)
    if strict_after:
        with tool_executed():
            if is_lazy_result(result):
                if inspect.isasyncgen(result) or isinstance(result, AsyncIteratorABC):
                    raise AwError("source_stream_withhold_requires_buffering")
                if not entry.buffer_stream:
                    close_lazy_result(result)
                    raise AwError("source_stream_withhold_requires_buffering")
                checked = buffer_sync_iterator(
                    result,
                    max_stream_items=entry.max_stream_items,
                    max_stream_bytes=entry.max_stream_bytes,
                )
                after = sync.run(
                    lambda session: session.after_tool(
                        entry.name, snapshot, checked, call_id=call_id
                    )
                )
                if "replaced_result" in after:
                    deliver = apply_stream_result(
                        checked,
                        after["replaced_result"],
                        apply_result=None,
                        custom_extract=False,
                    )
                else:
                    deliver = [deepcopy(item) if is_plain_json(item) else item for item in checked]
                return iter(deliver)
            isolated = isolate_strict_value(result)
            after = sync.run(
                lambda session: session.after_tool(
                    entry.name, snapshot, isolated, call_id=call_id
                )
            )
            if "replaced_result" in after:
                return apply_tool_result(
                    isolated, after["replaced_result"], apply_result=None, custom_extract=False
                )
            return deepcopy(isolated) if is_plain_json(isolated) else isolated
    if observe_after:
        after = sync.run(
            lambda session: session.after_tool(entry.name, snapshot, result, call_id=call_id)
        )
        if "replaced_result" in after:
            with tool_executed():
                return apply_tool_result(
                    result, after["replaced_result"], apply_result=None, custom_extract=False
                )
    return result


def run_sync_gen(
    sync: SyncBridge,
    original: Callable[..., Any],
    entry: AutoTarget,
    receiver: tuple[Any, ...],
    user_args: tuple[Any, ...],
    kwargs: dict[str, Any],
) -> Iterator[Any]:
    """Execute a synchronous generator target, preserving generator iteration protocol."""
    if entry.kind == "input":
        call_args, call_kwargs = isolate_call_args(user_args, kwargs, None)
        text = default_extract_text(*call_args, **call_kwargs)
        sync.run(lambda session: session.submit(text))
        if default_extract_text(*call_args, **call_kwargs) != text:
            raise AwError("source_input_mutated")
        gen = original(*receiver, *call_args, **call_kwargs)
        try:
            yield from gen
        finally:
            close_lazy_result(gen)
        return

    call_id, snapshot, call_args, call_kwargs, strict_after, observe_after = _admit_sync_tool(
        sync, entry, user_args, kwargs
    )
    if strict_after and not entry.buffer_stream:
        raise AwError("source_stream_withhold_requires_buffering")
    gen = original(*receiver, *call_args, **call_kwargs)
    with tool_executed():
        try:
            if strict_after:
                checked = buffer_sync_iterator(
                    gen,
                    max_stream_items=entry.max_stream_items,
                    max_stream_bytes=entry.max_stream_bytes,
                )
                after = sync.run(
                    lambda session: session.after_tool(
                        entry.name, snapshot, checked, call_id=call_id
                    )
                )
                if "replaced_result" in after:
                    deliver = apply_stream_result(
                        checked,
                        after["replaced_result"],
                        apply_result=None,
                        custom_extract=False,
                    )
                else:
                    deliver = [deepcopy(item) if is_plain_json(item) else item for item in checked]
                for item in deliver:
                    yield item
                return
            observed: list[Any] | None = [] if observe_after else None
            for item in gen:
                if observed is not None:
                    if len(observed) >= entry.max_stream_items:
                        raise AwError("source_stream_result_too_large")
                    observed.append(normalize_json(item))
                yield item
            if observed is not None:
                sync.run(
                    lambda session: session.after_tool(
                        entry.name, snapshot, observed, call_id=call_id
                    )
                )
        finally:
            close_lazy_result(gen)
