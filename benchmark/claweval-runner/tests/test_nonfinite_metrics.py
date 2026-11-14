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

"""One nonfinite metric must not abort the whole summary.

`fmt()` converted with `int()` before checking its magnitude bound, so a NaN
raised `ValueError` and either infinity raised `OverflowError` before the code
that formats nonfinite floats could run. A single `1e400` in a batch (which
Python's JSON reader turns into positive infinity) took down the complete CSV
and table report.

The formatter controls pin the rendering; the subprocess tests drive the real
CLI over a real batch file, because the defect only showed up end to end.
"""

from __future__ import annotations

import csv
import importlib.util
import io
import json
import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

SCRIPTS = Path(__file__).resolve().parents[1] / "scripts"

_spec = importlib.util.spec_from_file_location(
    "summarize_results", SCRIPTS / "summarize_results.py"
)
_summarize = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(_summarize)

fmt = _summarize.fmt


def _batch_json(input_tokens: str) -> str:
    """Build one-task batch text, substituting a raw JSON number for tokens."""
    payload = [
        {
            "task_id": "T001",
            "task_name": "nonfinite metric",
            "difficulty": "easy",
            "trials": [
                {
                    "trial": 1,
                    "trace_file": "T001_trial1.jsonl",
                    "task_score": 1.0,
                    "passed": True,
                    "completion": 1.0,
                    "robustness": 1.0,
                    "communication": 1.0,
                    "safety": 1.0,
                    "wall_time_s": 5.0,
                    "input_tokens": "__TOKENS__",
                    "output_tokens": 5,
                }
            ],
            "error": None,
            "avg_score": 1.0,
            "pass_at_1": 1.0,
            "pass_hat_k": 1.0,
            "avg_passed": True,
        }
    ]
    return json.dumps(payload).replace('"__TOKENS__"', input_tokens)


class FormatterControlsTest(unittest.TestCase):
    """`fmt` spells nonfinite floats out instead of raising."""

    def test_nonfinite_values_are_rendered(self) -> None:
        for value, expected in (
            (float("nan"), "nan"),
            (float("inf"), "inf"),
            (float("-inf"), "-inf"),
        ):
            with self.subTest(value=value):
                self.assertEqual(fmt(value), expected)

    def test_finite_and_non_numeric_spellings_are_unchanged(self) -> None:
        for value, expected in (
            (3.0, "3"),
            (0.0, "0"),
            (-0.0, "0"),
            (0.5, "0.50"),
            (1e7, "10000000.00"),
            (5, "5"),
            (True, "Y"),
            (False, "N"),
            (None, "N/A"),
            ("x", "x"),
        ):
            with self.subTest(value=value):
                self.assertEqual(fmt(value), expected)


class NonfiniteCliTest(unittest.TestCase):
    """The real CLI renders a batch holding 1e400 instead of aborting."""

    def setUp(self) -> None:
        temporary = tempfile.TemporaryDirectory(prefix="ce-nonfinite-")
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        trace_dir = self.root / "traces" / "run_alpha"
        trace_dir.mkdir(parents=True)
        # `1e400` is valid JSON text; Python's reader maps it to infinity.
        self.input_file = trace_dir / "batch_results.json"
        self.input_file.write_text(_batch_json("1e400"), encoding="utf-8")
        self.output = self.root / "summary"

    def run_cli(self, *arguments: str) -> subprocess.CompletedProcess[bytes]:
        env = os.environ.copy()
        env.update({"PYTHONUTF8": "1", "PYTHONIOENCODING": "utf-8"})
        return subprocess.run(
            [sys.executable, "-X", "utf8=1", str(SCRIPTS / "summarize_results.py"), *arguments],
            cwd=self.root,
            env=env,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            timeout=60,
            check=False,
        )

    def test_csv_report_completes_with_infinity(self) -> None:
        result = self.run_cli(
            "--input", str(self.input_file), "--format", "csv", "--output", str(self.output)
        )
        self.assertEqual(result.returncode, 0, result.stderr.decode("utf-8", "replace"))
        self.assertNotIn(b"Traceback", result.stderr)
        self.assertTrue(self.output.is_file(), "the run wrote no report")
        rows = list(csv.reader(io.StringIO(self.output.read_text(encoding="utf-8"))))
        # Header plus one trial row; the infinity stays visible in its column.
        self.assertEqual(len(rows), 2)
        self.assertIn("inf", rows[1])

    def test_table_report_completes_with_infinity(self) -> None:
        result = self.run_cli(
            "--input", str(self.input_file), "--format", "table", "--output", str(self.output)
        )
        self.assertEqual(result.returncode, 0, result.stderr.decode("utf-8", "replace"))
        self.assertNotIn(b"Traceback", result.stderr)
        self.assertTrue(self.output.is_file(), "the run wrote no report")
        self.assertIn("inf", self.output.read_text(encoding="utf-8"))


if __name__ == "__main__":
    unittest.main()
