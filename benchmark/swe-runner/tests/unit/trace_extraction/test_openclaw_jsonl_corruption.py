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

"""Regression coverage for damaged bytes in otherwise valid session files."""

from __future__ import annotations

import json
import logging
from pathlib import Path

import pytest

from swe_runner.trace_extraction.recording import record_openclaw_jsonl_traces_in_window


def _session_lines(session_id: str) -> list[bytes]:
    entries = [
        {"type": "session", "id": session_id, "timestamp_ns": 1_000_000_000},
        {
            "type": "message",
            "timestamp_ns": 1_000_000_001,
            "message": {"role": "user", "content": [{"type": "text", "text": "Issue ID: synthetic__project-1"}]},
        },
        {
            "type": "message",
            "timestamp_ns": 1_000_000_002,
            "message": {
                "role": "assistant",
                "content": [{"type": "text", "text": "valid 中文 response"}],
                "usage": {"input": 100, "output": 20, "totalTokens": 120},
            },
        },
    ]
    return [(json.dumps(entry, ensure_ascii=False) + "\n").encode("utf-8") for entry in entries]


@pytest.mark.parametrize("damage", ["utf8_tail", "utf8_middle", "json_tail", "crlf", "no_final_newline"])
def test_recording_preserves_complete_sessions_around_damaged_bytes(
    tmp_path: Path,
    caplog: pytest.LogCaptureFixture,
    damage: str,
) -> None:
    profiles = tmp_path / "profiles"
    sessions = profiles / "synthetic-profile" / "agents" / "synthetic-agent" / "sessions"
    sessions.mkdir(parents=True)
    lines = _session_lines("damaged")
    if damage == "utf8_tail":
        raw = b"".join(lines) + b'{"text":"' + bytes([0xE4, 0xB8])
        bad_line = 4
    elif damage == "utf8_middle":
        raw = lines[0] + b'{"text":"' + bytes([0xFF]) + b'"}\n' + b"".join(lines[1:])
        bad_line = 2
    elif damage == "json_tail":
        raw = b"".join(lines) + b'{"text":"truncated'
        bad_line = 4
    else:
        raw = b"".join(lines)
        raw = raw.replace(b"\n", b"\r\n") if damage == "crlf" else raw.rstrip(b"\n")
        bad_line = None
    damaged_file = sessions / "a-damaged.jsonl"
    damaged_file.write_bytes(raw)
    (sessions / "z-valid.jsonl").write_bytes(b"".join(_session_lines("valid")))

    with caplog.at_level(logging.WARNING):
        recorded = record_openclaw_jsonl_traces_in_window(
            0,
            2_000_000_000,
            profiles_root=profiles,
            trace_root=tmp_path / "traces",
        )

    assert len(recorded) == 2
    payloads = [json.loads(path.read_text(encoding="utf-8")) for path in recorded]
    assert {data["session_id"] for data in payloads} == {"damaged", "valid"}
    for data in payloads:
        assert data["total_input_tokens"] == 100
        assert data["total_output_tokens"] == 20
        assert data["total_steps"] == 1
        assert data["steps"][0]["assistant_output"][0]["content"] == "valid 中文 response"
    assert damaged_file.read_bytes() == raw
    warnings = [r for r in caplog.records if "Skipping malformed OpenClaw JSONL line" in r.getMessage()]
    if bad_line is None:
        assert not warnings
    else:
        assert len(warnings) == 1
        assert f"file={damaged_file} line={bad_line}" in warnings[0].getMessage()


def test_recording_keeps_real_io_errors_fatal(tmp_path: Path) -> None:
    from swe_runner.trace_extraction.openclaw_jsonl import reconstruct_openclaw_jsonl_session

    with pytest.raises(FileNotFoundError):
        reconstruct_openclaw_jsonl_session(tmp_path / "missing.jsonl")
    with pytest.raises(IsADirectoryError):
        reconstruct_openclaw_jsonl_session(tmp_path)
