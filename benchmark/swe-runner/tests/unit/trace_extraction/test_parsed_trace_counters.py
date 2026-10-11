"""Retain counter export semantics without reparsing serialized row counters."""

import json
from pathlib import Path

import pytest

from swe_runner.trace_extraction import analysis


@pytest.mark.parametrize(
    "value,expected",
    [
        ({"read": 2, "edit": 3}, {"edit": 3, "read": 2}),
        ({"read": True, "edit": False}, {"edit": 0, "read": 1}),
        ({"read": -2, "edit": 0}, {"edit": 0, "read": -2}),
        ({"read": 1.5, "edit": "3"}, {}),
        ({"read": None, "edit": {}}, {}),
        ({"工具": 4}, {"工具": 4}),
        ([], {}),
        (None, {}),
        ("invalid", {}),
    ],
)
def test_counter_rows_and_summary_retain_existing_values(tmp_path: Path, value, expected):
    root = tmp_path / "traces"
    directory = root / "instance"
    directory.mkdir(parents=True)
    for index in range(2):
        payload = {"session_id": str(index), "tool_call_counts": value, "tool_result_counts": value}
        (directory / f"trace{index}.json").write_text(json.dumps(payload, ensure_ascii=False), encoding="utf-8")
    rows, summaries = analysis.analyze_trace_files(root, include_metrics=True)
    serialized = (
        json.dumps(value, sort_keys=True, ensure_ascii=False, separators=(",", ":"))
        if isinstance(value, dict)
        else "{}"
    )
    assert [row["tool_call_counts"] for row in rows] == [serialized, serialized]
    assert [row["tool_result_counts"] for row in rows] == [serialized, serialized]
    for field in ("tool_call_counts", "tool_result_counts"):
        assert json.loads(summaries[0][field]) == {name: count * 2 for name, count in expected.items()}


def test_each_trace_json_is_decoded_once_for_counter_summary(tmp_path: Path, monkeypatch):
    root = tmp_path / "traces"
    for instance in ("i1", "i2"):
        directory = root / instance
        directory.mkdir(parents=True)
        (directory / "trace.json").write_text(
            '{"tool_call_counts":{"read":2},"tool_result_counts":{"read":1}}', encoding="utf-8"
        )
    real_loads = json.loads
    decoded = []

    def loads(payload, *args, **kwargs):
        decoded.append(payload)
        return real_loads(payload, *args, **kwargs)

    monkeypatch.setattr(analysis.json, "loads", loads)
    _, summaries = analysis.analyze_trace_files(root, include_metrics=True)
    assert len(summaries) == 2
    assert len(decoded) == 2


def test_instances_and_counter_fields_remain_independent(tmp_path: Path):
    root = tmp_path / "traces"
    for instance, calls, results in [("i1", 2, 1), ("i2", 7, 3)]:
        directory = root / instance
        directory.mkdir(parents=True)
        (directory / "trace.json").write_text(
            json.dumps({"tool_call_counts": {"read": calls}, "tool_result_counts": {"read": results}}), encoding="utf-8"
        )
    _, summaries = analysis.analyze_trace_files(root, include_metrics=True)
    assert [
        (row["instance_id"], json.loads(row["tool_call_counts"]), json.loads(row["tool_result_counts"]))
        for row in summaries
    ] == [("i1", {"read": 2}, {"read": 1}), ("i2", {"read": 7}, {"read": 3})]


def test_metrics_disabled_keep_legacy_summary_shape(tmp_path: Path):
    directory = tmp_path / "i1"
    directory.mkdir()
    (directory / "trace.json").write_text('{"tool_call_counts":{"read":2}}', encoding="utf-8")
    rows, summaries = analysis.analyze_trace_files(tmp_path)
    assert "tool_call_counts" not in rows[0]
    assert "tool_call_counts" not in summaries[0]
