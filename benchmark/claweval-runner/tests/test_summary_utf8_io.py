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

"""A summary must not depend on the host's preferred encoding.

`summarize_results.py` opened its inputs and its report without an encoding, so
the codec came from the locale, and stdout kept the ambient codec too. On an
ASCII/GBK host the first non-ASCII task name aborted the write, leaving no
usable summary, or was emitted as mojibake.

These tests force a locale-dependent codec on the child process instead of
relying on the host being non-UTF-8: `PYTHONUTF8=0` with an ASCII stdout makes
the preferred encoding `ANSI_X3.4-1968`, exactly what the report describes.
The fixtures are written with `ensure_ascii=False`, so the input files really
carry UTF-8 bytes and the read path is exercised as well.
"""

from __future__ import annotations

import csv
import io
import json
import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

SCRIPTS = Path(__file__).resolve().parents[1] / "scripts"
TASK_NAME = "邮件 € 😀"


def _ascii_locale() -> dict[str, str]:
    """Environment that reproduces the reported host without a non-UTF-8 OS."""
    env = os.environ.copy()
    env.update(
        {
            "PYTHONUTF8": "0",
            "PYTHONIOENCODING": "ascii",
            "LC_ALL": "C",
            "LANG": "C",
            "PYTHONCOERCECLOCALE": "0",
        }
    )
    return env


class SummaryUtf8IoTest(unittest.TestCase):
    def setUp(self) -> None:
        temporary = tempfile.TemporaryDirectory(prefix="ce-summary-utf8-")
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.batch = self.root / "batch_results.json"
        self.batch.write_text(json.dumps(_batch(), ensure_ascii=False), encoding="utf-8")
        self.report = self.root / "out"

    def run_cli(self, *arguments: str) -> subprocess.CompletedProcess[bytes]:
        return subprocess.run(
            [sys.executable, str(SCRIPTS / "summarize_results.py"), *arguments],
            cwd=self.root,
            env=_ascii_locale(),
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            timeout=60,
            check=False,
        )

    def test_csv_report_is_utf8_on_an_ascii_host(self) -> None:
        result = self.run_cli(
            "--input", str(self.batch), "--format", "csv", "--output", str(self.report)
        )
        self.assertEqual(result.returncode, 0, result.stderr.decode("utf-8", "replace"))
        self.assertNotIn(b"Traceback", result.stderr)
        raw = self.report.read_bytes()
        text = raw.decode("utf-8")
        rows = list(csv.reader(io.StringIO(text)))
        self.assertEqual(len(rows), 2)
        self.assertIn(TASK_NAME, rows[1])

    def test_table_on_ascii_stdout_is_utf8(self) -> None:
        result = self.run_cli("--input", str(self.batch), "--format", "table")
        self.assertEqual(result.returncode, 0, result.stderr.decode("utf-8", "replace"))
        text = result.stdout.decode("utf-8")
        self.assertIn(TASK_NAME, text)

    def test_ascii_task_text_survives_a_report_directory(self) -> None:
        reports = self.root / "reports"
        reports.mkdir()
        (reports / "T001_trial1.json").write_text(
            json.dumps({"trace_file": "T001_trial1.jsonl", "note": TASK_NAME}, ensure_ascii=False),
            encoding="utf-8",
        )
        result = self.run_cli(
            "--input", str(self.batch), "--report-dir", str(reports), "--format", "csv",
            "--output", str(self.report),
        )
        self.assertEqual(result.returncode, 0, result.stderr.decode("utf-8", "replace"))
        self.assertIn(TASK_NAME, self.report.read_text(encoding="utf-8"))


def _batch() -> list[dict]:
    return [
        {
            "task_id": "T001",
            "task_name": TASK_NAME,
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
                    "input_tokens": 17,
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


if __name__ == "__main__":
    unittest.main()
