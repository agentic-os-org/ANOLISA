"""An explicit collection root overrides recorded per-profile locations."""

import csv
import json
from pathlib import Path

import pytest
from typer.testing import CliRunner

from swe_runner.cli import app
from swe_runner.trace_extraction.helpers import parse_time_value
from swe_runner.trace_extraction.plan import TraceCollectionPlan


@pytest.fixture
def profile_sources(tmp_path: Path):
    old_root, new_root = tmp_path / "old-profiles", tmp_path / "new-profiles"

    def session(root, profile, identifier, input_tokens):
        directory = root / profile / "agents/agent/sessions"
        directory.mkdir(parents=True)
        rows = [
            {"type": "session", "id": identifier},
            {
                "type": "message",
                "timestamp": "2026-04-24T00:00:01Z",
                "message": {"role": "user", "content": [{"type": "text", "text": "Issue ID: task1"}]},
            },
            {
                "type": "message",
                "timestamp": "2026-04-24T00:00:02Z",
                "message": {
                    "role": "assistant",
                    "content": [{"type": "text", "text": "done"}],
                    "usage": {"input": input_tokens, "output": 1},
                },
            },
        ]
        (directory / f"{identifier}.jsonl").write_text(
            "".join(json.dumps(row) + "\n" for row in rows), encoding="utf-8"
        )
        return root / profile

    old_profile = session(old_root, "old", "sid1", 10)
    session(new_root, "new", "sid1", 20)
    session(new_root, "unrelated", "other-session", 999)
    metadata = tmp_path / "run_metadata.json"
    payload = {
        "started_at_ns": parse_time_value("2026-04-24T00:00:00Z"),
        "ended_at_ns": parse_time_value("2026-04-24T00:01:00Z"),
        "session_ids": {"task1": "sid1"},
        "instance_ids": ["task1"],
        "openclaw_profile_dirs": {"task1": str(old_profile)},
    }
    metadata.write_text(json.dumps(payload), encoding="utf-8")
    return metadata, payload, old_profile, new_root, tmp_path / "traces"


@pytest.mark.parametrize("old_missing", [False, True])
def test_explicit_root_collects_matching_session_from_override(profile_sources, old_missing):
    metadata, payload, _, new_root, traces = profile_sources
    if old_missing:
        payload["openclaw_profile_dirs"] = {"task1": str(new_root.parent / "missing-old-profile")}
        metadata.write_text(json.dumps(payload), encoding="utf-8")
    plan = TraceCollectionPlan.resolve(run_metadata_path=metadata, openclaw_profiles_dir=new_root)
    files = plan.collect(traces)
    assert len(files) == 1
    result = json.loads(files[0].read_text(encoding="utf-8"))
    assert result["session_id"] == "sid1"
    assert result["total_input_tokens"] == 20
    assert plan.session_ids == {"sid1"}
    assert plan.instance_ids == {"task1"}


def test_default_collection_keeps_recorded_directories(profile_sources):
    metadata, _, old_profile, _, traces = profile_sources
    plan = TraceCollectionPlan.resolve(run_metadata_path=metadata)
    files = plan.collect(traces)
    assert len(files) == 1
    assert json.loads(files[0].read_text(encoding="utf-8"))["total_input_tokens"] == 10
    assert plan.profile_dirs == [old_profile]


def test_root_override_without_recorded_directories_still_works(profile_sources):
    metadata, payload, _, new_root, traces = profile_sources
    del payload["openclaw_profile_dirs"]
    metadata.write_text(json.dumps(payload), encoding="utf-8")
    files = TraceCollectionPlan.resolve(run_metadata_path=metadata, openclaw_profiles_dir=new_root).collect(traces)
    assert len(files) == 1
    assert json.loads(files[0].read_text(encoding="utf-8"))["total_input_tokens"] == 20


def test_cli_exports_override_evidence(profile_sources, tmp_path: Path):
    metadata, _, _, new_root, traces = profile_sources
    output = tmp_path / "output"
    result = CliRunner().invoke(
        app,
        [
            "analyze-traces",
            "--run-metadata",
            str(metadata),
            "--openclaw-profiles-dir",
            str(new_root),
            "--trace-root",
            str(traces),
            "--output",
            str(output),
        ],
    )
    assert result.exit_code == 0, result.output
    with (output / "analyze-traces/trace_metrics/trace_metrics.csv").open(encoding="utf-8", newline="") as stream:
        rows = list(csv.DictReader(stream))
    assert len(rows) == 1
    assert rows[0]["总输入Token数"] == "20"
