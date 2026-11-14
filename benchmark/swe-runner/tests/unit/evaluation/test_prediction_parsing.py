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

"""Prediction-file parsing tests for ``_get_instance_ids``.

``_get_instance_ids`` decides which instances the evaluation harness runs. It
called ``pred.get("model_patch", "")`` on every value in the file, so a
prediction stored as a bare string or as ``null`` -- a merged or hand-edited
``preds.json`` -- raised ``AttributeError`` and aborted evaluation for the
whole file instead of skipping that one entry the way an empty patch is
skipped. A file that is not a JSON object failed the same way on ``.items()``.
"""

import json

import pytest

from swe_runner.evaluation.service import _get_instance_ids


def _write(tmp_path, payload):
    path = tmp_path / "preds.json"
    path.write_text(json.dumps(payload), encoding="utf-8")
    return path


class TestPredictionFileParsing:
    """Red on main: a non-object entry raises instead of being skipped."""

    def test_bare_string_entry_is_skipped(self, tmp_path):
        path = _write(tmp_path, {"django__django-1": "raw patch", "django__django-2": {"model_patch": "diff"}})

        assert _get_instance_ids(path) == ["django__django-2"]

    def test_null_entry_is_skipped(self, tmp_path):
        path = _write(tmp_path, {"django__django-1": None, "django__django-2": {"model_patch": "diff"}})

        assert _get_instance_ids(path) == ["django__django-2"]

    def test_list_entry_is_skipped(self, tmp_path):
        path = _write(tmp_path, {"django__django-1": ["diff"], "django__django-2": {"model_patch": "diff"}})

        assert _get_instance_ids(path) == ["django__django-2"]

    def test_empty_patch_is_skipped(self, tmp_path):
        path = _write(tmp_path, {"django__django-1": {"model_patch": ""}, "django__django-2": {"model_patch": "diff"}})

        assert _get_instance_ids(path) == ["django__django-2"]

    def test_empty_file_has_no_instances(self, tmp_path):
        assert _get_instance_ids(_write(tmp_path, {})) == []

    def test_non_object_file_is_reported(self, tmp_path):
        with pytest.raises(ValueError, match="must be a JSON object"):
            _get_instance_ids(_write(tmp_path, [{"model_patch": "diff"}]))

    def test_missing_file_is_reported(self, tmp_path):
        with pytest.raises(FileNotFoundError):
            _get_instance_ids(tmp_path / "absent.json")
