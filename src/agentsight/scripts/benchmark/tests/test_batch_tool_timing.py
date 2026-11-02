"""Carry completion evidence through the batch converter's call routing."""

import json
from pathlib import Path

from test_batch_analyze import batch_analyze


def convert(tmp_path: Path, events: list[dict]) -> dict:
    source = tmp_path / "session.jsonl"
    source.write_text("".join(json.dumps(event) + "\n" for event in events), encoding="utf-8")
    doc = batch_analyze.jsonl_to_atif(source)
    artifact = batch_analyze.export_atif(doc, source, tmp_path / "export")
    return json.loads(artifact.read_text(encoding="utf-8"))


def call(identifier: str) -> dict:
    return {"type": "tool_use", "id": identifier, "name": "Bash", "input": {"command": "pwd"}}


def result(identifier: str) -> dict:
    return {"type": "tool_result", "tool_use_id": identifier, "content": "done"}


def test_parallel_out_of_order_results_keep_individual_completion_times(tmp_path: Path) -> None:
    doc = convert(
        tmp_path,
        [
            {
                "type": "assistant",
                "timestamp": "2026-01-01T00:00:01Z",
                "message": {"content": [call("a"), call("b")]},
            },
            {
                "type": "user",
                "timestamp": "2026-01-01T00:00:04Z",
                "message": {"content": [result("b")]},
            },
            {
                "type": "user",
                "timestamp": "2026-01-01T00:00:08Z",
                "message": {"content": [result("a")]},
            },
        ],
    )
    calls = {c["tool_call_id"]: c for c in doc["steps"][0]["tool_calls"]}
    assert calls["a"]["extra"]["result_timestamp"] == "2026-01-01T00:00:08Z"
    assert calls["b"]["extra"]["result_timestamp"] == "2026-01-01T00:00:04Z"
    assert doc["steps"][0]["timestamp"] == "2026-01-01T00:00:01Z"
    assert len(doc["steps"]) == 1


def test_late_echo_updates_only_the_issuing_call(tmp_path: Path) -> None:
    doc = convert(
        tmp_path,
        [
            {"type": "assistant", "message": {"content": [call("call_A")]}},
            {"type": "assistant", "message": {"content": [call("other")]}},
            {
                "type": "user",
                "timestamp": "2026-01-01T00:00:08Z",
                "message": {"content": [result("calla"), {"type": "text", "text": "next request"}]},
            },
        ],
    )
    assert doc["steps"][0]["tool_calls"][0]["extra"]["result_timestamp"] == "2026-01-01T00:00:08Z"
    assert "extra" not in doc["steps"][1]["tool_calls"][0]
    assert doc["steps"][2]["message"] == "next request"


def test_missing_result_time_does_not_invent_a_completion(tmp_path: Path) -> None:
    doc = convert(
        tmp_path,
        [
            {"type": "assistant", "message": {"content": [call("a")]}},
            {"type": "user", "message": {"content": [result("a")]}},
        ],
    )
    assert "extra" not in doc["steps"][0]["tool_calls"][0]
    assert doc["steps"][0]["observation"]["results"][0]["content"] == "done"


def test_ambiguous_echo_does_not_stamp_either_call(tmp_path: Path) -> None:
    doc = convert(
        tmp_path,
        [
            {"type": "assistant", "message": {"content": [call("call_A"), call("call-A")]}},
            {
                "type": "user",
                "timestamp": "2026-01-01T00:00:08Z",
                "message": {"content": [result("calla"), {"type": "text", "text": "question"}]},
            },
        ],
    )
    assert all("extra" not in c for c in doc["steps"][0]["tool_calls"])
