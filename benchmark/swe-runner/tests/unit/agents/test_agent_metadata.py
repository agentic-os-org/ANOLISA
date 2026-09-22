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

"""Tests for agent run metadata collection."""

from swe_runner.agents.metadata import collect_result_metadata


def test_collect_result_metadata_extracts_common_session_id() -> None:
    collected = collect_result_metadata({"session_id": "sess-1"})

    assert collected.instance_result_fields == {"session_id": "sess-1"}
    assert collected.run_metadata_mappings == {"session_ids": "sess-1"}


def test_collect_result_metadata_extracts_openclaw_fields() -> None:
    collected = collect_result_metadata(
        {
            "session_id": "sess-1",
            "openclaw_profile_dir": "/tmp/profile",
            "openclaw_returncode": "2",
            "openclaw_error_log": "/tmp/openclaw-errors/case.log",
        }
    )

    assert collected.instance_result_fields == {
        "session_id": "sess-1",
        "openclaw_returncode": "2",
        "openclaw_error_log": "/tmp/openclaw-errors/case.log",
    }
    assert collected.run_metadata_mappings == {
        "session_ids": "sess-1",
        "openclaw_profile_dirs": "/tmp/profile",
    }
