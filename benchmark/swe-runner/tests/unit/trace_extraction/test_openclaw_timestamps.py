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

"""Malformed entry timestamps must not discard usable OpenClaw transcripts."""

import json
from pathlib import Path

import pytest

from swe_runner.trace_extraction.helpers import ExtractionError, parse_time_value
from swe_runner.trace_extraction.openclaw_jsonl import (
    _timestamp_ns,
    iter_openclaw_jsonl_traces,
    reconstruct_openclaw_jsonl_session,
)


@pytest.mark.parametrize("field", ["timestamp", "created_at", "createdAt", "time"])
def test_invalid_timestamp_fields_warn_and_return_unknown(field: str, caplog: pytest.LogCaptureFixture) -> None:
    assert _timestamp_ns({field: "not a timestamp"}) is None
    assert field in caplog.text
    assert "not a timestamp" in caplog.text


def test_invalid_primary_timestamp_uses_valid_fallback() -> None:
    assert _timestamp_ns({"timestamp": "invalid", "created_at": "2026-04-24T00:00:01Z"}) == parse_time_value(
        "2026-04-24T00:00:01Z"
    )


def _write_session(path: Path, *, malformed: bool) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    entries = [
        {"type": "session", "id": path.stem, "timestamp": "invalid" if malformed else "2026-04-24T00:00:00Z"},
        {"role": "user", "content": "Issue ID: django__django-1234", "timestamp": "2026-04-24T00:00:01Z"},
        {
            "role": "assistant",
            "timestamp": "invalid" if malformed else "2026-04-24T00:00:02Z",
            "content": "I will inspect the repository.",
            "usage": {"input": 25, "output": 5},
        },
        {
            "role": "assistant",
            "timestamp": "2026-04-24T00:00:03Z",
            "content": "The change is complete.",
            "usage": {"input": 30, "output": 8},
        },
    ]
    path.write_text("\n".join(json.dumps(entry) for entry in entries) + "\n", encoding="utf-8")


def test_reconstruction_preserves_messages_and_usage(tmp_path: Path) -> None:
    path = tmp_path / "session.jsonl"
    _write_session(path, malformed=True)

    trace = reconstruct_openclaw_jsonl_session(path)

    assert trace is not None
    assert trace["total_input_tokens"] == 55
    assert trace["total_output_tokens"] == 13
    assert trace["first_event_at"] == "2026-04-24T00:00:01+00:00"
    assert trace["last_event_at"] == "2026-04-24T00:00:03+00:00"
    assert trace["steps"][0]["timestamp_ns"] is None
    assert "timestamp" not in trace["steps"][0]
    assert trace["steps"][0]["assistant_output"] == [{"type": "text", "content": "I will inspect the repository."}]


def test_window_collection_preserves_other_sessions(tmp_path: Path) -> None:
    sessions = tmp_path / "profile" / "agents" / "agent" / "sessions"
    _write_session(sessions / "a-malformed.jsonl", malformed=True)
    _write_session(sessions / "b-valid.jsonl", malformed=False)

    traces = iter_openclaw_jsonl_traces(
        profiles_root=tmp_path,
        start_ns=parse_time_value("2026-04-24T00:00:00Z"),
        end_ns=parse_time_value("2026-04-24T00:00:10Z"),
    )

    assert {trace["session_id"] for trace in traces} == {"a-malformed", "b-valid"}
    assert all(trace["total_input_tokens"] == 55 for trace in traces)


def test_explicit_timestamp_validation_remains_strict() -> None:
    with pytest.raises(ExtractionError, match="Invalid timestamp"):
        parse_time_value("invalid")
