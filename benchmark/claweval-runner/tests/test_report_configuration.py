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

"""Characterize report settings in both standalone entry points."""

from __future__ import annotations

import importlib.util
import sys
from pathlib import Path
from types import ModuleType, SimpleNamespace

import pytest


@pytest.fixture(params=["analyze", "generate_trial_reports"])
def reporter(request, monkeypatch):
    directory = Path(__file__).resolve().parents[1] / "scripts"
    monkeypatch.syspath_prepend(str(directory))
    sdk = ModuleType("openai")

    def no_client(*args, **kwargs):
        pytest.fail("Configuration resolution must not create a model client")

    sdk.OpenAI = no_client
    monkeypatch.setitem(sys.modules, "openai", sdk)
    spec = importlib.util.spec_from_file_location(
        f"report_settings_{request.param}", directory / f"{request.param}.py"
    )
    assert spec is not None and spec.loader is not None
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return request.param, module


def overrides(**values):
    fields = {
        "trace_dir": None,
        "tasks_dir": None,
        "output_dir": None,
        "judge_model": None,
        "judge_base_url": None,
        "judge_api_key": None,
    }
    fields.update(values)
    return SimpleNamespace(**fields)


def assert_shape(kind, result):
    keys = {
        "trace_dir",
        "tasks_dir",
        "judge_model_id",
        "judge_base_url",
        "judge_api_key",
    }
    if kind == "generate_trial_reports":
        keys.add("output_dir")
    assert set(result) == keys
    ordered = ["trace_dir", "tasks_dir"]
    if kind == "generate_trial_reports":
        ordered.append("output_dir")
    assert list(result) == ordered + [
        "judge_api_key",
        "judge_base_url",
        "judge_model_id",
    ]


def test_repository_defaults_and_empty_judge_are_retained(reporter):
    kind, module = reporter
    result = module.load_config(None, overrides())
    assert_shape(kind, result)
    assert result["trace_dir"] == str(module.REPO_DIR / "claw-eval/traces")
    assert result["tasks_dir"] == str(module.REPO_DIR / "claw-eval/tasks")
    assert all(
        result[key] == ""
        for key in ("judge_model_id", "judge_base_url", "judge_api_key")
    )
    if kind == "generate_trial_reports":
        assert result["output_dir"] == str(module.REPO_DIR / "claw-eval/reports")


def test_yaml_judge_and_config_relative_defaults(reporter, tmp_path):
    kind, module = reporter
    path = tmp_path / "config.yaml"
    path.write_text(
        "judge:\n  api_key: fixture\n  base_url: https://fixture.invalid/v1\n  model_id: fixture-judge\nmodel:\n  api_key: ignored-agent-key\ndefaults:\n  trace_dir: custom-traces\n  tasks_dir: custom-tasks\n",
        encoding="utf-8",
    )
    result = module.load_config(str(path), overrides())
    assert_shape(kind, result)
    assert result["judge_api_key"] == "fixture"
    assert result["judge_model_id"] == "fixture-judge"
    assert result["judge_base_url"] == "https://fixture.invalid/v1"
    assert result["trace_dir"] == str(tmp_path / "custom-traces")
    assert result["tasks_dir"] == str(tmp_path / "custom-tasks")
    if kind == "generate_trial_reports":
        assert result["output_dir"] == str(tmp_path / "reports")


def test_explicit_cli_values_take_precedence_without_reanchoring(reporter, tmp_path):
    kind, module = reporter
    path = tmp_path / "config.yaml"
    path.write_text(
        "judge:\n  api_key: old\n  base_url: old\n  model_id: old\ndefaults:\n  trace_dir: old\n  tasks_dir: old\n",
        encoding="utf-8",
    )
    result = module.load_config(
        str(path),
        overrides(
            trace_dir="relative/trace",
            tasks_dir="relative/tasks",
            output_dir="relative/reports",
            judge_model="new-model",
            judge_base_url="new-url",
            judge_api_key="new-fixture",
        ),
    )
    assert_shape(kind, result)
    assert result["trace_dir"] == "relative/trace"
    assert result["tasks_dir"] == "relative/tasks"
    assert result["judge_model_id"] == "new-model"
    assert result["judge_base_url"] == "new-url"
    assert result["judge_api_key"] == "new-fixture"
    if kind == "generate_trial_reports":
        assert result["output_dir"] == "relative/reports"


def test_empty_cli_overrides_do_not_erase_yaml_settings(reporter, tmp_path):
    _, module = reporter
    path = tmp_path / "config.yaml"
    path.write_text(
        "judge:\n  api_key: fixture\n  base_url: fixture-url\n  model_id: fixture-model\n",
        encoding="utf-8",
    )
    result = module.load_config(
        str(path),
        overrides(
            trace_dir="",
            tasks_dir="",
            output_dir="",
            judge_model="",
            judge_base_url="",
            judge_api_key="",
        ),
    )
    assert result["judge_api_key"] == "fixture"
    assert result["judge_base_url"] == "fixture-url"
    assert result["judge_model_id"] == "fixture-model"
    assert result["trace_dir"] == str(tmp_path / "traces")
    assert result["tasks_dir"] == str(tmp_path / "tasks")


def test_absolute_yaml_paths_remain_absolute(reporter, tmp_path):
    _, module = reporter
    path = tmp_path / "config.yaml"
    selected = (tmp_path / "absolute-traces").as_posix()
    path.write_text(f"defaults:\n  trace_dir: '{selected}'\n", encoding="utf-8")
    result = module.load_config(str(path), overrides())
    assert Path(result["trace_dir"]) == tmp_path / "absolute-traces"


def test_relative_config_keeps_its_semantic_location(reporter, tmp_path, monkeypatch):
    _, module = reporter
    directory = tmp_path / "nested"
    directory.mkdir()
    (directory / "config.yaml").write_text(
        "defaults:\n  trace_dir: relative-traces\n", encoding="utf-8"
    )
    monkeypatch.chdir(tmp_path)
    result = module.load_config("nested/config.yaml", overrides())
    assert Path(result["trace_dir"]).resolve() == directory / "relative-traces"
    assert Path(result["tasks_dir"]).resolve() == directory / "tasks"


@pytest.mark.parametrize("content", ["", "null\n", "{}\n"])
def test_empty_yaml_preserves_fallback_shape(reporter, tmp_path, content):
    kind, module = reporter
    path = tmp_path / "config.yaml"
    path.write_text(content, encoding="utf-8")
    result = module.load_config(str(path), overrides())
    assert_shape(kind, result)
    assert result["trace_dir"] == str(tmp_path / "traces")
    assert result["judge_api_key"] == ""


def test_missing_configuration_retains_file_error(reporter, tmp_path):
    _, module = reporter
    with pytest.raises(FileNotFoundError):
        module.load_config(str(tmp_path / "missing.yaml"), overrides())
