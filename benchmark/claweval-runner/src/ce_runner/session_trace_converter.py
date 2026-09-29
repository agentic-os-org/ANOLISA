#!/usr/bin/env python3

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

"""Convert openclaw session JSONL to claw-eval trace JSONL format.

Reads an openclaw session trace file and converts it to the claw-eval
JSONL format expected by the grader, including:
- trace_start
- message events (user, assistant with tool_use/tool_result blocks)
- tool_dispatch events (HTTP-level request/response metadata)
- audit_snapshot events (fetched from mock services)
- trace_end with timing/score placeholders

Usage:
    python session_trace_converter.py \
        --session /path/to/session.jsonl \
        --task-yaml /path/to/task.yaml \
        --output /path/to/output.jsonl \
        --mock-audit http://localhost:9109/rss/audit
"""

from __future__ import annotations

import argparse
import json
import sys
import time
import uuid
from datetime import datetime, timezone
from pathlib import Path
from typing import Any

try:
    import yaml
except ImportError:
    print("pyyaml required: pip install pyyaml", file=sys.stderr)
    sys.exit(1)

try:
    import httpx
except ImportError:
    print("httpx required: pip install httpx", file=sys.stderr)
    sys.exit(1)


def now_iso() -> str:
    return datetime.now(timezone.utc).isoformat()


def normalize_timestamp(ts: str) -> str:
    """Normalize timestamp format: convert 'Z' suffix to '+00:00'."""
    if ts and ts.endswith("Z"):
        ts = ts[:-1] + "+00:00"
    return ts


import re

_TIMESTAMP_PREFIX_RE = re.compile(
    r"^\[(?:Mon|Tue|Wed|Thu|Fri|Sat|Sun) \d{4}-\d{2}-\d{2} \d{2}:\d{2} GMT[+-]\d+\]\s*"
)


def strip_timestamp_prefix(text: str) -> str:
    """Strip openclaw's timestamp prefix from user message text.

    openclaw CLI prepends a timestamp like ``[Wed 2026-05-27 11:09 GMT+8] ``
    to user messages.  This breaks the claw-eval grader's ``_split_phases``
    method, which looks for ``[user_agent]`` at position 0 to identify the
    clarification phase.  Stripping the prefix restores compatibility.
    """
    return _TIMESTAMP_PREFIX_RE.sub("", text)


def load_task_yaml(path: str) -> dict:
    with open(path) as f:
        return yaml.safe_load(f)


def fetch_audit_data(task: dict, port_offset: int = 0) -> dict[str, dict]:
    """Fetch audit data from all mock services declared in task.yaml.

    When running in batch mode, mock services are started with port offsets
    (e.g. port 9100 → 9100 + offset).  Pass the same *port_offset* here so
    the converter hits the correct audit endpoint.
    """
    audit_data = {}
    services = task.get("services", [])

    for svc in services:
        reset_ep = svc.get("reset_endpoint")
        if not reset_ep:
            continue
        # Derive audit URL: replace /reset with /audit
        audit_url = reset_ep.rsplit("/reset", 1)[0] + "/audit"
        # Apply port offset for batch mode (port 9100 → 9100 + offset)
        if port_offset:
            audit_url = re.sub(
                r"localhost:(\d+)",
                lambda m: f"localhost:{int(m.group(1)) + port_offset}",
                audit_url,
            )
        try:
            resp = httpx.get(audit_url, timeout=10)
            resp.raise_for_status()
            audit_data[svc["name"]] = resp.json()
        except Exception as e:
            print(f"[converter] WARN: Failed to fetch audit from {audit_url}: {e}", file=sys.stderr)

    return audit_data


def parse_openclaw_arguments(args_value: Any) -> dict:
    """Parse openclaw toolCall arguments.

    Openclaw stores arguments as either:
    - A direct dict: {'message_id': 'msg_2001'}
    - A JSON string: '{"kwargs": "{}"}'
    - A dict: {"kwargs": '{"foo": "bar"}'}

    We need to extract the actual tool parameters.
    """
    if isinstance(args_value, dict):
        # Check if it has a 'kwargs' wrapper
        if "kwargs" in args_value:
            kwargs_str = args_value["kwargs"]
            if isinstance(kwargs_str, str):
                try:
                    return json.loads(kwargs_str)
                except json.JSONDecodeError:
                    return {}
            return kwargs_str if isinstance(kwargs_str, dict) else {}
        # Direct dict - return as-is
        return args_value
    elif isinstance(args_value, str):
        try:
            return json.loads(args_value)
        except json.JSONDecodeError:
            return {}
    return {}


def derive_response_status(response_body: Any, is_error: bool) -> int:
    """Derive the HTTP response status for a tool_dispatch event.

    Openclaw's is_error flag does not reflect HTTP status codes: an MCP tool
    that "successfully" returns a body is marked is_error=False even when the
    underlying mock service responded with an HTTP error (e.g. FastAPI 422 on
    missing fields). To avoid recording such failures as 200, inspect the
    response body for a FastAPI validation-error structure and report 422.

    Falls back to the original is_error-based logic (500 if is_error else 200)
    when the body cannot be parsed or does not look like a validation error.
    """
    try:
        parsed = response_body
        if isinstance(parsed, str):
            parsed = json.loads(parsed)
        if isinstance(parsed, dict):
            detail = parsed.get("detail")
            if isinstance(detail, list) and detail and all(
                isinstance(item, dict)
                and "type" in item
                and "loc" in item
                and "msg" in item
                for item in detail
            ):
                return 422
    except (json.JSONDecodeError, TypeError, ValueError):
        pass
    return 500 if is_error else 200


def convert_session_to_trace(
    session_path: str,
    task: dict,
    output_path: str,
    mock_port_offset: int = 0,
    preloaded_audit_data: dict | None = None,
) -> dict:
    """Convert an openclaw session JSONL to claw-eval trace JSONL.

    When *preloaded_audit_data* is provided (a dict keyed by service name),
    it is used directly instead of fetching from live mock services.  This
    prevents cross-trial audit contamination when mock services have already
    been reset by a subsequent trial.

    Returns metadata about the conversion.
    """
    trace_id = str(uuid.uuid4())
    task_id = task.get("task_id", "unknown")
    model = "openclaw"

    # Read session events
    session_events = []
    with open(session_path) as f:
        for line in f:
            line = line.strip()
            if line:
                try:
                    session_events.append(json.loads(line))
                except json.JSONDecodeError:
                    pass

    # 1. trace_start — use first session event's timestamp as the real start time
    first_ts = now_iso()
    for event in session_events:
        ts = event.get("timestamp", "") or event.get("message", {}).get("timestamp", "")
        if ts:
            first_ts = normalize_timestamp(ts)
            break

    # 2. Convert messages — collect into body_events, sort by timestamp later
    # Track tool calls for tool_dispatch events
    tool_call_map = {}  # tool_call_id -> tool_call_info
    tool_dispatches = 0
    total_input_tokens = 0
    total_output_tokens = 0
    total_turns = 0
    wall_start = None
    wall_end = None

    body_events = []      # messages + tool_dispatches — will be sorted by timestamp

    # --- mcporter extraction helper ---
    def _try_extract_mcporter_dispatch(
        tool_call_id: str, tc_info: dict, task: dict, trace_id: str,
    ) -> dict | None:
        """If an exec call uses mcporter, return a virtual tool_dispatch for the actual tool."""
        cmd = tc_info.get("input", {}).get("command", "")
        # Match: mcporter call --config <config> <server_or_tool> [args...]
        import re
        m = re.search(r"mcporter\s+call\s+(?:--config\s+\S+\s+)?(\S+)", cmd)
        if not m:
            return None
        first_arg = m.group(1)
        # Check if first arg looks like an MCP server name (contains '-' or 'mock')
        # In mcporter, the pattern is: mcporter call --config <cfg> <server> <tool> <args>
        # or: mcporter call --config <cfg> <tool> <args> (when server name is omitted)
        task_tools = {t.get("name") for t in task.get("tools", [])}
        if first_arg in task_tools:
            tool_name = first_arg
        elif "." in first_arg and first_arg.startswith("claw-eval-"):
            # Dot syntax: mcporter call --config <cfg> server.tool args...
            parts = first_arg.split(".", 1)
            if parts[1] in task_tools:
                tool_name = parts[1]
            else:
                return None
        elif first_arg.startswith("claw-eval-"):
            # Server name — extract tool from remaining args
            rest = cmd[m.end():].strip().split()
            if rest and rest[0] in task_tools:
                tool_name = rest[0]
            else:
                return None
        else:
            return None

        # Build virtual tool_dispatch
        endpoint_url = ""
        for ep in task.get("tool_endpoints", []):
            if ep.get("tool_name") == tool_name:
                endpoint_url = ep.get("url", "")
                break

        # Extract input params from remaining command args
        input_params = {}
        rest_args = cmd[m.end():].strip()
        if first_arg in task_tools:
            rest_args = rest_args  # already after tool name
        elif "." in first_arg and first_arg.startswith("claw-eval-"):
            # Dot syntax: rest is already all tool args
            pass
        elif first_arg.startswith("claw-eval-"):
            # Space syntax: skip the server name, take tool args
            rest_args = cmd[m.end():].strip().split(None, 1)
            rest_args = rest_args[1] if len(rest_args) > 1 else ""

        for part in rest_args.split():
            if ":" in part:
                k, v = part.split(":", 1)
                input_params[k] = v

        result_body = tc_info.get("result", "")
        is_error = tc_info.get("is_error", False)
        # If mcporter returned an error, mark as error
        if "Error:" in result_body or "unknown" in result_body.lower():
            is_error = True

        return {
            "type": "tool_dispatch",
            "trace_id": trace_id,
            "tool_use_id": f"mcporter_{tool_call_id}",
            "tool_name": tool_name,
            "endpoint_url": endpoint_url,
            "request_body": input_params,
            "response_status": derive_response_status(result_body, is_error),
            "response_body": result_body,
            "latency_ms": 0.0,
            "timestamp": tc_info.get("timestamp", now_iso()),
        }

    def _make_tool_dispatch(tool_id: str, tc_info: dict):
        """Build a tool_dispatch event from tool_call_map entry."""
        raw_name = tc_info["name"]
        if "__" in raw_name:
            tool_name = raw_name.split("__", 1)[1]
        else:
            tool_name = raw_name

        endpoint_url = ""
        for tool_def in task.get("tools", []):
            if tool_def.get("name") == tool_name:
                for ep in task.get("tool_endpoints", []):
                    if ep.get("tool_name") == tool_name:
                        endpoint_url = ep.get("url", "")
                        break
                break

        return {
            "type": "tool_dispatch",
            "trace_id": trace_id,
            "tool_use_id": tool_id,
            "tool_name": tool_name,
            "endpoint_url": endpoint_url,
            "request_body": tc_info.get("input", {}),
            "response_status": derive_response_status(
                tc_info.get("result", ""), tc_info.get("is_error", False)
            ),
            "response_body": tc_info.get("result", ""),
            "latency_ms": 0.0,
            "timestamp": tc_info.get("timestamp", now_iso()),
        }

    for event in session_events:
        etype = event.get("type", "")

        if etype != "message":
            continue

        msg = event.get("message", {})
        role = msg.get("role")
        content = msg.get("content", [])
        timestamp = normalize_timestamp(event.get("timestamp", now_iso()))

        if role == "user":
            # Convert user message
            ce_content = []
            for block in content:
                btype = block.get("type", "")
                if btype == "text":
                    ce_content.append({
                        "type": "text",
                        "text": strip_timestamp_prefix(block["text"]),
                    })
                elif btype == "image":
                    ce_content.append({
                        "type": "image",
                        "data": block.get("data", ""),
                        "mime_type": block.get("mime_type") or block.get("mimeType", "image/jpeg"),
                        "source_path": block.get("source_path"),
                    })

            body_events.append({
                "type": "message",
                "trace_id": trace_id,
                "message": {
                    "role": "user",
                    "content": ce_content,
                    "reasoning_content": None,
                },
                "usage": {"input_tokens": 0, "output_tokens": 0},
                "timestamp": timestamp,
            })

        elif role == "assistant":
            stop_reason = msg.get("stopReason", "")
            usage = msg.get("usage", {})
            input_tok = usage.get("input", 0)
            output_tok = usage.get("output", 0)
            total_input_tokens += input_tok
            total_output_tokens += output_tok

            if wall_start is None:
                wall_start = time.time()
            wall_end = time.time()

            ce_content = []
            has_tool_call = False

            for block in content:
                btype = block.get("type", "")

                if btype == "text":
                    ce_content.append({"type": "text", "text": block["text"]})

                elif btype == "toolCall":
                    # Convert to claw-eval tool_use block
                    has_tool_call = True
                    tool_name = block.get("name", "")
                    tool_id = block.get("id", str(uuid.uuid4()))
                    tool_input = parse_openclaw_arguments(block.get("arguments", {}))

                    ce_content.append({
                        "type": "tool_use",
                        "id": tool_id,
                        "name": tool_name,
                        "input": tool_input,
                    })

                    # Track for tool_dispatch — timestamp from assistant message
                    tool_call_map[tool_id] = {
                        "name": tool_name,
                        "input": tool_input,
                        "timestamp": timestamp,
                        "is_error": True,  # default to error until result proves otherwise
                    }

                elif btype == "thinking":
                    pass  # We'll add it to the message level

            # Extract reasoning content if present
            reasoning = None
            for block in content:
                if block.get("type") == "thinking":
                    reasoning = block.get("thinking", "")
                    break

            body_events.append({
                "type": "message",
                "trace_id": trace_id,
                "message": {
                    "role": "assistant",
                    "content": ce_content,
                    "reasoning_content": reasoning,
                },
                "usage": {"input_tokens": input_tok, "output_tokens": output_tok},
                "timestamp": timestamp,
            })

            if has_tool_call:
                total_turns += 1

        elif role == "toolResult":
            # Convert tool result to tool_result block in a user message
            tool_name = msg.get("toolName", "")
            tool_call_id = msg.get("toolCallId", "")
            is_error = msg.get("isError", False)

            # Extract text content
            result_text = ""
            for block in content:
                if block.get("type") == "text":
                    result_text = block.get("text", "")
                    break

            ce_content = [{
                "type": "tool_result",
                "tool_use_id": tool_call_id,
                "content": [{"type": "text", "text": result_text}],
                "is_error": is_error,
            }]

            # Update tool_call_map with result
            if tool_call_id in tool_call_map:
                tc = tool_call_map[tool_call_id]
                tc["result"] = result_text
                tc["is_error"] = is_error

            body_events.append({
                "type": "message",
                "trace_id": trace_id,
                "message": {
                    "role": "user",
                    "content": ce_content,
                    "reasoning_content": None,
                },
                "usage": {"input_tokens": 0, "output_tokens": 0},
                "timestamp": timestamp,
            })

            # Collect tool_dispatch with assistant timestamp (from tool_use),
            # not tool_result timestamp. Will be sorted into correct position.
            if tool_call_id in tool_call_map:
                tc_info = dict(tool_call_map[tool_call_id])
                body_events.append(_make_tool_dispatch(tool_call_id, tc_info))
                tool_dispatches += 1

                # If this was an exec call with mcporter, also emit a virtual
                # tool_dispatch for the actual tool name so graders can detect it
                if tc_info.get("name") == "exec":
                    virtual = _try_extract_mcporter_dispatch(
                        tool_call_id, tc_info, task, trace_id,
                    )
                    if virtual:
                        body_events.append(virtual)
                        tool_dispatches += 1

                del tool_call_map[tool_call_id]

    # Sort body events by timestamp (stable sort preserves original order for ties)
    body_events.sort(key=lambda e: e["timestamp"])

    # 3. Build output: trace_start + sorted body + tail (audit + trace_end)
    output_events: list[dict] = [
        {
            "type": "trace_start",
            "trace_id": trace_id,
            "task_id": task_id,
            "model": model,
            "persona": "default",
            "timestamp": first_ts,
        },
    ]
    output_events.extend(body_events)

    # Extract final_text from the last assistant message with text content
    final_text = ""
    for event in reversed(session_events):
        if event.get("type") != "message":
            continue
        msg = event.get("message", {})
        if msg.get("role") != "assistant":
            continue
        content = msg.get("content", [])
        for block in content:
            if block.get("type") == "text" and block.get("text", "").strip():
                final_text = block["text"].strip()
                break
        if final_text:
            break

    # 4. Fetch audit data from mock services (or use preloaded data)
    if preloaded_audit_data is not None:
        audit_data = preloaded_audit_data
    else:
        audit_data = fetch_audit_data(task, port_offset=mock_port_offset)
    for svc_name, svc_audit in audit_data.items():
        audit_url = ""
        for svc in task.get("services", []):
            reset_ep = svc.get("reset_endpoint", "")
            if reset_ep:
                audit_url = reset_ep.rsplit("/reset", 1)[0] + "/audit"
                if svc["name"] == svc_name:
                    break

        output_events.append({
            "type": "audit_snapshot",
            "trace_id": trace_id,
            "service_name": svc_name,
            "audit_url": audit_url,
            "audit_data": svc_audit,
            "timestamp": now_iso(),
        })

    # 5. trace_end — compute wall_time from session timestamps when available
    _session_wall = 0.0
    if session_events:
        _first_ts = None
        _last_ts = None
        for _ev in session_events:
            _ts = _ev.get("timestamp", "")
            if _ts:
                try:
                    _dt_val = datetime.fromisoformat(normalize_timestamp(_ts))
                    _epoch = _dt_val.timestamp()
                except (ValueError, OSError):
                    continue
                if _first_ts is None or _epoch < _first_ts:
                    _first_ts = _epoch
                if _last_ts is None or _epoch > _last_ts:
                    _last_ts = _epoch
        if _first_ts and _last_ts:
            _session_wall = _last_ts - _first_ts
    wall_time = _session_wall if _session_wall > 0 else (
        (wall_end - wall_start) if wall_start and wall_end else 0.0
    )

    output_events.append({
        "type": "trace_end",
        "trace_id": trace_id,
        "total_turns": total_turns,
        "model_input_tokens": total_input_tokens,
        "model_output_tokens": total_output_tokens,
        "input_tokens": total_input_tokens,
        "output_tokens": total_output_tokens,
        "total_tokens": total_input_tokens + total_output_tokens,
        "model_time_s": round(wall_time, 2),
        "tool_time_s": 0.0,
        "other_time_s": 0.0,
        "wall_time_s": round(wall_time, 2),
        "final_text": final_text,
        "scores": {
            "completion": 0.0,
            "robustness": 0.0,
            "communication": 0.0,
            "safety": 1.0,
            "efficiency_turns": total_turns,
            "efficiency_tokens": total_input_tokens + total_output_tokens,
            "efficiency_wall_time_s": round(wall_time, 2),
        },
        "task_score": 0.0,
        "passed": False,
        "failure_modes": [],
        "user_agent_rounds": 0,
        "user_agent_max_rounds": 0,
        "user_agent_done": True,
        "timestamp": now_iso(),
    })

    # Write output
    out_path = Path(output_path)
    out_path.parent.mkdir(parents=True, exist_ok=True)
    with open(out_path, "w") as f:
        for event in output_events:
            f.write(json.dumps(event, ensure_ascii=False) + "\n")

    print(f"[converter] Wrote {len(output_events)} events to {output_path}", file=sys.stderr)
    print(f"[converter]   trace_id={trace_id}", file=sys.stderr)
    print(f"[converter]   turns={total_turns}, tool_dispatches={tool_dispatches}", file=sys.stderr)
    print(f"[converter]   tokens={total_input_tokens}in/{total_output_tokens}out", file=sys.stderr)
    print(f"[converter]   audit_services={list(audit_data.keys())}", file=sys.stderr)

    return {
        "trace_id": trace_id,
        "turns": total_turns,
        "tool_dispatches": tool_dispatches,
        "input_tokens": total_input_tokens,
        "output_tokens": total_output_tokens,
        "wall_time_s": round(wall_time, 2),
        "audit_services": list(audit_data.keys()),
    }


def main():
    parser = argparse.ArgumentParser(description="Convert openclaw session to claw-eval trace")
    parser.add_argument("--session", required=True, help="Path to openclaw session JSONL")
    parser.add_argument("--task-yaml", required=True, help="Path to task.yaml")
    parser.add_argument("--output", required=True, help="Output trace JSONL path")
    parser.add_argument("--mock-port-offset", type=int, default=0,
                        help="Port offset for mock services in batch mode")
    parser.add_argument("--audit-data", default=None,
                        help="Path to pre-saved audit JSON (skips live fetch)")
    args = parser.parse_args()

    task = load_task_yaml(args.task_yaml)
    preloaded = None
    if args.audit_data:
        with open(args.audit_data) as f:
            preloaded = json.load(f)
    meta = convert_session_to_trace(args.session, task, args.output,
                                     mock_port_offset=args.mock_port_offset,
                                     preloaded_audit_data=preloaded)
    print(json.dumps(meta, indent=2))


if __name__ == "__main__":
    main()
