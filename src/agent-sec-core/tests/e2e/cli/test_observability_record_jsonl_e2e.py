"""E2E tests for agent-sec-cli observability record JSONL persistence."""

import json
import os
from pathlib import Path

from .conftest import run_cli


def test_observability_record_json_creates_observability_jsonl() -> None:
    data_dir = Path(os.environ["AGENT_SEC_DATA_DIR"])
    payload = {
        "hook": "after_tool_call",
        "observedAt": "2026-05-11T12:00:00Z",
        "metadata": {
            "sessionId": "session-e2e",
            "runId": "run-e2e",
            "toolCallId": "tool-call-e2e",
        },
        "metrics": {
            "result": {"ok": True},
            "duration_ms": 25,
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
    records = [
        json.loads(line)
        for line in (data_dir / "observability.jsonl")
        .read_text(encoding="utf-8")
        .splitlines()
    ]
    assert records[0]["hook"] == "after_tool_call"
    assert "schemaVersion" not in records[0]
    assert records[0]["metadata"]["runId"] == "run-e2e"
    assert records[0]["metadata"]["toolCallId"] == "tool-call-e2e"
    assert records[0]["metrics"] == {"result": {"ok": True}, "duration_ms": 25}
    assert not (data_dir / "security-events.jsonl").exists()
