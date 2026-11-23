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

import json
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import patch

import pytest
from pytest_mock import MockerFixture
from typer.testing import CliRunner

from swe_runner.agents import AgentEnvironmentError
from swe_runner.cli import app
from swe_runner.run.io.report import RunReport

runner = CliRunner()


def test_help_shows_options():
    result = runner.invoke(app, ["run", "--help"])
    assert result.exit_code == 0
    assert "--agent" in result.output
    assert "Available:" in result.output
    assert "cosh" in result.output
    assert "openclaw" in result.output
    assert "--subset" in result.output
    assert "multilingual" in result.output
    assert "--slice" in result.output
    assert "--workers" in result.output
    assert "--docker-pull-registry" in result.output
    assert "--use-skill" in result.output
    assert "--tokenless" in result.output
    assert "--headroom" in result.output
    assert "--no-skill" not in result.output


def test_main_help_does_not_show_extract_traces():
    result = runner.invoke(app, ["--help"])
    assert result.exit_code == 0
    assert "extract-traces" not in result.output


def test_run_requires_agent():
    result = runner.invoke(app, ["run"])
    assert result.exit_code != 0


def test_run_invalid_agent():
    with patch(
        "swe_runner.cli_commands.RunSession.execute",
        side_effect=KeyError("Unknown agent 'nonexistent'. Available agents: cosh, openclaw"),
    ):
        result = runner.invoke(app, ["run", "--agent", "nonexistent", "--subset", "lite"])
        assert result.exit_code == 1
        assert "nonexistent" in result.output


def test_run_rejects_skill_and_per_case_prompt_together():
    result = runner.invoke(app, ["run", "--agent", "openclaw", "--use-skill", "--per-case-prompt"])

    assert result.exit_code == 1
    assert "--use-skill and --per-case-prompt are mutually exclusive" in result.output


def test_run_does_not_pass_openclaw_config_to_agent():
    with patch("swe_runner.cli_commands.RunSession") as mock_session_cls:
        mock_session_cls.return_value.execute.return_value = RunReport(succeeded=0, failed=0, total=0, instance_ids=[])

        result = runner.invoke(app, ["run", "--agent", "openclaw"])

        assert result.exit_code == 0
        settings = mock_session_cls.call_args.args[0]
        assert settings.agent.name == "openclaw"


def test_run_openclaw_env_check_failure_uses_generic_message():
    with patch(
        "swe_runner.cli_commands.RunSession.execute",
        side_effect=AgentEnvironmentError("Docker daemon is not accessible"),
    ):
        result = runner.invoke(app, ["run", "--agent", "openclaw"])

    assert result.exit_code == 1
    assert "Environment check failed" in result.output


def test_run_non_openclaw_env_check_failure_uses_generic_message():
    with patch(
        "swe_runner.cli_commands.RunSession.execute",
        side_effect=AgentEnvironmentError("Docker daemon is not accessible"),
    ):
        result = runner.invoke(app, ["run", "--agent", "cosh"])

    assert result.exit_code == 1
    assert "Environment check failed" in result.output


def test_evaluate_help():
    result = runner.invoke(app, ["evaluate", "--help"])
    assert result.exit_code == 0
    assert "predictions" in result.output
    assert "multilingual" in result.output
    assert "--namespace" in result.output


def test_evaluate_missing_predictions():
    result = runner.invoke(app, ["evaluate", "--predictions", "/nonexistent/preds.json"])
    assert result.exit_code == 1


def test_evaluate_registered():
    result = runner.invoke(app, ["--help"])
    assert result.exit_code == 0
    assert "evaluate" in result.output


def test_analyze_traces_registered():
    result = runner.invoke(app, ["--help"])
    assert result.exit_code == 0
    assert "analyze-traces" in result.output


def test_analyze_traces_help():
    result = runner.invoke(app, ["analyze-traces", "--help"])
    assert result.exit_code == 0
    assert "trace-root" in result.output
    assert "trim-ratio" in result.output
    assert "openclaw-profiles-dir" in result.output
    assert "--start" in result.output
    assert "--end" in result.output
    assert "--run-metadata" in result.output


def test_run_writes_run_metadata(tmp_path):
    report = RunReport(
        succeeded=1,
        failed=1,
        total=2,
        instance_ids=["inst-1", "inst-2"],
        metadata_mappings={
            "session_ids": {"inst-1": "session-inst-1"},
            "openclaw_profile_dirs": {"inst-1": "/tmp/profiles/inst-1"},
        },
        metadata_path=tmp_path / "run" / "run_metadata.json",
        started_at_ns=100,
        ended_at_ns=200,
    )

    with patch("swe_runner.cli_commands.RunSession") as mock_session_cls:
        mock_session_cls.return_value.execute.return_value = report

        result = runner.invoke(app, ["run", "--agent", "openclaw", "--output", str(tmp_path)])

    assert result.exit_code == 0
    assert "1/2 succeeded" in result.output
    assert "Run metadata" in result.output
    settings = mock_session_cls.call_args.args[0]
    assert settings.agent.name == "openclaw"
    assert settings.agent.workers == 1
    assert settings.output.output_dir == tmp_path / "run"


def test_run_passes_use_skill_into_settings(tmp_path):
    report = RunReport(
        succeeded=1,
        failed=0,
        total=1,
        instance_ids=["inst-1"],
        metadata_path=tmp_path / "run_metadata.json",
    )

    with patch("swe_runner.cli_commands.RunSession") as mock_session_cls:
        mock_session_cls.return_value.execute.return_value = report

        result = runner.invoke(
            app,
            [
                "run",
                "--agent",
                "cosh",
                "--output",
                str(tmp_path),
                "--use-skill",
                "--skills-dir",
                str(tmp_path / "skills"),
            ],
        )

    assert result.exit_code == 0
    settings = mock_session_cls.call_args.args[0]
    assert settings.agent.use_skill is True
    assert settings.agent.skills_dir == tmp_path / "skills"


def test_run_passes_prompts_dir_into_settings(tmp_path):
    report = RunReport(
        succeeded=1,
        failed=0,
        total=1,
        instance_ids=["inst-1"],
        metadata_path=tmp_path / "run_metadata.json",
    )

    with patch("swe_runner.cli_commands.RunSession") as mock_session_cls:
        mock_session_cls.return_value.execute.return_value = report

        result = runner.invoke(
            app,
            [
                "run",
                "--agent",
                "cosh",
                "--output",
                str(tmp_path),
                "--per-case-prompt",
                "--prompts-dir",
                str(tmp_path / "prompts"),
            ],
        )

    assert result.exit_code == 0
    settings = mock_session_cls.call_args.args[0]
    assert settings.agent.per_case_prompt is True
    assert settings.agent.prompts_dir == tmp_path / "prompts"


def test_run_passes_tokenless_into_settings(tmp_path):
    report = RunReport(
        succeeded=1,
        failed=0,
        total=1,
        instance_ids=["inst-1"],
        metadata_path=tmp_path / "run_metadata.json",
    )

    with patch("swe_runner.cli_commands.RunSession") as mock_session_cls:
        mock_session_cls.return_value.execute.return_value = report

        result = runner.invoke(
            app,
            [
                "run",
                "--agent",
                "openclaw",
                "--output",
                str(tmp_path),
                "--tokenless",
            ],
        )

    assert result.exit_code == 0
    settings = mock_session_cls.call_args.args[0]
    assert settings.agent.tokenless is True


def test_run_rejects_tokenless_for_unsupported_agent() -> None:
    result = runner.invoke(app, ["run", "--agent", "cosh", "--tokenless"])

    assert result.exit_code == 1
    assert "--tokenless is not supported by agent 'cosh'" in result.output


def test_run_passes_headroom_into_settings(tmp_path):
    report = RunReport(
        succeeded=1,
        failed=0,
        total=1,
        instance_ids=["inst-1"],
        metadata_path=tmp_path / "run_metadata.json",
    )

    with patch("swe_runner.cli_commands.RunSession") as mock_session_cls:
        mock_session_cls.return_value.execute.return_value = report

        result = runner.invoke(
            app,
            [
                "run",
                "--agent",
                "openclaw",
                "--output",
                str(tmp_path),
                "--headroom",
            ],
        )

    assert result.exit_code == 0
    settings = mock_session_cls.call_args.args[0]
    assert settings.agent.headroom is True
    assert settings.agent.tokenless is False


def test_run_rejects_headroom_for_unsupported_agent() -> None:
    result = runner.invoke(app, ["run", "--agent", "cosh", "--headroom"])

    assert result.exit_code == 1
    assert "--headroom is not supported by agent 'cosh'" in result.output


def test_run_rejects_tokenless_combined_with_headroom() -> None:
    result = runner.invoke(app, ["run", "--agent", "openclaw", "--tokenless", "--headroom"])

    assert result.exit_code == 1
    assert "mutually exclusive" in result.output


def test_analyze_traces_can_collect_from_openclaw_jsonl(tmp_path):
    metadata_path = tmp_path / "run_metadata.json"
    profiles_dir = tmp_path / "openclaw-profiles"
    fake_plan = SimpleNamespace(
        should_collect=True,
        collect=lambda trace_root: [tmp_path / "traces" / "inst" / "trace1.json"],
    )

    with (
        patch(
            "swe_runner.cli_commands.TraceCollectionPlan.resolve",
            return_value=fake_plan,
        ) as mock_resolve,
        patch(
            "swe_runner.cli_commands.write_trace_analysis_csvs",
            return_value=(tmp_path / "detail", tmp_path / "summary.csv"),
        ),
    ):
        result = runner.invoke(
            app,
            [
                "analyze-traces",
                "--run-metadata",
                str(metadata_path),
                "--openclaw-profiles-dir",
                str(profiles_dir),
            ],
        )

    assert result.exit_code == 0
    mock_resolve.assert_called_once_with(
        start=None,
        end="now",
        run_metadata_path=metadata_path,
        openclaw_profiles_dir=profiles_dir,
    )


def test_analyze_traces_collects_openclaw_jsonl_by_session_ids_and_profile_dirs(tmp_path):
    metadata_path = tmp_path / "run_metadata.json"
    fake_plan = SimpleNamespace(
        should_collect=True,
        collect=lambda trace_root: [tmp_path / "traces" / "inst" / "trace1.json"],
    )

    with (
        patch(
            "swe_runner.cli_commands.TraceCollectionPlan.resolve",
            return_value=fake_plan,
        ) as mock_resolve,
        patch(
            "swe_runner.cli_commands.write_trace_analysis_csvs",
            return_value=(tmp_path / "detail", tmp_path / "summary.csv"),
        ),
    ):
        result = runner.invoke(
            app,
            [
                "analyze-traces",
                "--run-metadata",
                str(metadata_path),
            ],
        )

    assert result.exit_code == 0
    mock_resolve.assert_called_once_with(
        start=None,
        end="now",
        run_metadata_path=metadata_path,
        openclaw_profiles_dir=None,
    )


def _forbid_side_effects():
    """Patch logger setup, trace collection, and CSV export to fail if touched."""
    return (
        patch("swe_runner.cli_commands.setup_logging", side_effect=AssertionError("dry-run must not set up logging")),
        patch(
            "swe_runner.trace_extraction.openclaw_source.record_openclaw_jsonl_traces_in_window",
            side_effect=AssertionError("dry-run must not collect traces"),
        ),
        patch(
            "swe_runner.cli_commands.write_trace_analysis_csvs",
            side_effect=AssertionError("dry-run must not export CSVs"),
        ),
    )


def test_analyze_traces_dry_run_previews_existing_trace_mode_without_side_effects(tmp_path):
    trace_root = tmp_path / "traces"
    output = tmp_path / "out"

    patches = _forbid_side_effects()
    with patches[0], patches[1], patches[2]:
        result = runner.invoke(
            app,
            ["analyze-traces", "--dry-run", "--trace-root", str(trace_root), "--output", str(output)],
        )

    assert result.exit_code == 0
    preview = json.loads(result.output)
    assert preview["trace_root"] == str(trace_root)
    assert preview["planned_report_paths"] == {
        "detail_dir": str(output / "analyze-traces" / "trace_details"),
        "summary_csv": str(output / "analyze-traces" / "trace_summary.csv"),
        "trace_metrics_csv": str(output / "analyze-traces" / "trace_metrics" / "trace_metrics.csv"),
    }
    assert preview["collection_plan"] == {
        "should_collect": False,
        "start_ns": 0,
        "end_ns": 0,
        "profiles_root": None,
        "profile_dirs": None,
        "instance_ids": None,
        "session_ids": None,
        "source_name": None,
    }
    assert not (output / "analyze-traces").exists()


def test_analyze_traces_dry_run_previews_explicit_window_and_profiles_dir(tmp_path):
    profiles_dir = tmp_path / "openclaw-profiles"
    output = tmp_path / "out"

    patches = _forbid_side_effects()
    with patches[0], patches[1], patches[2]:
        result = runner.invoke(
            app,
            [
                "analyze-traces",
                "--dry-run",
                "--start",
                "1700000000",
                "--end",
                "1700000005",
                "--openclaw-profiles-dir",
                str(profiles_dir),
                "--output",
                str(output),
            ],
        )

    assert result.exit_code == 0
    preview = json.loads(result.output)
    assert preview["trace_root"] == str(output / "analyze-traces" / "traces")
    plan = preview["collection_plan"]
    assert plan["should_collect"] is True
    assert plan["start_ns"] == 1_700_000_000_000_000_000
    assert plan["end_ns"] == 1_700_000_005_000_000_000
    assert plan["profiles_root"] == str(profiles_dir)
    assert plan["source_name"] == "openclaw_jsonl"
    assert not (output / "analyze-traces").exists()


def test_analyze_traces_dry_run_previews_metadata_window_with_time_padding(tmp_path):
    metadata_path = tmp_path / "run_metadata.json"
    metadata_path.write_text(
        json.dumps({"started_at_ns": 1000, "ended_at_ns": 2000, "session_ids": {"inst-1": "sess-1", "inst-2": "sess-2"}}),
        encoding="utf-8",
    )

    patches = _forbid_side_effects()
    with patches[0], patches[1], patches[2]:
        result = runner.invoke(
            app,
            ["analyze-traces", "--dry-run", "--run-metadata", str(metadata_path)],
        )

    assert result.exit_code == 0
    plan = json.loads(result.output)["collection_plan"]
    assert plan["should_collect"] is True
    assert plan["start_ns"] == 1000
    assert plan["end_ns"] == 2000 + 10_000_000_000
    assert plan["profiles_root"] == str(metadata_path.parent / "openclaw-profiles")
    assert plan["session_ids"] == ["sess-1", "sess-2"]
    assert plan["instance_ids"] is None


def test_analyze_traces_dry_run_sorts_metadata_identities_and_profile_lists(tmp_path):
    profile_dirs = [tmp_path / "p-béta", tmp_path / "p-alpha", tmp_path / "p-gamma"]
    metadata_path = tmp_path / "run_metadata.json"
    metadata_path.write_text(
        json.dumps(
            {
                "started_at_ns": 1000,
                "ended_at_ns": 2000,
                "instance_ids": ["zeta", "alpha", "Beta"],
                "session_ids": {"inst-1": "sess-b", "inst-2": "sess-a", "inst-3": "sess-c"},
                "openclaw_profile_dirs": {
                    "inst-1": str(profile_dirs[0]),
                    "inst-2": str(profile_dirs[1]),
                    "inst-3": str(profile_dirs[2]),
                },
            }
        ),
        encoding="utf-8",
    )

    patches = _forbid_side_effects()
    with patches[0], patches[1], patches[2]:
        result = runner.invoke(
            app,
            ["analyze-traces", "--dry-run", "--run-metadata", str(metadata_path)],
        )

    assert result.exit_code == 0
    plan = json.loads(result.output)["collection_plan"]
    assert plan["instance_ids"] == ["Beta", "alpha", "zeta"]
    assert plan["session_ids"] == ["sess-a", "sess-b", "sess-c"]
    assert plan["profile_dirs"] == sorted(str(item) for item in profile_dirs)


def test_analyze_traces_dry_run_emits_ascii_safe_json_for_unicode_targets(tmp_path):
    metadata_path = tmp_path / "run_metadata.json"
    unicode_profile = tmp_path / "配置-files"
    metadata_path.write_text(
        json.dumps(
            {
                "started_at_ns": 1000,
                "ended_at_ns": 2000,
                "instance_ids": ["实例-Zèda"],
                "session_ids": {"实例-Zèda": "会话-1"},
                "openclaw_profile_dirs": {"实例-Zèda": str(unicode_profile)},
            }
        ),
        encoding="utf-8",
    )

    patches = _forbid_side_effects()
    with patches[0], patches[1], patches[2]:
        result = runner.invoke(
            app,
            ["analyze-traces", "--dry-run", "--run-metadata", str(metadata_path)],
        )

    assert result.exit_code == 0
    assert result.output.isascii()
    plan = json.loads(result.output)["collection_plan"]
    assert plan["instance_ids"] == ["实例-Zèda"]
    assert plan["session_ids"] == ["会话-1"]
    assert plan["profile_dirs"] == [str(unicode_profile)]


def test_analyze_traces_dry_run_rejects_reversed_window(tmp_path):
    output = tmp_path / "out"

    patches = _forbid_side_effects()
    with patches[0], patches[1], patches[2]:
        result = runner.invoke(
            app,
            [
                "analyze-traces",
                "--dry-run",
                "--start",
                "1700000010",
                "--end",
                "1700000000",
                "--output",
                str(output),
            ],
        )

    assert result.exit_code == 1
    assert "Trace window end must be greater than or equal to start" in result.output
    assert not (output / "analyze-traces").exists()


def test_analyze_traces_dry_run_rejects_invalid_and_end_only_windows(tmp_path):
    patches = _forbid_side_effects()
    with patches[0], patches[1], patches[2]:
        invalid = runner.invoke(app, ["analyze-traces", "--dry-run", "--start", "not-a-timestamp"])

    assert invalid.exit_code == 1
    assert "Invalid timestamp value" in invalid.output

    patches = _forbid_side_effects()
    with patches[0], patches[1], patches[2]:
        end_only = runner.invoke(app, ["analyze-traces", "--dry-run", "--end", "1700000000"])

    assert end_only.exit_code == 1
    assert "--end requires --start or --run-metadata" in end_only.output


def test_analyze_traces_dry_run_rejects_malformed_metadata_without_output_dirs(tmp_path):
    metadata_path = tmp_path / "run_metadata.json"
    metadata_path.write_text("{not valid json", encoding="utf-8")
    output = tmp_path / "out"

    patches = _forbid_side_effects()
    with patches[0], patches[1], patches[2]:
        result = runner.invoke(
            app,
            ["analyze-traces", "--dry-run", "--run-metadata", str(metadata_path), "--output", str(output)],
        )

    assert result.exit_code == 1
    assert "Failed to load run metadata" in result.output
    assert not (output / "analyze-traces").exists()


def test_evaluate_success_mock(mocker: MockerFixture) -> None:
    """Test evaluate command with mocked evaluation module."""
    from swe_runner.evaluation import EvalInstanceResult, EvalReport

    mock_report = EvalReport(
        total_instances=1,
        resolved_full=1,
        resolved_partial=0,
        resolved_no=0,
        patch_failed=0,
        error_count=0,
        resolution_rate=1.0,
        instance_results=[
            EvalInstanceResult(
                instance_id="test-1", resolved=True, resolution_status="RESOLVED_FULL", patch_applied=True
            )
        ],
        run_id="test",
        dataset_name="princeton-nlp/SWE-bench_Lite",
        evaluated_at="2026-01-01T00:00:00Z",
    )
    mock_run_evaluation = mocker.patch("swe_runner.cli_commands.run_patch_evaluation", return_value=mock_report)

    # Create a dummy preds.json so the file-exists check passes
    import json
    import tempfile

    with tempfile.NamedTemporaryFile(mode="w", suffix=".json", delete=False) as f:
        json.dump({"i1": {"instance_id": "i1", "model_name_or_path": "cosh", "model_patch": "diff"}}, f)
        preds_path = f.name

    result = runner.invoke(app, ["evaluate", "--predictions", preds_path])
    assert result.exit_code == 0
    mock_run_evaluation.assert_called_once()
    assert mock_run_evaluation.call_args.args[1] == Path("output/evaluate")


def test_evaluate_none_namespace_uses_local_build_mode(mocker: MockerFixture) -> None:
    mock_run_evaluation = mocker.patch("swe_runner.cli_commands.run_patch_evaluation", return_value=None)

    import json
    import tempfile

    with tempfile.NamedTemporaryFile(mode="w", suffix=".json", delete=False) as f:
        json.dump({"i1": {"instance_id": "i1", "model_name_or_path": "cosh", "model_patch": "diff"}}, f)
        preds_path = f.name

    result = runner.invoke(app, ["evaluate", "--predictions", preds_path, "--namespace", "none"])

    assert result.exit_code == 0
    mock_run_evaluation.assert_called_once()
    assert mock_run_evaluation.call_args.kwargs["namespace"] is None

def test_run_exits_nonzero_when_every_instance_fails(tmp_path):
    report = RunReport(
        succeeded=0,
        failed=3,
        total=3,
        instance_ids=["inst-1", "inst-2", "inst-3"],
        metadata_path=tmp_path / "run" / "run_metadata.json",
    )

    with patch("swe_runner.cli_commands.RunSession") as mock_session_cls:
        mock_session_cls.return_value.execute.return_value = report

        result = runner.invoke(app, ["run", "--agent", "openclaw", "--output", str(tmp_path)])

    assert result.exit_code == 1
    assert "0/3 succeeded" in result.output
    assert "All 3 instance(s) failed" in result.output


def test_run_exits_zero_for_partial_success(tmp_path):
    report = RunReport(
        succeeded=1,
        failed=2,
        total=3,
        instance_ids=["inst-1", "inst-2", "inst-3"],
        metadata_path=tmp_path / "run" / "run_metadata.json",
    )

    with patch("swe_runner.cli_commands.RunSession") as mock_session_cls:
        mock_session_cls.return_value.execute.return_value = report

        result = runner.invoke(app, ["run", "--agent", "openclaw", "--output", str(tmp_path)])

    assert result.exit_code == 0
    assert "1/3 succeeded" in result.output
    assert "instance(s) failed" not in result.output


def test_run_exits_zero_with_message_when_nothing_to_process(tmp_path):
    report = RunReport(
        succeeded=0,
        failed=0,
        total=0,
        instance_ids=[],
        metadata_path=None,
    )

    with patch("swe_runner.cli_commands.RunSession") as mock_session_cls:
        mock_session_cls.return_value.execute.return_value = report

        result = runner.invoke(app, ["run", "--agent", "openclaw", "--output", str(tmp_path)])

    assert result.exit_code == 0
    assert "No instances to process" in result.output



def _write_token_store(profiles_root: Path, instance_id: str, *, aggregate_input: int) -> None:
    """Create one profile holding a two-turn transcript, in the layout OpenClaw uses."""
    import json as json_mod
    import sqlite3

    store = profiles_root / instance_id / "agents" / instance_id / "agent" / "openclaw-agent.sqlite"
    store.parent.mkdir(parents=True, exist_ok=True)
    connection = sqlite3.connect(store)
    with connection:
        connection.execute("CREATE TABLE transcript_events (session_id TEXT, seq INTEGER, event_json TEXT)")
        connection.execute("CREATE TABLE trajectory_runtime_events (event_json TEXT)")
        for seq in (1, 2):
            connection.execute(
                "INSERT INTO transcript_events VALUES (?, ?, ?)",
                (
                    "s1",
                    seq,
                    json_mod.dumps({"message": {"usage": {"input": 50, "output": 5, "cacheRead": 500}}}),
                ),
            )
        connection.execute(
            "INSERT INTO trajectory_runtime_events VALUES (?)",
            (
                json_mod.dumps(
                    {
                        "type": "model.completed",
                        "data": {"usage": {"input": aggregate_input, "output": 10, "cacheRead": 1000}},
                    }
                ),
            ),
        )
    connection.close()


def test_token_report_writes_totals_and_the_metric_definition(tmp_path: Path) -> None:
    import json as json_mod

    profiles_root = tmp_path / "openclaw-profiles"
    _write_token_store(profiles_root, "instance-a", aggregate_input=100)
    report_path = tmp_path / "reports" / "report.json"

    result = runner.invoke(
        app,
        ["token-report", "--profiles-dir", str(profiles_root), "--arm", "a-baseline", "--output", str(report_path)],
    )

    assert result.exit_code == 0, result.output
    report = json_mod.loads(report_path.read_text(encoding="utf-8"))
    assert report["arm"] == "a-baseline"
    assert report["totals"]["requests"] == 2
    assert report["totals"]["prompt_tokens"] == 1100
    assert "requests" in report["metric_definition"]["note"]
    assert "requests" in result.output, "prompt volume must never be printed without the round count"


def test_token_report_fails_when_the_two_readings_disagree(tmp_path: Path) -> None:
    """A schema drift must break the run instead of shipping a plausible number."""
    import json as json_mod

    profiles_root = tmp_path / "openclaw-profiles"
    _write_token_store(profiles_root, "instance-a", aggregate_input=999)
    report_path = tmp_path / "report.json"

    result = runner.invoke(
        app,
        ["token-report", "--profiles-dir", str(profiles_root), "--arm", "c-headroom", "--output", str(report_path)],
    )

    assert result.exit_code == 1
    assert "disagrees" in result.output
    report = json_mod.loads(report_path.read_text(encoding="utf-8"))
    assert report["totals"]["instances_with_disagreeing_aggregate"] == ["instance-a"]


def test_token_report_fails_on_a_missing_profiles_root(tmp_path: Path) -> None:
    result = runner.invoke(
        app,
        [
            "token-report",
            "--profiles-dir",
            str(tmp_path / "never-ran"),
            "--arm",
            "a-baseline",
            "--output",
            str(tmp_path / "report.json"),
        ],
    )

    assert result.exit_code == 1
    assert "profiles root not found" in result.output


@pytest.mark.parametrize("field", ["input", "output", "cacheRead"])
@pytest.mark.parametrize("value", [None, "missing"])
def test_token_report_refuses_missing_billing_fields(tmp_path: Path, field: str, value: object) -> None:
    import json
    import sqlite3

    profiles = tmp_path / "profiles"
    _write_token_store(profiles, "case", aggregate_input=100)
    store = profiles / "case" / "agents" / "case" / "agent" / "openclaw-agent.sqlite"
    with sqlite3.connect(store) as connection:
        for table in ("transcript_events", "trajectory_runtime_events"):
            rows = connection.execute(f"SELECT rowid, event_json FROM {table}").fetchall()
            for rowid, raw in rows:
                event = json.loads(raw)
                usage = event["message"]["usage"] if table == "transcript_events" else event["data"]["usage"]
                if value == "missing":
                    usage.pop(field)
                else:
                    usage[field] = value
                connection.execute(f"UPDATE {table} SET event_json=? WHERE rowid=?", (json.dumps(event), rowid))
    output = tmp_path / "report.json"
    result = runner.invoke(app, ["token-report", "--profiles-dir", str(profiles), "--arm", "a", "--output", str(output)])
    assert result.exit_code == 1
    assert "billing field" in result.output
    assert not output.exists()
