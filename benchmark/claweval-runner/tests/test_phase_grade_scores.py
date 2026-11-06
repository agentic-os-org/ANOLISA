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

"""phase_grade must read scores from the trace's grading_result event even
when other lines in the trace are malformed or blank."""

import json
from pathlib import Path

from pytest_mock import MockerFixture

from ce_runner.pipeline import phase_grade

JUDGE = {"model": "judge-m", "base_url": "http://127.0.0.1:1", "api_key": "k"}

GRADING_RESULT = {
    "type": "grading_result",
    "scores": {
        "completion": 0.9,
        "robustness": 0.8,
        "communication": 0.7,
        "safety": 1.0,
    },
    "task_score": 0.75,
    "passed": True,
}


def _run(trace: Path, mocker: MockerFixture) -> dict:
    mocker.patch(
        "ce_runner.pipeline.subprocess.run", return_value=mocker.Mock(returncode=0)
    )
    return phase_grade(str(trace), "task.yaml", JUDGE)


def test_phase_grade_reads_scores_after_a_malformed_line(tmp_path, mocker):
    trace = tmp_path / "trace.jsonl"
    trace.write_text(
        '{"type":"trace_start" broken\n'
        + json.dumps(GRADING_RESULT, ensure_ascii=False)
        + "\n",
        encoding="utf-8",
    )
    scores = _run(trace, mocker)
    assert scores["task_score"] == 0.75
    assert scores["passed"] is True
    assert scores["completion"] == 0.9


def test_phase_grade_skips_blank_lines(tmp_path, mocker):
    trace = tmp_path / "trace.jsonl"
    trace.write_text(
        "\n"
        + json.dumps(GRADING_RESULT, ensure_ascii=False)
        + "\n\n",
        encoding="utf-8",
    )
    scores = _run(trace, mocker)
    assert scores["task_score"] == 0.75
    assert scores["passed"] is True


def test_phase_grade_reads_utf8_bodies_under_any_locale(tmp_path, mocker):
    trace = tmp_path / "trace.jsonl"
    event = dict(GRADING_RESULT)
    event["scores"] = dict(GRADING_RESULT["scores"])
    trace.write_text(
        json.dumps(
            {"type": "message", "text": "评分：通过"},
            ensure_ascii=False,
        )
        + "\n"
        + json.dumps(event, ensure_ascii=False)
        + "\n",
        encoding="utf-8",
    )
    scores = _run(trace, mocker)
    assert scores["task_score"] == 0.75


def test_phase_grade_keeps_defaults_without_grading_result(tmp_path, mocker):
    trace = tmp_path / "trace.jsonl"
    trace.write_text(
        json.dumps({"type": "trace_end"}) + "\n", encoding="utf-8"
    )
    scores = _run(trace, mocker)
    assert scores == {
        "completion": 0.0,
        "robustness": 0.0,
        "communication": 0.0,
        "safety": 0.0,
        "task_score": 0.0,
        "passed": False,
    }
