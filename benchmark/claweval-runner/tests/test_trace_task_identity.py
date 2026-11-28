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

import os
from pathlib import Path

import pytest

from scripts import run_task_compare
from tests.helpers import find_latest_trace


@pytest.mark.parametrize("task_id", ["T001_lookup", "T001_[a]"])
def test_latest_trace_matches_full_task_id(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch, task_id: str
) -> None:
    monkeypatch.setattr(run_task_compare, "CLAW_EVAL_DIR", tmp_path)
    traces = tmp_path / "traces"
    run = traces / "openclaw_batch"
    run.mkdir(parents=True)

    older = run / f"{task_id}_00000001.jsonl"
    latest = run / f"{task_id}_00000002.jsonl"
    other = run / f"{task_id}_extended_00000003.jsonl"
    for stamp, trace in enumerate((older, latest, other), start=1):
        trace.touch()
        os.utime(trace, (stamp, stamp))

    assert run_task_compare._find_latest_trace(task_id, None) == latest
    assert find_latest_trace(traces, task_id, "openclaw") == (latest, run.name)

    older.unlink()
    latest.unlink()
    assert run_task_compare._find_latest_trace(task_id, None) is None
    assert find_latest_trace(traces, task_id, "openclaw") == (None, "")
