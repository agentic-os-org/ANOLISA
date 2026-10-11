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

"""Wheel-version extraction in verify-release must accept pre-releases."""

from __future__ import annotations

import importlib.util
from pathlib import Path

_SCRIPT = (
    Path(__file__).resolve().parents[1] / "packaging" / "raw" / "verify-release.py"
)
_spec = importlib.util.spec_from_file_location("verify_release", _SCRIPT)
_module = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(_module)

_referenced_wheel_versions = _module._referenced_wheel_versions


class TestReferencedWheelVersions:
    def test_prerelease_version_captured_whole(self):
        """A pre-release version must not be truncated at the first dash."""
        text = (
            "anolisa_tokenless-0.9.0-rc.1-cp311-cp311-"
            "manylinux_2_28_x86_64.whl\n"
        )
        assert _referenced_wheel_versions(text) == {"0.9.0-rc.1"}

    def test_plain_version(self):
        text = "anolisa_tokenless-0.9.0-cp311-cp311-win_amd64.whl\n"
        assert _referenced_wheel_versions(text) == {"0.9.0"}

    def test_multiple_wheels(self):
        text = (
            "anolisa_tokenless-0.9.0-rc.1-cp311-cp311-manylinux_2_28_x86_64.whl\n"
            "anolisa_tokenless-0.9.0-rc.1-cp312-cp312-manylinux_2_28_x86_64.whl\n"
        )
        assert _referenced_wheel_versions(text) == {"0.9.0-rc.1"}

    def test_no_wheel_reference(self):
        assert _referenced_wheel_versions("some other content\n") == set()
