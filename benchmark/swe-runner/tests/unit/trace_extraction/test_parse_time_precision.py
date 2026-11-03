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

"""Exact integer ISO-to-epoch nanosecond tests for parse_time_value.

Regression tests for issue #5951: parse_time_value converted accepted ISO
timestamps through ``int(dt.timestamp() * 1e9)``, so binary floating-point
error leaked into the integer nanosecond result (e.g.
``2026-05-01T12:34:56.012345+00:00`` became 1,777,638,896,012,345,088
instead of ...500). Down-rounding endpoints silently excluded sessions
whose integer timestamp sat precisely on the intended window boundary,
and the maximum supported datetime rounded into the next year. Accepted
format range, naive-as-UTC policy, fixed offsets, numeric epoch units and
invalid-ISO errors must all keep behaving exactly as before.
"""

import pytest

from swe_runner.trace_extraction.helpers import ExtractionError, parse_time_value
from swe_runner.trace_extraction.plan import TraceCollectionPlan

# Exact calendar references: (iso, exact nanoseconds since the Unix epoch).
UP_ROUNDING = ("2026-05-01T12:34:56.012345+00:00", 1_777_638_896_012_345_000)
DOWN_ROUNDING = ("2026-06-17T08:54:32.125+00:00", 1_781_686_472_125_000_000)
Z_SUFFIX = ("2026-11-23T19:44:21.921731Z", 1_795_463_061_921_731_000)
OFFSET_SHIFTED = ("2026-05-01T20:34:56.012345+08:00", 1_777_638_896_012_345_000)
MAX_DATETIME = ("9999-12-31T23:59:59.999999+00:00", 253_402_300_799_999_999_000)

# A session whose first step lands exactly on the intended end boundary.
BOUNDARY_SESSION_NS = 1_781_686_472_125_000_000


class TestExactIsoConversions:
    """Red on main: float conversion drifts from the exact integer value."""

    def test_up_rounding_utc_microsecond_is_exact(self):
        iso, expected = UP_ROUNDING
        assert parse_time_value(iso) == expected

    def test_down_rounding_endpoint_is_exact(self):
        iso, expected = DOWN_ROUNDING
        assert parse_time_value(iso) == expected

    def test_z_suffix_and_naive_utc_are_exact(self):
        iso, expected = Z_SUFFIX
        assert parse_time_value(iso) == expected
        assert parse_time_value(iso[:-1]) == expected  # naive → UTC

    def test_fixed_offset_preserves_instant_exactly(self):
        iso, expected = OFFSET_SHIFTED
        assert parse_time_value(iso) == expected

    def test_max_supported_datetime_stays_in_year_9999(self):
        iso, expected = MAX_DATETIME
        assert parse_time_value(iso) == expected
        # The float path rounded this endpoint into the year-10000 boundary.
        assert parse_time_value(iso) < 253_402_300_800_000_000_000

    def test_plan_end_ns_matches_exact_boundary(self):
        plan = TraceCollectionPlan.resolve(
            start="2026-06-17T08:54:31+00:00",
            end="2026-06-17T08:54:32.125+00:00",
        )
        assert plan.should_collect is True
        assert plan.end_ns == DOWN_ROUNDING[1]

    def test_plan_window_keeps_session_on_intended_boundary(self):
        plan = TraceCollectionPlan.resolve(
            start="2026-06-17T08:54:31+00:00",
            end="2026-06-17T08:54:32.125+00:00",
        )
        # Collection windows are inclusive: start_ns <= session <= end_ns.
        inside = plan.start_ns <= BOUNDARY_SESSION_NS <= plan.end_ns
        assert inside, (
            "session exactly on the intended ISO boundary must be collected; "
            f"window was [{plan.start_ns}, {plan.end_ns}]"
        )


class TestPreservedBehavior:
    """Controls: green on main — units, policy and errors are unchanged."""

    def test_numeric_epoch_units_unchanged(self):
        assert parse_time_value("10") == 10 * 1_000_000_000
        assert parse_time_value("1700000000000") == 1_700_000_000_000 * 1_000_000
        assert parse_time_value("1700000000000000") == 1_700_000_000_000_000 * 1_000
        assert parse_time_value("1700000000000000000") == 1_700_000_000_000_000_000

    def test_now_returns_positive_int(self):
        result = parse_time_value("now")
        assert isinstance(result, int)
        assert result > 0

    def test_invalid_iso_raises_extraction_error(self):
        with pytest.raises(ExtractionError):
            parse_time_value("not-a-timestamp")
        with pytest.raises(ExtractionError):
            parse_time_value("")

    def test_whole_second_offset_iso_unchanged(self):
        assert (
            parse_time_value("2026-04-21T10:45:00+08:00")
            == 1_776_739_500_000_000_000
        )

    def test_pre_epoch_half_second_exact(self):
        assert parse_time_value("1969-12-31T23:59:59.5+00:00") == -500_000_000

    def test_date_only_midnight_utc(self):
        assert parse_time_value("2026-05-01") == 1_777_593_600_000_000_000
