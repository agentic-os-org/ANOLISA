"""Triage calibration regressions.

The Python ``classify`` treated any trajectory with zero agent text as
``empty``, diverging from the Rust rule in ``src/reuse/triage.rs``:
``max_agent_len == 0 && n_tool_calls == 0``. Tool-only
transcripts (the 71-step / 84-call corpus documented in the Rust comment) were
therefore counted as empty and inflated the excluded percentage.
"""

from __future__ import annotations

import importlib.util
import json
import sqlite3
import subprocess
import sys
from pathlib import Path

import pytest

SCRIPT = Path(__file__).parents[2] / "calibrate-triage.py"


def load_module():
    spec = importlib.util.spec_from_file_location("calibrate_triage", SCRIPT)
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


calibrate = load_module()


def tool_only_document() -> str:
    # 71 steps, no agent text anywhere, 84 tool calls.
    steps = [{"source": "agent", "message": ""} for _ in range(70)]
    steps.append(
        {
            "source": "agent",
            "message": "",
            "tool_calls": [{"function_name": "bash"}] * 84,
        }
    )
    return json.dumps({"steps": steps})


def test_tool_only_document_is_not_empty():
    m = calibrate.measure(tool_only_document())
    assert m == {
        "n_steps": 71,
        "n_user_turns": 0,
        "n_tool_calls": 84,
        "max_agent_len": 0,
    }
    assert calibrate.classify(m, 2000) != "empty"


def test_document_without_answers_or_tools_stays_empty():
    doc = {"steps": [{"source": "user", "message": "hi"}] * 3}
    m = calibrate.measure(json.dumps(doc))
    assert calibrate.classify(m, 2000) == "empty"


def test_single_step_document_stays_empty():
    m = {"n_steps": 1, "n_user_turns": 1, "n_tool_calls": 0, "max_agent_len": 0}
    assert calibrate.classify(m, 2000) == "empty"


def test_injected_user_steps_do_not_count_as_turns():
    # The collector records `/clear` transcripts and null messages as user
    # steps. Counting them as turns made a one-question session look like a
    # followed-up topic and bypassed the useless bucket; `TriageMetrics::measure`
    # applies the same strip before counting.
    doc = {
        "steps": [
            {"source": "user", "message": "1+1?"},
            {"source": "agent", "message": "2"},
            {"source": "user", "message": "<command-message>clear</command-message>"},
            {"source": "user", "message": None},
        ]
    }
    m = calibrate.measure(json.dumps(doc))
    assert m["n_user_turns"] == 1
    assert calibrate.classify(m, 2000) == "useless"


def test_genuine_followup_after_injected_context_still_counts():
    doc = {
        "steps": [
            {"source": "user", "message": "first question"},
            {"source": "agent", "message": "first answer"},
            {
                "source": "user",
                "message": "<system-reminder>context</system-reminder>second question",
            },
            {"source": "agent", "message": "second answer"},
        ]
    }
    m = calibrate.measure(json.dumps(doc))
    assert m["n_user_turns"] == 2
    assert calibrate.classify(m, 2000) == "substantive"


@pytest.mark.parametrize(
    ("message", "expected"),
    [
        ("plain", "plain"),
        ("<system-reminder>x</system-reminder>", ""),
        ("<system-reminder>x</system-reminder>real", "real"),
        ("real<command-message>clear</command-message>", "real"),
        ("ok<system-reminder>unterminated", "ok"),
        ("a\n\n\nb", "a\n\nb"),
        ([{"type": "text", "text": "hi"}], '[{"type":"text","text":"hi"}]'),
    ],
)
def test_strip_system_context_matches_collector_contract(message, expected):
    assert calibrate.strip_system_context(calibrate.message_text(message)) == expected


@pytest.mark.parametrize(
    ("message", "expected"),
    [
        (None, 0),
        ("中文", 2),
        ([], 2),
        ([{"type": "text", "text": "a" * 2500}], 2527),
        ([{"type": "text", "text": "中文" * 1250}], 2527),
        ([{"type": "text", "text": "one"}, {"type": "text", "text": "two"}], 59),
    ],
)
def test_message_length_matches_shared_atif_reader(
    message: object, expected: int
) -> None:
    doc = {
        "steps": [
            {"source": "user", "message": "explain"},
            {"source": "agent", "message": message},
        ]
    }
    measured = calibrate.measure(json.dumps(doc))
    assert measured["max_agent_len"] == expected
    assert measured["n_user_turns"] == 1
    assert measured["n_tool_calls"] == 0


def test_cli_retains_substantive_content_parts_without_writing_database(
    tmp_path: Path,
) -> None:
    path = tmp_path / "trajectories.db"
    with sqlite3.connect(path) as connection:
        connection.execute(
            "CREATE TABLE collected_trajectories "
            "(session_id TEXT, first_user_message TEXT, atif_json TEXT)"
        )
        for name, message in [
            ("parts", [{"type": "text", "text": "文" * 2500}]),
            ("plain", "文" * 2500),
            ("short", "short"),
        ]:
            doc = {
                "steps": [
                    {"source": "user", "message": "explain"},
                    {"source": "agent", "message": message},
                ]
            }
            connection.execute(
                "INSERT INTO collected_trajectories VALUES (?, ?, ?)",
                (name, name, json.dumps(doc)),
            )
    original = path.read_bytes()
    result = subprocess.run(
        [sys.executable, str(SCRIPT), str(path)],
        capture_output=True,
        text=True,
        check=True,
    )
    assert "substantive      2" in result.stdout
    assert "useless          1" in result.stdout
    assert "'parts'" not in result.stdout
    assert "'short'" in result.stdout
    assert path.read_bytes() == original


if __name__ == "__main__":
    raise SystemExit(pytest.main([__file__, "-q"]))
