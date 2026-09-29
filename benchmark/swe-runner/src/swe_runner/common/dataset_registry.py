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

DATASET_MAPPING: dict[str, str] = {
    "lite": "princeton-nlp/SWE-bench_Lite",
    "verified": "princeton-nlp/SWE-bench_Verified",
    "full": "princeton-nlp/SWE-bench",
    "multilingual": "SWE-bench/SWE-bench_Multilingual",
}


def get_dataset_name(subset: str) -> str:
    """Resolve a subset shorthand to its full HuggingFace dataset name.

    Args:
        subset: One of the known subset keys (lite, verified, full, multilingual).

    Returns:
        The full dataset path (e.g. ``princeton-nlp/SWE-bench_Lite``).

    Raises:
        ValueError: If *subset* is not a known key.
    """
    if subset not in DATASET_MAPPING:
        raise ValueError(
            f"Unknown subset: {subset}. Must be one of: {', '.join(DATASET_MAPPING)}"
        )
    return DATASET_MAPPING[subset]
