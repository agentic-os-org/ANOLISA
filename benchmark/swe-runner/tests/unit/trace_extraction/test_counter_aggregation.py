"""Counter aggregation reuses parsed trace data without changing its output."""

import json

from swe_runner.trace_extraction import analysis as trace_analysis


def test_analyze_trace_files_aggregates_parsed_counter_maps(tmp_path, monkeypatch):
    trace_root = tmp_path / "traces"
    case_dir = trace_root / "django__django-12345"
    case_dir.mkdir(parents=True)
    trace_data = [
        {
            "session_id": "sess-1",
            "total_input_tokens": 100,
            "total_output_tokens": 50,
            "total_steps": 3,
            "tool_call_counts": {
                "edit": 2,
                "shared": 3,
                "boolean": True,
                "negative": -2,
                "zero": 0,
                "fraction": 1.5,
            },
            "tool_result_counts": {"exec": 1},
        },
        {
            "session_id": "sess-2",
            "total_input_tokens": 200,
            "total_output_tokens": 80,
            "total_steps": 5,
            "tool_call_counts": {"exec": 1, "shared": -1, "negative": 1},
            "tool_result_counts": {"read": 2},
        },
    ]
    for index, data in enumerate(trace_data, start=1):
        (case_dir / f"trace{index}.json").write_text(json.dumps(data), encoding="utf-8")

    original_loads = json.loads
    parsed_documents = []

    def count_loads(value, *args, **kwargs):
        parsed_documents.append(value)
        return original_loads(value, *args, **kwargs)

    monkeypatch.setattr(trace_analysis.json, "loads", count_loads)
    per_trace_rows, per_instance_rows = trace_analysis.analyze_trace_files(trace_root, include_metrics=True)

    assert len(parsed_documents) == len(trace_data)
    assert per_trace_rows[0]["tool_call_counts"] == (
        '{"boolean":true,"edit":2,"fraction":1.5,"negative":-2,"shared":3,"zero":0}'
    )
    assert per_instance_rows[0]["tool_call_counts"] == (
        '{"boolean":1,"edit":2,"exec":1,"negative":-1,"shared":2,"zero":0}'
    )
    assert per_instance_rows[0]["tool_result_counts"] == '{"exec":1,"read":2}'
