"""E2E tests for agent-sec-cli observability record SQLite indexing."""

import json
import os
import sqlite3
from datetime import datetime, timezone
from pathlib import Path

from .conftest import run_cli


def test_observability_record_json_creates_observability_sqlite_index() -> None:
    data_dir = Path(os.environ["AGENT_SEC_DATA_DIR"])
    payload = {
        "hook": "after_tool_call",
        "observedAt": datetime.now(timezone.utc).isoformat(),
        "metadata": {
            "sessionId": "session-e2e",
            "runId": "run-e2e",
            "callId": "call-e2e",
            "toolCallId": "tool-call-e2e",
        },
        "metrics": {
            "result": {"ok": True},
            "duration_ms": 25,
            "result_size_bytes": 128,
        },
    }

    result = run_cli(
        "observability",
        "record",
        "--format",
        "json",
        "--stdin",
        input_text=json.dumps(payload),
    )

    assert result.returncode == 0, result.stderr
    assert result.stdout == ""
    assert (data_dir / "observability.jsonl").exists()
    assert (data_dir / "observability.db").exists()
    assert not (data_dir / "security-events.db").exists()

    conn = sqlite3.connect(data_dir / "observability.db")
    try:
        row = conn.execute("""
            SELECT hook, session_id, run_id, call_id, tool_call_id, metrics_json
            FROM observability_events
            """).fetchone()
    finally:
        conn.close()

    assert row[0:5] == (
        "after_tool_call",
        "session-e2e",
        "run-e2e",
        "call-e2e",
        "tool-call-e2e",
    )
    assert json.loads(row[5])["result_size_bytes"] == 128
