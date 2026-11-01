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

"""Execution limits are rejected before CLI dispatch changes the environment."""

import sys
from unittest.mock import patch

import pytest
from ce_runner.run_task import main


@pytest.mark.parametrize(
    "option", ["--parallel", "--trials", "--chunk-size", "--timeout"]
)
@pytest.mark.parametrize("value", ["0", "-1"])
def test_nonpositive_batch_limits_fail_before_dispatch(
    option: str, value: str, capsys: pytest.CaptureFixture[str]
) -> None:
    with (
        patch.object(sys, "argv", ["ce-runner", "batch", option, value]),
        patch("ce_runner.run_task.batch_runner.run_batch") as batch,
        patch("ce_runner.run_task.run_single") as single,
    ):
        with pytest.raises(SystemExit) as error:
            main()
    assert error.value.code == 2
    assert option in capsys.readouterr().err
    batch.assert_not_called()
    single.assert_not_called()


def test_negative_grade_parallelism_fails_before_dispatch(
    capsys: pytest.CaptureFixture[str],
) -> None:
    with (
        patch.object(sys, "argv", ["ce-runner", "batch", "--grade-parallel", "-1"]),
        patch("ce_runner.run_task.batch_runner.run_batch") as batch,
    ):
        with pytest.raises(SystemExit) as error:
            main()
    assert error.value.code == 2
    assert "--grade-parallel" in capsys.readouterr().err
    batch.assert_not_called()


@pytest.mark.parametrize("value", ["0", "-1"])
def test_nonpositive_single_timeout_fails_before_dispatch(value: str) -> None:
    with (
        patch.object(sys, "argv", ["ce-runner", "run", "T001", "--timeout", value]),
        patch("ce_runner.run_task.run_single") as single,
    ):
        with pytest.raises(SystemExit) as error:
            main()
    assert error.value.code == 2
    single.assert_not_called()


@pytest.mark.parametrize("value", ["0", "1", "3"])
def test_grade_parallelism_keeps_auto_zero_and_positive_values(value: str) -> None:
    with (
        patch.object(sys, "argv", ["ce-runner", "batch", "--grade-parallel", value]),
        patch("ce_runner.run_task.batch_runner.run_batch") as batch,
    ):
        main()
    assert batch.call_args.args[0].grade_parallel == int(value)


def test_positive_batch_boundaries_remain_supported() -> None:
    argv = [
        "ce-runner",
        "batch",
        "--parallel",
        "1",
        "--trials",
        "1",
        "--chunk-size",
        "1",
        "--timeout",
        "1",
    ]
    with (
        patch.object(sys, "argv", argv),
        patch("ce_runner.run_task.batch_runner.run_batch") as batch,
    ):
        main()
    args = batch.call_args.args[0]
    assert (args.parallel, args.trials, args.chunk_size, args.timeout) == (1, 1, 1, 1)
