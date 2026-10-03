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

"""Regression tests for run_task_compare.py batch task-selection precedence.

run_native_batch and run_ce_runner_batch must build identical
task-selection arguments for the same inputs; when their precedence
differs, the two sides of the comparison execute different task sets
(e.g. ``--batch --range 1-10 --prefix T`` used to drop ``--range`` on
the native side). See issue #3857.

Run with stdlib unittest:
    python3 -m unittest test_run_task_compare -v
"""

import contextlib
import importlib.util
import io
import sys
import unittest
from pathlib import Path
from unittest import mock

SCRIPT = Path(__file__).resolve().parent / "run_task_compare.py"


def _load_module():
    spec = importlib.util.spec_from_file_location("run_task_compare", SCRIPT)
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


rtc = _load_module()

SELECTION_FLAGS = ("--filter", "--tag", "--range", "--prefix")


def selection_args(cmd):
    """Extract task-selection flags (and their values) from a CLI argv list."""
    picked = []
    i = 0
    while i < len(cmd):
        if cmd[i] in SELECTION_FLAGS:
            picked.append(cmd[i])
            if i + 1 < len(cmd) and not cmd[i + 1].startswith("-"):
                picked.append(cmd[i + 1])
                i += 1
        i += 1
    return picked


class _FakeCompleted:
    returncode = 0


class SelectionPrecedenceTest(unittest.TestCase):
    """Both batch branches must agree on task selection for the same args."""

    def _run_both(self, **selection):
        common = dict(
            task_ids=[], config=None, sandbox=False, sandbox_image=None,
            timeout=60, parallel=2, trials=1,
        )
        captured = []

        def fake_run(cmd, cwd=None):
            captured.append(list(cmd))
            return _FakeCompleted()

        fake_subprocess = mock.Mock(run=fake_run)
        with contextlib.redirect_stdout(io.StringIO()):
            with mock.patch.object(rtc, "subprocess", fake_subprocess), \
                    mock.patch.object(rtc, "_find_latest_trace",
                                      mock.Mock(return_value=None)):
                rtc.run_native_batch(tasks_dir=None, **common, **selection)
                rtc.run_ce_runner_batch(tasks_file=None, **common, **selection)
        self.assertEqual(len(captured), 2)
        return selection_args(captured[0]), selection_args(captured[1])

    def test_range_and_prefix_forwarded_together(self):
        native, ce = self._run_both(range_str="1-10", prefix="T")
        self.assertEqual(native, ce)
        self.assertEqual(native, ["--range", "1-10", "--prefix", "T"])

    def test_tag_only(self):
        native, ce = self._run_both(tag="general")
        self.assertEqual(native, ce)
        self.assertEqual(native, ["--tag", "general"])

    def test_filter_only(self):
        native, ce = self._run_both(filter_str="invoice")
        self.assertEqual(native, ce)
        self.assertEqual(native, ["--filter", "invoice"])

    def test_tag_wins_over_filter(self):
        native, ce = self._run_both(tag="general", filter_str="invoice")
        self.assertEqual(native, ce)
        self.assertEqual(native, ["--tag", "general"])

    def test_range_wins_over_tag_and_prefix(self):
        native, ce = self._run_both(range_str="1-10", tag="general", prefix="T")
        self.assertEqual(native, ce)
        self.assertEqual(native, ["--range", "1-10", "--prefix", "T"])


if __name__ == "__main__":
    unittest.main()
