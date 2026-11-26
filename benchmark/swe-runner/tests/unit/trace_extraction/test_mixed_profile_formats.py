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

"""Profile discovery must not publish an incomplete mixed-format export."""

import json
from pathlib import Path

import pytest

from swe_runner.trace_extraction.helpers import ExtractionError
from swe_runner.trace_extraction.openclaw_jsonl import iter_openclaw_jsonl_traces


def _jsonl_profile(root: Path, name: str) -> Path:
    profile = root / name
    session = profile / "agents" / name / "sessions" / "session.jsonl"
    session.parent.mkdir(parents=True)
    session.write_text(
        json.dumps({"type": "session", "id": name})
        + "\n"
        + json.dumps(
            {
                "type": "message",
                "timestamp_ns": 1,
                "message": {
                    "role": "assistant",
                    "content": [{"type": "text", "text": "done"}],
                    "usage": {"input": 10, "output": 2},
                },
            }
        )
        + "\n",
        encoding="utf-8",
    )
    return profile


def _sqlite_profile(root: Path, name: str) -> Path:
    profile = root / name
    store = profile / "agents" / name / "agent" / "openclaw-agent.sqlite"
    store.parent.mkdir(parents=True)
    # Discovery tests identify the unsupported store without opening its schema.
    store.touch()
    return profile


@pytest.mark.parametrize("sqlite_first", [False, True])
@pytest.mark.parametrize("explicit_profiles", [False, True])
def test_mixed_profiles_raise_before_returning_partial_traces(
    tmp_path: Path, sqlite_first: bool, explicit_profiles: bool
) -> None:
    jsonl = _jsonl_profile(tmp_path, "z-jsonl" if sqlite_first else "a-jsonl")
    sqlite = _sqlite_profile(tmp_path, "a-sqlite" if sqlite_first else "z-sqlite")
    selection = {"profiles_root": tmp_path}
    if explicit_profiles:
        selection = {"profiles_root": None, "profile_dirs": [jsonl, sqlite]}

    with pytest.raises(ExtractionError, match="has no session JSONL") as error:
        iter_openclaw_jsonl_traces(start_ns=0, end_ns=2, **selection)
    assert str(sqlite) in str(error.value)


def test_jsonl_profiles_and_empty_profiles_keep_complete_exports(tmp_path: Path) -> None:
    _jsonl_profile(tmp_path, "case-b")
    _jsonl_profile(tmp_path, "case-a")
    (tmp_path / "empty-profile").mkdir()

    traces = iter_openclaw_jsonl_traces(profiles_root=tmp_path, start_ns=0, end_ns=2)

    assert {trace["session_id"] for trace in traces} == {"case-a", "case-b"}
    assert sum(trace["total_input_tokens"] for trace in traces) == 20


def test_jsonl_remains_readable_when_its_profile_also_contains_sqlite(tmp_path: Path) -> None:
    _jsonl_profile(tmp_path, "case-a")
    _sqlite_profile(tmp_path, "case-a")

    traces = iter_openclaw_jsonl_traces(profiles_root=tmp_path, start_ns=0, end_ns=2)

    assert [trace["session_id"] for trace in traces] == ["case-a"]


def test_sqlite_only_root_keeps_the_existing_diagnostic(tmp_path: Path) -> None:
    _sqlite_profile(tmp_path, "case-a")

    with pytest.raises(ExtractionError, match="Use the SQLite reader instead"):
        iter_openclaw_jsonl_traces(profiles_root=tmp_path, start_ns=0, end_ns=2)
