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

"""Tests for output writers."""

import json
from pathlib import Path
from tempfile import TemporaryDirectory

from swe_runner.common.models import AgentResult, InstanceResult, Prediction, SWEInstance
from swe_runner.run.io.output_store import RunOutputStore, load_run_metadata
from swe_runner.run.io.run_metadata import RunMetadataSnapshot


def _write_metadata_snapshot(
    output_dir: Path,
    *,
    started_at_ns: int,
    ended_at_ns: int,
    agent_name: str,
    workers: int,
    instance_ids: list[str],
    succeeded: int,
    metadata_mappings: dict[str, dict[str, str]] | None = None,
) -> Path:
    return RunOutputStore(output_dir).write_run_metadata(
        RunMetadataSnapshot(
            started_at_ns=started_at_ns,
            ended_at_ns=ended_at_ns,
            agent_name=agent_name,
            workers=workers,
            instance_ids=instance_ids,
            succeeded=succeeded,
            metadata_mappings=metadata_mappings or {},
        )
    )


class TestWritePrediction:
    """Tests for prediction output storage."""

    def test_write_prediction_creates_file(self) -> None:
        """Test that prediction storage creates preds.json."""
        with TemporaryDirectory() as tmpdir:
            output_dir = Path(tmpdir)
            prediction = Prediction(
                instance_id="test-123",
                model_name_or_path="test-model",
                model_patch="diff --git a/file.py\n",
            )

            RunOutputStore(output_dir).write_prediction(prediction)

            preds_file = output_dir / "preds.json"
            assert preds_file.exists()

            with open(preds_file) as f:
                data = json.load(f)

            assert len(data) == 1
            assert data["test-123"]["instance_id"] == "test-123"
            assert data["test-123"]["model_name_or_path"] == "test-model"
            assert data["test-123"]["model_patch"] == "diff --git a/file.py\n"

    def test_write_prediction_appends(self) -> None:
        """Test that prediction storage appends to preds.json."""
        with TemporaryDirectory() as tmpdir:
            output_dir = Path(tmpdir)

            prediction1 = Prediction(
                instance_id="test-1",
                model_name_or_path="model-a",
                model_patch="patch1",
            )
            prediction2 = Prediction(
                instance_id="test-2",
                model_name_or_path="model-b",
                model_patch="patch2",
            )

            store = RunOutputStore(output_dir)
            store.write_prediction(prediction1)
            store.write_prediction(prediction2)

            preds_file = output_dir / "preds.json"
            with open(preds_file) as f:
                data = json.load(f)

            assert len(data) == 2
            assert "test-1" in data
            assert "test-2" in data

    def test_write_prediction_creates_dir(self) -> None:
        """Test that prediction storage creates output directories if needed."""
        with TemporaryDirectory() as tmpdir:
            output_dir = Path(tmpdir) / "nested" / "output"
            assert not output_dir.exists()

            prediction = Prediction(
                instance_id="test-123",
                model_name_or_path="test-model",
                model_patch="patch",
            )

            RunOutputStore(output_dir).write_prediction(prediction)

            assert output_dir.exists()
            assert (output_dir / "preds.json").exists()


class TestSaveInstanceResult:
    """Tests for instance result output storage."""

    def test_save_instance_result_success(self) -> None:
        """Test saving successful instance result with prediction."""
        with TemporaryDirectory() as tmpdir:
            output_dir = Path(tmpdir)

            instance = SWEInstance(
                instance_id="django-123",
                repo="django/django",
                version="3.2",
                base_commit="abc123",
                problem_statement="Fix the view bug",
                patch="original patch",
                test_patch="test patch",
            )

            prediction = Prediction(
                instance_id="django-123",
                model_name_or_path="gpt-4",
                model_patch="diff --git a/views.py\n",
            )

            agent_result = AgentResult(
                raw_output="Agent output text",
                patch="diff --git a/views.py\n",
                success=True,
                duration_seconds=30.5,
                metadata={
                    "agent_id": "django-123",
                    "session_id": "session-django-123",
                    "openclaw_returncode": "0",
                },
            )

            result = InstanceResult(
                instance=instance,
                prediction=prediction,
                agent_result=agent_result,
                success=True,
            )

            RunOutputStore(output_dir).save_instance_result(result)

            # Check preds.json
            preds_file = output_dir / "preds.json"
            assert preds_file.exists()
            with open(preds_file) as f:
                preds = json.load(f)
            assert len(preds) == 1
            assert preds["django-123"]["instance_id"] == "django-123"

            # Check per-instance result file
            result_file = output_dir / "results" / "django-123.json"
            assert result_file.exists()
            data = json.loads(result_file.read_text())
            assert data["instance_id"] == "django-123"
            assert data["success"] is True
            assert data["patch_produced"] is True
            assert data["error"] is None
            assert data["duration_seconds"] == 30.5
            assert data["agent_name"] == "gpt-4"
            assert data["session_id"] == "session-django-123"
            assert data["openclaw_returncode"] == "0"
            assert data["metadata"] == {
                "agent_id": "django-123",
                "session_id": "session-django-123",
                "openclaw_returncode": "0",
            }

    def test_save_instance_result_failure(self) -> None:
        """Test saving failed instance result without prediction."""
        with TemporaryDirectory() as tmpdir:
            output_dir = Path(tmpdir)

            instance = SWEInstance(
                instance_id="flask-456",
                repo="pallets/flask",
                version="2.0",
                base_commit="def456",
                problem_statement="Fix the route bug",
                patch="original patch",
                test_patch="test patch",
            )

            agent_result = AgentResult(
                raw_output="Agent failed",
                patch=None,
                success=False,
                duration_seconds=10.0,
                error="Timeout",
                metadata={"openclaw_returncode": "124", "openclaw_error_log": "/tmp/openclaw-errors/flask-456.log"},
            )

            result = InstanceResult(
                instance=instance,
                prediction=None,
                agent_result=agent_result,
                success=False,
            )

            RunOutputStore(output_dir).save_instance_result(result)

            # Check preds.json should NOT exist (no prediction)
            preds_file = output_dir / "preds.json"
            assert not preds_file.exists()

            # Per-instance result file should still exist
            result_file = output_dir / "results" / "flask-456.json"
            assert result_file.exists()
            data = json.loads(result_file.read_text())
            assert data["instance_id"] == "flask-456"
            assert data["success"] is False
            assert data["patch_produced"] is False
            assert data["error"] == "Timeout"
            assert data["agent_name"] is None
            assert data["openclaw_returncode"] == "124"
            assert data["openclaw_error_log"] == "/tmp/openclaw-errors/flask-456.log"


def test_concurrent_write_prediction() -> None:
    """Multiple threads writing predictions concurrently should not lose any."""
    import threading

    with TemporaryDirectory() as td:
        output_dir = Path(td)
        errors: list[Exception] = []

        def write_n(n: int) -> None:
            try:
                RunOutputStore(output_dir).write_prediction(
                    Prediction(
                        instance_id=f"inst-{n}",
                        model_name_or_path="test",
                        model_patch=f"diff-{n}",
                    )
                )
            except Exception as e:
                errors.append(e)

        threads = [threading.Thread(target=write_n, args=(i,)) for i in range(10)]
        for t in threads:
            t.start()
        for t in threads:
            t.join()

        assert len(errors) == 0, f"Errors: {errors}"
        preds_file = output_dir / "preds.json"
        data = json.loads(preds_file.read_text())
        assert len(data) == 10, f"Expected 10 predictions, got {len(data)}"


class TestDictFormat:
    """Tests for dict-based preds.json format."""

    def test_write_prediction_dedup(self) -> None:
        with TemporaryDirectory() as tmpdir:
            output_dir = Path(tmpdir)
            pred_v1 = Prediction(instance_id="dedup-1", model_name_or_path="v1", model_patch="patch-v1")
            pred_v2 = Prediction(instance_id="dedup-1", model_name_or_path="v2", model_patch="patch-v2")

            store = RunOutputStore(output_dir)
            store.write_prediction(pred_v1)
            store.write_prediction(pred_v2)

            data = json.loads((output_dir / "preds.json").read_text())
            assert len(data) == 1
            assert data["dedup-1"]["model_name_or_path"] == "v2"


class TestLoadAttemptedInstanceIds:
    """Tests for attempted instance ID loading."""

    def test_empty_when_no_results_dir(self) -> None:
        with TemporaryDirectory() as tmpdir:
            output_dir = Path(tmpdir)
            assert RunOutputStore(output_dir).load_attempted_instance_ids() == set()

    def test_reads_result_files(self) -> None:
        with TemporaryDirectory() as tmpdir:
            output_dir = Path(tmpdir)
            results_dir = output_dir / "results"
            results_dir.mkdir()

            (results_dir / "inst-a.json").write_text(
                json.dumps(
                    {
                        "instance_id": "inst-a",
                        "success": True,
                        "error": None,
                        "duration_seconds": 10.0,
                        "patch_produced": True,
                        "agent_name": "cosh",
                    }
                )
            )
            (results_dir / "inst-b.json").write_text(
                json.dumps(
                    {
                        "instance_id": "inst-b",
                        "success": False,
                        "error": "Timeout",
                        "duration_seconds": 5.0,
                        "patch_produced": False,
                        "agent_name": None,
                    }
                )
            )

            ids = RunOutputStore(output_dir).load_attempted_instance_ids()
            assert ids == {"inst-a", "inst-b"}

    def test_includes_failed_instances(self) -> None:
        """Attempted IDs should include failed (no-patch) instances."""
        with TemporaryDirectory() as tmpdir:
            output_dir = Path(tmpdir)
            instance = SWEInstance(
                instance_id="fail-1",
                repo="r/repo",
                version="1.0",
                base_commit="abc",
                problem_statement="bug",
                patch="",
                test_patch="",
            )
            result = InstanceResult(
                instance=instance,
                prediction=None,
                agent_result=AgentResult(
                    raw_output="fail", patch=None, success=False, duration_seconds=3.0, error="err"
                ),
                success=False,
            )
            RunOutputStore(output_dir).save_instance_result(result)

            attempted = RunOutputStore(output_dir).load_attempted_instance_ids()
            assert "fail-1" in attempted


class TestRunMetadata:
    def test_write_and_load_run_metadata(self) -> None:
        with TemporaryDirectory() as tmpdir:
            output_dir = Path(tmpdir)

            metadata_path = _write_metadata_snapshot(
                output_dir,
                started_at_ns=1_700_000_000_000_000_000,
                ended_at_ns=1_700_000_005_000_000_000,
                agent_name="openclaw",
                workers=4,
                instance_ids=["inst-a", "inst-b"],
                succeeded=1,
                metadata_mappings={
                    "session_ids": {"inst-a": "session-a"},
                    "openclaw_profile_dirs": {"inst-a": "/tmp/profiles/inst-a"},
                },
            )

            assert metadata_path == output_dir / "run_metadata.json"
            payload = load_run_metadata(metadata_path)
            assert payload["started_at_ns"] == 1_700_000_000_000_000_000
            assert payload["ended_at_ns"] == 1_700_000_005_000_000_000
            assert payload["agent_name"] == "openclaw"
            assert payload["workers"] == 4
            assert payload["instance_ids"] == ["inst-a", "inst-b"]
            assert payload["instance_count"] == 2
            assert payload["attempt_count"] == 2
            assert payload["succeeded"] == 1
            assert payload["failed"] == 1
            assert payload["run_count"] == 1
            assert payload["session_ids"] == {"inst-a": "session-a"}
            assert payload["openclaw_profile_dirs"] == {"inst-a": "/tmp/profiles/inst-a"}

    def test_write_run_metadata_merges_existing_runs(self) -> None:
        with TemporaryDirectory() as tmpdir:
            output_dir = Path(tmpdir)

            _write_metadata_snapshot(
                output_dir,
                started_at_ns=10,
                ended_at_ns=20,
                agent_name="openclaw",
                workers=2,
                instance_ids=["inst-a"],
                succeeded=1,
                metadata_mappings={
                    "session_ids": {"inst-a": "session-a"},
                    "openclaw_profile_dirs": {"inst-a": "/tmp/profiles/inst-a"},
                },
            )
            metadata_path = _write_metadata_snapshot(
                output_dir,
                started_at_ns=30,
                ended_at_ns=40,
                agent_name="openclaw",
                workers=4,
                instance_ids=["inst-b"],
                succeeded=0,
                metadata_mappings={
                    "session_ids": {"inst-b": "session-b"},
                    "openclaw_profile_dirs": {"inst-b": "/tmp/profiles/inst-b"},
                },
            )

            payload = load_run_metadata(metadata_path)
            assert payload["started_at_ns"] == 10
            assert payload["ended_at_ns"] == 40
            assert payload["agent_name"] == "openclaw"
            assert payload["workers"] == 4
            assert payload["instance_ids"] == ["inst-a", "inst-b"]
            assert payload["instance_count"] == 2
            assert payload["attempt_count"] == 2
            assert payload["succeeded"] == 1
            assert payload["failed"] == 1
            assert payload["run_count"] == 2
            assert payload["session_ids"] == {"inst-a": "session-a", "inst-b": "session-b"}
            assert payload["openclaw_profile_dirs"] == {
                "inst-a": "/tmp/profiles/inst-a",
                "inst-b": "/tmp/profiles/inst-b",
            }

    def test_write_run_metadata_overwrites_conflicting_instance_keys(self) -> None:
        with TemporaryDirectory() as tmpdir:
            output_dir = Path(tmpdir)

            _write_metadata_snapshot(
                output_dir,
                started_at_ns=10,
                ended_at_ns=20,
                agent_name="openclaw",
                workers=2,
                instance_ids=["inst-a"],
                succeeded=1,
                metadata_mappings={
                    "session_ids": {"inst-a": "session-a-1"},
                    "openclaw_profile_dirs": {"inst-a": "/tmp/profiles/inst-a-1"},
                },
            )
            metadata_path = _write_metadata_snapshot(
                output_dir,
                started_at_ns=30,
                ended_at_ns=40,
                agent_name="openclaw",
                workers=2,
                instance_ids=["inst-a"],
                succeeded=1,
                metadata_mappings={
                    "session_ids": {"inst-a": "session-a-2"},
                    "openclaw_profile_dirs": {"inst-a": "/tmp/profiles/inst-a-2"},
                },
            )

            payload = load_run_metadata(metadata_path)
            assert payload["instance_ids"] == ["inst-a"]
            assert payload["instance_count"] == 1
            assert payload["attempt_count"] == 2
            assert payload["session_ids"] == {"inst-a": "session-a-2"}
            assert payload["openclaw_profile_dirs"] == {"inst-a": "/tmp/profiles/inst-a-2"}

    def test_write_run_metadata_merges_custom_instance_metadata_mappings(self) -> None:
        with TemporaryDirectory() as tmpdir:
            output_dir = Path(tmpdir)

            _write_metadata_snapshot(
                output_dir,
                started_at_ns=10,
                ended_at_ns=20,
                agent_name="future-agent",
                workers=1,
                instance_ids=["inst-a"],
                succeeded=1,
                metadata_mappings={"future_trace_ids": {"inst-a": "trace-a"}},
            )
            metadata_path = _write_metadata_snapshot(
                output_dir,
                started_at_ns=30,
                ended_at_ns=40,
                agent_name="future-agent",
                workers=1,
                instance_ids=["inst-b"],
                succeeded=1,
                metadata_mappings={"future_trace_ids": {"inst-b": "trace-b"}},
            )

            payload = load_run_metadata(metadata_path)
            assert payload["future_trace_ids"] == {"inst-a": "trace-a", "inst-b": "trace-b"}
