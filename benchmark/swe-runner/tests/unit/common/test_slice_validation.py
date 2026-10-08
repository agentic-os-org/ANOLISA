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

"""A typo'd --slice must fail validation, not crash in get_slice().

``--slice abc:def`` (or ``:five``) passed pydantic construction and only
exploded later, deep in filter_instances, with a raw
``ValueError: invalid literal for int()`` traceback. Validate the shape
at the model boundary instead.
"""

import pytest
from pydantic import ValidationError

from swe_runner.common.models import DatasetConfig


class TestSliceRangeValidation:
    def test_valid_slices_construct(self) -> None:
        assert DatasetConfig(slice_range="0:5").get_slice() == (0, 5)
        assert DatasetConfig(slice_range="10:").get_slice() == (10, -1)
        assert DatasetConfig(slice_range=":5").get_slice() == (0, 5)
        assert DatasetConfig(slice_range=None).get_slice() is None

    def test_non_numeric_start_is_rejected_at_construction(self) -> None:
        with pytest.raises(ValidationError):
            DatasetConfig(slice_range="abc:def")

    def test_non_numeric_end_is_rejected_at_construction(self) -> None:
        with pytest.raises(ValidationError):
            DatasetConfig(slice_range=":five")

    def test_three_part_slice_is_rejected_at_construction(self) -> None:
        with pytest.raises(ValidationError):
            DatasetConfig(slice_range="0:5:2")

    def test_error_message_names_the_field(self) -> None:
        with pytest.raises(ValidationError) as excinfo:
            DatasetConfig(slice_range="abc:def")
        assert "slice_range" in str(excinfo.value)
