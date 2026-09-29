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
