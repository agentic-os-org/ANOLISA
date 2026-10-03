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

"""Test batch output format compatibility with native claw-eval.

Verifies:
- batch_results.json is a plain list[task] (not a dict wrapper)
- Each task has required fields: task_id, task_name, difficulty, trials, error,
  avg_score, pass_at_1, pass_hat_k, avg_passed
- batch_summary.json uses dynamic keys: pass_hat_{trials}, pass_at_{trials}

Usage:
    pytest tests/test_batch_output_format.py -v -s
"""

import json
import sys
from pathlib import Path

import pytest

REPO_ROOT = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(REPO_ROOT / "src"))

TRACES_DIR = REPO_ROOT / "claw-eval" / "traces"

# Fields that native claw-eval includes in each task entry
NATIVE_TASK_FIELDS = {
    "task_id", "task_name", "difficulty", "trials", "error",
    "avg_score", "pass_at_1", "pass_hat_k", "avg_passed",
}


# ── Helpers ────────────────────────────────────────────────────────────


def _find_ce_runner_dirs() -> list[Path]:
    """Find ce-runner trace dirs that contain batch output files.
    
    Returns only the MOST RECENT directory (by modification time) to avoid
    testing against stale/historical batch outputs that may use old formats.
    """
    if not TRACES_DIR.exists():
        return []
    
    all_dirs = [
        d for d in TRACES_DIR.iterdir()
        if d.is_dir()
        and d.name.startswith("openclaw")
        and (d / "batch_results.json").exists()
    ]
    
    if not all_dirs:
        return []
    
    # Return only the most recent directory (by mtime)
    latest = max(all_dirs, key=lambda d: d.stat().st_mtime)
    return [latest]


_CE_RUNNER_DIRS = _find_ce_runner_dirs()


# ── Unit tests: aggregation primitives ─────────────────────────────────


class TestPassHatK:
    """Test pass^k estimator: (c/n)^k."""

    def test_all_pass(self):
        """All trials pass -> pass_hat_k = 1.0."""
        n, c = 3, 3
        assert (c / n) ** n == 1.0

    def test_none_pass(self):
        """No trials pass -> pass_hat_k = 0.0."""
        n, c = 3, 0
        assert (c / n) ** n == 0.0

    def test_partial_3_trials(self):
        """2/3 pass -> (2/3)^3."""
        n, c = 3, 2
        expected = (2 / 3) ** 3
        result = (c / n) ** n
        assert abs(result - expected) < 1e-10

    def test_single_trial_pass(self):
        """Single trial pass -> 1.0."""
        assert (1 / 1) ** 1 == 1.0

    def test_single_trial_fail(self):
        """Single trial fail -> 0.0."""
        assert (0 / 1) ** 1 == 0.0


class TestAvgPassed:
    """Test avg_passed threshold (>= 0.75)."""

    def test_above_threshold(self):
        assert 0.80 >= 0.75

    def test_below_threshold(self):
        assert not (0.70 >= 0.75)

    def test_at_threshold(self):
        """Boundary: exactly 0.75 is a pass."""
        assert 0.75 >= 0.75


class TestTaskLevelError:
    """Test task-level error propagation logic."""

    def test_error_when_all_trials_errored(self):
        """Propagate first error when no valid trials exist."""
        trials = [
            {"passed": False, "error": "timeout", "task_score": 0.0, "wall_time_s": 10},
            {"passed": False, "error": "crash", "task_score": 0.0, "wall_time_s": 5},
        ]
        errors = [t for t in trials if t.get("error")]
        valid = [t for t in trials if not t.get("error")]
        error = errors[0].get("error", "all trials errored") if (errors and not valid) else None
        assert error == "timeout"

    def test_no_error_when_some_valid(self):
        """No task-level error when at least one trial succeeded."""
        trials = [
            {"passed": True, "error": None, "task_score": 0.8, "wall_time_s": 10},
            {"passed": False, "error": "crash", "task_score": 0.0, "wall_time_s": 5},
        ]
        errors = [t for t in trials if t.get("error")]
        valid = [t for t in trials if not t.get("error")]
        error = errors[0].get("error", "all trials errored") if (errors and not valid) else None
        assert error is None

    def test_no_error_all_clean(self):
        """No error when all trials are clean."""
        trials = [
            {"passed": True, "error": None, "task_score": 0.9, "wall_time_s": 10},
        ]
        errors = [t for t in trials if t.get("error")]
        valid = [t for t in trials if not t.get("error")]
        error = errors[0].get("error", "all trials errored") if (errors and not valid) else None
        assert error is None


# ── Integration tests: verify actual batch output files ────────────────


class TestBatchResultsFormat:
    """Verify batch_results.json format matches native claw-eval."""

    @pytest.fixture(autouse=True)
    def setup(self):
        if not _CE_RUNNER_DIRS:
            pytest.skip("No ce-runner batch_results.json found in traces")

    @pytest.mark.parametrize("trace_dir", _CE_RUNNER_DIRS or [None],
                             ids=[d.name for d in _CE_RUNNER_DIRS] or ["none"])
    def test_top_level_is_list(self, trace_dir):
        """batch_results.json must be a plain list, not a dict wrapper."""
        if trace_dir is None:
            pytest.skip("No batch results")
        with open(trace_dir / "batch_results.json") as f:
            data = json.load(f)
        print(f"\n  {trace_dir.name}: top-level type = {type(data).__name__}")
        assert isinstance(data, list), (
            f"{trace_dir.name}/batch_results.json top-level is "
            f"{type(data).__name__}, expected list"
        )

    @pytest.mark.parametrize("trace_dir", _CE_RUNNER_DIRS or [None],
                             ids=[d.name for d in _CE_RUNNER_DIRS] or ["none"])
    def test_task_entry_has_required_fields(self, trace_dir):
        """Each task entry must have all native claw-eval required fields."""
        if trace_dir is None:
            pytest.skip("No batch results")
        with open(trace_dir / "batch_results.json") as f:
            data = json.load(f)
        if not isinstance(data, list):
            pytest.skip("Top-level not a list (legacy format)")

        print(f"\n  {trace_dir.name}: {len(data)} tasks")
        for i, task in enumerate(data):
            missing = NATIVE_TASK_FIELDS - set(task.keys())
            assert not missing, (
                f"{trace_dir.name} task[{i}] ({task.get('task_id', '?')}) "
                f"missing fields: {missing}"
            )
        print(f"  All {len(data)} tasks have required fields: {sorted(NATIVE_TASK_FIELDS)}")

    @pytest.mark.parametrize("trace_dir", _CE_RUNNER_DIRS or [None],
                             ids=[d.name for d in _CE_RUNNER_DIRS] or ["none"])
    def test_task_field_types(self, trace_dir):
        """Verify field types match native claw-eval conventions."""
        if trace_dir is None:
            pytest.skip("No batch results")
        with open(trace_dir / "batch_results.json") as f:
            data = json.load(f)
        if not isinstance(data, list):
            pytest.skip("Top-level not a list (legacy format)")

        for task in data:
            tid = task.get("task_id", "?")
            assert isinstance(task["task_id"], str), f"{tid}: task_id not str"
            assert isinstance(task["task_name"], str), f"{tid}: task_name not str"
            assert isinstance(task["difficulty"], str), f"{tid}: difficulty not str"
            assert isinstance(task["trials"], list), f"{tid}: trials not list"
            assert task["error"] is None or isinstance(task["error"], str), (
                f"{tid}: error should be None or str, got {type(task['error']).__name__}"
            )
            assert isinstance(task["avg_score"], (int, float)), f"{tid}: avg_score not numeric"
            assert isinstance(task["pass_at_1"], (int, float)), f"{tid}: pass_at_1 not numeric"
            assert isinstance(task["pass_hat_k"], (int, float)), f"{tid}: pass_hat_k not numeric"
            assert isinstance(task["avg_passed"], bool), f"{tid}: avg_passed not bool"

    @pytest.mark.parametrize("trace_dir", _CE_RUNNER_DIRS or [None],
                             ids=[d.name for d in _CE_RUNNER_DIRS] or ["none"])
    def test_pass_hat_k_formula(self, trace_dir):
        """pass_hat_k must equal (c/n)^n computed from trial data."""
        if trace_dir is None:
            pytest.skip("No batch results")
        with open(trace_dir / "batch_results.json") as f:
            data = json.load(f)
        if not isinstance(data, list):
            pytest.skip("Top-level not a list (legacy format)")

        for task in data:
            tid = task.get("task_id", "?")
            trials = task.get("trials", [])
            n = len(trials)
            if n == 0:
                continue
            c = sum(1 for t in trials if t.get("passed"))
            expected = (c / n) ** n
            actual = task["pass_hat_k"]
            assert abs(actual - expected) < 1e-6, (
                f"{tid}: pass_hat_k={actual} != expected ({c}/{n})^{n} = {expected:.6f}"
            )

    @pytest.mark.parametrize("trace_dir", _CE_RUNNER_DIRS or [None],
                             ids=[d.name for d in _CE_RUNNER_DIRS] or ["none"])
    def test_avg_passed_matches_threshold(self, trace_dir):
        """avg_passed must be True iff avg_score >= 0.75."""
        if trace_dir is None:
            pytest.skip("No batch results")
        with open(trace_dir / "batch_results.json") as f:
            data = json.load(f)
        if not isinstance(data, list):
            pytest.skip("Top-level not a list (legacy format)")

        for task in data:
            tid = task.get("task_id", "?")
            expected = task["avg_score"] >= 0.75
            assert task["avg_passed"] == expected, (
                f"{tid}: avg_passed={task['avg_passed']} but avg_score={task['avg_score']}"
            )


class TestBatchSummaryFormat:
    """Verify batch_summary.json format matches native claw-eval."""

    @pytest.fixture(autouse=True)
    def setup(self):
        summaries = [d for d in _CE_RUNNER_DIRS if (d / "batch_summary.json").exists()]
        if not summaries:
            pytest.skip("No ce-runner batch_summary.json found in traces")

    @pytest.mark.parametrize("trace_dir", _CE_RUNNER_DIRS or [None],
                             ids=[d.name for d in _CE_RUNNER_DIRS] or ["none"])
    def test_dynamic_pass_hat_key(self, trace_dir):
        """pass_hat key must be pass_hat_{trials}, not hardcoded pass_hat_1."""
        if trace_dir is None:
            pytest.skip("No batch summary")
        summary_path = trace_dir / "batch_summary.json"
        if not summary_path.exists():
            pytest.skip(f"No batch_summary.json in {trace_dir.name}")
        with open(summary_path) as f:
            data = json.load(f)

        trials = data.get("trials_per_task", 1)
        expected_key = f"pass_hat_{trials}"
        print(f"\n  {trace_dir.name}: trials={trials}, expecting key '{expected_key}'")
        assert expected_key in data, (
            f"{trace_dir.name}/batch_summary.json missing '{expected_key}'. "
            f"Keys: {list(data.keys())}"
        )

    @pytest.mark.parametrize("trace_dir", _CE_RUNNER_DIRS or [None],
                             ids=[d.name for d in _CE_RUNNER_DIRS] or ["none"])
    def test_dynamic_pass_at_key(self, trace_dir):
        """pass_at key must be pass_at_{trials}, not hardcoded pass_at_1."""
        if trace_dir is None:
            pytest.skip("No batch summary")
        summary_path = trace_dir / "batch_summary.json"
        if not summary_path.exists():
            pytest.skip(f"No batch_summary.json in {trace_dir.name}")
        with open(summary_path) as f:
            data = json.load(f)

        trials = data.get("trials_per_task", 1)
        expected_key = f"pass_at_{trials}"
        assert expected_key in data, (
            f"{trace_dir.name}/batch_summary.json missing '{expected_key}'. "
            f"Keys: {list(data.keys())}"
        )

    @pytest.mark.parametrize("trace_dir", _CE_RUNNER_DIRS or [None],
                             ids=[d.name for d in _CE_RUNNER_DIRS] or ["none"])
    def test_summary_core_fields(self, trace_dir):
        """Summary must have core fields: tasks, trials_per_task, errored, avg_score."""
        if trace_dir is None:
            pytest.skip("No batch summary")
        summary_path = trace_dir / "batch_summary.json"
        if not summary_path.exists():
            pytest.skip(f"No batch_summary.json in {trace_dir.name}")
        with open(summary_path) as f:
            data = json.load(f)

        for field in ["tasks", "trials_per_task", "errored", "avg_score"]:
            assert field in data, (
                f"{trace_dir.name}/batch_summary.json missing '{field}'"
            )
