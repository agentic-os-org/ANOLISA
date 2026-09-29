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

"""TDD tests for core domain models."""

import json

import pytest
from pydantic import ValidationError

from swe_runner.common.models import AgentResult, InstanceResult, Prediction, SWEInstance


class TestSWEInstance:
    """Tests for SWEInstance model."""

    def test_swe_instance_creation(self) -> None:
        """Create SWEInstance, verify all fields."""
        instance = SWEInstance(
            instance_id="django__django-12345",
            repo="django/django",
            version="3.2",
            base_commit="abc123",
            problem_statement="Fix the bug",
            patch="diff --git a/file.py",
            test_patch="diff --git a/test_file.py",
        )

        assert instance.instance_id == "django__django-12345"
        assert instance.repo == "django/django"
        assert instance.version == "3.2"
        assert instance.base_commit == "abc123"
        assert instance.problem_statement == "Fix the bug"
        assert instance.patch == "diff --git a/file.py"
        assert instance.test_patch == "diff --git a/test_file.py"

    def test_swe_instance_from_dataset_row(self) -> None:
        """Create from dict, verify mapping."""
        row = {
            "instance_id": "django__django-12345",
            "repo": "django/django",
            "version": "3.2",
            "base_commit": "abc123",
            "problem_statement": "Fix the bug",
            "patch": "diff --git a/file.py",
            "test_patch": "diff --git a/test_file.py",
        }

        instance = SWEInstance.from_dataset_row(row)

        assert instance.instance_id == "django__django-12345"
        assert instance.repo == "django/django"
        assert instance.version == "3.2"
        assert instance.base_commit == "abc123"
        assert instance.problem_statement == "Fix the bug"
        assert instance.patch == "diff --git a/file.py"
        assert instance.test_patch == "diff --git a/test_file.py"


class TestPrediction:
    """Tests for Prediction model."""

    def test_prediction_json_matches_swe_bench_format(self) -> None:
        """Serialize to JSON, verify keys match SWE-bench format."""
        prediction = Prediction(
            instance_id="django__django-12345",
            model_name_or_path="cosh",
            model_patch="diff --git a/file.py",
        )

        # Serialize to JSON
        json_str = prediction.model_dump_json()
        data = json.loads(json_str)

        # Verify keys match SWE-bench format
        assert "instance_id" in data
        assert "model_name_or_path" in data
        assert "model_patch" in data
        assert data["instance_id"] == "django__django-12345"
        assert data["model_name_or_path"] == "cosh"
        assert data["model_patch"] == "diff --git a/file.py"

    def test_prediction_missing_field_raises(self) -> None:
        """Try to create Prediction without required field, verify ValidationError."""
        with pytest.raises(ValidationError):
            Prediction(
                instance_id="django__django-12345",
                model_name_or_path="cosh",
                # missing model_patch
            )


class TestAgentResult:
    """Tests for AgentResult model."""

    def test_agent_result_success(self) -> None:
        """Create successful AgentResult, verify patch is set, success=True."""
        result = AgentResult(
            raw_output="Agent output here",
            patch="diff --git a/file.py",
            success=True,
            duration_seconds=10.5,
        )

        assert result.raw_output == "Agent output here"
        assert result.patch == "diff --git a/file.py"
        assert result.success is True
        assert result.duration_seconds == 10.5
        assert result.error is None

    def test_agent_result_failure(self) -> None:
        """Create failed AgentResult with error, verify success=False, patch=None."""
        result = AgentResult(
            raw_output="Agent failed",
            patch=None,
            success=False,
            duration_seconds=5.0,
            error="Timeout exceeded",
        )

        assert result.raw_output == "Agent failed"
        assert result.patch is None
        assert result.success is False
        assert result.duration_seconds == 5.0
        assert result.error == "Timeout exceeded"


class TestInstanceResult:
    """Tests for InstanceResult model."""

    def test_instance_result_success(self) -> None:
        """Create with valid prediction, verify success=True."""
        instance = SWEInstance(
            instance_id="django__django-12345",
            repo="django/django",
            version="3.2",
            base_commit="abc123",
            problem_statement="Fix the bug",
            patch="diff --git a/file.py",
            test_patch="diff --git a/test_file.py",
        )
        prediction = Prediction(
            instance_id="django__django-12345",
            model_name_or_path="cosh",
            model_patch="diff --git a/file.py",
        )
        agent_result = AgentResult(
            raw_output="Agent output",
            patch="diff --git a/file.py",
            success=True,
            duration_seconds=10.0,
        )

        result = InstanceResult(
            instance=instance,
            prediction=prediction,
            agent_result=agent_result,
            success=True,
        )

        assert result.instance == instance
        assert result.prediction == prediction
        assert result.agent_result == agent_result
        assert result.success is True

    def test_instance_result_failure(self) -> None:
        """Create with no prediction, verify success=False."""
        instance = SWEInstance(
            instance_id="django__django-12345",
            repo="django/django",
            version="3.2",
            base_commit="abc123",
            problem_statement="Fix the bug",
            patch="diff --git a/file.py",
            test_patch="diff --git a/test_file.py",
        )
        agent_result = AgentResult(
            raw_output="Agent failed",
            patch=None,
            success=False,
            duration_seconds=5.0,
            error="Timeout",
        )

        result = InstanceResult(
            instance=instance,
            prediction=None,
            agent_result=agent_result,
            success=False,
        )

        assert result.instance == instance
        assert result.prediction is None
        assert result.agent_result == agent_result
        assert result.success is False
