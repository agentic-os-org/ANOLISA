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

"""Dataset name registry and resolution."""

from __future__ import annotations

# The `SWE-bench/*` org is the current home of these datasets and the only variant
# swebench >=5 can evaluate: its harness reads per-instance `image` and
# `eval_script` columns that the legacy `princeton-nlp/*` copies do not carry, and
# it fails with KeyError('image') on them. The two are otherwise the same data --
# verified over this benchmark's 48-instance shard: identical instance ids,
# base_commit, gold patch and problem_statement, and identical FAIL_TO_PASS /
# PASS_TO_PASS contents (the legacy copy stores them as JSON strings where the
# current one stores real lists).
DATASET_MAPPING: dict[str, str] = {
    "lite": "SWE-bench/SWE-bench_Lite",
    "verified": "SWE-bench/SWE-bench_Verified",
    "full": "SWE-bench/SWE-bench",
    "multilingual": "SWE-bench/SWE-bench_Multilingual",
}


def get_dataset_name(subset: str) -> str:
    """Resolve a subset shorthand to its full HuggingFace dataset name.

    Args:
        subset: One of the known subset keys (lite, verified, full, multilingual).

    Returns:
        The full dataset path (e.g. ``SWE-bench/SWE-bench_Lite``).

    Raises:
        ValueError: If *subset* is not a known key.
    """
    if subset not in DATASET_MAPPING:
        raise ValueError(
            f"Unknown subset: {subset}. Must be one of: {', '.join(DATASET_MAPPING)}"
        )
    return DATASET_MAPPING[subset]
