"""`_safe_int` tolerates non-numeric trace metric strings.

Trace JSON files come from hand-edited or third-party trace dirs; a metric
like `"total_steps": "many"` used to raise a bare ValueError out of
`analyze_trace_files` (the CLI catches only ExtractionError), showing the
user a raw traceback instead of a diagnostic.
"""

import json

import pytest

from swe_runner.trace_extraction.analysis import analyze_trace_files
from swe_runner.trace_extraction.helpers import _safe_int


class TestSafeInt:
    def test_non_numeric_string_is_invalid(self):
        """修复前：非空字符串直接 int(value) 抛裸 ValueError；静默回 0 会把
        无效测量与测得的 0 混同——无效测量必须是 None 供统计层排除。"""
        assert _safe_int("many") is None
        assert _safe_int("n/a") is None

    def test_numeric_string_still_converts(self):
        assert _safe_int("42") == 42

    def test_known_types_still_convert(self):
        assert _safe_int(None) == 0
        assert _safe_int(7) == 7
        assert _safe_int(2.9) == 2
        assert _safe_int("") == 0
        assert _safe_int("  ") == 0


class TestAnalyzeTraceFiles:
    def test_non_numeric_metric_does_not_crash_analysis(self, tmp_path):
        """含畸形指标的 trace 文件不得让整个 analyze-traces 裸崩。"""
        trace = {
            "instance_id": "org__repo-1",
            "session_id": "s-1",
            "model": "test-model",
            "total_steps": "many",  # 畸形指标
        }
        instance_dir = tmp_path / "org__repo-1"
        instance_dir.mkdir()
        trace_file = instance_dir / "trace-1.json"
        trace_file.write_text(json.dumps(trace), encoding="utf-8")

        per_trace_rows, _per_instance_rows = analyze_trace_files(trace_root=tmp_path)
        # 行照常产出，且无效测量保留原始字符串（诊断可见）
        row = next(r for r in per_trace_rows if r["instance_id"] == "org__repo-1")
        assert row["total_steps"] == "many"

    def test_invalid_measurement_excluded_from_statistics(self, tmp_path):
        """同一实例的 "n/a" 与 30：均值应为 30 而非 15（无效测量排除出统计）。"""
        inst = tmp_path / "org__repo-2"
        inst.mkdir()
        (inst / "trace-1.json").write_text(json.dumps({
            "instance_id": "org__repo-2", "session_id": "s-1", "model": "m",
            "total_input_tokens": "n/a", "total_output_tokens": 5, "total_steps": 3,
        }), encoding="utf-8")
        (inst / "trace-2.json").write_text(json.dumps({
            "instance_id": "org__repo-2", "session_id": "s-2", "model": "m",
            "total_input_tokens": 30, "total_output_tokens": 5, "total_steps": 7,
        }), encoding="utf-8")

        per_trace_rows, per_instance_rows = analyze_trace_files(trace_root=tmp_path)
        summary = next(r for r in per_instance_rows if r["instance_id"] == "org__repo-2")
        assert summary["execution_count"] == 2
        assert summary["avg_input_tokens"] == "30.00"
        assert summary["trimmed_avg_input_tokens"] == "30.00"
        rows = {r["task_id"]: r for r in per_trace_rows}
        assert rows["s-1"]["total_input_tokens"] == "n/a"
        assert rows["s-2"]["total_input_tokens"] == 30
