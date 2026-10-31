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

"""Run window merge tests for ``merge_run_metadata``.

``write_run_metadata`` merges the current snapshot into the payload already in
``run_metadata.json``. The merge widened the window with a plain ``min`` /
``max`` over ``_safe_int(...)``, and ``_safe_int`` maps an absent timestamp to
0, so a payload missing ``started_at_ns`` or ``ended_at_ns`` pulled the window
to the Unix epoch. Trace extraction then resolves its window from this file, so
one legacy or hand-edited payload widened it to every trace file on disk.
"""

from swe_runner.run.io.run_metadata import merge_run_metadata

STARTED = 1_700_000_000_000_000_000
ENDED = 1_700_000_100_000_000_000
CURRENT = {
    "started_at_ns": STARTED,
    "ended_at_ns": ENDED,
    "agent_name": "cosh",
    "instance_ids": ["django__django-1"],
    "succeeded": 1,
    "run_count": 1,
}


class TestMergedWindow:
    """Red on main: an absent timestamp becomes the epoch."""

    def test_missing_started_at_keeps_the_known_start(self):
        merged = merge_run_metadata({"instance_ids": ["django__django-2"], "run_count": 1}, CURRENT)

        assert merged["started_at_ns"] == STARTED
        assert merged["started_at"] == "2023-11-14T22:13:20+00:00"

    def test_missing_ended_at_keeps_the_known_end(self):
        merged = merge_run_metadata({"started_at_ns": STARTED, "run_count": 1}, CURRENT)

        assert merged["ended_at_ns"] == ENDED
        assert merged["ended_at"] == "2023-11-14T22:15:00+00:00"

    def test_earlier_start_and_later_end_still_win(self):
        earlier = STARTED - 60_000_000_000
        later = ENDED + 60_000_000_000

        merged = merge_run_metadata({"started_at_ns": earlier, "ended_at_ns": later}, CURRENT)

        assert merged["started_at_ns"] == earlier
        assert merged["ended_at_ns"] == later

    def test_current_values_are_used_when_the_old_payload_has_none(self):
        merged = merge_run_metadata({}, CURRENT)

        assert merged["started_at_ns"] == STARTED
        assert merged["ended_at_ns"] == ENDED

    def test_run_counters_still_accumulate(self):
        merged = merge_run_metadata(
            {"instance_ids": ["django__django-2"], "succeeded": 1, "failed": 0, "run_count": 1},
            CURRENT,
        )

        assert merged["run_count"] == 2
        assert merged["succeeded"] == 2
        assert merged["instance_count"] == 2
