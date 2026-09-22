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

"""Tests for dataset loading and filtering."""

from __future__ import annotations

from unittest.mock import patch

from swe_runner.common.dataset_registry import DATASET_MAPPING, get_dataset_name
from swe_runner.common.models import DatasetConfig, SWEInstance
from swe_runner.run.dataset import (
    filter_instances,
    load_dataset,
)
from swe_runner.run.workspace.docker_refs import get_docker_image_name

MOCK_DATASET_ROWS = [
    {
        "instance_id": "django__django-11179",
        "repo": "django/django",
        "version": "3.0",
        "base_commit": "abc",
        "problem_statement": "Bug 1",
        "patch": "",
        "test_patch": "",
    },
    {
        "instance_id": "django__django-16379",
        "repo": "django/django",
        "version": "3.1",
        "base_commit": "def",
        "problem_statement": "Bug 2",
        "patch": "",
        "test_patch": "",
    },
    {
        "instance_id": "flask__flask-4992",
        "repo": "pallets/flask",
        "version": "2.0",
        "base_commit": "ghi",
        "problem_statement": "Bug 3",
        "patch": "",
        "test_patch": "",
    },
]


def _mock_instances() -> list[SWEInstance]:
    return [SWEInstance.from_dataset_row(row) for row in MOCK_DATASET_ROWS]


class TestDatasetMapping:
    def test_dataset_mapping_has_required_subsets(self):
        for key in ("lite", "verified", "full", "multilingual"):
            assert key in DATASET_MAPPING

    def test_dataset_mapping_casing_consistent(self):
        """All mapped names should use canonical lowercase 'b' in SWE-bench."""
        for value in DATASET_MAPPING.values():
            assert "SWE-bench" in value, f"Expected canonical 'SWE-bench' casing in {value!r}"


class TestGetDatasetName:
    def test_known_subsets(self):
        assert get_dataset_name("lite") == "princeton-nlp/SWE-bench_Lite"
        assert get_dataset_name("verified") == "princeton-nlp/SWE-bench_Verified"
        assert get_dataset_name("full") == "princeton-nlp/SWE-bench"
        assert get_dataset_name("multilingual") == "SWE-bench/SWE-bench_Multilingual"

    def test_unknown_subset_raises(self):
        import pytest

        with pytest.raises(ValueError, match="Unknown subset"):
            get_dataset_name("nonexistent")


class TestLoadDataset:
    @patch("swe_runner.run.dataset.hf_datasets.load_dataset")
    def test_load_dataset_uses_correct_path(self, mock_load):
        mock_load.return_value = MOCK_DATASET_ROWS
        config = DatasetConfig(subset="lite", split="dev")
        load_dataset(config)
        mock_load.assert_called_once_with("princeton-nlp/SWE-bench_Lite", split="dev")

    @patch("swe_runner.run.dataset.hf_datasets.load_dataset")
    def test_load_dataset_uses_multilingual_path(self, mock_load):
        mock_load.return_value = MOCK_DATASET_ROWS
        config = DatasetConfig(subset="multilingual", split="test")
        load_dataset(config)
        mock_load.assert_called_once_with("SWE-bench/SWE-bench_Multilingual", split="test")

    @patch("swe_runner.run.dataset.hf_datasets.load_dataset")
    def test_load_dataset_custom_path(self, mock_load):
        mock_load.return_value = MOCK_DATASET_ROWS
        config = DatasetConfig(subset="org/custom-dataset", split="test")
        load_dataset(config)
        mock_load.assert_called_once_with("org/custom-dataset", split="test")


class TestFilterInstances:
    def test_filter_by_instance_ids(self):
        instances = _mock_instances()
        config = DatasetConfig(instance_ids=["django__django-11179", "flask__flask-4992"])
        result = filter_instances(instances, config)
        assert len(result) == 2
        ids = {i.instance_id for i in result}
        assert ids == {"django__django-11179", "flask__flask-4992"}

    def test_filter_by_regex(self):
        instances = _mock_instances()
        config = DatasetConfig(filter_regex=r"django__django-\d+")
        result = filter_instances(instances, config)
        assert len(result) == 2
        assert all("django__django" in i.instance_id for i in result)

    def test_filter_by_slice(self):
        instances = _mock_instances()
        config = DatasetConfig(slice_range="0:2")
        result = filter_instances(instances, config)
        assert len(result) == 2
        assert result[0].instance_id == "django__django-11179"
        assert result[1].instance_id == "django__django-16379"

    def test_filter_combined(self):
        instances = _mock_instances()
        config = DatasetConfig(
            filter_regex=r"django",
            slice_range="0:1",
        )
        result = filter_instances(instances, config)
        assert len(result) == 1
        assert result[0].instance_id == "django__django-11179"


class TestGetDockerImageName:
    def test_get_docker_image_name_default(self):
        instance = SWEInstance.from_dataset_row(MOCK_DATASET_ROWS[0])
        name = get_docker_image_name(instance)
        assert name == "swebench/sweb.eval.x86_64.django_1776_django-11179:latest"

    def test_get_docker_image_name_with_explicit_image_name(self):
        row = MOCK_DATASET_ROWS[0].copy()
        row["image_name"] = "custom.registry.com/my-image:tag"
        instance = SWEInstance.from_dataset_row(row)
        name = get_docker_image_name(instance)
        assert name == "custom.registry.com/my-image:tag"

    def test_get_docker_image_name_with_explicit_docker_image(self):
        row = MOCK_DATASET_ROWS[0].copy()
        row["docker_image"] = "another.registry.com/docker-img:1.0"
        instance = SWEInstance.from_dataset_row(row)
        name = get_docker_image_name(instance)
        assert name == "another.registry.com/docker-img:1.0"

    def test_get_docker_image_name_image_name_priority(self):
        row = MOCK_DATASET_ROWS[0].copy()
        row["image_name"] = "image-name-priority:latest"
        row["docker_image"] = "docker-image-secondary:latest"
        instance = SWEInstance.from_dataset_row(row)
        name = get_docker_image_name(instance)
        assert name == "image-name-priority:latest"
