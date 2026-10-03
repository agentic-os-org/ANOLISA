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

"""Test batch result aggregation and statistics.

Covers:
- pass_at_k calculation
- batch_results.json structure
- batch_summary.json structure  
- Multi-trial aggregation
- Error handling

Task type coverage:
- T tasks: single/multi trial
- M tasks: sandbox flag + env_snapshot
- C tasks: user_agent_rounds
"""

import pytest
import math

class TestPassAtK:
    """Test pass@k estimator."""

    def test_pass_at_k_all_pass(self):
        """All trials pass -> pass@k = 1.0."""
        from ce_runner.batch_runner import pass_at_k
        assert pass_at_k(5, 5, 1) == 1.0

    def test_pass_at_k_none_pass(self):
        """No trials pass -> pass@k = 0.0."""
        from ce_runner.batch_runner import pass_at_k
        assert pass_at_k(5, 0, 1) == 0.0

    def test_pass_at_k_partial(self):
        """Partial pass rate."""
        from ce_runner.batch_runner import pass_at_k
        result = pass_at_k(10, 5, 1)
        assert 0.0 < result < 1.0

    def test_pass_at_k_k_greater_than_n_minus_c(self):
        """k > n-c returns 1.0."""
        from ce_runner.batch_runner import pass_at_k
        # n=5, c=4, k=2 -> n-c=1 < k -> returns 1.0
        assert pass_at_k(5, 4, 2) == 1.0

class TestErroredTrialExclusion:
    """pass@k / pass^k must exclude infra-errored trials (audit p3 scenario).

    avg_score already divides over valid trials only (and the fixture-skip
    policy documents errored trials as excluded); the pass metrics must use
    the same denominator or a 1-valid-pass + 1-infra-error task reports
    pass@1=0.5 next to avg_score=0.9.
    """

    @staticmethod
    def _trial(passed, score, error=None):
        return {
            "trial": 1,
            "task_score": score,
            "passed": passed,
            "completion": score,
            "robustness": score,
            "communication": score,
            "safety": score,
            "error": error,
            "wall_time_s": 1.0,
        }

    def test_mixed_pass_and_error_excludes_errored(self):
        """1 valid PASS (0.9) + 1 infra error: pass@1 == 1.0 over valid."""
        from ce_runner.batch_runner import aggregate_task_pass_metrics

        trials = [
            self._trial(True, 0.9),
            self._trial(False, 0.0, error="judge API 429 (infra)"),
        ]
        m = aggregate_task_pass_metrics(trials)
        assert m["pass_at_1"] == 1.0
        assert m["pass_hat_k"] == 1.0
        # avg over valid trials is unchanged (0.9), not dragged to 0.45
        valid = [t for t in trials if not t.get("error")]
        assert sum(t["task_score"] for t in valid) / len(valid) == 0.9
        # raw reporting counts still cover every executed trial
        assert (m["n"], m["c"]) == (2, 1)

    def test_all_valid_mixed_outcome_unchanged(self):
        """All-valid control: metrics identical to the old all-trials math."""
        from ce_runner.batch_runner import aggregate_task_pass_metrics, pass_at_k

        trials = [self._trial(True, 0.9), self._trial(False, 0.2)]
        m = aggregate_task_pass_metrics(trials)
        assert m["pass_at_1"] == pass_at_k(2, 1, 1)
        assert m["pass_hat_k"] == 0.25
        assert (m["n_valid"], m["c_valid"]) == (2, 1)

    def test_all_valid_all_pass_unchanged(self):
        """All-valid all-pass control."""
        from ce_runner.batch_runner import aggregate_task_pass_metrics

        trials = [self._trial(True, 0.9), self._trial(True, 0.95)]
        m = aggregate_task_pass_metrics(trials)
        assert m["pass_at_1"] == 1.0
        assert m["pass_hat_k"] == 1.0

    def test_all_error_reports_zero_not_one(self):
        """All-errored control: 0.0, never pass_at_k(0, 0, 1)'s degenerate 1.0."""
        from ce_runner.batch_runner import aggregate_task_pass_metrics

        trials = [
            self._trial(False, 0.0, error="sandbox crash"),
            self._trial(False, 0.0, error="judge API 429 (infra)"),
        ]
        m = aggregate_task_pass_metrics(trials)
        assert m["pass_at_1"] == 0.0
        assert m["pass_hat_k"] == 0.0
        assert (m["n"], m["c"]) == (2, 0)
