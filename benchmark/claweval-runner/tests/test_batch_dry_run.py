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

"""Preview batch selection without requiring an evaluation environment."""

import sys
from contextlib import ExitStack
from pathlib import Path
from unittest.mock import patch

import pytest
from ce_runner import batch_runner
from ce_runner.run_task import main


def _tasks(tmp_path: Path) -> Path:
    root = tmp_path / "tasks"
    for name, tags in [
        ("T001_email", ["general"]),
        ("T002_email", ["general"]),
        ("T003_email", ["general"]),
        ("M001_clock", ["multimodal"]),
    ]:
        directory = root / name
        directory.mkdir(parents=True)
        (directory / "task.yaml").write_text(
            f"task_id: {name}\ntags: {tags}\n", encoding="utf-8"
        )
    return root


@pytest.mark.parametrize("selector", ["filters", "string", "file"])
def test_dry_run_uses_real_selection_without_environment_setup(
    tmp_path: Path, capsys: pytest.CaptureFixture[str], selector: str
) -> None:
    root = _tasks(tmp_path)
    if selector == "filters":
        options = [
            "--prefix",
            "T",
            "--filter",
            "email",
            "--tag",
            "general",
            "--range",
            "1-3:2",
        ]
    elif selector == "string":
        options = ["--tasks-string", "T001_email,T003_email"]
    else:
        names = tmp_path / "selection.txt"
        names.write_text("T001_email\nT003_email\n", encoding="utf-8")
        options = ["--tasks-file", str(names)]
    argv = [
        "ce-runner",
        "batch",
        "--tasks-dir",
        str(root),
        "--dry-run",
        "--trials",
        "3",
        "--config",
        "missing-config.yaml",
        *options,
    ]

    with ExitStack() as stack:
        stack.enter_context(patch.object(sys, "argv", argv))
        guards = [
            stack.enter_context(
                patch.object(
                    batch_runner,
                    name,
                    side_effect=AssertionError(f"must not call {name}"),
                )
            )
            for name in [
                "load_config",
                "require_valid_config",
                "check_gateway",
                "make_trace_dir",
                "run_preflight_checks",
                "setup_parallel_workers",
            ]
        ]
        main()
    output = capsys.readouterr().out

    assert "T001_email" in output
    assert "T003_email" in output
    assert "T002_email" not in output
    assert "M001_clock" not in output
    assert "2 tasks x 3 trials = 6 runs" in output
    assert all(not guard.called for guard in guards)


@pytest.mark.parametrize(
    "options",
    [
        ["--prefix", "Z"],
        ["--tasks-string", "missing"],
        ["--prefix", "T", "--tasks-string", "T001_email"],
    ],
)
def test_dry_run_retains_selection_errors(tmp_path: Path, options: list[str]) -> None:
    root = _tasks(tmp_path)
    argv = ["ce-runner", "batch", "--tasks-dir", str(root), "--dry-run", *options]
    with (
        patch.object(sys, "argv", argv),
        patch.object(
            batch_runner,
            "require_valid_config",
            side_effect=AssertionError("must not validate config"),
        ),
    ):
        with pytest.raises(SystemExit) as error:
            main()
    assert error.value.code == 1


def test_normal_batch_still_validates_configuration(tmp_path: Path) -> None:
    root = _tasks(tmp_path)
    argv = ["ce-runner", "batch", "--tasks-dir", str(root)]
    with (
        patch.object(sys, "argv", argv),
        patch.object(
            batch_runner,
            "require_valid_config",
            side_effect=RuntimeError("configuration reached"),
        ),
        patch.object(batch_runner, "check_gateway") as gateway,
    ):
        with pytest.raises(RuntimeError, match="configuration reached"):
            main()
    gateway.assert_not_called()
