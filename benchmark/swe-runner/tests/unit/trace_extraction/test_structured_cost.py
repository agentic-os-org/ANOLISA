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

"""Read recorded OpenClaw cost totals without recalculating provider billing."""

import json
from pathlib import Path
from typing import Any

import pytest

from swe_runner.trace_extraction.openclaw_jsonl import _normalize_usage, reconstruct_openclaw_jsonl_session


@pytest.mark.parametrize("total", [0, 0.025, 2])
def test_structured_cost_reads_reported_total(total: float) -> None:
    usage = {"cost": {"input": 100, "output": 50, "cacheRead": 10, "cacheWrite": 5, "total": total}}
    assert _normalize_usage(usage)["cost"] == float(total)


@pytest.mark.parametrize("key", ["cost", "estimated_cost", "estimatedCost"])
def test_scalar_cost_aliases_remain_supported(key: str) -> None:
    assert _normalize_usage({key: 0.25})["cost"] == 0.25


@pytest.mark.parametrize("cost", [{}, {"input": 0.1}, {"total": None}, {"total": "unknown"}])
def test_missing_numeric_total_is_not_invented(cost: dict[str, Any]) -> None:
    assert "cost" not in _normalize_usage({"cost": cost})
    assert _normalize_usage({"cost": cost, "estimatedCost": 0.1})["cost"] == 0.1


def test_nested_message_costs_reach_steps_and_session_total(tmp_path: Path) -> None:
    path = tmp_path / "session.jsonl"
    entries = [
        {"type": "session", "id": "session"},
        {"type": "message", "message": {"role": "user", "content": "Issue ID: django__django-1234"}},
        {"type": "message", "message": {"role": "assistant", "content": "First", "usage": {"input": 10, "output": 2, "cost": {"total": 0.125, "input": 0.01}}}},
        {"type": "message", "message": {"role": "assistant", "content": "Second", "usage": {"input": 20, "output": 3, "cost": {"total": 0.25}}}},
    ]
    path.write_text("\n".join(json.dumps(entry) for entry in entries) + "\n", encoding="utf-8")

    trace = reconstruct_openclaw_jsonl_session(path)

    assert trace is not None
    assert [step["cost"] for step in trace["steps"]] == [0.125, 0.25]
    assert trace["total_cost"] == 0.375
    assert trace["total_input_tokens"] == 30
