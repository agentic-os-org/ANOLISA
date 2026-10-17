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

import importlib.util
import os
import sys
from argparse import Namespace
from pathlib import Path
from types import ModuleType

import pytest

SCRIPTS = Path(__file__).resolve().parents[1] / "scripts"


def _script(name: str, monkeypatch: pytest.MonkeyPatch) -> ModuleType:
    monkeypatch.syspath_prepend(str(SCRIPTS))
    spec = importlib.util.spec_from_file_location(
        f"batch_report_{name}", SCRIPTS / f"{name}.py"
    )
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def _batch(root: Path, name: str, modified: int) -> Path:
    directory = root / name
    directory.mkdir(parents=True)
    (directory / "batch_results.json").write_text("[]", encoding="utf-8")
    os.utime(directory, (modified, modified))
    return directory


def _newer_single(root: Path) -> None:
    directory = root / "single-task"
    directory.mkdir(parents=True)
    (directory / "trial.jsonl").write_text("{}", encoding="utf-8")
    os.utime(directory, (300, 300))


def test_default_summary_ignores_newer_single_task(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    module = _script("summarize_results", monkeypatch)
    root = tmp_path / "claw-eval" / "traces"
    _batch(root, "old-batch", 100)
    expected = _batch(root, "new-batch", 200)
    _newer_single(root)
    assert module.find_latest_trace_dir(tmp_path) == expected


@pytest.mark.parametrize(
    "configured", ["traces", "custom-traces", "absolute", "direct-batch"]
)
def test_config_summary_resolves_usable_batch(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch, configured: str
) -> None:
    module = _script("summarize_results", monkeypatch)
    config_dir = tmp_path / "claw-eval"
    config_dir.mkdir()
    root = config_dir / (configured if configured != "absolute" else "absolute-root")
    expected = _batch(root, "batch", 100)
    _newer_single(root)
    trace_value = str(root) if configured == "absolute" else configured
    if configured == "direct-batch":
        trace_value = str(expected)
    config = config_dir / "config.yaml"
    config.write_text(f"defaults:\n  trace_dir: {trace_value}\n", encoding="utf-8")
    args = Namespace(input=None, config=str(config))
    assert module.resolve_input(args) == expected / "batch_results.json"


def test_explicit_input_remains_authoritative(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    module = _script("summarize_results", monkeypatch)
    path = tmp_path / "custom.json"
    assert (
        module.resolve_input(Namespace(input=str(path), config="missing.yaml")) == path
    )


def test_no_completed_batch_reports_no_input(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch, capsys: pytest.CaptureFixture[str]
) -> None:
    module = _script("summarize_results", monkeypatch)
    _newer_single(tmp_path / "claw-eval" / "traces")
    monkeypatch.setattr(module, "DEFAULT_BASE_DIR", tmp_path)
    with pytest.raises(SystemExit) as error:
        module.resolve_input(Namespace(input=None, config=None))
    assert error.value.code == 1
    assert "Use --input" in capsys.readouterr().err


@pytest.mark.parametrize("direct", [False, True])
def test_analysis_chooses_batch_before_judge_validation(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
    capsys: pytest.CaptureFixture[str],
    direct: bool,
) -> None:
    module = _script("analyze", monkeypatch)
    root = tmp_path / "traces"
    expected = _batch(root, "batch", 100)
    _newer_single(root)
    if direct:
        root = expected
        (root / "sessions").mkdir()
    settings = {"trace_dir": str(root), "tasks_dir": str(tmp_path), "judge_api_key": ""}
    monkeypatch.setattr(module, "load_config", lambda *args: settings)
    monkeypatch.setattr(sys, "argv", ["analyze.py"])
    with pytest.raises(SystemExit) as error:
        module.main()
    assert error.value.code == 1
    output = capsys.readouterr().err
    assert f"Using latest trace dir: {expected}" in output.splitlines()
    assert "judge api_key not configured" in output


def test_analysis_without_batch_stops_before_reports(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch, capsys: pytest.CaptureFixture[str]
) -> None:
    module = _script("analyze", monkeypatch)
    _newer_single(tmp_path)
    monkeypatch.setattr(
        module, "load_config", lambda *args: {"trace_dir": str(tmp_path)}
    )
    monkeypatch.setattr(sys, "argv", ["analyze.py"])
    with pytest.raises(SystemExit) as error:
        module.main()
    assert error.value.code == 1
    assert "no batch results found" in capsys.readouterr().err
    assert sorted(path.name for path in tmp_path.iterdir()) == ["single-task"]
