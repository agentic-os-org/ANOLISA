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

"""Tests for OpenClaw-specific artifact metadata."""

from swe_runner.agents.openclaw.artifacts import OpenClawArtifacts


def test_openclaw_artifacts_preserve_known_and_extra_metadata() -> None:
    artifacts = OpenClawArtifacts.from_metadata(
        {
            "openclaw_profile_dir": "/tmp/profile",
            "openclaw_returncode": "0",
            "session_id": "session-1",
            "ignored_non_string": 3,
        }
    )

    assert artifacts.openclaw_profile_dir == "/tmp/profile"
    assert artifacts.openclaw_returncode == "0"
    assert artifacts.extra_metadata == {"session_id": "session-1"}
    assert artifacts.to_metadata() == {
        "session_id": "session-1",
        "openclaw_profile_dir": "/tmp/profile",
        "openclaw_returncode": "0",
    }


def test_openclaw_artifacts_updates_known_fields() -> None:
    artifacts = OpenClawArtifacts.from_metadata({"openclaw_profile": "profile-1"}).with_updates(
        openclaw_error_log="/tmp/openclaw-errors/case.log",
    )

    assert artifacts.to_metadata() == {
        "openclaw_profile": "profile-1",
        "openclaw_error_log": "/tmp/openclaw-errors/case.log",
    }
