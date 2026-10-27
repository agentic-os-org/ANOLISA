"""The six-Skill acceptance harness must exercise the installed native CLI."""

import json
import os
import subprocess
import sys
from pathlib import Path
from textwrap import dedent

import pytest

HARNESS = (
    Path(__file__).resolve().parents[2] / "integration-test/skill-score/assess-six.py"
)


def run_stage(root: Path, stage: str) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        [
            sys.executable,
            str(HARNESS),
            str(root),
            str(root / "out"),
            "--skills",
            "sample",
            "--stage",
            stage,
        ],
        env={**os.environ, "PATH": str(root)},
        capture_output=True,
        text=True,
        check=False,
    )


@pytest.mark.parametrize("legacy_cli", [False, True])
def test_requires_native_cli_before_creating_outputs(tmp_path: Path, legacy_cli: bool):
    if legacy_cli:
        cli = tmp_path / "agent-sec-cli"
        cli.write_text('#!/bin/sh\n[ "$1" = "skill-score" ]\n')
        cli.chmod(0o755)
    result = run_stage(tmp_path, "plan")
    assert result.returncode == 1
    assert "agent-sec-cli" in result.stderr
    assert not (tmp_path / "out").exists()


def test_native_stages_propagate_failure_and_keep_artifacts(tmp_path: Path):
    cli = tmp_path / "agent-sec-cli"
    cli.write_text(f"#!{sys.executable}\n" + dedent("""
            import json
            import sys
            from pathlib import Path

            group, command, stage, *args = sys.argv[1:]
            assert (group, command) == ("skillsec", "score")
            if args == ["--help"]:
                sys.exit(0)
            if stage == "plan":
                assert Path(args[0]).parts[-2:] == ("skills", "sample")
                data = {"audit": {"probes": [{"id": "baseline"}]}}
            else:
                assert Path(args[0]).name == "task.json"
                assert args[3] == "--evidence-dir"
                data = {"execution": [], "decision": "incomplete", "score": None, "findings": []}
            assert args[1] == "--output-file"
            Path(args[2]).write_text(json.dumps(data))
            print(json.dumps(data))
            sys.exit(0 if stage == "plan" else 1)
            """))
    cli.chmod(0o755)
    plan = run_stage(tmp_path, "plan")
    assert plan.returncode == 0, plan.stderr
    run = run_stage(tmp_path, "run")
    assert run.returncode == 1, run.stderr
    status = json.loads((tmp_path / "out/sample/run-status.json").read_text())
    assert status["exit_code"] == 1 and status["stdout_matches_json"]
    assert not status["all_tasks_reported"]
    assert (tmp_path / "out/summary.json").is_file()
