"""Snapshot one text submission and close its policy event before native dispatch."""

from __future__ import annotations

import asyncio
import json
from typing import Any
from uuid import uuid4

import anyio

from aw_claude import CallbackFailure


class SubmissionDenied(ValueError):
    """This input was rejected before native dispatch; the client remains usable."""


def identity(value: Any) -> str:
    if not isinstance(value, str) or not value.strip() or len(value.encode()) > 512:
        raise SubmissionDenied("AW submission requires bounded session identity")
    return value


def snapshot(message: Any, session: str) -> tuple[dict[str, Any], str, str]:
    """Preserve supported SDK fields and reject content without a text projection."""
    if (not isinstance(message, dict)
            or set(message) - {"type", "message", "parent_tool_use_id", "session_id", "uuid"}
            or message.get("type") != "user"
            or message.get("parent_tool_use_id") is not None):
        raise SubmissionDenied("AW submission requires one top-level user text message")
    body = message.get("message")
    if (not isinstance(body, dict) or set(body) != {"role", "content"}
            or body["role"] != "user"):
        raise SubmissionDenied("AW submission requires user text content")
    content = body["content"]
    if isinstance(content, str):
        text = content
    elif isinstance(content, list) and content and all(
        isinstance(block, dict) and set(block) == {"type", "text"}
        and block["type"] == "text" and isinstance(block["text"], str)
        for block in content
    ):
        text = "\n".join(block["text"] for block in content)
    else:
        raise SubmissionDenied("AW submission supports text only")
    session = identity(message.get("session_id", session))
    if "uuid" in message:
        identity(message["uuid"])
    # The caller retains its message; later mutation cannot change checked input.
    copied = json.loads(json.dumps(message, ensure_ascii=False, allow_nan=False))
    return copied, text, session


async def prepare(prompt: Any, session: str, cwd: str, budget_ms: int) -> tuple[Any, dict[str, Any]]:
    session = identity(session)
    native = prompt
    if isinstance(prompt, str):
        text = prompt
    else:
        from aw_transport import single_input

        async with asyncio.timeout(budget_ms / 1000):
            message = await single_input(prompt)
        message, text, session = snapshot(message, session)

        async def inputs():
            yield message

        native = inputs()
    if not text.strip():
        raise SubmissionDenied("AW submission requires non-blank text")
    payload = {"hook_event_name": "AWInputSubmit", "session_id": session,
               "aw_turn_id": str(uuid4()), "cwd": cwd, "input_text": text}
    # Leave room for normalized input, native evidence and the scope envelope in
    # the one-MiB transport. Count escaped JSON bytes, including control characters.
    if len(json.dumps(payload, ensure_ascii=False, allow_nan=False).encode()) > 256 * 1024:
        raise SubmissionDenied("AW submission exceeds 256 KiB of encoded input")
    return native, payload


async def check(bridge: Any, payload: dict[str, Any]) -> None:
    hooks = [hook for hook in bridge.config["hooks"] if hook["event"] == "input.submit"]
    if not hooks:
        return
    if hasattr(bridge, "scope"):
        payload = {**payload, "aw_turn_id": await bridge.scope.next_submission()}
    try:
        for hook in hooks:
            code, stdout = await bridge._transport(hook, payload)
            if code == 2:
                reason = json.loads(stdout.strip() or b'"Input blocked by AW policy"')
                if not isinstance(reason, str):
                    raise CallbackFailure("invalid_submission_response")
                raise SubmissionDenied(reason)
            if code != 0 or stdout.strip():
                raise CallbackFailure("invalid_submission_response")
    finally:
        async def close():
            code, stdout = await bridge._transport(
                {**hooks[0], "close_event": True, "budget_ms": 1}, payload,
            )
            if code != 0 or stdout.strip():
                raise CallbackFailure("AW input event cleanup failed")

        # CloseInputEvent cancels remaining steps and joins scanner cleanup. The
        # turn stays occupied until its acknowledgement even when query is cancelled.
        with anyio.CancelScope(shield=True):
            cleanup = asyncio.create_task(close())
            cancelled = False
            while not cleanup.done():
                try:
                    await asyncio.shield(cleanup)
                except asyncio.CancelledError:
                    # Direct Task.cancel() bypasses the AnyIO shield. Every wait
                    # must shield the bounded close task, including later cancels.
                    cancelled = True
            if cleanup.cancelled():
                raise CallbackFailure("AW input event cleanup was cancelled")
            cleanup.result()
            if cancelled:
                raise asyncio.CancelledError
