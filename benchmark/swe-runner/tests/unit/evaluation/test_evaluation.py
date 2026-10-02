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
from contextlib import suppress
from pathlib import Path

from pytest_mock import MockerFixture

from swe_runner.common.dataset_registry import get_dataset_name as _get_dataset_name
from swe_runner.evaluation import (
    EvalInstanceResult,
    EvalReport,
    _get_instance_ids,
    generate_report_text,
    run_evaluation,
    save_report_json,
)


def test_get_instance_ids_valid(tmp_path: Path) -> None:
    preds_file = tmp_path / "preds.json"
    preds_file.write_text(
        json.dumps(
            {
                "i1": {"instance_id": "i1", "model_name_or_path": "cosh", "model_patch": "diff --git"},
                "i2": {"instance_id": "i2", "model_name_or_path": "cosh", "model_patch": "diff --git b"},
            }
        )
    )
    result = _get_instance_ids(preds_file)
    assert result == ["i1", "i2"]


def test_get_instance_ids_empty_patch_skipped(tmp_path: Path) -> None:
    preds_file = tmp_path / "preds.json"
    preds_file.write_text(
        json.dumps(
            {
                "i1": {"instance_id": "i1", "model_name_or_path": "cosh", "model_patch": "diff --git"},
                "i2": {"instance_id": "i2", "model_name_or_path": "cosh", "model_patch": ""},
            }
        )
    )
    result = _get_instance_ids(preds_file)
    assert result == ["i1"]


def test_get_instance_ids_missing_file(tmp_path: Path) -> None:
    import pytest

    with pytest.raises(FileNotFoundError):
        _get_instance_ids(tmp_path / "nonexistent.json")


def test_get_instance_ids_empty_file(tmp_path: Path) -> None:
    preds_file = tmp_path / "preds.json"
    preds_file.write_text("{}")
    result = _get_instance_ids(preds_file)
    assert result == []


def test_get_dataset_name() -> None:
    assert _get_dataset_name("lite") == "princeton-nlp/SWE-bench_Lite"
    assert _get_dataset_name("verified") == "princeton-nlp/SWE-bench_Verified"
    assert _get_dataset_name("full") == "princeton-nlp/SWE-bench"
    assert _get_dataset_name("multilingual") == "SWE-bench/SWE-bench_Multilingual"


def test_get_dataset_name_unknown() -> None:
    import pytest

    with pytest.raises(ValueError, match="Unknown subset"):
        _get_dataset_name("bogus")


def test_save_report_json(tmp_path: Path) -> None:
    report = EvalReport(
        total_instances=1,
        resolved_full=1,
        resolved_partial=0,
        resolved_no=0,
        patch_failed=0,
        error_count=0,
        resolution_rate=1.0,
        instance_results=[
            EvalInstanceResult(
                instance_id="x",
                resolved=True,
                resolution_status="RESOLVED_FULL",
                patch_applied=True,
            )
        ],
        run_id="test-run",
        dataset_name="princeton-nlp/SWE-bench_Lite",
        evaluated_at="2025-01-01T00:00:00+00:00",
    )
    path = save_report_json(report, tmp_path)
    assert path.exists()
    data = json.loads(path.read_text())
    assert data["total_instances"] == 1
    assert data["resolved_full"] == 1
    assert len(data["instance_results"]) == 1
    assert data["instance_results"][0]["instance_id"] == "x"


def test_generate_report_text() -> None:
    report = EvalReport(
        total_instances=2,
        resolved_full=1,
        resolved_partial=0,
        resolved_no=1,
        patch_failed=0,
        error_count=0,
        resolution_rate=0.5,
        instance_results=[
            EvalInstanceResult(
                instance_id="django__django-1",
                resolved=True,
                resolution_status="RESOLVED_FULL",
                patch_applied=True,
            ),
            EvalInstanceResult(
                instance_id="flask__flask-2",
                resolved=False,
                resolution_status="RESOLVED_NO",
                patch_applied=True,
            ),
        ],
        run_id="test",
        dataset_name="princeton-nlp/SWE-bench_Lite",
        evaluated_at="2025-01-01T00:00:00+00:00",
    )
    text = generate_report_text(report)
    assert "django__django-1" in text
    assert "flask__flask-2" in text
    assert "RESOLVED_FULL" in text
    assert "RESOLVED_NO" in text


def test_run_evaluation_empty_predictions(tmp_path: Path, mocker: MockerFixture) -> None:
    preds_file = tmp_path / "preds.json"
    preds_file.write_text("{}")
    output_dir = tmp_path / "output"

    mock_swebench = mocker.patch("swebench.run_evaluation", return_value=None)

    run_evaluation(preds_file, output_dir, run_id="empty-test")

    mock_swebench.assert_called_once()
    call_kwargs = mock_swebench.call_args[1]
    assert call_kwargs["instance_ids"] == []
    assert call_kwargs["predictions_path"] == str(preds_file)


def test_run_evaluation_calls_swebench(tmp_path: Path, mocker: MockerFixture) -> None:
    """Verify run_evaluation passes the JSON path directly to swebench."""
    preds_file = tmp_path / "preds.json"
    preds_file.write_text(
        json.dumps(
            {
                "i1": {"instance_id": "i1", "model_name_or_path": "cosh", "model_patch": "diff"},
            }
        )
    )
    output_dir = tmp_path / "output"

    mock_swebench = mocker.patch("swebench.run_evaluation", return_value=None)

    run_evaluation(preds_file, output_dir, run_id="test-run", workers=2, timeout=600)

    mock_swebench.assert_called_once()
    call_kwargs = mock_swebench.call_args[1]
    assert call_kwargs["dataset_name"] == "princeton-nlp/SWE-bench_Lite"
    # Verify the original JSON path is passed directly (no JSONL conversion)
    assert call_kwargs["predictions_path"] == str(preds_file)
    assert call_kwargs["predictions_path"].endswith(".json")
    assert call_kwargs["instance_ids"] == ["i1"]
    assert call_kwargs["max_workers"] == 2
    assert call_kwargs["timeout"] == 600
    assert call_kwargs["namespace"] == "swebench"
    assert call_kwargs["modal"] is False


def test_run_evaluation_accepts_multilingual_subset(tmp_path: Path, mocker: MockerFixture) -> None:
    preds_file = tmp_path / "preds.json"
    preds_file.write_text(
        json.dumps(
            {
                "apache__druid-13704": {
                    "instance_id": "apache__druid-13704",
                    "model_name_or_path": "cosh",
                    "model_patch": "diff",
                },
            }
        )
    )
    output_dir = tmp_path / "output"

    mock_swebench = mocker.patch("swebench.run_evaluation", return_value=None)

    run_evaluation(preds_file, output_dir, subset="multilingual", run_id="multilingual-test")

    call_kwargs = mock_swebench.call_args[1]
    assert call_kwargs["dataset_name"] == "SWE-bench/SWE-bench_Multilingual"
    assert call_kwargs["split"] == "test"
    assert call_kwargs["instance_ids"] == ["apache__druid-13704"]


def test_run_evaluation_skips_empty_patches(tmp_path: Path, mocker: MockerFixture) -> None:
    """Verify run_evaluation skips instances with empty patches."""
    preds_file = tmp_path / "preds.json"
    preds_file.write_text(
        json.dumps(
            {
                "i1": {"instance_id": "i1", "model_name_or_path": "cosh", "model_patch": "diff"},
                "i2": {"instance_id": "i2", "model_name_or_path": "cosh", "model_patch": ""},
            }
        )
    )
    output_dir = tmp_path / "output"

    mock_swebench = mocker.patch("swebench.run_evaluation", return_value=None)

    run_evaluation(preds_file, output_dir, run_id="filter-test")

    call_kwargs = mock_swebench.call_args[1]
    assert call_kwargs["instance_ids"] == ["i1"]


def test_run_evaluation_handles_swebench_error(tmp_path: Path, mocker: MockerFixture) -> None:
    """Verify run_evaluation handles swebench exceptions gracefully."""
    preds_file = tmp_path / "preds.json"
    preds_file.write_text(
        json.dumps(
            {
                "i1": {"instance_id": "i1", "model_name_or_path": "cosh", "model_patch": "diff"},
            }
        )
    )
    output_dir = tmp_path / "output"

    mocker.patch("swebench.run_evaluation", side_effect=RuntimeError("Docker error"))

    with suppress(RuntimeError):
        run_evaluation(preds_file, output_dir, run_id="error-test")


def test_run_evaluation_accepts_local_build_namespace(tmp_path: Path, mocker: MockerFixture) -> None:
    preds_file = tmp_path / "preds.json"
    preds_file.write_text(
        json.dumps(
            {
                "i1": {"instance_id": "i1", "model_name_or_path": "cosh", "model_patch": "diff"},
            }
        )
    )
    output_dir = tmp_path / "output"

    mock_swebench = mocker.patch("swebench.run_evaluation", return_value=None)

    run_evaluation(preds_file, output_dir, run_id="local-build", namespace=None)

    call_kwargs = mock_swebench.call_args[1]
    assert call_kwargs["namespace"] is None
