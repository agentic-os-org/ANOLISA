"""Observability capability — records Hermes agent-loop hooks via agent-sec-cli."""

from __future__ import annotations

import logging
import os
import threading
from collections.abc import Callable
from typing import Any

from ..cli_runner import record_hermes_observability
from ..hook_config import env_flag_enabled
from ..observability.record import build_record
from .base import AgentSecCoreCapability

logger = logging.getLogger("agent-sec-core")
_LOG_DETAIL_MAX_CHARS = 1000
_MAX_OBSERVABILITY_TIMEOUT_SECONDS = 5.0


def _read_observability_timeout(default: float) -> float:
    bounded_default = min(default, _MAX_OBSERVABILITY_TIMEOUT_SECONDS)
    raw_timeout = os.environ.get("OBSERVABILITY_TIMEOUT")
    if raw_timeout is None:
        return bounded_default
    try:
        timeout = int(raw_timeout)
    except ValueError:
        return bounded_default
    if timeout <= 0:
        return bounded_default
    return min(timeout, _MAX_OBSERVABILITY_TIMEOUT_SECONDS)


def _log_detail(value: Any) -> str:
    if value is None:
        return ""
    if isinstance(value, bytes):
        text = value.decode("utf-8", errors="replace")
    else:
        text = str(value)
    text = " ".join(text.strip().split())
    if len(text) <= _LOG_DETAIL_MAX_CHARS:
        return text
    return text[:_LOG_DETAIL_MAX_CHARS] + "...<truncated>"


def _format_error(error: Exception) -> str:
    message = _log_detail(error)
    if message:
        return f"{error.__class__.__name__}: {message}"
    return error.__class__.__name__


class ObservabilityCapability(AgentSecCoreCapability):
    id = "observability"
    name = "Observability"

    def _on_register(self, config: dict) -> None:
        self._hook_enabled = env_flag_enabled("OBSERVABILITY_HOOK_ENABLED", True)
        self._timeout = _read_observability_timeout(self._timeout)

    def get_hooks_define(self) -> dict[str, Callable]:
        return {
            "pre_llm_call": self._on_pre_llm_call,
            "pre_api_request": self._on_pre_api_request,
            "post_api_request": self._on_post_api_request,
            "pre_tool_call": self._on_pre_tool_call,
            "post_tool_call": self._on_post_tool_call,
            "post_llm_call": self._on_post_llm_call,
        }

    def _emit(self, hook_name: str, data: dict[str, Any]) -> None:
        if not getattr(self, "_hook_enabled", True):
            return

        record = build_record(hook_name, data)
        if record is None:
            return
        thread = threading.Thread(
            target=self._record,
            args=(record,),
            name="agent-sec-observability-record",
            daemon=True,
        )
        thread.start()

    def _record(self, record: dict[str, Any]) -> None:
        hook = _log_detail(record.get("hook")) or "unknown"
        try:
            result = record_hermes_observability(record, timeout=self._timeout)
        except Exception as error:
            logger.warning(
                f"[agent-sec-core] observability record error hook={hook} error={_format_error(error)}"
            )
            return

        if result.exit_code != 0:
            fields = [
                "[agent-sec-core] observability record failed",
                f"hook={hook}",
                f"exit_code={result.exit_code}",
            ]
            stderr = _log_detail(result.stderr)
            if stderr:
                fields.append(f"stderr={stderr}")
            stdout = _log_detail(result.stdout)
            if stdout:
                fields.append(f"stdout={stdout}")
            logger.warning(" ".join(fields))

    def _on_pre_llm_call(self, messages: Any = None, **kwargs: Any) -> None:
        data = dict(kwargs)
        if messages is not None:
            data.setdefault("conversation_history", messages)
        self._emit("pre_llm_call", data)
        return None

    def _on_pre_api_request(self, **kwargs: Any) -> None:
        self._emit("pre_api_request", dict(kwargs))
        return None

    def _on_post_api_request(self, **kwargs: Any) -> None:
        self._emit("post_api_request", dict(kwargs))
        return None

    def _on_pre_tool_call(
        self,
        *,
        tool_name: Any,
        args: Any,
        **kwargs: Any,
    ) -> None:
        data = {"tool_name": tool_name, "args": args, **kwargs}
        self._emit("pre_tool_call", data)
        return None

    def _on_post_tool_call(
        self,
        *,
        tool_name: Any,
        args: Any,
        result: Any,
        **kwargs: Any,
    ) -> None:
        data: dict[str, Any] = {
            "tool_name": tool_name,
            "args": args,
            "result": result,
            **kwargs,
        }
        self._emit("post_tool_call", data)
        return None

    def _on_post_llm_call(
        self,
        messages: Any = None,
        response: Any = None,
        **kwargs: Any,
    ) -> None:
        data = dict(kwargs)
        if messages is not None:
            data.setdefault("conversation_history", messages)
        if response is not None:
            data.setdefault("assistant_response", response)
        self._emit("post_llm_call", data)
        return None
