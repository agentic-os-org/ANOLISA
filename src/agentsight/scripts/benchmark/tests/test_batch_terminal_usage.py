"""Measured assistant records must survive even without visible content."""

import json
import subprocess
from pathlib import Path
from unittest.mock import patch

import pytest
from test_batch_analyze import batch_analyze


def write_session(tmp_path: Path, records: list[dict]) -> Path:
    path = tmp_path / "session.jsonl"
    path.write_text("".join(json.dumps(record) + "\n" for record in records), encoding="utf-8")
    return path


def record(content: object, usage: object, identity: str = "fixture") -> dict:
    return {"type": "assistant", "message": {"id": identity, "content": content, "usage": usage}}


@pytest.mark.parametrize("content", [[], "", None])
def test_terminal_snapshot_replaces_early_usage(tmp_path: Path, content: object) -> None:
    path = write_session(
        tmp_path,
        [
            record([{"type": "text", "text": "answer"}], {"input_tokens": 100, "output_tokens": 1}),
            record(content, {"input_tokens": 100, "output_tokens": 9}),
        ],
    )
    trajectory = batch_analyze.jsonl_to_atif(path)
    assert (
        sum(step.get("metrics", {}).get("completion_tokens", 0) for step in trajectory["steps"])
        == 9
    )
    assert (
        sum(step.get("metrics", {}).get("prompt_tokens", 0) for step in trajectory["steps"]) == 100
    )
    assert any(step["message"] == "answer" for step in trajectory["steps"])


def test_usage_only_file_reaches_analyzer_and_export(tmp_path: Path) -> None:
    path = write_session(tmp_path, [record([], {"input_tokens": 12, "output_tokens": 0})])
    trajectory = batch_analyze.jsonl_to_atif(path)
    assert trajectory is not None
    assert trajectory["steps"][0]["metrics"] == {"prompt_tokens": 12, "completion_tokens": 0}
    artifact = batch_analyze.export_atif(trajectory, path, tmp_path / "export")
    assert json.loads(artifact.read_text(encoding="utf-8")) == trajectory
    observed = []

    def analyze(args: list[str], **kwargs: object) -> subprocess.CompletedProcess:
        observed.append(json.loads(Path(args[1]).read_text(encoding="utf-8")))
        return subprocess.CompletedProcess(args, 0, stdout="{}", stderr="")

    with patch.object(batch_analyze.subprocess, "run", analyze):
        status, _, _ = batch_analyze.analyze_file(path, 1, ["analyzer"], "cost")
    assert status == "analyzed"
    assert observed == [trajectory]


@pytest.mark.parametrize("usage", [None, {}, {"output_tokens": -1}, {"input_tokens": True}])
def test_empty_unmeasured_records_still_do_not_create_steps(tmp_path: Path, usage: object) -> None:
    assert batch_analyze.jsonl_to_atif(write_session(tmp_path, [record([], usage)])) is None
