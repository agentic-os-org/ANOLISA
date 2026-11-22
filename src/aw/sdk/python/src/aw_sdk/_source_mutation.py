"""Input/result rewrites, strict output isolation, and tool wrapper builder for aw_sdk.sources."""

from __future__ import annotations

from collections.abc import AsyncIterator as AsyncIteratorABC
from collections.abc import Iterator as IteratorABC
from copy import deepcopy
import functools
import inspect
import json
from typing import Any, AsyncIterator, Callable, Mapping

from aw_sdk._source_helpers import (
    ActiveStream,
    default_extract_input,
    is_plain_json,
    isolate_call_args,
    normalize_json,
    snapshot_stream_chunk,
    snapshot_tool_input,
    tool_executed,
    validate_identity,
)
from aw_sdk._source_profile import DEFAULT_MAX_STREAM_BYTES
from aw_sdk.transport import AwError


def is_lazy_result(value: Any) -> bool:
    """Return True when value is a lazy iterator, generator or async iterator."""
    return (
        inspect.isgenerator(value)
        or inspect.isasyncgen(value)
        or isinstance(value, (IteratorABC, AsyncIteratorABC))
    )


def close_lazy_result(value: Any) -> None:
    """Best-effort close of a synchronous generator or iterator."""
    closer = getattr(value, "close", None)
    if callable(closer):
        try:
            closer()
        except BaseException:
            pass


async def aclose_lazy_result(value: Any) -> None:
    """Best-effort close of a sync or async lazy result."""
    acloser = getattr(value, "aclose", None)
    if callable(acloser):
        try:
            await acloser()
            return
        except BaseException:
            return
    close_lazy_result(value)


def json_encoded_bytes(value: Any) -> int:
    """Return exact UTF-8 JSON byte length of a normalized payload."""
    return len(json.dumps(value, ensure_ascii=False, separators=(",", ":")).encode("utf-8"))


def prepare_audited_result(
    value: Any,
    *,
    extract_result: Callable[[Any], Any] | None,
    max_bytes: int | None = None,
    isolate_for_extract: bool = False,
) -> Any:
    """Evaluate extract_result once, normalize once, and apply max_bytes to the audited JSON."""
    if extract_result is None:
        projected = (
            deepcopy(value)
            if (isolate_for_extract and is_plain_json(value))
            else normalize_json(value)
        )
    else:
        source = deepcopy(value) if (isolate_for_extract and is_plain_json(value)) else value
        projected = normalize_json(extract_result(source))
    if max_bytes is not None and json_encoded_bytes(projected) > max_bytes:
        raise AwError("source_stream_result_too_large")
    return projected


def isolate_strict_value(
    value: Any,
    *,
    snapshot_result: Callable[[Any], Any] | None = None,
    require_plain_json: bool = False,
    custom_extract: bool = False,
) -> Any:
    """Return an isolated copy suitable for strict-after audit and delivery."""
    if is_lazy_result(value):
        close_lazy_result(value)
        raise AwError("source_stream_withhold_requires_buffering")
    if is_plain_json(value):
        return deepcopy(value)
    if snapshot_result is not None:
        copied = snapshot_result(value)
        if is_lazy_result(copied):
            close_lazy_result(copied)
            raise AwError("source_stream_withhold_requires_buffering")
        if copied is value:
            raise AwError("source_result_isolation_required")
        if is_plain_json(copied):
            return deepcopy(copied)
        if require_plain_json or not custom_extract:
            raise AwError("source_result_isolation_required")
        return copied
    raise AwError("source_result_isolation_required")


def buffer_sync_iterator(
    iterator: Any,
    *,
    max_stream_items: int,
    max_stream_bytes: int,
    snapshot_result: Callable[[Any], Any] | None = None,
    custom_extract: bool = False,
) -> list[Any]:
    """Consume a sync iterator/generator into bounded, accurately metered plain-JSON snapshots."""
    checked: list[Any] = []
    buffered_bytes = 2
    try:
        for item in iterator:
            if len(checked) >= max_stream_items:
                raise AwError("source_stream_result_too_large")
            isolated = isolate_strict_value(
                item,
                snapshot_result=snapshot_result,
                require_plain_json=True,
                custom_extract=custom_extract,
            )
            buffered_bytes += json_encoded_bytes(isolated) + (1 if checked else 0)
            if buffered_bytes > max_stream_bytes:
                raise AwError("source_stream_result_too_large")
            checked.append(isolated)
    finally:
        close_lazy_result(iterator)
    return checked


def apply_tool_input(
    updated_input: Mapping[str, Any],
    call_args: tuple[Any, ...],
    call_kwargs: dict[str, Any],
    *,
    apply_input: Callable[[dict[str, Any], tuple[Any, ...], dict[str, Any]], Any] | None,
    custom_extract: bool,
    snapshot_arg: Callable[[Any], Any] | None,
) -> tuple[tuple[Any, ...], dict[str, Any]]:
    """Rebuild isolated native call arguments from an approved replace_input object."""
    revised = snapshot_tool_input(updated_input)
    if apply_input is not None:
        rebuilt = apply_input(deepcopy(revised), call_args, call_kwargs)
        if not isinstance(rebuilt, tuple) or len(rebuilt) != 2:
            raise AwError("source_input_replace_unsupported")
        new_args, new_kwargs = rebuilt
        if (
            not isinstance(new_args, (tuple, list))
            or not isinstance(new_kwargs, dict)
            or any(not isinstance(k, str) for k in new_kwargs)
        ):
            raise AwError("source_input_replace_unsupported")
        return isolate_call_args(tuple(new_args), dict(new_kwargs), snapshot_arg)
    if custom_extract:
        raise AwError("source_input_replace_unsupported")
    if len(call_args) == 1 and not call_kwargs and type(call_args[0]) is dict:
        return (deepcopy(revised),), {}
    if not call_args and call_kwargs:
        return (), deepcopy(revised)
    if (
        set(revised) <= {"args", "kwargs"}
        and isinstance(revised.get("args"), list)
        and ("kwargs" not in revised or isinstance(revised["kwargs"], dict))
        and all(is_plain_json(arg) for arg in call_args)
        and all(is_plain_json(val) for val in call_kwargs.values())
    ):
        new_kwargs = deepcopy(revised.get("kwargs", {}))
        if any(not isinstance(k, str) for k in new_kwargs):
            raise AwError("source_input_replace_unsupported")
        return tuple(deepcopy(revised["args"])), new_kwargs
    raise AwError("source_input_replace_unsupported")


def apply_tool_result(
    original: Any,
    replaced: Any,
    *,
    apply_result: Callable[[Any, Any], Any] | None,
    custom_extract: bool,
) -> Any:
    """Apply a checked tool.after replacement without silently leaking original output."""
    if apply_result is not None:
        return apply_result(original, deepcopy(replaced))
    if custom_extract:
        raise AwError("source_result_replace_unsupported")
    if type(original) is str and type(replaced) is str:
        return replaced
    if type(original) is dict and type(replaced) is dict and is_plain_json(replaced):
        return deepcopy(replaced)
    raise AwError("source_result_replace_unsupported")


def apply_stream_result(
    buffered: list[Any],
    replaced: Any,
    *,
    apply_result: Callable[[list[Any], Any], Any] | None,
    custom_extract: bool,
) -> list[Any]:
    """Rebuild buffered stream chunks from an approved tool.after replacement."""
    if apply_result is not None:
        rebuilt = apply_result(buffered, deepcopy(replaced))
        if not isinstance(rebuilt, (list, tuple)):
            raise AwError("source_result_replace_unsupported")
        return list(rebuilt)
    if custom_extract:
        raise AwError("source_result_replace_unsupported")
    if type(replaced) is list:
        return deepcopy(replaced)
    if type(replaced) is str:
        return [replaced]
    raise AwError("source_result_replace_unsupported")


async def deliver_strict_result(
    result: Any,
    *,
    snapshot: dict[str, Any],
    call_id: str,
    observe_result: Callable[..., Any],
    extract_result: Callable[[Any], Any] | None,
    apply_result: Callable[[Any, Any], Any] | None,
    snapshot_result: Callable[[Any], Any] | None,
    buffer_stream: bool,
    max_stream_items: int,
    max_stream_bytes: int,
) -> Any:
    """Isolate, audit and deliver a tool result on a strict-after route."""
    custom_extract = extract_result is not None
    with tool_executed():
        if is_lazy_result(result):
            if inspect.isasyncgen(result) or isinstance(result, AsyncIteratorABC):
                await aclose_lazy_result(result)
                raise AwError("source_stream_withhold_requires_buffering")
            if not buffer_stream:
                close_lazy_result(result)
                raise AwError("source_stream_withhold_requires_buffering")
            checked_items = buffer_sync_iterator(
                result,
                max_stream_items=max_stream_items,
                max_stream_bytes=max_stream_bytes,
                snapshot_result=snapshot_result,
                custom_extract=custom_extract,
            )
            decision = await observe_result(
                checked_items,
                snapshot,
                call_id,
                max_bytes=max_stream_bytes,
                isolate_for_extract=True,
            )
            if "replaced_result" in decision:
                deliver = apply_stream_result(
                    checked_items,
                    decision["replaced_result"],
                    apply_result=apply_result,
                    custom_extract=custom_extract,
                )
            else:
                deliver = [deepcopy(item) for item in checked_items]
            return iter(deliver)
        isolated = isolate_strict_value(
            result,
            snapshot_result=snapshot_result,
            require_plain_json=False,
            custom_extract=custom_extract,
        )
        decision = await observe_result(
            isolated, snapshot, call_id, isolate_for_extract=True
        )
        if "replaced_result" in decision:
            return apply_tool_result(
                isolated,
                decision["replaced_result"],
                apply_result=apply_result,
                custom_extract=custom_extract,
            )
        return deepcopy(isolated) if is_plain_json(isolated) else isolated


async def run_tool_stream(
    session: Any,
    fn: Callable[..., Any],
    call_args: tuple[Any, ...],
    call_kwargs: dict[str, Any],
    *,
    snapshot: dict[str, Any],
    call_id: str,
    observe_result: Callable[..., Any],
    extract_result: Callable[[Any], Any] | None,
    apply_result: Callable[[list[Any], Any], Any] | None,
    snapshot_result: Callable[[Any], Any] | None = None,
    max_stream_items: int,
    max_stream_bytes: int = DEFAULT_MAX_STREAM_BYTES,
    buffer_stream: bool,
) -> AsyncIterator[Any]:
    """Execute a streaming tool, enforcing bounded isolated buffering on strict routes."""
    strict_after = session._strict_after()
    if strict_after and not buffer_stream:
        raise AwError("source_stream_withhold_requires_buffering")
    custom_extract = extract_result is not None
    items: list[Any] | None = [] if session._observe_after() else None
    stream = ActiveStream(fn(*call_args, **call_kwargs))
    session._active_streams.add(stream)

    async def finish(buffered: list[Any], *, strict: bool) -> dict[str, Any] | None:
        await stream.close()
        if session._observe_after():
            return await observe_result(
                buffered,
                snapshot,
                call_id,
                max_bytes=max_stream_bytes if strict else None,
                isolate_for_extract=strict,
            )
        return None

    with tool_executed():
        try:
            if strict_after:
                checked_items: list[Any] = []
                buffered_bytes = 2
                decision: dict[str, Any] | None = None
                while True:
                    session._require_open()
                    try:
                        item = await stream.read()
                    except StopAsyncIteration:
                        session._require_open()
                        decision = await session._run_admitted_call(
                            lambda: finish(checked_items, strict=True)
                        )
                        break
                    session._require_open()
                    if len(checked_items) >= max_stream_items:
                        raise AwError("source_stream_result_too_large")
                    isolated = isolate_strict_value(
                        item,
                        snapshot_result=snapshot_result,
                        require_plain_json=True,
                        custom_extract=custom_extract,
                    )
                    buffered_bytes += json_encoded_bytes(isolated) + (1 if checked_items else 0)
                    if buffered_bytes > max_stream_bytes:
                        raise AwError("source_stream_result_too_large")
                    checked_items.append(isolated)
                if decision is not None and "replaced_result" in decision:
                    deliver = apply_stream_result(
                        checked_items,
                        decision["replaced_result"],
                        apply_result=apply_result,
                        custom_extract=custom_extract,
                    )
                else:
                    deliver = [deepcopy(out) for out in checked_items]
                for out in deliver:
                    session._require_open()
                    yield out
                return
            while True:
                session._require_open()
                try:
                    item = await stream.read()
                except StopAsyncIteration:
                    session._require_open()
                    if items is not None:
                        await session._run_admitted_call(lambda: finish(items, strict=False))
                    break
                session._require_open()
                if items is not None and session._observe_after():
                    if len(items) >= max_stream_items:
                        raise AwError("source_stream_result_too_large")
                    items.append(
                        normalize_json(item)
                        if extract_result is None
                        else snapshot_stream_chunk(item)
                    )
                yield item
        finally:
            try:
                await stream.close()
            finally:
                if stream.closing is not None and stream.closing.done():
                    session._active_streams.discard(stream)


def build_tool_wrapper(
    session: Any,
    fn: Callable[..., Any],
    *,
    name: str | None,
    extract_input: Callable[..., Mapping[str, Any]] | None,
    apply_input: Callable[[dict[str, Any], tuple[Any, ...], dict[str, Any]], Any] | None,
    extract_call_id: Callable[..., str | None] | None,
    extract_result: Callable[[Any], Any] | None,
    apply_result: Callable[[Any, Any], Any] | None,
    snapshot_arg: Callable[[Any], Any] | None,
    snapshot_result: Callable[[Any], Any] | None,
    max_stream_items: int,
    max_stream_bytes: int,
    buffer_stream: bool,
    instrumented_attr: str,
) -> Callable[..., Any]:
    """Construct a wrapped tool callable or async generator bound to one SourceSession."""
    if not callable(fn):
        raise AwError("source_callable_required")
    if (
        type(max_stream_items) is not int
        or max_stream_items < 1
        or type(max_stream_bytes) is not int
        or max_stream_bytes < 1
    ):
        raise AwError("source_invalid_stream_limit")
    tool_name = validate_identity(getattr(fn, "__name__", "") if name is None else name)
    input_extractor = default_extract_input if extract_input is None else extract_input
    custom_extract_input = extract_input is not None
    custom_extract_result = extract_result is not None
    is_sync_gen = inspect.isgeneratorfunction(fn)

    async def admit_call(args: tuple[Any, ...], kwargs: dict[str, Any]):
        call_args, call_kwargs = isolate_call_args(args, kwargs, snapshot_arg)
        raw_input = input_extractor(*call_args, **call_kwargs)
        original_snapshot = snapshot_tool_input(raw_input)
        call_id = (
            extract_call_id(*call_args, **call_kwargs) if extract_call_id is not None else None
        )
        resolved_call, snapshot, decision = await session.before_tool(
            tool_name, raw_input, call_id=call_id
        )
        session._require_open()
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
        result: Any,
        snapshot: dict[str, Any],
        call_id: str,
        *,
        max_bytes: int | None = None,
        isolate_for_extract: bool = False,
    ) -> dict[str, Any]:
        with tool_executed():
            projected = prepare_audited_result(
                result,
                extract_result=extract_result,
                max_bytes=max_bytes,
                isolate_for_extract=isolate_for_extract,
            )
            return await session._after_admitted_tool(
                tool_name, snapshot, projected, call_id=call_id, normalized=True
            )

    if inspect.isasyncgenfunction(fn):

        @functools.wraps(fn)
        async def wrapped_stream(*args: Any, **kwargs: Any) -> AsyncIterator[Any]:
            resolved_call, snapshot, call_args, call_kwargs = await admit_call(args, kwargs)
            session._require_open()
            inner = run_tool_stream(
                session,
                fn,
                call_args,
                call_kwargs,
                snapshot=snapshot,
                call_id=resolved_call,
                observe_result=observe_result,
                extract_result=extract_result,
                apply_result=apply_result,
                snapshot_result=snapshot_result,
                max_stream_items=max_stream_items,
                max_stream_bytes=max_stream_bytes,
                buffer_stream=buffer_stream,
            )
            try:
                async for item in inner:
                    yield item
            finally:
                await inner.aclose()

        setattr(wrapped_stream, instrumented_attr, True)
        return wrapped_stream

    @functools.wraps(fn)
    async def wrapped(*args: Any, **kwargs: Any) -> Any:
        resolved_call, snapshot, call_args, call_kwargs = await admit_call(args, kwargs)
        if is_sync_gen and session._strict_after() and not buffer_stream:
            raise AwError("source_stream_withhold_requires_buffering")

        async def invoke() -> Any:
            result = fn(*call_args, **call_kwargs)
            if inspect.isawaitable(result):
                result = await result
            if session._strict_after():
                return await deliver_strict_result(
                    result,
                    snapshot=snapshot,
                    call_id=resolved_call,
                    observe_result=observe_result,
                    extract_result=extract_result,
                    apply_result=apply_result,
                    snapshot_result=snapshot_result,
                    buffer_stream=buffer_stream,
                    max_stream_items=max_stream_items,
                    max_stream_bytes=max_stream_bytes,
                )
            if session._observe_after():
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

        return await session._run_admitted_call(invoke)

    setattr(wrapped, instrumented_attr, True)
    return wrapped
