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

"""Unit tests for TraceCollectionPlan resolve and collect logic."""

import json
from pathlib import Path
from unittest.mock import patch

import pytest

from swe_runner.trace_extraction.helpers import ExtractionError
from swe_runner.trace_extraction.plan import TraceCollectionPlan


class TestResolveSkip:
    """Cases where resolve returns should_collect=False."""

    def test_no_inputs_returns_skip(self):
        plan = TraceCollectionPlan.resolve()
        assert plan.should_collect is False

    def test_no_start_no_metadata_returns_skip(self):
        plan = TraceCollectionPlan.resolve(start=None, end="now", run_metadata_path=None)
        assert plan.should_collect is False

    def test_skip_plan_collect_returns_none(self):
        plan = TraceCollectionPlan.resolve()
        result = plan.collect(Path("/tmp/traces"))
        assert result is None


class TestResolveWithStart:
    """Cases where --start is provided directly."""

    def test_start_epoch_seconds(self):
        plan = TraceCollectionPlan.resolve(start="1000", end="2000")
        assert plan.should_collect is True
        assert plan.start_ns == 1000 * 1_000_000_000
        assert plan.end_ns == 2000 * 1_000_000_000

    def test_start_with_end_now(self):
        plan = TraceCollectionPlan.resolve(start="1000")
        assert plan.should_collect is True
        assert plan.start_ns == 1000 * 1_000_000_000
        assert plan.end_ns > 0


class TestResolveWithMetadata:
    """Cases where --run-metadata is provided."""

    def test_metadata_provides_start_and_end(self, tmp_path):
        metadata_path = tmp_path / "run_metadata.json"
        metadata_path.write_text(json.dumps({
            "started_at_ns": 100,
            "ended_at_ns": 200,
            "instance_ids": ["inst-1"],
            "session_ids": {"inst-1": "sess-1"},
        }))

        plan = TraceCollectionPlan.resolve(run_metadata_path=metadata_path)

        assert plan.should_collect is True
        assert plan.source_name == "openclaw_jsonl"
        assert plan.start_ns == 100
        assert plan.end_ns == 200 + 10_000_000_000
        assert plan.instance_ids == {"inst-1"}
        assert plan.session_ids == {"sess-1"}

    def test_metadata_with_profile_dirs(self, tmp_path):
        metadata_path = tmp_path / "run_metadata.json"
        metadata_path.write_text(json.dumps({
            "started_at_ns": 100,
            "ended_at_ns": 200,
            "instance_ids": ["inst-1"],
            "session_ids": {"inst-1": "sess-1"},
            "openclaw_profile_dirs": {"inst-1": "/tmp/profiles/inst-1"},
        }))

        plan = TraceCollectionPlan.resolve(run_metadata_path=metadata_path)
        assert plan.profile_dirs == [Path("/tmp/profiles/inst-1")]

    def test_metadata_profiles_root_defaults_to_parent(self, tmp_path):
        metadata_path = tmp_path / "run_metadata.json"
        metadata_path.write_text(json.dumps({
            "started_at_ns": 100,
            "ended_at_ns": 200,
            "instance_ids": ["inst-1"],
            "session_ids": {"inst-1": "sess-1"},
        }))

        plan = TraceCollectionPlan.resolve(run_metadata_path=metadata_path)
        assert plan.profiles_root == tmp_path / "openclaw-profiles"

    def test_explicit_profiles_dir_overrides_default(self, tmp_path):
        metadata_path = tmp_path / "run_metadata.json"
        profiles_dir = tmp_path / "custom-profiles"
        metadata_path.write_text(json.dumps({
            "started_at_ns": 100,
            "ended_at_ns": 200,
            "instance_ids": ["inst-1"],
            "session_ids": {"inst-1": "sess-1"},
        }))

        plan = TraceCollectionPlan.resolve(
            run_metadata_path=metadata_path,
            openclaw_profiles_dir=profiles_dir,
        )
        assert plan.profiles_root == profiles_dir


class TestResolveErrors:
    """Error cases in resolve."""

    def test_end_without_start_or_metadata_raises(self):
        with pytest.raises(ExtractionError, match="--end requires --start or --run-metadata"):
            TraceCollectionPlan.resolve(end="1000")

    def test_end_less_than_start_raises(self):
        with pytest.raises(ExtractionError, match="end must be greater than or equal to start"):
            TraceCollectionPlan.resolve(start="2000", end="1000")

    def test_missing_session_ids_in_metadata_raises(self, tmp_path):
        metadata_path = tmp_path / "run_metadata.json"
        metadata_path.write_text(json.dumps({
            "started_at_ns": 100,
            "ended_at_ns": 200,
            "instance_ids": ["inst-1"],
        }))

        with pytest.raises(ExtractionError, match="requires session_ids"):
            TraceCollectionPlan.resolve(run_metadata_path=metadata_path)

    def test_invalid_metadata_json_raises(self, tmp_path):
        metadata_path = tmp_path / "run_metadata.json"
        metadata_path.write_text("not json")

        with pytest.raises(ExtractionError, match="Failed to load run metadata"):
            TraceCollectionPlan.resolve(run_metadata_path=metadata_path)

    def test_metadata_not_a_dict_raises(self, tmp_path):
        metadata_path = tmp_path / "run_metadata.json"
        metadata_path.write_text(json.dumps([1, 2, 3]))

        with pytest.raises(ExtractionError, match="must be a JSON object"):
            TraceCollectionPlan.resolve(run_metadata_path=metadata_path)


class TestCollect:
    """Test the collect() method delegates correctly."""

    def test_collect_calls_record_function(self, tmp_path):
        metadata_path = tmp_path / "run_metadata.json"
        metadata_path.write_text(json.dumps({
            "started_at_ns": 100,
            "ended_at_ns": 200,
            "instance_ids": ["inst-1"],
            "session_ids": {"inst-1": "sess-1"},
        }))
        plan = TraceCollectionPlan.resolve(run_metadata_path=metadata_path)

        trace_root = tmp_path / "traces"
        with patch(
            "swe_runner.trace_extraction.openclaw_source.record_openclaw_jsonl_traces_in_window",
            return_value=[trace_root / "inst-1" / "trace1.json"],
        ) as mock_record:
            result = plan.collect(trace_root)

        assert result == [trace_root / "inst-1" / "trace1.json"]
        mock_record.assert_called_once_with(
            start_ns=100,
            end_ns=200 + 10_000_000_000,
            profiles_root=(tmp_path / "openclaw-profiles").expanduser(),
            profile_dirs=None,
            trace_root=trace_root,
            instance_ids={"inst-1"},
            session_ids={"sess-1"},
        )
