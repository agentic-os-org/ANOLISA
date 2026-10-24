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

"""Shared trial table and rendering contract for batch reports."""

import csv
import io
import os
from typing import Any


def extract_failure_info(report: dict[str, Any] | None) -> str:
    """Extract failure reason from a trial report.

    Returns formatted string: 'category | key_reason_zh' for failed trials,
    '-' for passed trials, 'N/A' if no report.
    """
    if not report:
        return "N/A"
    if report.get("status") == "succ":
        return "-"
    fc = report.get("failure_classification", {})
    category = fc.get("category", "")
    reason = fc.get("key_reason_zh", "")
    if category and reason:
        result = f"{category} | {reason}"
    elif category:
        result = category
    elif reason:
        result = reason
    else:
        reasons = report.get("failure_reason", [])
        if reasons:
            snippets = []
            for r in reasons:
                if isinstance(r, dict):
                    msg = r.get("reasoning", r.get("message", ""))
                    if msg:
                        snippets.append(str(msg)[:80])
            result = "; ".join(snippets[:2]) if snippets else "unknown"
        else:
            return "unknown"
    # Replace commas with semicolons to avoid CSV parsing issues
    return result.replace(",", ";").replace("，", "；")


def extract_trial_hash(trace_field: str) -> str:
    """Extract the hash part from a trace field.

    E.g. 'traces/.../M001_clock_8a61ea06.jsonl' -> '8a61ea06'
    Or 'M001_clock_8a61ea06.jsonl' -> '8a61ea06'
    """
    base = os.path.basename(trace_field).replace(".jsonl", "")
    parts = base.rsplit("_", 1)
    return parts[1] if len(parts) == 2 else ""


def fmt(val: Any) -> str:
    """Format a numeric value, keeping reasonable precision."""
    if val is None:
        return "N/A"
    if isinstance(val, bool):
        return "Y" if val else "N"
    if isinstance(val, float):
        if val == int(val) and abs(val) < 1e6:
            return str(int(val))
        return f"{val:.2f}"
    return str(val)


def build_table(
    data: list[dict[str, Any]], reports: dict[str, dict[str, Any]] | None = None
) -> list[list[str]]:
    # Sort tasks alphabetically by task_id
    sorted_data = sorted(data, key=lambda t: t["task_id"])

    has_reports = bool(reports)

    # Column headers
    trial_cols = [
        "Trial",
        "Trial ID",
        "Input Toks",
        "Output Toks",
        "Model Time(s)",
        "Tool Time(s)",
        "Other Time(s)",
        "Wall Time(s)",
        "Completion",
        "Robustness",
        "Communication",
        "Safety",
        "Task Score",
        "Passed",
    ]
    header = (
        ["Task ID", "Task Name", "Difficulty"]
        + trial_cols
        + ["Avg Score", "Pass@1", "PassHatK", "Overall"]
    )
    if has_reports:
        header.append("Failure Reason")

    rows = [header]

    for task in sorted_data:
        task_id = task["task_id"]
        task_name = task["task_name"]
        difficulty = task["difficulty"]
        avg_score = task.get("avg_score")
        pass_at_1 = task.get("pass_at_1")
        pass_hat_k = task.get("pass_hat_k")
        avg_passed = task.get("avg_passed")

        trials = task.get("trials", [])
        for i, trial in enumerate(trials, 1):
            trace_field = trial.get("trace", "")
            trace_basename = os.path.basename(trace_field) if trace_field else ""
            trial_hash = extract_trial_hash(trace_field)
            # Look up report by basename (e.g. M001_clock_xxxx.jsonl)
            report = reports.get(trace_basename) if reports else None
            failure_info = extract_failure_info(report) if has_reports else ""

            row = [
                task_id if i == 1 else "",  # only show task_id on first sub-row
                task_name if i == 1 else "",
                difficulty if i == 1 else "",
                f"#{i}",
                trial_hash,
                fmt(trial.get("input_tokens")),
                fmt(trial.get("output_tokens")),
                fmt(trial.get("model_time_s")),
                fmt(trial.get("tool_time_s")),
                fmt(trial.get("other_time_s")),
                fmt(trial.get("wall_time_s")),
                fmt(trial.get("completion")),
                fmt(trial.get("robustness")),
                fmt(trial.get("communication")),
                fmt(trial.get("safety")),
                fmt(trial.get("task_score")),
                fmt(trial.get("passed")),
                fmt(avg_score) if i == 1 else "",
                fmt(pass_at_1) if i == 1 else "",
                fmt(pass_hat_k) if i == 1 else "",
                fmt(avg_passed) if i == 1 else "",
            ]
            if has_reports:
                row.append(failure_info)
            rows.append(row)

    return rows


def col_widths(rows: list[list[str]]) -> list[int]:
    widths = [0] * len(rows[0])
    for row in rows:
        for i, cell in enumerate(row):
            widths[i] = max(widths[i], len(str(cell)))
    return widths


def render_table(rows: list[list[str]]) -> str:
    widths = col_widths(rows)
    sep = "+" + "+".join("-" * (w + 2) for w in widths) + "+"

    def format_row(row: list[str]) -> str:
        return (
            "|"
            + "|".join(f" {str(cell):<{widths[i]}} " for i, cell in enumerate(row))
            + "|"
        )

    lines = [sep, format_row(rows[0]), sep]
    for row in rows[1:]:
        lines.append(format_row(row))
        # Add separator after each task's last trial
        lines.append(sep)

    return "\n".join(lines)


def render_csv(rows: list[list[str]]) -> str:
    buf = io.StringIO()
    writer = csv.writer(buf)
    for row in rows:
        writer.writerow(row)
    return buf.getvalue()
