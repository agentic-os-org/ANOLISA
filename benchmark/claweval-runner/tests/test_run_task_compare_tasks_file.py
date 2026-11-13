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

"""--tasks-file must reach BOTH comparison lanes.

run_ce_runner_batch passes the file through, but the native claw-eval
invocation has no --tasks-file support: without loading the file here the
native lane ran the ENTIRE corpus while the ce-runner lane ran the file's
tasks, and the batch summary was keyed off the empty task list (0 tasks).
"""

import sys
from argparse import Namespace
from pathlib import Path
from unittest.mock import patch

import pytest

REPO_ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(REPO_ROOT / "scripts"))

import run_task_compare  # noqa: E402


def _args(tmp_path: Path, mode: str) -> Namespace:
    return Namespace(
        task=None,
        mode=mode,
        batch=True,
        tasks=None,
        tasks_file=str(tmp_path / "input_tasks.txt"),
        range_str=None,
        tag=None,
        prefix=None,
        filter_str=None,
        parallel=2,
        timeout=60,
        config=None,
        sandbox=False,
        sandbox_image=None,
    )


class TestTasksFileReachesBothLanes:
    def test_tasks_file_loads_into_native_lane_selection(self, tmp_path):
        (tmp_path / "input_tasks.txt").write_text(
            "T001zh_email_triage\nT002zh_invoice\n", encoding="utf-8"
        )
        args = _args(tmp_path, mode="native")

        with patch.object(
            run_task_compare, "run_native_batch", return_value={}
        ) as native:
            run_task_compare._run_batch_mode(args)

        assert native.call_args.kwargs["task_ids"] == [
            "T001zh_email_triage",
            "T002zh_invoice",
        ]

    def test_tasks_file_loads_into_ce_runner_lane_selection(self, tmp_path):
        (tmp_path / "input_tasks.txt").write_text(
            "T001zh_email_triage\n", encoding="utf-8"
        )
        args = _args(tmp_path, mode="ce-runner")

        with patch.object(
            run_task_compare, "run_ce_runner_batch", return_value={}
        ) as ce:
            run_task_compare._run_batch_mode(args)

        assert ce.call_args.kwargs["task_ids"] == ["T001zh_email_triage"]

    def test_explicit_tasks_still_win(self, tmp_path):
        (tmp_path / "input_tasks.txt").write_text(
            "T009zh_contact_lookup\n", encoding="utf-8"
        )
        args = _args(tmp_path, mode="native")
        args.tasks = ["T001zh_email_triage"]

        with patch.object(
            run_task_compare, "run_native_batch", return_value={}
        ) as native:
            run_task_compare._run_batch_mode(args)

        assert native.call_args.kwargs["task_ids"] == ["T001zh_email_triage"]
