"""Scan fresh user text before native reply admission and memory insertion."""

from __future__ import annotations

import asyncio
import logging
from uuid import uuid4


class SubmissionDenied(RuntimeError):
    """A rejected submission must never enter native reasoning."""


def submission_text(inputs):
    """Exclude native resumes and tool returns; retain every fresh user text block."""
    from agentscope.event import (
        ExternalExecutionResultEvent, UserConfirmResultEvent, UserInterruptEvent,
    )
    from agentscope.message import Msg, TextBlock

    if inputs is None or isinstance(inputs, (
        UserConfirmResultEvent, UserInterruptEvent, ExternalExecutionResultEvent,
    )):
        return None
    messages = inputs if isinstance(inputs, list) else [inputs]
    if any(not isinstance(message, Msg) for message in messages):
        raise ValueError("unsupported_submission_input")
    texts = []
    for message in messages:
        if message.role != "user":
            continue
        for block in message.content:
            if not isinstance(block, TextBlock):
                raise ValueError("submission_requires_text_content")
            texts.append(block.text)
    return "\n".join(texts) if texts else None


async def check_submission(owner, input_kwargs):
    """Wait for the immutable task's steps before creating the native reply stream."""
    hooks = [hook for hook in owner.config["hooks"] if hook["event"] == "input.submit"]
    if not hooks:
        return
    payload = None
    mapping_error = None
    try:
        if "inputs" not in input_kwargs:
            raise ValueError("submission_input_missing")
        text = submission_text(input_kwargs["inputs"])
        if text is None:
            return
        payload = {"event": "input.submit", "session_id": owner.session,
                   "aw_turn_id": str(uuid4()), "input_text": text}
    except (TypeError, ValueError) as error:
        mapping_error = error
    for hook in hooks:
        try:
            if mapping_error is not None:
                raise mapping_error
            middleware = owner.evaluate(owner.config, hook)
            middleware.scope = owner.scope
            code, stdout, _stderr = await middleware._invoke(payload)
            if code == 2:
                raise SubmissionDenied("Input blocked by AW policy")
            if code != 0 or stdout.strip():
                raise ValueError("invalid_submission_response")
            owner.check()
        except asyncio.CancelledError:
            raise
        except SubmissionDenied:
            raise
        except Exception as error:
            logging.getLogger(__name__).error(
                "AW input.submit step %s failed: %s; on_error=%s",
                hook["step"], type(error).__name__, hook["on_error"],
            )
            if hook["on_error"] == "block":
                raise SubmissionDenied("AW input check failed; submission blocked") from None
