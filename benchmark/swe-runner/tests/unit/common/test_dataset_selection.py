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

"""Dataset selectors fail before execution and preserve Python slice bounds."""

import pytest
from pydantic import ValidationError
from swe_runner.common.models import DatasetConfig


@pytest.mark.parametrize("value", ["", "5", "0:1:2", "start:5", "0:end"])
def test_invalid_slice_is_rejected_on_configuration(value: str) -> None:
    with pytest.raises(ValidationError, match="slice_range"):
        DatasetConfig(slice_range=value)


@pytest.mark.parametrize("value", ["[", "(unclosed", "*prefix"])
def test_invalid_regex_is_rejected_on_configuration(value: str) -> None:
    with pytest.raises(ValidationError, match="filter_regex"):
        DatasetConfig(filter_regex=value)


@pytest.mark.parametrize(
    ("value", "bounds"),
    [("0:5", (0, 5)), ("10:", (10, None)), (":5", (0, 5)), (":", (0, None)), ("0:-1", (0, -1)), ("-2:", (-2, None))],
)
def test_slice_bounds_distinguish_omitted_end(value: str, bounds: tuple[int, int | None]) -> None:
    assert DatasetConfig(slice_range=value).get_slice() == bounds
