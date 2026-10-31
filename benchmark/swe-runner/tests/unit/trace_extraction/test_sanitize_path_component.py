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

"""Path-component tests for ``sanitize_path_component``.

The helper feeds ``trace_root / sanitize_path_component(instance_id)`` and
``detail_dir / f"{sanitize_path_component(instance_id)}.csv"``. Its
substitution keeps ``.`` and ``..`` because they are valid in a directory name,
so an instance id of ``..`` wrote the instance's traces into the parent of the
trace root -- outside the export tree, and shared with the ``.`` case.
"""

import pytest

from swe_runner.trace_extraction.helpers import sanitize_path_component


class TestSanitizePathComponent:
    @pytest.mark.parametrize("value", [None, ""])
    def test_missing_value_uses_the_placeholder(self, value):
        assert sanitize_path_component(value) == "__unknown__"

    @pytest.mark.parametrize("value", [".", "..", "..."])
    def test_all_dot_names_use_the_placeholder(self, value):
        assert sanitize_path_component(value) == "__unknown__"

    @pytest.mark.parametrize(
        ("value", "expected"),
        [
            ("django__django-1234", "django__django-1234"),
            ("org/repo#1", "org_repo_1"),
            ("a b", "a_b"),
            ("..hidden", "..hidden"),
            ("1.2.3", "1.2.3"),
        ],
    )
    def test_ordinary_names_are_only_substituted(self, value, expected):
        assert sanitize_path_component(value) == expected

    def test_parent_component_cannot_escape_the_output_directory(self, tmp_path):
        detail_dir = tmp_path / "details"
        detail_dir.mkdir()

        detail_path = detail_dir / f"{sanitize_path_component('..')}.csv"

        assert detail_path.parent == detail_dir
        assert detail_path.name == "__unknown__.csv"
