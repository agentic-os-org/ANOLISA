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

"""Tests for typed run artifact metadata."""

from swe_runner.run.io.artifacts import RunArtifacts, merge_metadata


def test_run_artifacts_preserve_known_and_extra_metadata() -> None:
    artifacts = RunArtifacts.from_metadata(
        {
            "session_id": "session-1",
            "openclaw_profile_dir": "/tmp/profile",
            "custom_key": "custom-value",
            "ignored_non_string": 3,
        }
    )

    assert artifacts.session_id == "session-1"
    assert artifacts.extra_metadata == {
        "openclaw_profile_dir": "/tmp/profile",
        "custom_key": "custom-value",
    }
    assert artifacts.to_metadata() == {
        "openclaw_profile_dir": "/tmp/profile",
        "custom_key": "custom-value",
        "session_id": "session-1",
    }


def test_run_artifacts_updates_known_fields() -> None:
    artifacts = RunArtifacts.from_metadata({"session_id": "session-1"}).with_updates(
        input_manifest_path="/tmp/input_manifest.json"
    )

    assert artifacts.to_metadata() == {
        "session_id": "session-1",
        "input_manifest_path": "/tmp/input_manifest.json",
    }


def test_merge_metadata_later_values_win() -> None:
    merged = merge_metadata(
        {"session_id": "prepare-session", "custom": "first"},
        {"session_id": "run-session", "openclaw_returncode": "0"},
    )

    assert merged == {
        "custom": "first",
        "session_id": "run-session",
        "openclaw_returncode": "0",
    }
