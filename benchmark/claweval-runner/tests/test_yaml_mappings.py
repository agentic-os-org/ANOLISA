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

"""YAML loader contracts reject wrong root shapes with useful file diagnostics."""

from collections.abc import Callable
from pathlib import Path
from typing import Any

import pytest
import yaml
from ce_runner._common import load_config, load_task_yaml


@pytest.mark.parametrize("loader", [load_task_yaml, load_config])
@pytest.mark.parametrize("content", ["[]", "- item", "false", "0", "a string"])
def test_loaders_reject_nonmapping_yaml(
    tmp_path: Path, loader: Callable[[str], dict[str, Any]], content: str
) -> None:
    path = tmp_path / "input.yaml"
    path.write_text(content, encoding="utf-8")

    with pytest.raises(ValueError, match="mapping") as error:
        loader(str(path))

    assert str(path) in str(error.value)


def test_empty_task_is_rejected_with_filename(tmp_path: Path) -> None:
    path = tmp_path / "task.yaml"
    path.write_text("# no task definition\n", encoding="utf-8")

    with pytest.raises(ValueError, match="mapping") as error:
        load_task_yaml(str(path))

    assert str(path) in str(error.value)


def test_optional_empty_config_keeps_environment_fallback(tmp_path: Path) -> None:
    path = tmp_path / "config.yaml"
    path.write_text("# use environment defaults\n", encoding="utf-8")
    assert load_config(str(path)) == {}
    assert load_config(None) == {}
    assert load_config(str(tmp_path / "missing.yaml")) == {}


@pytest.mark.parametrize("loader", [load_task_yaml, load_config])
def test_invalid_yaml_reports_its_filename(tmp_path: Path, loader: Callable[[str], dict[str, Any]]) -> None:
    path = tmp_path / "input.yaml"
    path.write_text("broken: [\n", encoding="utf-8")

    with pytest.raises(yaml.YAMLError) as error:
        loader(str(path))

    assert str(path) in str(error.value)


@pytest.mark.parametrize("loader", [load_task_yaml, load_config])
def test_valid_mapping_keeps_unicode_content(tmp_path: Path, loader: Callable[[str], dict[str, Any]]) -> None:
    path = tmp_path / "input.yaml"
    path.write_text("prompt: 你好\noptions:\n  enabled: true\n", encoding="utf-8")
    assert loader(str(path)) == {"prompt": "你好", "options": {"enabled": True}}
