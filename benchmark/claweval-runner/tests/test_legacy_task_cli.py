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

"""Legacy bare-task invocations share the modern single-run argument contract."""

import sys
from unittest.mock import patch

import pytest
from ce_runner.run_task import main


@pytest.mark.parametrize(
    "task",
    [
        "T001_email",
        "./tasks/T001",
        "/tmp/tasks/T001/task.yaml",
        "./tasks/task with spaces",
    ],
)
def test_bare_task_dispatches_single_run(task: str) -> None:
    with (
        patch.object(sys, "argv", ["ce-runner", task]),
        patch("ce_runner.run_task.run_single") as single,
        patch("ce_runner.run_task.batch_runner.run_batch") as batch,
    ):
        main()
    single.assert_called_once()
    args = single.call_args.args[0]
    assert (args.command, args.task, args.timeout, args.trace_prefix) == (
        "run",
        task,
        600,
        "openclaw",
    )
    batch.assert_not_called()


@pytest.mark.parametrize(
    "argv",
    [
        ["T001", "--timeout", "30", "--config", "config.yaml"],
        ["--timeout", "30", "T001", "--config", "config.yaml"],
    ],
)
def test_legacy_options_use_single_run_parser(argv: list[str]) -> None:
    with (
        patch.object(sys, "argv", ["ce-runner", *argv]),
        patch("ce_runner.run_task.run_single") as single,
    ):
        main()
    args = single.call_args.args[0]
    assert (args.task, args.timeout, args.config) == ("T001", 30, "config.yaml")


@pytest.mark.parametrize("flag", ["--help", "-h", "--version", "-v"])
def test_root_help_and_version_remain_global(
    flag: str, capsys: pytest.CaptureFixture[str]
) -> None:
    with (
        patch.object(sys, "argv", ["ce-runner", flag]),
        patch("ce_runner.run_task.run_single") as single,
        patch("ce_runner.run_task.batch_runner.run_batch") as batch,
    ):
        with pytest.raises(SystemExit) as error:
            main()
    assert error.value.code == 0
    output = capsys.readouterr().out
    if "help" in flag or flag == "-h":
        assert "{run,batch}" in output
    assert output.strip()
    single.assert_not_called()
    batch.assert_not_called()


def test_missing_legacy_task_fails_without_dispatch() -> None:
    with (
        patch.object(sys, "argv", ["ce-runner", "--timeout", "30"]),
        patch("ce_runner.run_task.run_single") as single,
    ):
        with pytest.raises(SystemExit) as error:
            main()
    assert error.value.code == 2
    single.assert_not_called()
