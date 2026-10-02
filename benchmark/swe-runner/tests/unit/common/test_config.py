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

"""Tests for configuration models."""

from pathlib import Path

from swe_runner.common.models import AgentConfig, DatasetConfig, OutputConfig, Settings


class TestAgentConfig:
    """Tests for AgentConfig."""

    def test_agent_config_defaults(self) -> None:
        """Verify default values for AgentConfig."""
        config = AgentConfig(name="cosh")
        assert config.name == "cosh"
        assert config.timeout == 1800
        assert config.step_limit == 0
        assert config.workers == 1
        assert config.docker_pull_registry is None
        assert config.use_skill is False
        assert config.tokenless is False

    def test_agent_config_custom_values(self) -> None:
        """Create AgentConfig with custom values."""
        config = AgentConfig(
            name="openclaw",
            timeout=3600,
            step_limit=10,
            docker_pull_registry="registry.example.com",
            use_skill=True,
            tokenless=True,
        )
        assert config.name == "openclaw"
        assert config.timeout == 3600
        assert config.step_limit == 10
        assert config.docker_pull_registry == "registry.example.com"
        assert config.use_skill is True
        assert config.tokenless is True

    def test_workers_validation(self) -> None:
        """Verify workers must be >= 1."""
        import pytest
        from pydantic import ValidationError

        with pytest.raises(ValidationError):
            AgentConfig(name="cosh", workers=0)

        with pytest.raises(ValidationError):
            AgentConfig(name="cosh", workers=-1)

    def test_skill_and_per_case_prompt_are_mutually_exclusive(self) -> None:
        """Verify only one guidance injection mode can be enabled."""
        import pytest
        from pydantic import ValidationError

        with pytest.raises(ValidationError, match="mutually exclusive"):
            AgentConfig(name="openclaw", use_skill=True, per_case_prompt=True)


class TestDatasetConfig:
    """Tests for DatasetConfig."""

    def test_dataset_config_defaults(self) -> None:
        """Verify default values for DatasetConfig."""
        config = DatasetConfig()
        assert config.subset == "lite"
        assert config.split == "dev"
        assert config.filter_regex is None
        assert config.slice_range is None
        assert config.instance_ids is None

    def test_dataset_config_slice_parsing(self) -> None:
        """Test slice_range parsing."""
        # "0:5" -> (0, 5)
        config = DatasetConfig(slice_range="0:5")
        assert config.get_slice() == (0, 5)

        # "10:" -> (10, -1)
        config = DatasetConfig(slice_range="10:")
        assert config.get_slice() == (10, -1)

        # ":5" -> (0, 5)
        config = DatasetConfig(slice_range=":5")
        assert config.get_slice() == (0, 5)

        # None -> None
        config = DatasetConfig()
        assert config.get_slice() is None


class TestOutputConfig:
    """Tests for OutputConfig."""

    def test_output_config_default_path(self) -> None:
        """Verify default is Path('./output')."""
        config = OutputConfig()
        assert config.output_dir == Path("./output")

    def test_output_config_custom_path(self) -> None:
        """Test custom output directory."""
        config = OutputConfig(output_dir=Path("/custom/output"))
        assert config.output_dir == Path("/custom/output")


class TestSettings:
    """Tests for Settings."""

    def test_settings_composition(self) -> None:
        """Create Settings with all sub-configs, verify nesting works."""
        agent = AgentConfig(name="openclaw", timeout=3600)
        dataset = DatasetConfig(subset="verified", split="test")
        output = OutputConfig(output_dir=Path("/results"))

        settings = Settings(agent=agent, dataset=dataset, output=output)

        assert settings.agent.name == "openclaw"
        assert settings.agent.timeout == 3600
        assert settings.dataset.subset == "verified"
        assert settings.dataset.split == "test"
        assert settings.output.output_dir == Path("/results")

    def test_settings_defaults(self) -> None:
        """Verify Settings creates default sub-configs."""
        settings = Settings()
        assert settings.agent.name == "cosh"
        assert settings.dataset.subset == "lite"
        assert settings.output.output_dir == Path("./output")
