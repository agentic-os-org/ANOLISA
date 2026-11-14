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

"""Counter defaulting tests for ``_safe_int`` and ``analyze_trace_files``.

``_safe_int`` documents "defaulting to 0" but passes any non-empty string to
``int()``, so a trace whose counters were written as ``12.5`` or ``n/a`` raised
``ValueError`` out of ``analyze_trace_files`` and aborted the CSV export for
every instance, not just the unreadable one.
"""

import json

import pytest

from swe_runner.trace_extraction.analysis import analyze_trace_files
from swe_runner.trace_extraction.helpers import _safe_int


class TestSafeIntDefaults:
    @pytest.mark.parametrize(
        ("value", "expected"),
        [
            (None, 0),
            (0, 0),
            (7, 7),
            (7.9, 7),
            ("", 0),
            ("   ", 0),
            ("42", 42),
            (" 42 ", 42),
            ("-3", -3),
            (True, 1),
            ([], 0),
            ({}, 0),
        ],
    )
    def test_numeric_values_convert(self, value, expected):
        assert _safe_int(value) == expected

    @pytest.mark.parametrize("value", ["n/a", "1,234", "0x10", "inf", "nan", "--", "12.5.6"])
    def test_unreadable_counters_default_to_zero(self, value):
        assert _safe_int(value) == 0

    @pytest.mark.parametrize(("value", "expected"), [("12.9", 12), ("1e3", 1000)])
    def test_numeric_string_with_a_fraction_keeps_its_integer_part(self, value, expected):
        assert _safe_int(value) == expected


def _write_trace(root, instance_id, payload):
    trace_dir = root / instance_id
    trace_dir.mkdir(parents=True)
    trace_file = trace_dir / "trace.json"
    trace_file.write_text(json.dumps(payload), encoding="utf-8")
    return trace_file


class TestAnalysisSurvivesUnreadableCounters:
    """Red on main: one unreadable counter aborts every trace file."""

    def test_float_and_prose_counters_become_zero(self, tmp_path):
        _write_trace(
            tmp_path,
            "django__django-1",
            {"total_input_tokens": "12.5", "total_output_tokens": "n/a", "total_steps": 4},
        )

        per_trace, per_instance = analyze_trace_files(tmp_path)

        assert per_trace[0]["total_input_tokens"] == 12
        assert per_trace[0]["total_output_tokens"] == 0
        assert per_trace[0]["total_steps"] == 4
        assert per_instance[0]["execution_count"] == 1

    def test_one_unreadable_trace_does_not_hide_the_others(self, tmp_path):
        _write_trace(tmp_path, "django__django-1", {"total_input_tokens": "n/a"})
        _write_trace(tmp_path, "django__django-2", {"total_input_tokens": "30"})

        per_trace, _ = analyze_trace_files(tmp_path)

        assert [row["total_input_tokens"] for row in per_trace] == [0, 30]
