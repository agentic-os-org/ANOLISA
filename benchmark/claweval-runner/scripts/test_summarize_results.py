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

"""Regression tests for summarize_results.py table-mode summary stats.

An empty batch_results.json (``[]``, e.g. produced by a filter or range
that matched no tasks) used to crash the table-mode summary with
``ZeroDivisionError``. See issue #3859.

Run with stdlib unittest:
    python3 -m unittest test_summarize_results -v
"""

import contextlib
import importlib.util
import io
import json
import sys
import tempfile
import unittest
from pathlib import Path
from unittest import mock

SCRIPT = Path(__file__).resolve().parent / "summarize_results.py"


def _load_module():
    spec = importlib.util.spec_from_file_location("summarize_results", SCRIPT)
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


summarize = _load_module()


def _run_main(data):
    """Feed ``data`` through main()'s table path; return captured stdout."""
    with tempfile.NamedTemporaryFile(
            "w", suffix=".json", delete=False) as tf:
        json.dump(data, tf)
        path = tf.name
    argv = [sys.argv[0], "--input", path]
    out = io.StringIO()
    try:
        with mock.patch.object(sys, "argv", argv):
            with contextlib.redirect_stdout(out):
                summarize.main()
    finally:
        Path(path).unlink(missing_ok=True)
    return out.getvalue()


class EmptyBatchResultsTest(unittest.TestCase):

    def test_empty_batch_does_not_crash(self):
        out = _run_main([])
        self.assertIn("Summary: 0 tasks, 0 passed (0.0%)", out)
        self.assertIn("Trials: 0 total, 0 passed (0.0%)", out)

    def test_non_empty_batch_summary_unchanged(self):
        task = {
            "task_id": "T001_test",
            "task_name": "Test",
            "difficulty": "easy",
            "avg_passed": True,
            "trials": [{"passed": True}, {"passed": False}],
        }
        out = _run_main([task])
        self.assertIn("Summary: 1 tasks, 1 passed (100.0%)", out)
        self.assertIn("Trials: 2 total, 1 passed (50.0%)", out)


if __name__ == "__main__":
    unittest.main()
