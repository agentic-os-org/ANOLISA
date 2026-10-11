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

"""Select batch artifacts without mistaking single-task runs for batches."""

from pathlib import Path


def find_latest_batch_dir(trace_root: Path) -> Path | None:
    """Accept a batch directory or find its newest child with batch results."""
    if (trace_root / "batch_results.json").is_file():
        return trace_root
    if not trace_root.is_dir():
        return None
    candidates = [
        child
        for child in trace_root.iterdir()
        if child.is_dir() and (child / "batch_results.json").is_file()
    ]
    return max(
        candidates, key=lambda child: (child.stat().st_mtime, child.name), default=None
    )
