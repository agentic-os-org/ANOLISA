"""Rewrite only client tool identities at the Anthropic message production boundary."""

from __future__ import annotations

import json
import math
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

_SURROGATE = re.compile(r"[\ud800-\udfff]")
_LINE_END = re.compile(rb"\r\n|[\r\n]")
_EVENTS = (
    "message_start", "content_block_start", "content_block_delta", "content_block_stop",
    "message_delta", "message_stop", "ping", "error",
)

# Some Python decoders include the offending character in escape errors.
_JSON_ERRORS = {
    "Expecting value", "Extra data", "Expecting property name enclosed in double quotes",
    "Expecting ':' delimiter", "Expecting ',' delimiter", "Unterminated string starting at",
    r"Invalid \escape", r"Invalid \uXXXX escape", "Invalid control character at",
    "Illegal trailing comma before end of object", "Illegal trailing comma before end of array",
    "Unexpected UTF-8 BOM (decode using utf-8-sig)",
}


def json_error_label(error: json.JSONDecodeError) -> str:
    return error.msg if error.msg in _JSON_ERRORS else "Invalid JSON"


def reject_constant(_: str) -> None:
    # JSON.parse rejects all three named non-finite constants.
    raise ValueError("Non-standard JSON constant")


def native_number(token: str) -> int | float | None:
    # JSON.stringify sends a binary64 overflow to the callback as null.
    number = float(token)
    if not math.isfinite(number):
        return None
    return int(token) if token.lstrip("-").isdigit() else number


def decode_json(text: str) -> Any:
    return json.loads(text, parse_constant=reject_constant, parse_int=native_number, parse_float=native_number)


def fits_service_input(value: Any, depth: int = 0) -> bool:
    # The IPC reader allows 40 containers, including the request and operation.
    if isinstance(value, (dict, list)):
        if depth >= 38:
            return False
        if isinstance(value, dict):
            return all(fits_service_input(key) and fits_service_input(item, depth + 1)
                       for key, item in value.items())
        return all(fits_service_input(item, depth + 1) for item in value)
    return not isinstance(value, str) or not _SURROGATE.search(value)


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
        self.blocks: dict[int, tuple[Invocation | None, bytearray, Any]] = {}
        self.started = False
        self.stopped = False
        self.next_index = 0

    def warn_once(self, reason: str, **fields: int | str) -> None:
        if reason not in self.warned:
            self.warned.add(reason)
            relay_log("warning:", {"reason": reason, **fields})

    async def event(self, value: dict[str, Any]) -> tuple[dict[str, Any], ...]:
        kind = value.get("type")
        if self.stopped:
            raise CallbackFailure("AW model stream continued after its terminal event")
        if kind == "message_start":
            if self.started or value.get("message", {}).get("content", []) != []:
                raise CallbackFailure("AW model stream has an unsupported message start")
            self.started = True
        elif kind == "content_block_start":
            index, block = value["index"], value["content_block"]
            if (
                not self.started
                or type(index) is not int
                or index < self.next_index
                or self.blocks
            ):
                raise CallbackFailure("AW model block identity is invalid")
            self.next_index = index + 1
            if block.get("type") not in {"text", "thinking", "redacted_thinking", "tool_use"}:
                raise CallbackFailure("AW model response block type is unsupported")
            entry = None
            initial = block.get("input", {})
            if block["type"] == "tool_use":
                entry = self.invocations.produce(block["name"])
                block["id"] = entry.call_id
                block["input"] = {}
            self.blocks[index] = (entry, bytearray(), initial)
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
                partial.extend(delta["partial_json"].encode(errors="surrogatepass"))
                if len(partial) > MAX_FRAME:
                    raise CallbackFailure("AW model tool input exceeds one MiB")
                return ()
        elif kind == "content_block_stop":
            if value["index"] not in self.blocks:
                raise CallbackFailure("AW model block was already completed")
            entry, partial, initial = self.blocks.pop(value["index"])
            if entry is not None:
                partial_bytes = len(partial)
                encoded = partial.decode(errors="surrogatepass")
                self.details.update(stage="tool_input_json", partial_bytes=partial_bytes)
                try:
                    if encoded.strip(" \t\r\n"):
                        arguments = decode_json(encoded)
                        if _SURROGATE.search(encoded):
                            # A delta boundary may split a valid UTF-16 pair.
                            arguments = json.loads(json.dumps(arguments))
                        if not isinstance(arguments, dict):
                            self.warn_once("non_object_tool_input", partial_bytes=partial_bytes)
                            arguments = {}
                            encoded = "{}"
                    else:
                        if partial:
                            self.warn_once("blank_tool_input", partial_bytes=partial_bytes)
                        arguments = initial
                        encoded = json.dumps(initial, ensure_ascii=False, separators=(",", ":"))
                    representable = fits_service_input(arguments)
                except json.JSONDecodeError as error:
                    self.warn_once("invalid_tool_input", partial_bytes=partial_bytes,
                                   json_err=json_error_label(error), json_pos=error.pos)
                    arguments, representable = {}, True
                except ValueError:
                    self.warn_once("invalid_tool_input", partial_bytes=partial_bytes)
                    arguments, representable = {}, True
                except RecursionError:
                    representable = False
                self.details["stage"] = "stream_event"
                if representable:
                    await self.invocations.register(entry, arguments)
                else:
                    # Keep the rejected identity local. An empty wire object lets
                    # the CLI dispatch or report a schema error without decoding
                    # an input its Python SDK or the service cannot represent.
                    self.warn_once("unrepresentable_tool_input", partial_bytes=partial_bytes)
                    entry.phase = "rejected"
                    encoded = "{}"
                # JSON escaping can use six bytes per character in an SSE frame.
                chunk_size = MAX_FRAME // 8
                deltas = ({
                    "type": "content_block_delta", "index": value["index"],
                    "delta": {"type": "input_json_delta", "partial_json": encoded[pos:pos + chunk_size]},
                } for pos in range(0, len(encoded), chunk_size))
                return (*deltas, value)
        elif kind == "message_stop":
            if not self.started or self.blocks:
                raise CallbackFailure("AW model stream ended with incomplete tool inputs")
            self.stopped = True
        elif kind == "error":
            for entry, _, _ in self.blocks.values():
                if entry is not None:
                    if entry.phase != "producing" or entry.sequence != 0:
                        raise CallbackFailure("AW model stream error cannot drop a registered tool")
                    self.invocations.active.pop(entry.call_id, None)
            self.blocks.clear()
            self.stopped = True
            self.warn_once("upstream_stream_error")
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
