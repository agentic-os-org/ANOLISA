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

import csv
import importlib.util
import json
import subprocess
import sys
import tracemalloc
from pathlib import Path
from typing import Any

import pytest

SCRIPT = (
    Path(__file__).resolve().parents[1] / "scripts" / "compare_session_trace_tokens.py"
)
SPEC = importlib.util.spec_from_file_location("stream_token_comparison", SCRIPT)
MODULE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(MODULE)


def _record(reader: str, text: str = "response") -> dict[str, Any]:
    message = {"role": "assistant", "content": [{"type": "text", "text": text}]}
    if reader == "session_totals":
        message["usage"] = {
            "input": 3,
            "output": 2,
            "totalTokens": 5,
            "cacheRead": 1,
            "cacheWrite": 2,
        }
        return {"message": message}
    return {
        "type": "message",
        "message": message,
        "usage": {"input_tokens": 3, "output_tokens": 2},
    }


@pytest.mark.parametrize("reader", ["session_totals", "trace_totals"])
@pytest.mark.parametrize("newline", ["\n", "\r\n"])
def test_counter_golden_values_and_tolerant_lines(
    tmp_path: Path, reader: str, newline: str
) -> None:
    path = tmp_path / "log.jsonl"
    lines = [
        "",
        "{broken",
        "null",
        "[]",
        '"non-record"',
        "42",
        "false",
        json.dumps({"message": {"role": "user"}}),
        json.dumps(_record(reader)),
        json.dumps(_record(reader)),
    ]
    if reader == "trace_totals":
        lines.append(
            json.dumps(
                {
                    "type": "trace_end",
                    "model_input_tokens": 6,
                    "model_output_tokens": 4,
                    "total_tokens": 10,
                }
            )
        )
    path.write_bytes(newline.join(lines).encode("utf-8"))
    result = getattr(MODULE, reader)(path)
    expected = {"assistants": 2, "has_usage_evidence": True, "input": 6, "output": 4}
    if reader == "session_totals":
        expected.update(total=10, cache_read=2, cache_write=4, last_total=5)
    else:
        expected.update(end_input=6, end_output=4, end_total=10)
    assert result == expected


@pytest.mark.parametrize("reader", ["session_totals", "trace_totals"])
def test_large_logs_use_less_memory_than_the_file(tmp_path: Path, reader: str) -> None:
    path = tmp_path / "large.jsonl"
    line = json.dumps(_record(reader, "x" * 8192)) + "\n"
    records = 1024
    with path.open("w", encoding="utf-8") as destination:
        for _ in range(records):
            destination.write(line)
    tracemalloc.start()
    try:
        result = getattr(MODULE, reader)(path)
        _, peak = tracemalloc.get_traced_memory()
    finally:
        tracemalloc.stop()
    assert result["assistants"] == records
    assert (result["input"], result["output"]) == (3 * records, 2 * records)
    assert peak < path.stat().st_size // 2, f"peak={peak}, file={path.stat().st_size}"


@pytest.mark.parametrize("reader", ["session_totals", "trace_totals"])
def test_unicode_separators_inside_content_keep_one_record(
    tmp_path: Path, reader: str
) -> None:
    path = tmp_path / "unicode.jsonl"
    path.write_text(
        json.dumps(
            _record(reader, "first\u2028second\u2029third\u0085fourth"),
            ensure_ascii=False,
        )
        + "\n",
        encoding="utf-8",
    )
    assert getattr(MODULE, reader)(path)["assistants"] == 1


def test_offline_comparison_csv_contract(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch, capsys: pytest.CaptureFixture[str]
) -> None:
    run = tmp_path / "run-one"
    sessions = run / "sessions"
    sessions.mkdir(parents=True)
    session = {
        "message": {
            "role": "assistant",
            "usage": {"input": 3, "output": 2, "totalTokens": 5},
        }
    }
    (sessions / "case-1.session.jsonl").write_text(
        json.dumps(session) + "\n", encoding="utf-8"
    )
    (run / "case-1.jsonl").write_text(
        json.dumps(_record("trace_totals"))
        + "\n"
        + json.dumps(
            {
                "type": "trace_end",
                "model_input_tokens": 3,
                "model_output_tokens": 2,
                "total_tokens": 5,
            }
        ),
        encoding="utf-8",
    )
    output = tmp_path / "comparison.csv"
    monkeypatch.setattr(
        sys,
        "argv",
        [
            "compare_session_trace_tokens.py",
            "--root",
            str(tmp_path),
            "--csv",
            str(output),
        ],
    )
    MODULE.main()
    assert "matched: 1" in capsys.readouterr().out
    with output.open(encoding="utf-8", newline="") as source:
        rows = list(csv.DictReader(source))
    assert len(rows) == 1
    assert rows[0]["status"] == "ok"
    assert rows[0]["session_input"] == rows[0]["trace_input"] == "3"


@pytest.mark.parametrize(
    ("case", "status"),
    [
        ("non-objects", "insufficient_evidence"),
        ("session-missing-usage", "insufficient_evidence"),
        ("trace-missing-usage", "insufficient_evidence"),
        ("partial-usage", "insufficient_evidence"),
        ("missing-count", "insufficient_evidence"),
        ("trace-end-only", "insufficient_evidence"),
        ("zero", "ok"),
        ("numeric-string-zero", "ok"),
        ("mismatch", "input"),
    ],
)
def test_cli_compares_only_measured_usage(tmp_path: Path, case: str, status: str) -> None:
    run = tmp_path / "run-one"
    sessions = run / "sessions"
    sessions.mkdir(parents=True)
    session = {"message": {"role": "assistant", "usage": {"input": 0, "output": 0}}}
    trace = {
        "type": "message",
        "message": {"role": "assistant"},
        "usage": {"input_tokens": 0, "output_tokens": 0},
    }
    trace_end = {"type": "trace_end", "model_input_tokens": 0, "model_output_tokens": 0}
    session_rows: list[Any] = [session]
    trace_rows: list[Any] = [trace, trace_end]
    if case == "non-objects":
        session_rows = trace_rows = [None, [], "non-record", 42, False]
    elif case == "session-missing-usage":
        del session["message"]["usage"]
    elif case == "trace-missing-usage":
        del trace["usage"]
    elif case == "partial-usage":
        session_rows.append({"message": {"role": "assistant"}})
        trace_rows.insert(1, trace)
    elif case == "missing-count":
        del trace["usage"]["output_tokens"]
    elif case == "trace-end-only":
        trace_rows = [trace_end]
    elif case == "numeric-string-zero":
        session["message"]["usage"] = {
            "input": "0",
            "output": "0",
            "totalTokens": "0",
            "cacheRead": "0",
            "cacheWrite": "0",
        }
        trace_end.update(
            model_input_tokens="0", model_output_tokens="0", total_tokens="0"
        )
    elif case == "mismatch":
        session["message"]["usage"]["input"] = 1

    (sessions / "trial.session.jsonl").write_text(
        "".join(json.dumps(row) + "\n" for row in session_rows), encoding="utf-8"
    )
    (run / "trial.jsonl").write_text(
        "".join(json.dumps(row) + "\n" for row in trace_rows), encoding="utf-8"
    )
    output = tmp_path / "audit.csv"
    result = subprocess.run(
        [sys.executable, str(SCRIPT), "--root", str(tmp_path), "--detail", "--csv", str(output)],
        capture_output=True,
        text=True,
        encoding="utf-8",
        timeout=30,
        check=False,
    )
    assert result.returncode == 0, result.stderr
    assert f"[{status}] run-one/trial" in result.stdout, result.stdout
    assert f"  matched: {int(status == 'ok')}\n" in result.stdout
    insufficient = int(status == "insufficient_evidence")
    assert f"  insufficient_evidence: {insufficient}\n" in result.stdout
    with output.open(encoding="utf-8", newline="") as source:
        row, = csv.DictReader(source)
    assert row["status"] == status
    if status == "ok":
        fields = ("session_input", "session_output", "trace_input", "trace_output")
        assert [row[field] for field in fields] == ["0"] * 4


OPTIONAL_COUNTERS = [
    ("session_totals", "totalTokens", "total"),
    ("session_totals", "cacheRead", "cache_read"),
    ("session_totals", "cacheWrite", "cache_write"),
    ("trace_totals", "model_input_tokens", "end_input"),
    ("trace_totals", "model_output_tokens", "end_output"),
    ("trace_totals", "total_tokens", "end_total"),
]


@pytest.mark.parametrize(("reader", "field", "result_field"), OPTIONAL_COUNTERS)
@pytest.mark.parametrize("value", [0, "0", 7, "7"])
def test_optional_counts_preserve_integer_values(
    tmp_path: Path, reader: str, field: str, result_field: str, value: Any
) -> None:
    record = _record(reader)
    records = [record]
    if reader == "session_totals":
        record["message"]["usage"][field] = value
    else:
        records.append({"type": "trace_end", field: value})
    path = tmp_path / "counts.jsonl"
    path.write_text(
        "".join(json.dumps(row) + "\n" for row in records), encoding="utf-8"
    )
    result = getattr(MODULE, reader)(path)
    assert result["has_usage_evidence"] is True
    assert result[result_field] == int(value)


@pytest.mark.parametrize(("reader", "field", "_result_field"), OPTIONAL_COUNTERS)
@pytest.mark.parametrize(
    "value",
    [None, True, -1, 1.5, "unavailable", float("inf"), float("nan")],
)
def test_cli_keeps_healthy_trials_after_invalid_optional_count(
    tmp_path: Path, reader: str, field: str, _result_field: str, value: Any
) -> None:
    run = tmp_path / "run-one"
    sessions = run / "sessions"
    sessions.mkdir(parents=True)
    for stem in ("a-invalid", "b-healthy"):
        session = _record("session_totals")
        session["message"]["usage"].update(cacheRead=0, cacheWrite=0)
        trace = _record("trace_totals")
        trace_end = {
            "type": "trace_end",
            "model_input_tokens": 3,
            "model_output_tokens": 2,
            "total_tokens": 5,
        }
        if stem == "a-invalid":
            counters = (
                session["message"]["usage"] if reader == "session_totals" else trace_end
            )
            counters[field] = value
        (sessions / f"{stem}.session.jsonl").write_text(
            json.dumps(session) + "\n", encoding="utf-8"
        )
        (run / f"{stem}.jsonl").write_text(
            json.dumps(trace) + "\n" + json.dumps(trace_end) + "\n", encoding="utf-8"
        )

    output = tmp_path / "audit.csv"
    result = subprocess.run(
        [
            sys.executable,
            str(SCRIPT),
            "--root",
            str(tmp_path),
            "--detail",
            "--csv",
            str(output),
        ],
        capture_output=True,
        text=True,
        encoding="utf-8",
        timeout=30,
        check=False,
    )
    assert result.returncode == 0, result.stderr
    assert "  total_pairs: 2\n" in result.stdout
    assert "  matched: 1\n" in result.stdout
    assert "  insufficient_evidence: 1\n" in result.stdout
    assert "[ok] run-one/b-healthy" in result.stdout
    with output.open(encoding="utf-8", newline="") as source:
        rows = list(csv.DictReader(source))
    assert [(row["trial"], row["status"]) for row in rows] == [
        ("a-invalid", "insufficient_evidence"),
        ("b-healthy", "ok"),
    ]
