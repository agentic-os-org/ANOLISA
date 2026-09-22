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

"""Test config extraction and validation.

Covers:
- get_judge_config / get_model_config / get_user_agent_config
- require_valid_config fail-fast protection
- load_config / is_sandbox_task helpers

Task type coverage:
- T tasks: basic model+judge config
- M tasks: + sandbox config
- C tasks: + user_agent_model config
"""

import os
import sys
from pathlib import Path
from unittest.mock import patch, mock_open
import pytest

REPO_ROOT = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(REPO_ROOT / "src"))


class TestGetJudgeConfig:
    """Test judge config extraction."""

    def test_from_yaml_complete(self):
        """Extract complete judge config from yaml."""
        from ce_runner.run_task import get_judge_config
        
        config = {
            "judge": {
                "api_key": "sk-judge-key",
                "base_url": "https://judge.api.com",
                "model_id": "judge-model-v1"
            }
        }
        result = get_judge_config(config)
        assert result["api_key"] == "sk-judge-key"
        assert result["base_url"] == "https://judge.api.com"
        assert result["model"] == "judge-model-v1"

    def test_from_env_fallback(self):
        """Fallback to env vars when yaml missing."""
        from ce_runner.run_task import get_judge_config
        
        with patch.dict(os.environ, {
            "JUDGE_API_KEY": "env-judge-key",
            "JUDGE_BASE_URL": "https://env.judge.com",
            "JUDGE_MODEL_ID": "env-judge-model"
        }):
            result = get_judge_config({})
            assert result["api_key"] == "env-judge-key"
            assert result["base_url"] == "https://env.judge.com"
            assert result["model"] == "env-judge-model"

    def test_yaml_takes_precedence(self):
        """Yaml config takes precedence over env vars."""
        from ce_runner.run_task import get_judge_config
        
        with patch.dict(os.environ, {
            "JUDGE_API_KEY": "env-key",
            "JUDGE_BASE_URL": "https://env.com",
            "JUDGE_MODEL_ID": "env-model"
        }):
            config = {
                "judge": {
                    "api_key": "yaml-key",
                    "base_url": "https://yaml.com",
                    "model_id": "yaml-model"
                }
            }
            result = get_judge_config(config)
            assert result["api_key"] == "yaml-key"


class TestGetModelConfig:
    """Test model config extraction."""

    def test_from_yaml_complete(self):
        """Extract complete model config from yaml."""
        from ce_runner.run_task import get_model_config
        
        config = {
            "model": {
                "api_key": "sk-model-key",
                "base_url": "https://model.api.com",
                "model_id": "model-v2"
            }
        }
        result = get_model_config(config)
        assert result["api_key"] == "sk-model-key"
        assert result["base_url"] == "https://model.api.com"
        assert result["model_id"] == "model-v2"

    def test_from_env_fallback(self):
        """Fallback to env vars for model config."""
        from ce_runner.run_task import get_model_config
        
        with patch.dict(os.environ, {
            "MODEL_API_KEY": "env-model-key",
            "MODEL_BASE_URL": "https://env.model.com",
            "MODEL_ID": "env-model-id"
        }):
            result = get_model_config({})
            assert result["api_key"] == "env-model-key"


class TestGetUserAgentConfig:
    """Test user_agent config extraction (for C tasks)."""

    def test_dedicated_config(self):
        """Use user_agent_model when available."""
        from ce_runner.run_task import get_user_agent_config
        
        config = {
            "user_agent_model": {
                "api_key": "sk-ua-key",
                "base_url": "https://ua.api.com",
                "model_id": "ua-model"
            },
            "judge": {
                "api_key": "sk-judge-key",
                "base_url": "https://judge.api.com",
                "model_id": "judge-model"
            }
        }
        result = get_user_agent_config(config)
        assert result["api_key"] == "sk-ua-key"

    def test_fallback_to_judge(self):
        """Fallback to judge config when user_agent_model missing."""
        from ce_runner.run_task import get_user_agent_config
        
        config = {
            "judge": {
                "api_key": "sk-judge-key",
                "base_url": "https://judge.api.com",
                "model_id": "judge-model"
            }
        }
        result = get_user_agent_config(config)
        assert result["api_key"] == "sk-judge-key"


class TestRequireValidConfig:
    """Test fail-fast config validation."""

    def test_valid_config_passes(self):
        """Valid config should not exit."""
        from ce_runner._common import require_valid_config
        
        judge = {"api_key": "key", "base_url": "url", "model": "model"}
        model = {"api_key": "key", "base_url": "url", "model_id": "model"}
        
        # Should not raise - with env vars set
        with patch.dict('os.environ', {'JUDGE_API_KEY': 'key', 'MODEL_API_KEY': 'key'}):
            with patch('sys.exit') as mock_exit:
                require_valid_config(None, judge, model)
                mock_exit.assert_not_called()

    def test_missing_judge_api_key_exits(self):
        """Missing judge api_key should trigger sys.exit."""
        from ce_runner._common import require_valid_config
        
        judge = {"api_key": "", "base_url": "url", "model": "model"}
        model = {"api_key": "key", "base_url": "url", "model_id": "model"}
        
        with patch('sys.exit') as mock_exit:
            require_valid_config(None, judge, model)
            mock_exit.assert_called_once_with(1)

    def test_missing_model_base_url_exits(self):
        """Missing model base_url should trigger sys.exit."""
        from ce_runner._common import require_valid_config
        
        judge = {"api_key": "key", "base_url": "url", "model": "model"}
        model = {"api_key": "key", "base_url": "", "model_id": "model"}
        
        with patch('sys.exit') as mock_exit:
            require_valid_config(None, judge, model)
            mock_exit.assert_called_once_with(1)


class TestLoadConfig:
    """Test config file loading."""

    def test_load_existing_file(self, tmp_path):
        """Load config from existing yaml file."""
        from ce_runner._common import load_config
        
        config_file = tmp_path / "config.yaml"
        config_file.write_text("model:\n  api_key: test-key\n")
        
        result = load_config(str(config_file))
        assert result["model"]["api_key"] == "test-key"

    def test_load_nonexistent_file(self):
        """Return empty dict for nonexistent file."""
        from ce_runner._common import load_config
        
        result = load_config("/nonexistent/path.yaml")
        assert result == {}

    def test_load_none_path(self):
        """Return empty dict for None path."""
        from ce_runner._common import load_config
        
        result = load_config(None)
        assert result == {}


class TestIsSandboxTask:
    """Test sandbox task detection (M tasks)."""

    def test_task_with_sandbox_files(self, tmp_path):
        """Task with sandbox_files is sandbox task."""
        from ce_runner._common import is_sandbox_task
        
        task_yaml = tmp_path / "task.yaml"
        task_yaml.write_text("task_id: M001\nsandbox_files:\n  - /workspace/file.html\n")
        
        assert is_sandbox_task(str(task_yaml)) is True

    def test_task_without_sandbox_files(self, tmp_path):
        """Task without sandbox_files is not sandbox task."""
        from ce_runner._common import is_sandbox_task
        
        task_yaml = tmp_path / "task.yaml"
        task_yaml.write_text("task_id: T001\nprompt: Test\n")
        
        assert is_sandbox_task(str(task_yaml)) is False
