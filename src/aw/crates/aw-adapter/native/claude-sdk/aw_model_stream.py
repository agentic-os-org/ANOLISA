"""Rewrite only client tool identities at the Anthropic message production boundary."""

from __future__ import annotations

import json
import re
import sys
from collections.abc import AsyncIterator
from typing import Any

if __package__:
    from .aw_claude import CallbackFailure
    from .aw_invocations import Invocations, Invocation
else:
    from aw_claude import CallbackFailure
    from aw_invocations import Invocations, Invocation

MAX_FRAME = 1024 * 1024

_LINE_END = re.compile(rb"\r\n|[\r\n]")
_EVENTS = (
    "message_start", "content_block_start", "content_block_delta", "content_block_stop",
    "message_delta", "message_stop", "ping", "error",
)


def relay_log(label: str, fields: dict) -> None:
    print(f"AW managed model response {label}", *(f"{key}={value}" for key, value in fields.items()),
          file=sys.stderr, flush=True)


def event_name(name: object) -> str:
    return name if isinstance(name, str) and name in _EVENTS else "other"


async def sse_frames(chunks: AsyncIterator[bytes], details: dict) -> AsyncIterator[tuple[bytes, bool]]:
    """Yield bounded frames and distinguish a blank line from an unfinished EOF."""
    frame = bytearray()
    line_start = 0
    trailing_cr = False
    async for chunk in chunks:
        if not chunk:
            continue
        pos = int(trailing_cr and chunk.startswith(b"\n"))
        if pos and frame:
            frame.extend(b"\n")
            line_start += 1
        while pos < len(chunk):
            ending = _LINE_END.search(chunk, pos)
            end = ending.start() if ending else len(chunk)
            size = len(frame) + end - pos
            if end > pos and size > MAX_FRAME:
                details.update(stage="sse_frame", frame_bytes=size)
                raise CallbackFailure("AW model SSE frame exceeds one MiB")
            frame.extend(memoryview(chunk)[pos:end])
            if not ending:
                break
            if len(frame) == line_start:
                yield bytes(frame), True
                frame.clear()
            else:
                frame.extend(ending[0])
            line_start = len(frame)
            pos = ending.end()
        trailing_cr = chunk.endswith(b"\r")
    if frame:
        yield bytes(frame), False


def sse_fields(frame: bytes) -> tuple[bytes, int, str]:
    data = bytearray()
    count, event = 0, "other"
    for line in re.finditer(rb"[^\r\n]+", frame):
        field, _, value = line[0].partition(b":")
        value = value.removeprefix(b" ")
        if field == b"data":
            if count:
                data.extend(b"\n")
            data.extend(value)
            count += 1
        elif field == b"event":
            event = event_name(value.decode("ascii", errors="replace"))
    return bytes(data), count, event


class MessageStream:
    """Forward streaming text while withholding tool completion until identity admission."""

    def __init__(self, invocations: Invocations, details: dict | None = None) -> None:
        self.details = {} if details is None else details
        self.warned: set[str] = set()
        self.invocations = invocations
        self.blocks: dict[int, tuple[Invocation | None, str, Any]] = {}
        self.started = False
        self.stopped = False
        self.next_index = 0

    def warn_once(self, reason: str, **counts: int) -> None:
        if reason not in self.warned:
            self.warned.add(reason)
            relay_log("warning:", {"reason": reason, **counts})

    async def event(self, value: dict[str, Any]) -> tuple[dict[str, Any], ...]:
        kind = value.get("type")
        if self.stopped:
            raise CallbackFailure("AW model stream continued after its terminal event")
        if kind == "message_start":
            if self.started or value.get("message", {}).get("content") != []:
                raise CallbackFailure("AW model stream has an unsupported message start")
            self.started = True
        elif kind == "content_block_start":
            index, block = value["index"], value["content_block"]
            if (
                not self.started
                or type(index) is not int
                or index != self.next_index
                or self.blocks
            ):
                raise CallbackFailure("AW model block identity is invalid")
            self.next_index += 1
            if block.get("type") not in {"text", "thinking", "redacted_thinking", "tool_use"}:
                raise CallbackFailure("AW model response block type is unsupported")
            entry = None
            initial = block.get("input", {})
            if block["type"] == "tool_use":
                entry = self.invocations.produce(block["name"])
                block["id"] = entry.call_id
                block["input"] = {}
            self.blocks[index] = (entry, "", initial)
        elif kind == "content_block_delta":
            index, delta = value["index"], value["delta"]
            if index not in self.blocks:
                raise CallbackFailure("AW model delta has no active block")
            entry, partial, initial = self.blocks[index]
            if entry is not None:
                if delta.get("type") != "input_json_delta" or not isinstance(
                    delta.get("partial_json"), str
                ):
                    raise CallbackFailure("AW model tool input delta is invalid")
                partial += delta["partial_json"]
                if len(partial.encode()) > MAX_FRAME:
                    raise CallbackFailure("AW model tool input exceeds one MiB")
                self.blocks[index] = (entry, partial, initial)
        elif kind == "content_block_stop":
            if value["index"] not in self.blocks:
                raise CallbackFailure("AW model block was already completed")
            entry, partial, initial = self.blocks.pop(value["index"])
            if entry is not None:
                self.details.update(stage="tool_input_json", partial_bytes=len(partial.encode()))
                has_json = bool(partial.strip(" \t\r\n"))
                if has_json:
                    arguments = json.loads(partial)
                else:
                    if partial:
                        self.warn_once("blank_tool_input", partial_bytes=self.details["partial_bytes"])
                    arguments = initial
                self.details["stage"] = "stream_event"
                await self.invocations.register(entry, arguments)
                if not has_json:
                    # Claude builds tool input from deltas, including the initial-input fallback.
                    encoded = json.dumps(arguments, ensure_ascii=False, separators=(",", ":"))
                    return ({
                        "type": "content_block_delta", "index": value["index"],
                        "delta": {"type": "input_json_delta", "partial_json": encoded},
                    }, value)
        elif kind == "message_stop":
            if not self.started or self.blocks:
                raise CallbackFailure("AW model stream ended with incomplete tool inputs")
            self.stopped = True
        elif kind not in {"message_delta", "ping"}:
            raise CallbackFailure("AW model stream event is unsupported or failed")
        return (value,)


async def rewrite_message(value: dict[str, Any], invocations: Invocations) -> dict[str, Any]:
    if value.get("type") != "message" or not isinstance(value.get("content"), list):
        raise CallbackFailure("AW requires an Anthropic message response")
    for block in value["content"]:
        if block.get("type") not in {"text", "thinking", "redacted_thinking", "tool_use"}:
            raise CallbackFailure("AW model response block type is unsupported")
        if block["type"] == "tool_use":
            entry = invocations.produce(block["name"])
            block["id"] = entry.call_id
            await invocations.register(entry, block["input"])
    return value
