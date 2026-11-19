"""Pure JSON normalization, argument isolation and descriptor helpers for aw_sdk.sources."""

from __future__ import annotations

import asyncio
from contextlib import contextmanager
from contextvars import copy_context
from copy import deepcopy
import math
from pathlib import Path
from typing import Any, AsyncGenerator, Callable, Iterator, Mapping

from aw_sdk.transport import AwError

_MAX_DEPTH = 32


def validate_identity(value: Any) -> str:
    if not isinstance(value, str) or not value.strip() or len(value.encode("utf-8")) > 512:
        raise AwError("source_invalid_identity")
    return value


def normalize_json(value: Any, *, depth: int = 0, seen: set[int] | None = None) -> Any:
    if depth > _MAX_DEPTH:
        raise AwError("source_invalid_payload")
    if value is None or isinstance(value, (bool, int, str)):
        return value
    if isinstance(value, float):
        if not math.isfinite(value):
            raise AwError("source_invalid_payload")
        return value
    if isinstance(value, Path):
        return str(value)
    if isinstance(value, bytes):
        return value.decode("utf-8", errors="replace")
    active = set() if seen is None else seen
    marker = id(value)
    if isinstance(value, (list, tuple)):
        if marker in active:
            raise AwError("source_invalid_payload")
        active.add(marker)
        try:
            return [normalize_json(item, depth=depth + 1, seen=active) for item in value]
        finally:
            active.discard(marker)
    if isinstance(value, Mapping):
        if marker in active:
            raise AwError("source_invalid_payload")
        active.add(marker)
        try:
            normalized: dict[str, Any] = {}
            for key, item in value.items():
                if not isinstance(key, str):
                    raise AwError("source_invalid_payload")
                normalized[key] = normalize_json(item, depth=depth + 1, seen=active)
            return normalized
        finally:
            active.discard(marker)
    return str(value)


def is_plain_json(value: Any, *, depth: int = 0) -> bool:
    if depth > _MAX_DEPTH:
        return False
    if value is None or type(value) in (bool, int, str):
        return True
    if type(value) is float:
        return math.isfinite(value)
    if type(value) is list:
        return all(is_plain_json(item, depth=depth + 1) for item in value)
    if type(value) is dict:
        return all(
            type(k) is str and is_plain_json(v, depth=depth + 1) for k, v in value.items()
        )
    return False


def default_extract_text(*args: Any, **kwargs: Any) -> str:
    for key in ("text", "prompt", "input"):
        candidate = kwargs.get(key)
        if isinstance(candidate, str):
            return candidate
    for arg in args:
        if isinstance(arg, str):
            return arg
    raise AwError("source_input_text_required")


def default_extract_input(*args: Any, **kwargs: Any) -> dict[str, Any]:
    if len(args) == 1 and not kwargs and isinstance(args[0], Mapping):
        result = normalize_json(args[0])
        if isinstance(result, dict):
            return result
    if not args and kwargs:
        result = normalize_json(kwargs)
        if isinstance(result, dict):
            return result
    payload: dict[str, Any] = {"args": normalize_json(list(args))}
    if kwargs:
        payload["kwargs"] = normalize_json(kwargs)
    return payload


def snapshot_tool_input(raw: Any) -> dict[str, Any]:
    normalized = normalize_json(raw)
    if not isinstance(normalized, dict):
        raise AwError("source_tool_input_object_required")
    return normalized


def isolate_call_args(
    args: tuple[Any, ...],
    kwargs: dict[str, Any],
    snapshot_arg: Callable[[Any], Any] | None,
) -> tuple[tuple[Any, ...], dict[str, Any]]:
    """Detach positional and keyword arguments before extracting checked input."""
    detached_args = tuple(snapshot_input_arg(arg, snapshot_arg) for arg in args)
    detached_kwargs = {k: snapshot_input_arg(v, snapshot_arg) for k, v in kwargs.items()}
    return detached_args, detached_kwargs


def snapshot_input_arg(value: Any, snapshot_arg: Callable[[Any], Any] | None) -> Any:
    if is_plain_json(value):
        return deepcopy(value)
    if type(value) in (bytes, float, complex):
        return value
    if snapshot_arg is not None:
        copy = snapshot_arg(value)
        if copy is not value:
            return copy
    raise AwError("source_input_snapshot_required")


def snapshot_stream_chunk(value: Any) -> Any:
    """Deep-copy plain JSON chunks while preserving native objects for extract_result."""
    return deepcopy(value) if is_plain_json(value) else value


def find_class_descriptor(target: Any, attr: str) -> Any:
    for cls in type(target).__mro__:
        if attr in cls.__dict__:
            return cls.__dict__[attr]
    return None


def is_slot_descriptor(descriptor: Any) -> bool:
    return type(descriptor).__name__ == "member_descriptor"


class Instrumentation:
    """Reversible handle for methods patched on a Python object."""

    def __init__(self, entries: list[tuple[Any, str, str, Any, Any]]) -> None:
        self._entries = entries
        self.restored = False

    def restore(self) -> None:
        """Restore all patched attributes idempotently, attempting every entry."""
        if self.restored:
            return
        self.restored = True
        errors: list[BaseException] = []
        for target, attr, storage, original, wrapper in reversed(self._entries):
            try:
                current = (
                    target.__dict__.get(attr) if storage == "dict" else getattr(target, attr, None)
                )
                if current is not wrapper:
                    continue
                if storage in ("dict", "slot"):
                    setattr(target, attr, original)
                else:
                    delattr(target, attr)
            except BaseException as error:
                errors.append(error)
        if errors:
            raise errors[0]

    def __enter__(self) -> "Instrumentation":
        return self

    def __exit__(self, *_: Any) -> None:
        self.restore()

    async def __aenter__(self) -> "Instrumentation":
        return self

    async def __aexit__(self, *_: Any) -> None:
        self.restore()


def patch_method(
    target: Any,
    attr: str,
    factory: Callable[..., Any],
    opts: Mapping[str, Any],
    instrumented_attr: str,
) -> tuple[Any, str, str, Any, Any]:
    if not hasattr(target, attr):
        raise AwError("source_attribute_missing")
    bound = getattr(target, attr)
    if not callable(bound):
        raise AwError("source_callable_required")
    if getattr(bound, instrumented_attr, False):
        raise AwError("source_already_instrumented")
    class_desc = find_class_descriptor(target, attr)
    has_data_desc = class_desc is not None and (
        hasattr(class_desc, "__set__") or hasattr(class_desc, "__delete__")
    )
    obj_dict = getattr(target, "__dict__", None)
    if has_data_desc:
        if not is_slot_descriptor(class_desc):
            raise AwError("source_not_patchable")
        storage, original = "slot", bound
    elif isinstance(obj_dict, dict) and attr in obj_dict:
        storage, original = "dict", obj_dict[attr]
    elif isinstance(obj_dict, dict):
        storage, original = "inherited", bound
    else:
        raise AwError("source_not_patchable")
    wrapped = factory(bound, **opts)
    try:
        setattr(target, attr, wrapped)
    except Exception as error:
        raise AwError("source_not_patchable") from error
    if getattr(target, attr, None) is not wrapped:
        raise AwError("source_not_patchable")
    return target, attr, storage, original, wrapped


@contextmanager
def tool_executed() -> Iterator[None]:
    try:
        yield
    except BaseException as error:
        try:
            error.tool_executed = True
        except (AttributeError, TypeError):
            failure = AwError("source_result_processing_failed")
            failure.tool_executed = True
            raise failure from error
        raise


class ActiveStream:
    """Own the native generator and cancel its read before closing it."""

    def __init__(self, stream: AsyncGenerator[Any, None]) -> None:
        self.stream = stream
        self.context = copy_context()
        self.pending: asyncio.Task | None = None
        self.closing: asyncio.Task | None = None

    async def read(self) -> Any:
        if self.closing is not None:
            raise AwError("source_session_closed")
        self.pending = asyncio.create_task(anext(self.stream), context=self.context)
        try:
            result = await asyncio.shield(self.pending)
        except asyncio.CancelledError:
            # The consumer may leave before the read settles. Close owns that read.
            raise
        except BaseException:
            self.pending = None
            raise
        self.pending = None
        return result

    async def close(self) -> None:
        if self.closing is None:
            self.closing = asyncio.create_task(self._close())
        await asyncio.shield(self.closing)

    async def _close(self) -> None:
        pending = self.pending
        cancelled = False
        if pending is not None:
            cancelled = pending.cancel()
            await asyncio.gather(pending, return_exceptions=True)
        await asyncio.create_task(self.stream.aclose(), context=self.context)
        if pending is not None and (cancelled or self.pending is pending):
            self.pending = None
            if not pending.cancelled():
                error = pending.exception()
                if error is not None and not isinstance(error, StopAsyncIteration):
                    raise error
