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

"""Batch mode must print the trace-comparison hint after a complete run.

Usage:
    pytest tests/test_compare_trace_hint.py -v
"""

import importlib.util
from pathlib import Path
from types import SimpleNamespace

SCRIPT = Path(__file__).resolve().parent.parent / "scripts" / "run_task_compare.py"
HINT = "Compare traces:"


def _load_script():
    spec = importlib.util.spec_from_file_location("run_task_compare", SCRIPT)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def _batch_args(**overrides):
    defaults = {
        "tasks": ["T001", "T002"],
        "tasks_file": None,
        "range_str": None,
        "tag": None,
        "prefix": None,
        "filter_str": None,
        "sandbox": False,
        "sandbox_image": None,
        "timeout": 1,
        "parallel": 1,
        "config": None,
        "mode": "both",
    }
    defaults.update(overrides)
    return SimpleNamespace(**defaults)


def test_batch_mode_prints_the_hint_for_task_keyed_results(monkeypatch, capsys):
    module = _load_script()
    monkeypatch.setattr(
        module, "run_native_batch", lambda **_: {"T001": Path("/traces/T001_native.jsonl")}
    )
    monkeypatch.setattr(
        module,
        "run_ce_runner_batch",
        lambda **_: {"T001": Path("/traces/T001_ce.jsonl")},
    )

    module._run_batch_mode(_batch_args())

    assert HINT in capsys.readouterr().out


def test_batch_mode_omits_the_hint_when_one_side_produced_no_trace(monkeypatch, capsys):
    module = _load_script()
    monkeypatch.setattr(
        module, "run_native_batch", lambda **_: {"T001": Path("/traces/T001_native.jsonl")}
    )
    monkeypatch.setattr(module, "run_ce_runner_batch", lambda **_: {"T001": None})

    module._run_batch_mode(_batch_args())

    assert HINT not in capsys.readouterr().out
