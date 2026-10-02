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

"""CSV export for trace analysis results."""

import csv
from pathlib import Path

from swe_runner.trace_extraction.analysis import analyze_trace_files
from swe_runner.trace_extraction.helpers import sanitize_path_component

_DETAIL_COLUMNS: tuple[tuple[str, str], ...] = (
    ("用例ID", "instance_id"),
    ("任务ID", "task_id"),
    ("模型", "model"),
    ("总输入Token数", "total_input_tokens"),
    ("总输出Token数", "total_output_tokens"),
    ("总执行步数", "total_steps"),
)

_SUMMARY_COLUMNS: tuple[tuple[str, str], ...] = (
    ("用例ID", "instance_id"),
    ("执行次数", "execution_count"),
    ("平均执行步骤数", "avg_steps"),
    ("最小执行步骤数", "min_steps"),
    ("最大执行步骤数", "max_steps"),
    ("平均输入Token数", "avg_input_tokens"),
    ("平均输出Token数", "avg_output_tokens"),
    ("平均总Token数", "avg_total_tokens"),
    ("截尾平均输入Token数", "trimmed_avg_input_tokens"),
    ("截尾平均输出Token数", "trimmed_avg_output_tokens"),
    ("截尾平均总Token数", "trimmed_avg_total_tokens"),
    ("最小总Token数", "min_total_tokens"),
    ("最大总Token数", "max_total_tokens"),
)

_METRIC_COLUMNS: tuple[tuple[str, str], ...] = (
    ("用例ID", "instance_id"),
    ("任务ID", "task_id"),
    ("模型", "model"),
    ("总输入Token数", "total_input_tokens"),
    ("总输出Token数", "total_output_tokens"),
    ("总执行步数", "total_steps"),
    ("LLM轮次数", "llm_turn_count"),
    ("缓存读取Token数", "total_cache_read_tokens"),
    ("缓存写入Token数", "total_cache_write_tokens"),
    ("推理Token数", "total_reasoning_tokens"),
    ("模型报告总Token数", "total_reported_tokens"),
    ("最大单轮输入Token数", "max_step_input_tokens"),
    ("最大单轮输出Token数", "max_step_output_tokens"),
    ("工具调用次数", "tool_call_count"),
    ("工具结果次数", "tool_result_count"),
    ("失败工具结果次数", "failed_tool_result_count"),
    ("工具调用分布", "tool_call_counts"),
    ("工具结果分布", "tool_result_counts"),
    ("工具结果字符数", "tool_result_chars"),
    ("工具结果行数", "tool_result_lines"),
    ("工具结果近似Token数", "tool_result_tokens_approx"),
    ("Exec命令次数", "exec_command_count"),
    ("Pytest命令次数", "pytest_command_count"),
    ("GitDiff命令次数", "git_diff_command_count"),
    ("搜索命令次数", "search_command_count"),
    ("文件读取工具次数", "file_read_tool_count"),
    ("文件编辑工具次数", "file_edit_tool_count"),
)


def _localized_row(row: dict[str, str | int], columns: tuple[tuple[str, str], ...]) -> dict[str, str | int]:
    return {header: row.get(key, "") for header, key in columns}


def write_trace_analysis_csvs(
    trace_root: str | Path,
    output_dir: str | Path,
    trim_ratio: float = 0.1,
) -> tuple[Path, Path]:
    """Write trace summary CSVs plus a separate detailed trace metrics CSV."""
    per_trace_rows, per_instance_rows = analyze_trace_files(
        trace_root,
        trim_ratio=trim_ratio,
        include_metrics=True,
    )

    output_path = Path(output_dir)
    output_path.mkdir(parents=True, exist_ok=True)

    detail_dir = output_path / "trace_details"
    detail_dir.mkdir(parents=True, exist_ok=True)
    for stale_csv in detail_dir.glob("*.csv"):
        stale_csv.unlink()

    grouped_trace_rows: dict[str, list[dict[str, str | int]]] = {}
    for row in per_trace_rows:
        grouped_trace_rows.setdefault(str(row["instance_id"]), []).append(row)

    for instance_id, rows in grouped_trace_rows.items():
        detail_csv = detail_dir / f"{sanitize_path_component(instance_id)}.csv"
        with open(detail_csv, "w", encoding="utf-8", newline="") as f:
            writer = csv.DictWriter(
                f,
                fieldnames=[header for header, _ in _DETAIL_COLUMNS],
            )
            writer.writeheader()
            writer.writerows([_localized_row(row, _DETAIL_COLUMNS) for row in rows])

    per_instance_csv = output_path / "trace_summary.csv"
    with open(per_instance_csv, "w", encoding="utf-8", newline="") as f:
        writer = csv.DictWriter(
            f,
            fieldnames=[header for header, _ in _SUMMARY_COLUMNS],
        )
        writer.writeheader()
        writer.writerows([_localized_row(row, _SUMMARY_COLUMNS) for row in per_instance_rows])

    trace_metrics_dir = output_path / "trace_metrics"
    trace_metrics_dir.mkdir(parents=True, exist_ok=True)
    trace_metrics_csv = trace_metrics_dir / "trace_metrics.csv"
    with open(trace_metrics_csv, "w", encoding="utf-8", newline="") as f:
        writer = csv.DictWriter(
            f,
            fieldnames=[header for header, _ in _METRIC_COLUMNS],
        )
        writer.writeheader()
        writer.writerows([_localized_row(row, _METRIC_COLUMNS) for row in per_trace_rows])

    return detail_dir, per_instance_csv
