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
    expected = {"assistants": 2, "input": 6, "output": 4}
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
