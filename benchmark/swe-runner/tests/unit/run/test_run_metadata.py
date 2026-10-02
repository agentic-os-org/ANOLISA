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

from __future__ import annotations

from swe_runner.run.io.run_metadata import RunMetadataSnapshot, merge_run_metadata


def test_run_metadata_snapshot_builds_persisted_payload() -> None:
    payload = RunMetadataSnapshot(
        started_at_ns=1_700_000_000_000_000_000,
        ended_at_ns=1_700_000_005_000_000_000,
        agent_name="openclaw",
        workers=4,
        instance_ids=["inst-a", "inst-b"],
        succeeded=1,
        metadata_mappings={"session_ids": {"inst-a": "session-a"}},
    ).to_payload()

    assert payload["started_at"] == "2023-11-14T22:13:20+00:00"
    assert payload["ended_at"] == "2023-11-14T22:13:25+00:00"
    assert payload["instance_count"] == 2
    assert payload["attempt_count"] == 2
    assert payload["failed"] == 1
    assert payload["run_count"] == 1
    assert payload["session_ids"] == {"inst-a": "session-a"}


def test_merge_run_metadata_preserves_attempts_and_overwrites_instance_metadata() -> None:
    existing = RunMetadataSnapshot(
        started_at_ns=10,
        ended_at_ns=20,
        agent_name="openclaw",
        workers=1,
        instance_ids=["inst-a"],
        succeeded=1,
        metadata_mappings={"session_ids": {"inst-a": "session-a-1"}},
    ).to_payload()
    current = RunMetadataSnapshot(
        started_at_ns=30,
        ended_at_ns=40,
        agent_name="openclaw",
        workers=2,
        instance_ids=["inst-a", "inst-b"],
        succeeded=1,
        metadata_mappings={"session_ids": {"inst-a": "session-a-2", "inst-b": "session-b"}},
    ).to_payload()

    payload = merge_run_metadata(existing, current)

    assert payload["started_at_ns"] == 10
    assert payload["ended_at_ns"] == 40
    assert payload["workers"] == 2
    assert payload["instance_ids"] == ["inst-a", "inst-b"]
    assert payload["instance_count"] == 2
    assert payload["attempt_count"] == 3
    assert payload["succeeded"] == 2
    assert payload["failed"] == 1
    assert payload["run_count"] == 2
    assert payload["session_ids"] == {"inst-a": "session-a-2", "inst-b": "session-b"}
