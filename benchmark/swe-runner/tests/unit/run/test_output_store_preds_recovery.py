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

"""Truncated ``preds.json`` recovery tests for ``RunOutputStore``.

``write_prediction`` rewrote ``preds.json`` in place and re-parsed it with
``json.loads`` on the next call. A run interrupted mid-write leaves a truncated
or empty file, and every later prediction then raised ``JSONDecodeError``, so a
resumed batch could never write another prediction. A file that is valid JSON
but not an object failed the same way, one step later, on item assignment.
"""

import json

import pytest

from swe_runner.common.models import Prediction
from swe_runner.run.io.output_store import RunOutputStore


def _prediction(instance_id: str, patch: str = "diff") -> Prediction:
    return Prediction(instance_id=instance_id, model_name_or_path="test", model_patch=patch)


@pytest.fixture
def store(tmp_path):
    return RunOutputStore(tmp_path / "run")


class TestTruncatedPredictionsFile:
    """Red on main: the next prediction raises instead of being written."""

    @pytest.mark.parametrize("content", ["", '{"django__django-1": ', "null", "[1, 2]"])
    def test_unreadable_preds_file_is_rebuilt(self, store, content):
        store.output_dir.mkdir(parents=True)
        store.predictions_path.write_text(content, encoding="utf-8")

        store.write_prediction(_prediction("django__django-2"))

        written = json.loads(store.predictions_path.read_text(encoding="utf-8"))
        assert list(written) == ["django__django-2"]

    def test_existing_entries_are_kept(self, store):
        store.write_prediction(_prediction("django__django-1"))
        store.write_prediction(_prediction("django__django-2"))

        written = json.loads(store.predictions_path.read_text(encoding="utf-8"))
        assert sorted(written) == ["django__django-1", "django__django-2"]

    def test_one_prediction_replaces_its_own_entry(self, store):
        store.write_prediction(_prediction("django__django-1", "old"))
        store.write_prediction(_prediction("django__django-1", "new"))

        written = json.loads(store.predictions_path.read_text(encoding="utf-8"))
        assert written["django__django-1"]["model_patch"] == "new"

    def test_no_temporary_file_is_left_behind(self, store):
        store.write_prediction(_prediction("django__django-1"))

        assert [path.name for path in store.output_dir.iterdir()] == ["preds.json"]
