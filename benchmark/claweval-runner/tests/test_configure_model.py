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

"""Regression tests for updating model configuration without discarding settings."""

import importlib.util
from pathlib import Path
from types import ModuleType

import pytest
import yaml


SCRIPT_PATH = Path(__file__).resolve().parents[1] / "scripts" / "configure_model.py"


def load_configure_model() -> ModuleType:
    spec = importlib.util.spec_from_file_location("configure_model", SCRIPT_PATH)
    assert spec is not None and spec.loader is not None
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def test_update_preserves_unrelated_settings_in_each_role(tmp_path: Path) -> None:
    module = load_configure_model()
    config_path = tmp_path / "config.yaml"
    config_path.write_text(
        "model:\n"
        "  api_key: old-model-key\n"
        "  base_url: https://old-model.example/v1\n"
        "  model_id: old-model\n"
        "  input_modalities: [text, audio]\n"
        "  temperature: 0.25\n"
        "judge:\n"
        "  api_key: old-judge-key\n"
        "  base_url: https://old-judge.example/v1\n"
        "  model_id: old-judge\n"
        "  enabled: false\n"
        "  timeout: 90\n"
        "user_agent_model:\n"
        "  api_key: old-agent-key\n"
        "  base_url: https://old-agent.example/v1\n"
        "  model_id: old-agent\n"
        "  temperature: 0.8\n",
        encoding="utf-8",
    )

    module.update_config_file(
        config_path,
        {
            "api_key": "new-model-key",
            "base_url": "https://model.example/v1",
            "model_id": "model-v2",
        },
        {
            "api_key": "new-judge-key",
            "base_url": "https://judge.example/v1",
            "model_id": "judge-v2",
        },
        {
            "api_key": "new-agent-key",
            "base_url": "https://agent.example/v1",
            "model_id": "agent-v2",
        },
    )

    saved = yaml.safe_load(config_path.read_text(encoding="utf-8"))
    assert saved["model"] == {
        "api_key": "new-model-key",
        "base_url": "https://model.example/v1",
        "model_id": "model-v2",
        "input_modalities": ["text", "audio"],
        "temperature": 0.25,
    }
    assert saved["judge"] == {
        "api_key": "new-judge-key",
        "base_url": "https://judge.example/v1",
        "model_id": "judge-v2",
        "enabled": False,
        "timeout": 90,
    }
    assert saved["user_agent_model"] == {
        "api_key": "new-agent-key",
        "base_url": "https://agent.example/v1",
        "model_id": "agent-v2",
        "temperature": 0.8,
    }


def test_new_config_keeps_default_roles_and_modalities(tmp_path: Path) -> None:
    module = load_configure_model()
    config_path = tmp_path / "config.yaml"

    module.update_config_file(
        config_path,
        {
            "api_key": "model-key",
            "base_url": "https://model.example/v1",
            "model_id": "model-v1",
        },
        {
            "api_key": "judge-key",
            "base_url": "https://judge.example/v1",
            "model_id": "judge-v1",
        },
        {
            "api_key": "agent-key",
            "base_url": "https://agent.example/v1",
            "model_id": "agent-v1",
        },
    )

    saved = yaml.safe_load(config_path.read_text(encoding="utf-8"))
    assert saved["model"]["input_modalities"] == ["text", "image"]
    assert saved["judge"]["enabled"] is True
    assert saved["user_agent_model"]["api_key"] == "agent-key"


@pytest.mark.parametrize("configure_user_agent", [False, True])
def test_yaml_aliases_keep_role_updates_independent(
    tmp_path: Path, configure_user_agent: bool
) -> None:
    module = load_configure_model()
    config_path = tmp_path / "config.yaml"
    config_path.write_text(
        "model: &role\n"
        "  api_key: original-key\n"
        "  base_url: https://original.example/v1\n"
        "  model_id: original-model\n"
        "  input_modalities: [text, audio]\n"
        "  enabled: false\n"
        "  temperature: 0.25\n"
        "judge: *role\n"
        "user_agent_model: *role\n",
        encoding="utf-8",
    )
    loaded = module.load_yaml(config_path)
    assert loaded["model"] is loaded["judge"] is loaded["user_agent_model"]
    original = dict(loaded["model"])
    updates = {
        role: {
            "api_key": f"{role}-key",
            "base_url": f"https://{role}.example/v1",
            "model_id": f"{role}-model",
        }
        for role in ("model", "judge", "user_agent_model")
    }

    module.update_config_file(
        config_path,
        updates["model"],
        updates["judge"],
        updates["user_agent_model"] if configure_user_agent else None,
    )

    saved = module.load_yaml(config_path)
    assert saved["model"] == original | updates["model"]
    assert saved["judge"] == original | updates["judge"]
    assert saved["user_agent_model"] == (
        original | updates["user_agent_model"] if configure_user_agent else original
    )
