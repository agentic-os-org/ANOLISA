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

"""Test that all config YAML files include 'image' in model.input_modalities."""

import pytest
from pathlib import Path

try:
    import yaml
except ImportError:
    pytest.skip("pyyaml not installed", allow_module_level=True)


REPO_ROOT = Path(__file__).resolve().parent.parent
CONFIG_DIR = REPO_ROOT / "claw-eval"

CONFIG_FILES = [
    "config.yaml",
    "config_general.yaml",
    "config_multimodal.yaml",
    "config_user_agent.yaml",
]


def _load_config(path: Path) -> dict:
    with open(path) as f:
        return yaml.safe_load(f) or {}


class TestConfigInputModalities:
    """Verify that model.input_modalities includes 'image'."""

    @pytest.mark.parametrize("fname", CONFIG_FILES)
    def test_config_has_image_modality(self, fname: str):
        config_path = CONFIG_DIR / fname
        if not config_path.exists():
            pytest.skip(f"Config file not found: {config_path}")

        cfg = _load_config(config_path)
        model = cfg.get("model", {})
        modalities = model.get("input_modalities", [])

        assert "image" in modalities, (
            f"{fname}: model.input_modalities={modalities!r} does not include 'image'.\n"
            f"  Run: python src/ce_runner/configure_model.py to regenerate configs."
        )
