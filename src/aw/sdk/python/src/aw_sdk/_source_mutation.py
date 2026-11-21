"""Capabilities, exceptions, input/result rewrites and strict stream helpers for aw_sdk.sources."""

from __future__ import annotations

from collections.abc import AsyncIterator as AsyncIteratorABC
from collections.abc import Iterator as IteratorABC
from copy import deepcopy
import inspect
import json
from typing import Any, AsyncIterator, Callable, Iterable, Mapping

from aw_sdk._source_helpers import (
    ActiveStream,
    is_plain_json,
    isolate_call_args,
    normalize_json,
    snapshot_stream_chunk,
    snapshot_tool_input,
    tool_executed,
)
from aw_sdk.transport import AwError

SOURCE_PROTOCOL = "aw-hook-source/v1"
DEFAULT_MAX_STREAM_BYTES = 1024 * 1024
SUPPORTED_EVENTS: dict[str, tuple[str, ...]] = {
    "input.submit": ("observe", "block"),
    "tool.before": ("observe", "block"),
    "tool.after": ("observe",),
}
DEFAULT_EVENTS: tuple[str, ...] = ("input.submit", "tool.before", "tool.after")


class SourceDenied(AwError):
    """Raised when AW returns a block decision before native work starts."""

    def __init__(
        self,
        event: str,
        *,
        session_id: str,
        call_id: str | None = None,
        turn_id: str | None = None,
        tool_name: str | None = None,
    ) -> None:
        super().__init__("source_denied", unavailable=False)
        self.event = event
        self.session_id = session_id
        self.call_id = call_id
        self.turn_id = turn_id
        self.tool_name = tool_name


class SourceResultWithheld(AwError):
    """Raised when tool.after fails with on_error: withhold_result."""

    def __init__(
        self,
        event: str = "tool.after",
        *,
        session_id: str,
        call_id: str | None = None,
        tool_name: str | None = None,
    ) -> None:
        super().__init__("source_result_withheld", unavailable=False)
        self.event = event
        self.session_id = session_id
        self.call_id = call_id
        self.tool_name = tool_name
        self.tool_executed = True


def source_capabilities(
    events: Iterable[str] = DEFAULT_EVENTS,
    *,
    replace_input: bool = False,
    replace_result: bool = False,
    withhold_result: bool = False,
) -> dict[str, Any]:
    """Build canonical aw-hook-source/v1 capabilities for the selected events."""
    selected = list(dict.fromkeys(events))
    if not selected or any(
        not isinstance(event, str) or event not in SUPPORTED_EVENTS for event in selected
    ):
        raise AwError("semantic_event_unavailable")
    if replace_input and "tool.before" not in selected:
        raise AwError("semantic_event_unavailable")
    if (replace_result or withhold_result) and "tool.after" not in selected:
        raise AwError("semantic_event_unavailable")
    declared: dict[str, list[str]] = {event: list(SUPPORTED_EVENTS[event]) for event in selected}
    if replace_input:
        declared["tool.before"].append("replace_input")
    if replace_result:
        declared["tool.after"].append("replace_result")
    caps: dict[str, Any] = {
        "adapter": "sdk",
        "version": SOURCE_PROTOCOL,
        "entrypoint": "application",
        "events": declared,
    }
    if withhold_result:
        caps["failure_actions"] = {"tool.after": ["withhold_result"]}
    return caps


def confirm_source_routes(capabilities: Mapping[str, Any], routes: Mapping[str, Any]) -> bool:
    """Confirm that every route matches declared semantic events, effects and failure actions."""
    events = capabilities.get("events")
    failure_actions = capabilities.get("failure_actions") or {}
    hooks = routes.get("hooks") if isinstance(routes, Mapping) else None
    if (
        not isinstance(events, Mapping)
        or not isinstance(failure_actions, Mapping)
        or not isinstance(hooks, list)
    ):
        return False
    for hook in hooks:
        if not isinstance(hook, Mapping) or hook.get("skill_check"):
            return False
        event = hook.get("event")
        if event not in events:
            return False
        allowed_effects = events.get(event) or ()
        if hook.get("replace_input") and "replace_input" not in allowed_effects:
            return False
        if hook.get("replace_result") and "replace_result" not in allowed_effects:
            return False
        if hook.get("on_error") == "withhold_result" and "withhold_result" not in (
            failure_actions.get(event) or ()
        ):
            return False
    return True


def route_has_strict_after(routes: Mapping[str, Any] | None) -> bool:
    """Return whether admitted tool.after routes require replacement or withholding."""
    hooks = routes.get("hooks") if isinstance(routes, Mapping) else None
    if not isinstance(hooks, list):
        return False
    return any(
        isinstance(hook, Mapping)
        and hook.get("event") == "tool.after"
        and (bool(hook.get("replace_result")) or hook.get("on_error") == "withhold_result")
        for hook in hooks
    )


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


def isolate_strict_value(
    value: Any,
    *,
    snapshot_result: Callable[[Any], Any] | None = None,
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
        if copied is value:
            raise AwError("source_result_isolation_required")
        return copied
    if custom_extract:
        if type(value) in (bytes, complex):
            return value
        try:
            copied = deepcopy(value)
        except Exception as error:
            raise AwError("source_result_isolation_required") from error
        if copied is value:
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
    """Consume a sync iterator/generator into bounded isolated snapshots."""
    checked: list[Any] = []
    buffered_bytes = 0
    try:
        for item in iterator:
            if len(checked) >= max_stream_items:
                raise AwError("source_stream_result_too_large")
            isolated = isolate_strict_value(
                item, snapshot_result=snapshot_result, custom_extract=custom_extract
            )
            encoded_source = normalize_json(isolated) if custom_extract else isolated
            buffered_bytes += json_encoded_bytes(encoded_source)
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
    observe_result: Callable[[Any, dict[str, Any], str], Any],
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
            decision = await observe_result(checked_items, snapshot, call_id)
            if "replaced_result" in decision:
                deliver = apply_stream_result(
                    checked_items,
                    decision["replaced_result"],
                    apply_result=apply_result,
                    custom_extract=custom_extract,
                )
            else:
                deliver = [deepcopy(item) if is_plain_json(item) else item for item in checked_items]
            return iter(deliver)
        isolated = isolate_strict_value(
            result, snapshot_result=snapshot_result, custom_extract=custom_extract
        )
        decision = await observe_result(isolated, snapshot, call_id)
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
    observe_result: Callable[[Any, dict[str, Any], str], Any],
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

    async def finish(buffered: list[Any]) -> dict[str, Any] | None:
        await stream.close()
        if session._observe_after():
            return await observe_result(buffered, snapshot, call_id)
        return None

    with tool_executed():
        try:
            if strict_after:
                checked_items: list[Any] = []
                buffered_bytes = 0
                decision: dict[str, Any] | None = None
                while True:
                    session._require_open()
                    try:
                        item = await stream.read()
                    except StopAsyncIteration:
                        session._require_open()
                        decision = await session._run_admitted_call(
                            lambda: finish(checked_items)
                        )
                        break
                    session._require_open()
                    if len(checked_items) >= max_stream_items:
                        raise AwError("source_stream_result_too_large")
                    isolated = isolate_strict_value(
                        item,
                        snapshot_result=snapshot_result,
                        custom_extract=custom_extract,
                    )
                    encoded_source = normalize_json(isolated) if custom_extract else isolated
                    buffered_bytes += json_encoded_bytes(encoded_source)
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
                    deliver = [
                        deepcopy(out) if is_plain_json(out) else out for out in checked_items
                    ]
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
                        await session._run_admitted_call(lambda: finish(items))
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
