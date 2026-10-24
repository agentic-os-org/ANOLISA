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

"""Exact integer nanosecond-to-ISO rendering tests for ``ns_to_iso``.

Renders the other direction of the pair that ``test_parse_time_precision``
pins: ``ns_to_iso`` converted through ``datetime.fromtimestamp(ns / 1e9)``, so
every rendered instant inherited binary floating-point error from a float whose
resolution shrinks as the epoch value grows.

* Present-day stamps drift by up to ~1 us, so a boundary stamp could render
  outside the window it belongs to.
* At epoch second 1e11 the drift is ~10 us: 100_000_000_012_345_678_900
  renders as ``.345670`` instead of ``.345679``.
* ``253_402_300_799_999_999_000`` -- the largest value ``parse_time_value``
  accepts, and the value the parse test pins -- converts to the float
  ``253402300800.0``, i.e. the next year, and raises instead of rendering
  ``9999-12-31T23:59:59.999999+00:00``.

``run.io.run_metadata`` keeps a second copy of the same conversion, so both
entry points must be exact. Half-up sub-microsecond rounding, carry into the
next second, and the epoch itself must keep behaving as before.
"""

import pytest

from swe_runner.run.io.run_metadata import ns_to_iso as run_metadata_ns_to_iso
from swe_runner.trace_extraction.helpers import ns_to_iso

MAX_NS = 253_402_300_799_999_999_000
FAR_FUTURE_NS = 100_000_000_012_345_678_900

CONVERTERS = (
    pytest.param(ns_to_iso, id="trace_extraction"),
    pytest.param(run_metadata_ns_to_iso, id="run_metadata"),
)


@pytest.mark.parametrize("convert", CONVERTERS)
class TestExactNanosecondRendering:
    """Red on main: the float conversion drifts or raises."""

    def test_largest_supported_timestamp_renders(self, convert):
        assert convert(MAX_NS) == "9999-12-31T23:59:59.999999+00:00"

    def test_far_future_fraction_keeps_its_microseconds(self, convert):
        assert convert(FAR_FUTURE_NS) == "5138-11-16T09:46:52.345679+00:00"

    def test_far_future_whole_second_is_unchanged(self, convert):
        assert convert(100_000_000_000_000_000_000) == "5138-11-16T09:46:40+00:00"

    def test_half_microsecond_rounds_up(self, convert):
        assert convert(1_700_000_000_123_456_500) == "2023-11-14T22:13:20.123457+00:00"

    def test_sub_half_microsecond_rounds_down(self, convert):
        assert convert(1_700_000_000_123_456_499) == "2023-11-14T22:13:20.123456+00:00"

    def test_nanoseconds_are_dropped_below_half_a_microsecond(self, convert):
        assert convert(1_700_000_000_123_456_789) == "2023-11-14T22:13:20.123457+00:00"

    def test_rounded_up_microsecond_carries_into_next_second(self, convert):
        assert convert(1_700_000_000_999_999_999) == "2023-11-14T22:13:21+00:00"

    def test_epoch_renders_without_a_fraction(self, convert):
        assert convert(0) == "1970-01-01T00:00:00+00:00"
