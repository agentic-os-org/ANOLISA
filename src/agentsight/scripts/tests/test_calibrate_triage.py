"""Contract tests for the session-level triage calibration script.

`calibrate-triage.py` decides which trajectories the "useless" rule would
drop from retrieval scope. The classify() bucket boundaries and the 30%
exclusion ceiling come from design §5.2/§14, so a silent drift in either
changes what the retrieval layer keeps — the reason this script exists is
that these numbers must come from measured data, not guesses.
"""

from __future__ import annotations

import importlib.util
import json
import sqlite3
import sys
from pathlib import Path

import pytest

SCRIPT = Path(__file__).parents[1] / "calibrate-triage.py"

spec = importlib.util.spec_from_file_location("calibrate_triage", SCRIPT)
calibrate = importlib.util.module_from_spec(spec)
sys.modules["calibrate_triage"] = calibrate
spec.loader.exec_module(calibrate)


def atif(steps: list[dict]) -> str:
    return json.dumps({"steps": steps})


def user(message: str = "hello") -> dict:
    return {"source": "user", "message": message}


def agent(message: str = "a" * 100, tool_calls: list | None = None) -> dict:
    return {"source": "agent", "message": message, "tool_calls": tool_calls or []}


# ─── measure ──────────────────────────────────────────────────────────────────


def test_measure_extracts_the_structural_triage_inputs() -> None:
    doc = atif([
        user("question one"),
        agent("short answer", tool_calls=[{"name": "grep"}, {"name": "ls"}]),
        user("follow up"),
        agent("b" * 500),
    ])

    m = calibrate.measure(doc)

    assert m == {
        "n_steps": 4,
        "n_user_turns": 2,
        "n_tool_calls": 2,
        "max_agent_len": 500,
    }


def test_measure_reports_zero_agent_length_when_no_agent_spoke() -> None:
    m = calibrate.measure(atif([user("question")]))

    assert m["max_agent_len"] == 0


def test_measure_tolerates_steps_without_messages_or_tool_calls() -> None:
    m = calibrate.measure(atif([{"source": "agent"}, {"source": "user", "message": "q"}]))

    assert m["n_steps"] == 2
    assert m["n_user_turns"] == 1
    assert m["n_tool_calls"] == 0
    assert m["max_agent_len"] == 0


# ─── classify: the §5.2 rule ─────────────────────────────────────────────────


def test_a_single_step_session_is_empty() -> None:
    assert calibrate.classify({"n_steps": 1, "n_user_turns": 1, "n_tool_calls": 0, "max_agent_len": 50}, 2000) == "empty"


def test_a_session_where_the_agent_said_nothing_is_empty() -> None:
    m = {"n_steps": 4, "n_user_turns": 2, "n_tool_calls": 0, "max_agent_len": 0}
    assert calibrate.classify(m, 2000) == "empty"


def test_a_toolless_single_turn_short_session_is_useless() -> None:
    m = {"n_steps": 2, "n_user_turns": 1, "n_tool_calls": 0, "max_agent_len": 1999}
    assert calibrate.classify(m, 2000) == "useless"


def test_the_answer_length_boundary_is_exclusive() -> None:
    # exactly at the ceiling the session is substantive, not useless
    m = {"n_steps": 2, "n_user_turns": 1, "n_tool_calls": 0, "max_agent_len": 2000}
    assert calibrate.classify(m, 2000) == "substantive"


def test_a_carried_forward_topic_is_substantive_even_without_tools() -> None:
    m = {"n_steps": 4, "n_user_turns": 2, "n_tool_calls": 0, "max_agent_len": 10}
    assert calibrate.classify(m, 2000) == "substantive"


def test_any_tool_call_makes_a_session_substantive() -> None:
    m = {"n_steps": 2, "n_user_turns": 1, "n_tool_calls": 1, "max_agent_len": 10}
    assert calibrate.classify(m, 2000) == "substantive"


def test_the_default_ceiling_is_the_design_default() -> None:
    assert calibrate.DEFAULT_MAX_AGENT_LEN == 2000


# ─── main: end to end against a real trajectories database ────────────────────


@pytest.fixture()
def trajectories_db(tmp_path: Path) -> Path:
    db = tmp_path / "trajectories.db"
    conn = sqlite3.connect(db)
    conn.execute(
        "CREATE TABLE collected_trajectories "
        "(session_id TEXT, first_user_message TEXT, atif_json TEXT)"
    )
    rows = [
        # 6 substantive: tool calls
        ("s1", "q1", atif([user(), agent(tool_calls=[{"name": "grep"}])])),
        ("s2", "q2", atif([user(), agent(tool_calls=[{"name": "grep"}])])),
        ("s3", "q3", atif([user(), agent(tool_calls=[{"name": "grep"}])])),
        ("s4", "q4", atif([user(), agent(tool_calls=[{"name": "grep"}])])),
        ("s5", "q5", atif([user(), agent(tool_calls=[{"name": "grep"}])])),
        ("s6", "q6", atif([user(), agent(tool_calls=[{"name": "grep"}])])),
        ("s7", "q7", atif([user(), agent(tool_calls=[{"name": "grep"}])])),
        # 2 useless: toolless single-turn short answers
        ("s8", "q8", atif([user(), agent("short")])),
        ("s9", "q9", atif([user(), agent("short")])),
        # 1 empty: single step
        ("s10", "q10", atif([user()])),
        # 1 unparsable row
        ("s11", "q11", "{not json"),
    ]
    conn.executemany(
        "INSERT INTO collected_trajectories VALUES (?, ?, ?)", rows
    )
    conn.commit()
    conn.close()
    return db


def run_main(db: Path, *args: str) -> tuple[int, str]:
    import contextlib
    import io

    stdout, stderr = io.StringIO(), io.StringIO()
    argv = sys.argv
    sys.argv = ["calibrate-triage.py", str(db), *args]
    try:
        with contextlib.redirect_stdout(stdout), contextlib.redirect_stderr(stderr):
            code = calibrate.main()
    finally:
        sys.argv = argv
    return code, stdout.getvalue() + stderr.getvalue()


def test_main_reports_buckets_exclusion_and_tool_distribution(trajectories_db: Path) -> None:
    code, out = run_main(trajectories_db)

    assert code == 0
    assert "total=11 unparsable=1 max_agent_len=2000" in out
    assert "substantive      7  (63.6%)" in out
    assert "useless          2  (18.2%)" in out
    assert "empty            1  (9.1%)" in out
    # 2 useless + 1 empty of 11 parsable-and-total rows = 27.3%, under the ceiling
    assert "would be excluded from retrieval: 27.3%" in out
    assert "WARNING" not in out
    # 3 tool-less sessions (2 useless + 1 empty + 1 unparsable never measured)
    assert "tools=0         3" in out
    assert "tools=1-5       7" in out
    # the excluded preview names the bucket and the first user message
    assert "[useless   ]" in out
    assert "'q8'" in out


def test_main_limits_the_excluded_sample_list(trajectories_db: Path) -> None:
    code, out = run_main(trajectories_db, "--samples", "1")

    assert code == 0
    assert out.count("[useless   ]") == 1
    assert out.count("[empty     ]") <= 1


def test_main_warns_above_the_thirty_percent_ceiling(trajectories_db: Path) -> None:
    # drop three substantive rows, add three more useless ones:
    # 4 substantive + 5 useless + 1 empty + 1 unparsable of 11 -> 6/11 = 54.5%
    conn = sqlite3.connect(trajectories_db)
    conn.execute("DELETE FROM collected_trajectories WHERE session_id IN ('s3','s4','s5')")
    for i in (12, 13, 14):
        conn.execute(
            "INSERT INTO collected_trajectories VALUES (?, ?, ?)",
            (f"s{i}", f"q{i}", atif([user(), agent("short")])),
        )
    conn.commit()
    conn.close()

    code, out = run_main(trajectories_db)

    assert code == 0
    assert "would be excluded from retrieval: 54.5%" in out
    assert "WARNING: above the 30% ceiling" in out


def test_main_fails_cleanly_on_an_unreadable_database(tmp_path: Path) -> None:
    code, out = run_main(tmp_path / "missing.db")

    assert code == 1
    assert "cannot read" in out


def test_main_fails_cleanly_on_an_empty_database(tmp_path: Path) -> None:
    db = tmp_path / "empty.db"
    conn = sqlite3.connect(db)
    conn.execute(
        "CREATE TABLE collected_trajectories "
        "(session_id TEXT, first_user_message TEXT, atif_json TEXT)"
    )
    conn.commit()
    conn.close()

    code, out = run_main(db)

    assert code == 1
    assert "no collected trajectories" in out
