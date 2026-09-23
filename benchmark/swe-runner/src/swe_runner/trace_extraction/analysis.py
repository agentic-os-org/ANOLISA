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

"""Trace file analysis and statistics computation."""

import json
import logging
from collections import Counter
from collections.abc import Iterator
from pathlib import Path
from typing import Any

from swe_runner.trace_extraction.helpers import (
    ExtractionError,
    _format_metric,
    _mean,
    _safe_int,
    _trimmed_mean,
)

logger = logging.getLogger(__name__)

_TRACE_NUMERIC_FIELDS = (
    "llm_turn_count",
    "total_cache_read_tokens",
    "total_cache_write_tokens",
    "total_reasoning_tokens",
    "total_reported_tokens",
    "max_step_input_tokens",
    "max_step_output_tokens",
    "tool_call_count",
    "tool_result_count",
    "failed_tool_result_count",
    "tool_result_chars",
    "tool_result_lines",
    "tool_result_tokens_approx",
    "exec_command_count",
    "pytest_command_count",
    "git_diff_command_count",
    "search_command_count",
    "file_read_tool_count",
    "file_edit_tool_count",
)
_TRACE_COUNTER_FIELDS = ("tool_call_counts", "tool_result_counts")


def _extract_model_value(trace_data: dict[str, Any]) -> str:
    """Extract a stable model display value from trace JSON."""
    models = trace_data.get("models") or []
    if models:
        return ";".join(str(model) for model in models)

    single_model = trace_data.get("model")
    if single_model:
        return str(single_model)

    steps = trace_data.get("steps") or []
    step_models: list[str] = []
    for step in steps:
        if not isinstance(step, dict):
            continue
        model = step.get("model")
        if isinstance(model, str) and model and model not in step_models:
            step_models.append(model)
    return ";".join(step_models)


def _iter_trace_files(trace_root: str | Path) -> Iterator[tuple[str, Path]]:
    """Yield trace JSON files under <trace_root>/<instance_id>/trace*.json."""
    root = Path(trace_root)
    if not root.exists():
        raise ExtractionError(f"Trace directory not found: {root}")

    for instance_dir in sorted(path for path in root.iterdir() if path.is_dir()):
        for trace_file in sorted(instance_dir.glob("trace*.json")):
            yield instance_dir.name, trace_file


def _iter_selected_trace_files(trace_files: list[Path]) -> Iterator[tuple[str, Path]]:
    """Yield explicitly selected trace files."""
    for trace_file in sorted(trace_files):
        yield trace_file.parent.name, trace_file


def _json_counter_value(value: Any) -> str:
    if not isinstance(value, dict):
        return "{}"
    return json.dumps(value, sort_keys=True, ensure_ascii=False, separators=(",", ":"))


def _counter_from_row(row: dict[str, str | int], key: str) -> Counter[str]:
    value = row.get(key)
    if not isinstance(value, str) or not value:
        return Counter()
    try:
        parsed = json.loads(value)
    except json.JSONDecodeError:
        return Counter()
    if not isinstance(parsed, dict):
        return Counter()
    counter: Counter[str] = Counter()
    for raw_key, raw_value in parsed.items():
        if isinstance(raw_key, str) and isinstance(raw_value, int):
            counter[raw_key] += raw_value
    return counter


def analyze_trace_files(
    trace_root: str | Path,
    trim_ratio: float = 0.1,
    trace_files: list[Path] | None = None,
    include_metrics: bool = False,
) -> tuple[list[dict[str, str | int]], list[dict[str, str | int]]]:
    """Analyze trace JSON files and return per-trace and per-instance rows."""
    if not 0 <= trim_ratio < 0.5:
        raise ExtractionError(f"trim_ratio must be in [0, 0.5), got {trim_ratio}")

    per_trace_rows: list[dict[str, str | int]] = []
    grouped_rows: dict[str, list[dict[str, str | int]]] = {}

    file_iter = _iter_selected_trace_files(trace_files) if trace_files is not None else _iter_trace_files(trace_root)

    for instance_id, trace_file in file_iter:
        try:
            trace_data = json.loads(trace_file.read_text(encoding="utf-8"))
        except json.JSONDecodeError as exc:
            raise ExtractionError(f"Failed to parse trace file {trace_file}: {exc}") from exc

        task_id = trace_data.get("session_id") or trace_file.stem
        model_value = _extract_model_value(trace_data)
        input_tokens = _safe_int(trace_data.get("total_input_tokens"))
        output_tokens = _safe_int(trace_data.get("total_output_tokens"))
        total_steps = _safe_int(trace_data.get("total_steps"))

        row: dict[str, str | int] = {
            "instance_id": instance_id,
            "task_id": str(task_id),
            "model": model_value,
            "total_input_tokens": input_tokens,
            "total_output_tokens": output_tokens,
            "total_steps": total_steps,
        }
        if include_metrics:
            for field in _TRACE_NUMERIC_FIELDS:
                row[field] = _safe_int(trace_data.get(field))
            for field in _TRACE_COUNTER_FIELDS:
                row[field] = _json_counter_value(trace_data.get(field))
        per_trace_rows.append(row)
        grouped_rows.setdefault(instance_id, []).append(row)

    per_instance_rows: list[dict[str, str | int]] = []
    for instance_id in sorted(grouped_rows):
        rows = grouped_rows[instance_id]
        step_counts = [int(row["total_steps"]) for row in rows]
        input_tokens_list = [int(row["total_input_tokens"]) for row in rows]
        output_tokens_list = [int(row["total_output_tokens"]) for row in rows]
        total_tokens_list = [inp + out for inp, out in zip(input_tokens_list, output_tokens_list, strict=False)]
        summary_row: dict[str, str | int] = {
            "instance_id": instance_id,
            "execution_count": len(rows),
            "avg_steps": _format_metric(_mean(step_counts)),
            "min_steps": min(step_counts),
            "max_steps": max(step_counts),
            "avg_input_tokens": _format_metric(_mean(input_tokens_list)),
            "avg_output_tokens": _format_metric(_mean(output_tokens_list)),
            "avg_total_tokens": _format_metric(_mean(total_tokens_list)),
            "trimmed_avg_input_tokens": _format_metric(_trimmed_mean(input_tokens_list, trim_ratio)),
            "trimmed_avg_output_tokens": _format_metric(_trimmed_mean(output_tokens_list, trim_ratio)),
            "trimmed_avg_total_tokens": _format_metric(_trimmed_mean(total_tokens_list, trim_ratio)),
            "min_total_tokens": min(total_tokens_list),
            "max_total_tokens": max(total_tokens_list),
        }
        if include_metrics:
            for field in _TRACE_NUMERIC_FIELDS:
                values = [int(row[field]) for row in rows]
                summary_row[f"avg_{field}"] = _format_metric(_mean(values))
            for field in _TRACE_COUNTER_FIELDS:
                counter: Counter[str] = Counter()
                for row in rows:
                    counter.update(_counter_from_row(row, field))
                summary_row[field] = json.dumps(
                    dict(sorted(counter.items())),
                    sort_keys=True,
                    ensure_ascii=False,
                    separators=(",", ":"),
                )
        per_instance_rows.append(summary_row)

    return per_trace_rows, per_instance_rows
