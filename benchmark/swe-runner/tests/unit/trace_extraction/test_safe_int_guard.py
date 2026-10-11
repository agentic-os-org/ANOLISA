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
    def test_non_numeric_string_returns_default(self):
        """修复前：非空字符串直接 int(value) —— int("many") 抛裸 ValueError。"""
        assert _safe_int("many") == 0

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
        # 畸形指标按默认值 0 落表，行本身照常产出
        assert any(r["instance_id"] == "org__repo-1" for r in per_trace_rows)
