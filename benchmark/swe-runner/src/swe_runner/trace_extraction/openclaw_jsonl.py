# Copyright 2026 Alibaba Cloud
#
# Licensed under the Apache License, Version 2.0 (the "License");
# you may not use this file except in compliance with the License.
# You may obtain a copy of the License at
#
#     http://www.apache.org/licenses/LICENSE-2.0
#
# Unless required by applicable law or agreed to in writing, software
# distributed under the License is distributed on an "AS IS" BASIS,
# WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
# See the License for the specific language governing permissions and
# limitations under the License.

"""OpenClaw session JSONL trace reconstruction."""

from __future__ import annotations

import json
import logging
import re
from collections import Counter
from collections.abc import Iterable
from pathlib import Path
from typing import Any

from swe_runner.trace_extraction.helpers import extract_issue_id, ns_to_iso, parse_time_value
from swe_runner.trace_extraction.token_counting import count_tokens

logger = logging.getLogger(__name__)

DEFAULT_OPENCLAW_PROFILES_DIR = Path("output/run/openclaw-profiles")


def _read_jsonl(path: Path) -> list[dict[str, Any]]:
    entries: list[dict[str, Any]] = []
    for line_number, raw_line in enumerate(path.read_text(encoding="utf-8").splitlines(), start=1):
        line = raw_line.strip()
        if not line:
            continue
        try:
            entry = json.loads(line)
        except json.JSONDecodeError as exc:
            logger.warning("Skipping malformed OpenClaw JSONL line file=%s line=%s error=%s", path, line_number, exc)
            continue
        if isinstance(entry, dict):
            entries.append(entry)
    return entries


def _timestamp_ns(entry: dict[str, Any]) -> int | None:
    for key in ("timestamp_ns", "timestampNs", "created_at_ns", "createdAtNs"):
        value = entry.get(key)
        if isinstance(value, int):
            return value
    for key in ("timestamp", "created_at", "createdAt", "time"):
        value = entry.get(key)
        if isinstance(value, str) and value.strip():
            return parse_time_value(value)
    return None


def _entry_role(entry: dict[str, Any]) -> str | None:
    role = entry.get("role")
    if isinstance(role, str):
        return role
    message = entry.get("message")
    if isinstance(message, dict):
        message_role = message.get("role")
        if isinstance(message_role, str):
            return message_role
    return None


def _normalize_part(part: dict[str, Any]) -> dict[str, Any]:
    ptype = part.get("type")
    if ptype == "text":
        content = part.get("content")
        if not isinstance(content, str):
            content = part.get("text")
        return {"type": "text", "content": content if isinstance(content, str) else ""}

    if ptype == "thinking":
        content = part.get("thinking")
        if not isinstance(content, str):
            content = part.get("content")
        return {"type": "reasoning", "content": content if isinstance(content, str) else ""}

    if ptype == "toolCall":
        return {
            "type": "tool_call",
            "id": part.get("id", ""),
            "name": part.get("name", ""),
            "arguments": part.get("arguments", {}),
        }

    return part


def _format_assistant_part(part: dict[str, Any]) -> dict[str, Any]:
    """Normalize an assistant output part for display."""
    ptype = part.get("type", "unknown")
    out = {"type": ptype}
    if ptype in ("text", "reasoning"):
        out["content"] = part.get("content", "")
    elif ptype == "tool_call":
        out["id"] = part.get("id", "")
        out["name"] = part.get("name", "")
        out["arguments"] = part.get("arguments", {})
    else:
        out.update(part)
    return out


def _entry_parts(entry: dict[str, Any]) -> list[dict[str, Any]]:
    parts = entry.get("parts")
    if not isinstance(parts, list):
        message = entry.get("message")
        if isinstance(message, dict):
            parts = message.get("parts")
            if not isinstance(parts, list):
                parts = message.get("content")
    if isinstance(parts, list):
        return [_normalize_part(part) for part in parts if isinstance(part, dict)]

    content = entry.get("content")
    if isinstance(content, str) and content:
        return [{"type": "text", "content": content}]

    text = entry.get("text")
    if isinstance(text, str) and text:
        return [{"type": "text", "content": text}]

    message = entry.get("message")
    if isinstance(message, dict):
        content = message.get("content")
        if isinstance(content, str) and content:
            return [{"type": "text", "content": content}]
        text = message.get("text")
        if isinstance(text, str) and text:
            return [{"type": "text", "content": text}]

    return []


def _text_from_parts(parts: list[dict[str, Any]]) -> str | None:
    texts = [part.get("content") for part in parts if part.get("type") == "text" and isinstance(part.get("content"), str)]
    return "\n".join(text for text in texts if text) or None


def _entry_message(entry: dict[str, Any]) -> dict[str, Any]:
    message = entry.get("message")
    return message if isinstance(message, dict) else {}


def _entry_value(entry: dict[str, Any], key: str) -> Any:
    message = _entry_message(entry)
    return message.get(key, entry.get(key))


def _tool_response_from_entry(entry: dict[str, Any], parts: list[dict[str, Any]]) -> dict[str, Any]:
    details = _entry_value(entry, "details")
    is_error = _entry_value(entry, "isError")
    response = _text_from_parts(parts)
    if response is None and isinstance(details, dict) and isinstance(details.get("aggregated"), str):
        response = details["aggregated"]

    tool_response: dict[str, Any] = {
        "tool_call_id": _entry_value(entry, "toolCallId") or "",
        "tool_name": _entry_value(entry, "toolName") or "",
        "response": response or "",
    }
    if isinstance(is_error, bool):
        tool_response["is_error"] = is_error
    if isinstance(details, dict):
        tool_response["details"] = details
    return tool_response


def _entry_usage(entry: dict[str, Any]) -> dict[str, Any]:
    usage = entry.get("usage")
    if not isinstance(usage, dict):
        message = _entry_message(entry)
        if isinstance(message.get("usage"), dict):
            usage = message["usage"]
    return usage if isinstance(usage, dict) else {}


def _usage_int(usage: dict[str, Any], *keys: str) -> int:
    for key in keys:
        value = usage.get(key)
        if isinstance(value, int):
            return value
        if isinstance(value, float):
            return int(value)
    return 0


def _usage_float(usage: dict[str, Any], *keys: str) -> float | None:
    for key in keys:
        value = usage.get(key)
        if isinstance(value, int | float):
            return float(value)
    return None


def _normalize_usage(usage: dict[str, Any]) -> dict[str, int | float]:
    normalized: dict[str, int | float] = {}
    input_keys = ("input", "input_tokens", "prompt_tokens", "prompt")
    output_keys = ("output", "output_tokens", "completion_tokens", "completion")
    cache_read_keys = ("cacheRead", "cache_read", "cache_read_tokens", "cached_tokens")
    cache_write_keys = ("cacheWrite", "cache_write", "cache_write_tokens")
    reasoning_keys = ("reasoningTokens", "reasoning_tokens")
    total_keys = ("totalTokens", "total_tokens", "total")
    input_tokens = _usage_int(usage, *input_keys)
    output_tokens = _usage_int(usage, *output_keys)
    cache_read = _usage_int(usage, *cache_read_keys)
    cache_write = _usage_int(usage, *cache_write_keys)
    reasoning_tokens = _usage_int(usage, *reasoning_keys)
    total_tokens = _usage_int(usage, *total_keys)
    cost = _usage_float(usage, "cost", "estimated_cost", "estimatedCost")

    if input_tokens or any(key in usage for key in input_keys):
        normalized["input_tokens"] = input_tokens
    if output_tokens or any(key in usage for key in output_keys):
        normalized["output_tokens"] = output_tokens
    if cache_read or any(key in usage for key in cache_read_keys):
        normalized["cache_read_tokens"] = cache_read
    if cache_write or any(key in usage for key in cache_write_keys):
        normalized["cache_write_tokens"] = cache_write
    if reasoning_tokens or any(key in usage for key in reasoning_keys):
        normalized["reasoning_tokens"] = reasoning_tokens
    if total_tokens or any(key in usage for key in total_keys):
        normalized["total_tokens"] = total_tokens
    if cost is not None:
        normalized["cost"] = cost
    return normalized


def _entry_model(entry: dict[str, Any]) -> str | None:
    for key in ("model", "modelId", "runtimeModel"):
        value = entry.get(key)
        if isinstance(value, str) and value:
            return value
    message = entry.get("message")
    if isinstance(message, dict):
        value = message.get("model")
        if isinstance(value, str) and value:
            return value
    usage = _entry_usage(entry)
    value = usage.get("model")
    return value if isinstance(value, str) and value else None


def _entry_provider(entry: dict[str, Any]) -> str | None:
    value = entry.get("provider")
    if isinstance(value, str) and value:
        return value
    message = entry.get("message")
    if isinstance(message, dict):
        value = message.get("provider")
        if isinstance(value, str) and value:
            return value
    usage = _entry_usage(entry)
    value = usage.get("provider")
    return value if isinstance(value, str) and value else None


def _session_id_from_entries(path: Path, entries: list[dict[str, Any]]) -> str:
    for entry in entries:
        if entry.get("type") == "session":
            raw_id = entry.get("id")
            if isinstance(raw_id, str) and raw_id:
                return raw_id
    return path.stem


def _local_agent_id_from_session_file(path: Path) -> str | None:
    """Return the local OpenClaw agent id from <profile>/agents/<agent>/sessions/<session>.jsonl."""
    if len(path.parents) < 3:
        return None
    if path.parent.name != "sessions":
        return None
    if path.parents[2].name != "agents":
        return None
    agent_id = path.parents[1].name
    return agent_id if agent_id and agent_id != "main" else None


def _tool_call_parts(parts: list[dict[str, Any]]) -> list[dict[str, Any]]:
    return [part for part in parts if part.get("type") == "tool_call"]


def _tool_name(value: Any) -> str:
    return value if isinstance(value, str) and value else "__unknown__"


def _tool_response_text_metrics(responses: list[dict[str, Any]]) -> dict[str, int]:
    chars = 0
    lines = 0
    tokens = 0
    for response in responses:
        text = response.get("response")
        if not isinstance(text, str):
            continue
        chars += len(text)
        lines += len(text.splitlines()) if text else 0
        tokens += count_tokens(text)
    return {
        "tool_response_chars": chars,
        "tool_response_lines": lines,
        "tool_response_tokens_approx": tokens,
    }


def _is_failed_tool_response(response: dict[str, Any]) -> bool:
    is_error = response.get("is_error")
    if isinstance(is_error, bool) and is_error:
        return True
    details = response.get("details")
    if isinstance(details, dict):
        exit_code = details.get("exitCode")
        if isinstance(exit_code, int):
            return exit_code != 0
    return False


def _attach_tool_responses(step: dict[str, Any], responses: list[dict[str, Any]]) -> dict[str, int]:
    if not responses:
        metrics = {
            "tool_response_count": 0,
            "failed_tool_response_count": 0,
            "tool_response_chars": 0,
            "tool_response_lines": 0,
            "tool_response_tokens_approx": 0,
        }
        step.update(metrics)
        return metrics

    step["tool_responses"] = responses
    metrics = _tool_response_text_metrics(responses)
    metrics["tool_response_count"] = len(responses)
    metrics["failed_tool_response_count"] = sum(1 for response in responses if _is_failed_tool_response(response))
    step.update(metrics)
    return metrics


def _exec_command_from_tool_call(part: dict[str, Any]) -> str | None:
    if part.get("name") != "exec":
        return None
    arguments = part.get("arguments")
    if not isinstance(arguments, dict):
        return None
    for key in ("command", "cmd"):
        value = arguments.get(key)
        if isinstance(value, str) and value.strip():
            return value.strip()
    return None


def _is_pytest_command(command: str) -> bool:
    return bool(re.search(r"(^|[\s;&|])(?:python\s+-m\s+)?(?:pytest|py\.test)(\s|$)", command))


def _is_git_diff_command(command: str) -> bool:
    return bool(re.search(r"(^|[\s;&|])git\s+diff(\s|$)", command))


def _is_search_command(command: str) -> bool:
    return bool(re.search(r"(^|[\s;&|])(?:rg|grep)(\s|$)", command))


def reconstruct_openclaw_jsonl_session(path: Path) -> dict[str, Any] | None:
    """Reconstruct one OpenClaw transcript JSONL file into the runner trace schema."""
    entries = _read_jsonl(path)
    if not entries:
        return None

    session_id = _session_id_from_entries(path, entries)
    first_user_msg: str | None = None
    steps: list[dict[str, Any]] = []
    models: set[str] = set()
    providers: set[str] = set()
    first_ts: int | None = None
    last_ts: int | None = None
    total_input = 0
    total_output = 0
    total_cache_read = 0
    total_cache_write = 0
    total_reasoning = 0
    total_reported = 0
    max_step_input = 0
    max_step_output = 0
    total_cost = 0.0
    has_cost = False
    tool_call_count = 0
    tool_result_count = 0
    failed_tool_result_count = 0
    tool_result_chars = 0
    tool_result_lines = 0
    tool_result_tokens_approx = 0
    exec_command_count = 0
    pytest_command_count = 0
    git_diff_command_count = 0
    search_command_count = 0
    file_read_tool_count = 0
    file_edit_tool_count = 0
    tool_call_counts: Counter[str] = Counter()
    tool_result_counts: Counter[str] = Counter()
    pending_tool_responses: list[dict[str, Any]] = []

    for entry in entries:
        timestamp_ns = _timestamp_ns(entry)
        if timestamp_ns is not None:
            first_ts = timestamp_ns if first_ts is None else min(first_ts, timestamp_ns)
            last_ts = timestamp_ns if last_ts is None else max(last_ts, timestamp_ns)

        parts = _entry_parts(entry)
        role = _entry_role(entry)
        if role == "user" and first_user_msg is None:
            first_user_msg = _text_from_parts(parts)
        if role == "toolResult":
            tool_response = _tool_response_from_entry(entry, parts)
            pending_tool_responses.append(tool_response)
            tool_result_count += 1
            response_tool_name = _tool_name(tool_response.get("tool_name"))
            tool_result_counts[response_tool_name] += 1
            if _is_failed_tool_response(tool_response):
                failed_tool_result_count += 1
            response_text_metrics = _tool_response_text_metrics([tool_response])
            tool_result_chars += response_text_metrics["tool_response_chars"]
            tool_result_lines += response_text_metrics["tool_response_lines"]
            tool_result_tokens_approx += response_text_metrics["tool_response_tokens_approx"]

        raw_usage = _entry_usage(entry)
        usage = _normalize_usage(raw_usage)
        if not usage:
            continue

        input_tokens = int(usage.get("input_tokens", 0))
        output_tokens = int(usage.get("output_tokens", 0))
        cache_read_tokens = int(usage.get("cache_read_tokens", 0))
        cache_write_tokens = int(usage.get("cache_write_tokens", 0))
        reasoning_tokens = int(usage.get("reasoning_tokens", 0))
        reported_tokens = int(usage.get("total_tokens", 0))
        cost = usage.get("cost")
        if isinstance(cost, int | float):
            total_cost += float(cost)
            has_cost = True

        model = _entry_model(entry)
        provider = _entry_provider(entry)
        if model:
            models.add(model)
        if provider:
            providers.add(provider)

        step: dict[str, Any] = {
            "step_index": len(steps),
            "event_id": entry.get("id") or f"{path.stem}:{len(steps)}",
            "trace_id": session_id,
            "timestamp_ns": timestamp_ns,
            "model": model,
            "provider": provider,
            "input_tokens": input_tokens,
            "output_tokens": output_tokens,
            "reasoning_tokens": reasoning_tokens,
            "total_tokens": reported_tokens,
        }
        if timestamp_ns is not None:
            step["timestamp"] = ns_to_iso(timestamp_ns)
        if cache_read_tokens:
            step["cache_read_tokens"] = cache_read_tokens
        if cache_write_tokens:
            step["cache_write_tokens"] = cache_write_tokens
        if isinstance(cost, int | float):
            step["cost"] = float(cost)

        assistant_parts = [_format_assistant_part(part) for part in parts] if role == "assistant" else []
        tool_calls = _tool_call_parts(assistant_parts)
        step["tool_call_count"] = len(tool_calls)
        for tool_call in tool_calls:
            call_name = _tool_name(tool_call.get("name"))
            tool_call_counts[call_name] += 1
            tool_call_count += 1
            if call_name == "read":
                file_read_tool_count += 1
            if call_name in {"edit", "write"}:
                file_edit_tool_count += 1
            command = _exec_command_from_tool_call(tool_call)
            if command is None:
                continue
            exec_command_count += 1
            if _is_pytest_command(command):
                pytest_command_count += 1
            if _is_git_diff_command(command):
                git_diff_command_count += 1
            if _is_search_command(command):
                search_command_count += 1
        _attach_tool_responses(step, pending_tool_responses)
        pending_tool_responses = []

        if assistant_parts:
            step["assistant_output"] = assistant_parts

        steps.append(step)
        total_input += input_tokens
        total_output += output_tokens
        total_cache_read += cache_read_tokens
        total_cache_write += cache_write_tokens
        total_reasoning += reasoning_tokens
        total_reported += reported_tokens
        max_step_input = max(max_step_input, input_tokens)
        max_step_output = max(max_step_output, output_tokens)

    if not steps:
        return None
    if pending_tool_responses:
        step = steps[-1]
        step_tool_responses = step.setdefault("tool_responses", [])
        if isinstance(step_tool_responses, list):
            step_tool_responses.extend(pending_tool_responses)
            metrics = _tool_response_text_metrics(pending_tool_responses)
            step["tool_response_count"] = int(step.get("tool_response_count", 0)) + len(pending_tool_responses)
            step["failed_tool_response_count"] = int(step.get("failed_tool_response_count", 0)) + sum(
                1 for response in pending_tool_responses if _is_failed_tool_response(response)
            )
            step["tool_response_chars"] = int(step.get("tool_response_chars", 0)) + metrics["tool_response_chars"]
            step["tool_response_lines"] = int(step.get("tool_response_lines", 0)) + metrics["tool_response_lines"]
            step["tool_response_tokens_approx"] = int(step.get("tool_response_tokens_approx", 0)) + metrics[
                "tool_response_tokens_approx"
            ]

    issue_id = extract_issue_id(first_user_msg) or _local_agent_id_from_session_file(path)

    return {
        "source": "openclaw-jsonl",
        "session_id": session_id,
        "trace_ids_included": [session_id],
        "session_file": str(path),
        "initial_user_message": first_user_msg,
        "issue_id": issue_id,
        "total_steps": len(steps),
        "llm_turn_count": len(steps),
        "total_input_tokens": total_input,
        "total_output_tokens": total_output,
        "total_cache_read_tokens": total_cache_read,
        "total_cache_write_tokens": total_cache_write,
        "total_reasoning_tokens": total_reasoning,
        "total_reported_tokens": total_reported,
        "max_step_input_tokens": max_step_input,
        "max_step_output_tokens": max_step_output,
        "tool_call_count": tool_call_count,
        "tool_result_count": tool_result_count,
        "failed_tool_result_count": failed_tool_result_count,
        "tool_call_counts": dict(sorted(tool_call_counts.items())),
        "tool_result_counts": dict(sorted(tool_result_counts.items())),
        "tool_result_chars": tool_result_chars,
        "tool_result_lines": tool_result_lines,
        "tool_result_tokens_approx": tool_result_tokens_approx,
        "exec_command_count": exec_command_count,
        "pytest_command_count": pytest_command_count,
        "git_diff_command_count": git_diff_command_count,
        "search_command_count": search_command_count,
        "file_read_tool_count": file_read_tool_count,
        "file_edit_tool_count": file_edit_tool_count,
        "total_cost": total_cost if has_cost else None,
        "models": sorted(models),
        "providers": sorted(providers),
        "first_event_at": ns_to_iso(first_ts) if first_ts is not None else None,
        "last_event_at": ns_to_iso(last_ts) if last_ts is not None else None,
        "steps": steps,
    }


def _session_files_in_profile(profile_dir: Path) -> list[Path]:
    return sorted(profile_dir.glob("agents/*/sessions/*.jsonl"))


def _iter_session_files(
    profiles_root: Path | None,
    profile_dirs: Iterable[str | Path] | None,
) -> list[Path]:
    session_files: list[Path] = []

    if profile_dirs is not None:
        for raw_profile_dir in profile_dirs:
            profile_dir = Path(raw_profile_dir).expanduser()
            if not profile_dir.exists():
                logger.warning("OpenClaw profile dir not found, skipping JSONL trace record: %s", profile_dir)
                continue
            session_files.extend(_session_files_in_profile(profile_dir))
        return sorted(dict.fromkeys(session_files))

    if profiles_root is None:
        return []

    profiles_root = profiles_root.expanduser()
    if not profiles_root.exists():
        logger.warning("OpenClaw profiles dir not found, skipping JSONL trace record: %s", profiles_root)
        return []
    return sorted(profiles_root.glob("*/agents/*/sessions/*.jsonl"))


def _trace_started_in_window(trace_data: dict[str, Any], start_ns: int, end_ns: int) -> bool:
    first_step_ts: int | None = None
    for step in trace_data.get("steps", []):
        if not isinstance(step, dict):
            continue
        timestamp_ns = step.get("timestamp_ns")
        if isinstance(timestamp_ns, int):
            first_step_ts = timestamp_ns
            break
    if first_step_ts is None:
        return True
    return start_ns <= first_step_ts <= end_ns


def iter_openclaw_jsonl_traces(
    *,
    profiles_root: str | Path | None = DEFAULT_OPENCLAW_PROFILES_DIR,
    profile_dirs: Iterable[str | Path] | None = None,
    start_ns: int,
    end_ns: int,
    instance_ids: set[str] | None = None,
    session_ids: set[str] | None = None,
) -> list[dict[str, Any]]:
    """Return OpenClaw JSONL traces matching explicit sessions or a time window."""
    traces: list[dict[str, Any]] = []
    root = Path(profiles_root).expanduser() if profiles_root is not None else None
    for session_file in _iter_session_files(root, profile_dirs):
        trace_data = reconstruct_openclaw_jsonl_session(session_file)
        if trace_data is None:
            continue
        session_id = trace_data.get("session_id")
        if session_ids is not None:
            if not isinstance(session_id, str) or session_id not in session_ids:
                continue
            traces.append(trace_data)
            continue
        issue_id = trace_data.get("issue_id")
        if not isinstance(issue_id, str) or not issue_id:
            logger.warning("Skipping OpenClaw session without issue_id: %s", trace_data.get("session_id"))
            continue
        if instance_ids is not None and issue_id not in instance_ids:
            continue
        if not _trace_started_in_window(trace_data, start_ns, end_ns):
            continue
        traces.append(trace_data)
    traces.sort(key=lambda item: item.get("first_event_at") or "")
    return traces
