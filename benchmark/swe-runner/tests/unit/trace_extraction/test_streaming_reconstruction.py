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

"""Characterize whole-session output while bounding ignored-record memory."""

from __future__ import annotations

import hashlib
import json
import tracemalloc
from pathlib import Path

import pytest

from swe_runner.trace_extraction.helpers import ExtractionError
from swe_runner.trace_extraction.openclaw_jsonl import reconstruct_openclaw_jsonl_session


def events(header_position: str = "late") -> list[dict]:
    rows = [
        {"type": "message", "timestamp_ns": 10, "message": {"role": "user", "content": "Fix issue: owner__repo-17"}},
        {
            "type": "message",
            "id": "first",
            "timestamp_ns": 20,
            "message": {
                "role": "assistant",
                "content": [
                    {"type": "text", "text": "answer"},
                    {"type": "toolCall", "id": "call", "name": "read", "arguments": {"path": "fixture"}},
                ],
                "model": "fixture-model",
                "provider": "fixture-provider",
                "usage": {
                    "input": 12,
                    "output": 3,
                    "cacheRead": 2,
                    "cacheWrite": 1,
                    "reasoningTokens": 1,
                    "totalTokens": 16,
                    "cost": 0.25,
                },
            },
        },
        {
            "type": "message",
            "timestamp_ns": 30,
            "message": {
                "role": "toolResult",
                "toolCallId": "call",
                "toolName": "read",
                "isError": False,
                "content": [],
            },
        },
        {
            "type": "message",
            "id": "second",
            "timestamp_ns": 40,
            "message": {"role": "assistant", "content": "done", "usage": {"input": 4, "output": 1}},
        },
    ]
    header = {"type": "session", "id": "session-fixture"}
    if header_position == "first":
        rows.insert(0, header)
    elif header_position == "late":
        rows.extend([{"type": "session", "id": ""}, header, {"type": "session", "id": "ignored-second-id"}])
    return rows


def write_events(path: Path, rows: list[dict], ending: str = "\n") -> None:
    path.write_bytes((ending.join(json.dumps(row, ensure_ascii=False) for row in rows) + ending).encode("utf-8"))


@pytest.mark.parametrize("position", ["first", "late", "absent"])
@pytest.mark.parametrize("ending", ["\n", "\r\n", "\r"])
def test_canonical_trace_matches_baseline_golden(tmp_path, position, ending):
    path = tmp_path / "fallback-session.jsonl"
    write_events(path, events(position), ending)
    trace = reconstruct_openclaw_jsonl_session(path)
    assert trace is not None
    trace["session_file"] = "<fixture>"
    canonical = json.dumps(trace, sort_keys=True, ensure_ascii=False, separators=(",", ":"))
    expected = {
        "first": "0113f86fd1ab999fffb11218c37ff86d2ce191abbac758a2ae6f8bf5108be8ea",
        "late": "0113f86fd1ab999fffb11218c37ff86d2ce191abbac758a2ae6f8bf5108be8ea",
        "absent": "bf9a02f3348361aee819826f63019356cffe4c5c8f73c61b99897470e4aee2ab",
    }
    assert hashlib.sha256(canonical.encode("utf-8")).hexdigest() == expected[position]


def test_reconstruction_does_not_bulk_read_transcript(tmp_path, monkeypatch):
    path = tmp_path / "fixture.jsonl"
    write_events(path, events())
    original = Path.read_text

    def bounded_read(candidate, *args, **kwargs):
        if candidate == path:
            raise AssertionError("Reconstruction must not allocate the whole raw transcript")
        return original(candidate, *args, **kwargs)

    monkeypatch.setattr(Path, "read_text", bounded_read)
    result = reconstruct_openclaw_jsonl_session(path)
    assert result is not None and result["total_steps"] == 2


def test_ignored_record_memory_is_bounded_by_one_record(tmp_path):
    path = tmp_path / "fixture.jsonl"
    write_events(path, events())
    expected = reconstruct_openclaw_jsonl_session(path)
    assert expected is not None
    ignored = json.dumps({"type": "progress", "unused": "x" * 32768}) + "\n"
    with path.open("w", encoding="utf-8", newline="") as handle:
        handle.write(ignored * 128)
        handle.writelines(json.dumps(row) + "\n" for row in events())
    tracemalloc.start()
    try:
        actual = reconstruct_openclaw_jsonl_session(path)
        _, peak = tracemalloc.get_traced_memory()
    finally:
        tracemalloc.stop()
    assert actual == expected
    assert peak < 2_000_000, f"Ignored 4 MiB transcript retained {peak} bytes"


def test_malformed_lines_keep_legacy_line_numbers(tmp_path, caplog):
    path = tmp_path / "fixture.jsonl"
    # splitlines has historically treated U+2028 as a line boundary as well.
    path.write_text(
        "\n[]\n{broken\u2028{broken\n" + "\n".join(json.dumps(row) for row in events()) + "\n", encoding="utf-8"
    )
    trace = reconstruct_openclaw_jsonl_session(path)
    assert trace is not None and trace["total_steps"] == 2
    assert "line=3" in caplog.text
    assert "line=4" in caplog.text


def test_no_usage_records_still_produce_no_trace(tmp_path):
    path = tmp_path / "empty.jsonl"
    for content in ("", "\n[]\nnull\n", json.dumps({"type": "session", "id": "session"})):
        path.write_text(content, encoding="utf-8")
        assert reconstruct_openclaw_jsonl_session(path) is None


def test_source_handle_closes_on_processing_error(tmp_path, monkeypatch):
    path = tmp_path / "fixture.jsonl"
    write_events(path, [{"timestamp": "not-a-timestamp", "usage": {"input": 1}}])
    handle = path.open("r", encoding="utf-8")
    original = Path.open

    def tracked_open(candidate, *args, **kwargs):
        return handle if candidate == path else original(candidate, *args, **kwargs)

    monkeypatch.setattr(Path, "open", tracked_open)
    with pytest.raises(ExtractionError):
        reconstruct_openclaw_jsonl_session(path)
    assert handle.closed
