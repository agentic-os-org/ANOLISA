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

"""Test that all task trace scores meet a minimum threshold.

Usage:
    pytest tests/test_task_scores.py -v -s
    python tests/test_task_scores.py

    # Custom threshold
    MIN_SCORE=0.7 pytest tests/test_task_scores.py -v -s
"""

import json
import os
import warnings
import pytest
from pathlib import Path

from .helpers import discover_task_ids, find_latest_trace, TRACES_DIR

# Configurable threshold via env var, default 0.5
_SCORE_THRESHOLD = float(os.environ.get("MIN_SCORE", "0.5"))

# Discover task IDs at module level for parametrize
_DISCOVERED_TASK_IDS = discover_task_ids(TRACES_DIR)
if not _DISCOVERED_TASK_IDS:
    _DISCOVERED_TASK_IDS = ["T009zh_contact_lookup"]


def extract_scores(trace_path: Path) -> dict | None:
    """Extract scores from a trace JSONL file.

    Prefers grading_result over trace_end for task_score.
    Returns dict with keys: task_score, passed, completion, robustness, communication, safety.
    """
    trace_end_score = None
    grading_score = None

    with open(trace_path) as f:
        for line in f:
            line = line.strip()
            if not line:
                continue
            try:
                event = json.loads(line)
            except json.JSONDecodeError:
                continue

            if event.get("type") == "trace_end":
                trace_end_score = event
            elif event.get("type") == "grading_result":
                grading_score = event

    # Prefer grading_result, fallback to trace_end
    source = grading_score or trace_end_score
    if source is None:
        return None

    scores = source.get("scores", {})
    return {
        "task_score": source.get("task_score", 0.0),
        "passed": source.get("passed", False),
        "completion": scores.get("completion", 0.0),
        "robustness": scores.get("robustness", 0.0),
        "communication": scores.get("communication", 0.0),
        "safety": scores.get("safety", 0.0),
    }


def find_trace_files(base_dir: Path, keyword: str) -> dict[str, tuple[Path | None, str]]:
    """Find the latest trace for both native and ce-runner modes.

    Returns {"native": (path, dir_name), "ce-runner": (path, dir_name)}
    """
    native = find_latest_trace(base_dir, keyword, dir_pattern="")
    ce_runner = find_latest_trace(base_dir, keyword, dir_pattern="openclaw")
    return {"native": native, "ce-runner": ce_runner}


def _dir_timestamp(dir_name: str) -> str:
    """Extract timestamp part from directory name.

    E.g. 'openclaw_26-04-28-10-41' -> '26-04-28-10-41'
         'qwen3.6-plus_26-04-27-12-38' -> '26-04-27-12-38'
    """
    import re
    m = re.search(r"_(\d{2}-\d{2}-\d{2}-\d{2}-\d{2})$", dir_name)
    return m.group(1) if m else dir_name


class TestTaskScores:
    """Verify that all task traces meet minimum score threshold."""

    @pytest.fixture(autouse=True)
    def setup(self):
        """Check if traces directory exists."""
        if not TRACES_DIR.exists():
            pytest.skip(f"Traces directory not found: {TRACES_DIR}")

    @pytest.mark.parametrize("task_id", _DISCOVERED_TASK_IDS)
    def test_task_scores_above_threshold(self, task_id: str):
        """Each task's latest trace (both native and ce-runner) should have score >= threshold."""
        traces = find_trace_files(TRACES_DIR, task_id)

        # Collect scores and directory names for both modes
        results: dict[str, dict | None] = {}
        for mode, (trace_path, dir_name) in traces.items():
            if trace_path is None:
                results[mode] = None
                continue
            scores = extract_scores(trace_path)
            results[mode] = {**(scores or {}), "dir_ts": _dir_timestamp(dir_name)}

        # Always print comparison summary
        print(f"\n  === 评分比较: {task_id} (阈值={_SCORE_THRESHOLD}) ===")
        print(f"  {'':10s} {'时间':>19s} {'综合':>8s} {'完成':>8s} {'鲁棒':>8s} {'安全':>8s} {'状态':>6s}")
        print(f"  {'-'*10} {'-'*19} {'-'*8} {'-'*8} {'-'*8} {'-'*8} {'-'*6}")

        failures: list[str] = []

        for mode, data in results.items():
            if data is None or "task_score" not in data:
                trace_path, _ = traces[mode]
                if trace_path:
                    print(f"  {mode:10s} {'(无评分)':>19s}")
                    failures.append(f"No scoring data in {mode} trace: {trace_path.name}")
                else:
                    print(f"  {mode:10s} {'(无trace)':>19s}")
                continue

            ts_short = data.get("dir_ts", "")
            status = "PASS" if data["task_score"] >= _SCORE_THRESHOLD else "FAIL"
            print(f"  {mode:10s} {ts_short:>19s} {data['task_score']:>8.3f} {data['completion']:>8.3f} "
                  f"{data['robustness']:>8.3f} {data['safety']:>8.3f} {status:>6s}")

            if data["task_score"] < _SCORE_THRESHOLD:
                trace_path, _ = traces[mode]
                failures.append(
                    f"{mode}: task_score={data['task_score']:.3f} < {_SCORE_THRESHOLD}\n"
                    f"  Trace: {trace_path}\n"
                    f"  Run: claw-eval grade --trace {trace_path} --task tasks/{task_id}"
                )

        print()

        # Warn (not fail) if any mode has scores below threshold
        if failures:
            warnings.warn(
                f"{task_id}: {len(failures)} mode(s) below threshold or missing scores:\n\n"
                + "\n\n".join(failures),
                stacklevel=1,
            )
