"""Rewrite only client tool identities at the Anthropic message production boundary."""

from __future__ import annotations

import json
from typing import Any

from aw_claude import CallbackFailure
from aw_invocations import Invocations, Invocation

MAX_FRAME = 1024 * 1024


class MessageStream:
    """Forward streaming text while withholding tool completion until identity admission."""

    def __init__(self, invocations: Invocations) -> None:
        self.invocations = invocations
        self.blocks: dict[int, tuple[Invocation | None, str, Any]] = {}
        self.started = False
        self.stopped = False
        self.next_index = 0

    async def event(self, value: dict[str, Any]) -> dict[str, Any]:
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
            if block["type"] == "tool_use":
                entry = self.invocations.produce(block["name"])
                block["id"] = entry.call_id
            self.blocks[index] = (entry, "", block.get("input", {}))
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
                await self.invocations.register(entry, json.loads(partial) if partial else initial)
        elif kind == "message_stop":
            if not self.started or self.blocks:
                raise CallbackFailure("AW model stream ended with incomplete tool inputs")
            self.stopped = True
        elif kind not in {"message_delta", "ping"}:
            raise CallbackFailure("AW model stream event is unsupported or failed")
        return value


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
