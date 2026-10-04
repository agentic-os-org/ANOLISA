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

"""Each claw-eval invocation owns an independent trace directory."""

from concurrent.futures import ThreadPoolExecutor
from datetime import datetime
from pathlib import Path
from unittest.mock import patch

import pytest
from ce_runner import _common

from .helpers import find_latest_trace
from .test_task_scores import _dir_timestamp


def test_same_minute_runs_preserve_independent_artifacts(tmp_path: Path) -> None:
    with patch.object(_common, "_REPO_DIR", tmp_path), patch("datetime.datetime") as clock:
        clock.now.return_value = datetime(2026, 10, 4, 12, 30)
        first = Path(_common.make_trace_dir())
        artifact = first / "batch_results.json"
        artifact.write_text('{"first": true}', encoding="utf-8")
        second = Path(_common.make_trace_dir())

    assert first != second
    assert artifact.read_text(encoding="utf-8") == '{"first": true}'
    assert not (second / "batch_results.json").exists()
    for directory in (first, second):
        assert directory.parent == tmp_path / "claw-eval" / "traces"
        assert directory.name.startswith("openclaw_26-10-04-12-30-")
        assert directory.is_dir()


def test_concurrent_runs_allocate_distinct_directories(tmp_path: Path) -> None:
    with patch.object(_common, "_REPO_DIR", tmp_path), patch("datetime.datetime") as clock:
        clock.now.return_value = datetime(2026, 10, 4, 12, 30)
        with ThreadPoolExecutor(max_workers=8) as executor:
            directories = list(executor.map(_common.make_trace_dir, ["custom"] * 24))

    assert len(set(directories)) == 24
    assert all(Path(directory).is_dir() for directory in directories)
    assert all(Path(directory).name.startswith("custom_26-10-04-12-30-") for directory in directories)


def test_trace_discovery_keeps_openclaw_prefix(tmp_path: Path) -> None:
    with patch.object(_common, "_REPO_DIR", tmp_path):
        directory = Path(_common.make_trace_dir())
    trace = directory / "T001_12345678.jsonl"
    trace.write_text("{}\n", encoding="utf-8")

    assert find_latest_trace(directory.parent, "T001", "openclaw") == (trace, directory.name)
    assert find_latest_trace(directory.parent, "T001", "") == (None, "")


@pytest.mark.parametrize(
    "name",
    ["openclaw_26-10-04-12-30", "openclaw_26-10-04-12-30-ab12_cd3", "custom_26-10-04-12-30-abcdef01"],
)
def test_report_timestamp_supports_unique_suffix(name: str) -> None:
    assert _dir_timestamp(name) == "26-10-04-12-30"
