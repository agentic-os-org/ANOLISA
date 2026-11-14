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

import pytest
from pytest_mock import MockerFixture

from swe_runner.common.dataset_registry import get_dataset_name as _get_dataset_name
from swe_runner.evaluation import (
    EvalInstanceResult,
    EvalReport,
    _load_normalized_predictions,
    generate_report_text,
    run_evaluation,
    save_report_json,
)

_PRED_I1 = {"instance_id": "i1", "model_name_or_path": "cosh", "model_patch": "diff --git"}
_PRED_I2 = {"instance_id": "i2", "model_name_or_path": "cosh", "model_patch": "diff --git b"}


def test_load_normalized_predictions_valid(tmp_path: Path) -> None:
    preds_file = tmp_path / "preds.json"
    preds_file.write_text(json.dumps({"i1": _PRED_I1, "i2": _PRED_I2}))
    result = _load_normalized_predictions(preds_file)
    assert result == {"i1": _PRED_I1, "i2": _PRED_I2}


def test_load_normalized_predictions_empty_patch_skipped(tmp_path: Path) -> None:
    preds_file = tmp_path / "preds.json"
    preds_file.write_text(
        json.dumps(
            {
                "i1": _PRED_I1,
                "i2": {"instance_id": "i2", "model_name_or_path": "cosh", "model_patch": ""},
            }
        )
    )
    result = _load_normalized_predictions(preds_file)
    assert result == {"i1": _PRED_I1}


def test_load_normalized_predictions_list_format(tmp_path: Path) -> None:
    """swebench 上游明确接受 list[record] 形态的 predictions 文件
    （get_predictions_from_file 同时支持 list 与 dict）——此前对 list 直接
    data.items() 裸崩 AttributeError。
    """
    preds_file = tmp_path / "preds.json"
    preds_file.write_text(
        json.dumps(
            [
                _PRED_I1,
                {"instance_id": "i2", "model_name_or_path": "cosh", "model_patch": ""},
            ]
        )
    )
    result = _load_normalized_predictions(preds_file)
    assert result == {"i1": _PRED_I1}


def test_load_normalized_predictions_skips_null_prediction(tmp_path: Path) -> None:
    """dict 形态里值为 null 的条目按既有空补丁语义跳过，不裸崩。"""
    preds_file = tmp_path / "preds.json"
    preds_file.write_text(json.dumps({"i1": None, "i2": _PRED_I2}))
    result = _load_normalized_predictions(preds_file)
    assert result == {"i2": _PRED_I2}


def test_load_normalized_predictions_diagnoses_malformed_list_entries(tmp_path: Path) -> None:
    """list 形态里的非 dict / 缺 instance_id 条目被逐一诊断后跳过。"""
    preds_file = tmp_path / "preds.json"
    preds_file.write_text(json.dumps([_PRED_I1, None, 42, {"model_patch": "diff"}]))
    result = _load_normalized_predictions(preds_file)
    assert result == {"i1": _PRED_I1}


def test_load_normalized_predictions_rejects_wrong_top_level_type(tmp_path: Path) -> None:
    """既非 dict 也非 list 的顶层形态显式报错，而不是假装跳过成功。"""
    preds_file = tmp_path / "preds.json"
    preds_file.write_text(json.dumps("not a mapping"))
    with pytest.raises(ValueError, match="Predictions must be"):
        _load_normalized_predictions(preds_file)


def test_load_normalized_predictions_jsonl_format(tmp_path: Path) -> None:
    """swebench 的 parser 同样接受 .jsonl（每行一条记录）——标题声称的
    全部形态里这是唯一还缺的。
    """
    preds_file = tmp_path / "preds.jsonl"
    preds_file.write_text(
        json.dumps(_PRED_I1) + "\n"
        + json.dumps({"instance_id": "i2", "model_name_or_path": "cosh", "model_patch": ""}) + "\n"
    )
    result = _load_normalized_predictions(preds_file)
    assert result == {"i1": _PRED_I1}


def test_load_normalized_predictions_missing_file(tmp_path: Path) -> None:
    with pytest.raises(FileNotFoundError):
        _load_normalized_predictions(tmp_path / "nonexistent.json")


def test_load_normalized_predictions_empty_file(tmp_path: Path) -> None:
    preds_file = tmp_path / "preds.json"
    preds_file.write_text("{}")
    result = _load_normalized_predictions(preds_file)
    assert result == {}


def test_get_dataset_name() -> None:
    assert _get_dataset_name("lite") == "SWE-bench/SWE-bench_Lite"
    assert _get_dataset_name("verified") == "SWE-bench/SWE-bench_Verified"
    assert _get_dataset_name("full") == "SWE-bench/SWE-bench"
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
        dataset_name="SWE-bench/SWE-bench_Lite",
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
        dataset_name="SWE-bench/SWE-bench_Lite",
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
    # The harness sees the normalized (here: empty) file, not the original
    normalized = (tmp_path / "output").resolve() / "predictions.normalized.json"
    assert call_kwargs["predictions_path"] == str(normalized)
    assert json.loads(Path(call_kwargs["predictions_path"]).read_text()) == {}


def test_run_evaluation_calls_swebench(tmp_path: Path, mocker: MockerFixture) -> None:
    """run_evaluation hands swebench the normalized JSON subset."""
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
    assert call_kwargs["dataset_name"] == "SWE-bench/SWE-bench_Lite"
    # The normalized file is still JSON and carries exactly the counted data
    assert call_kwargs["predictions_path"].endswith(".json")
    assert json.loads(Path(call_kwargs["predictions_path"]).read_text()) == {
        "i1": {"instance_id": "i1", "model_name_or_path": "cosh", "model_patch": "diff"},
    }
    assert call_kwargs["instance_ids"] == ["i1"]
    assert call_kwargs["max_workers"] == 2
    assert call_kwargs["timeout"] == 600
    assert call_kwargs["namespace"] == "swebench"
    assert call_kwargs["modal"] is False


def test_run_evaluation_gives_harness_the_processed_data(
    tmp_path: Path, mocker: MockerFixture
) -> None:
    """The skip contract must reach the harness input (#4636 P1).

    swebench's parser validates every prediction before applying the
    instance_ids filter, so handing it the original file after skipping a
    null entry makes the harness reject input the wrapper just claimed to
    skip successfully. The harness must receive exactly the processed
    subset, and the user's file must stay untouched.
    """
    original = {"i1": None, "i2": _PRED_I2}
    preds_file = tmp_path / "preds.json"
    preds_file.write_text(json.dumps(original))
    output_dir = tmp_path / "output"

    mock_swebench = mocker.patch("swebench.run_evaluation", return_value=None)

    run_evaluation(preds_file, output_dir, run_id="skip-test")

    mock_swebench.assert_called_once()
    call_kwargs = mock_swebench.call_args[1]
    assert call_kwargs["instance_ids"] == ["i2"]
    harness_data = json.loads(Path(call_kwargs["predictions_path"]).read_text())
    assert harness_data == {"i2": _PRED_I2}
    # the user's file is preserved exactly as written
    assert json.loads(preds_file.read_text()) == original


def test_run_evaluation_accepts_custom_dataset_name(
    tmp_path: Path, mocker: MockerFixture
) -> None:
    """run/dataset.py passes custom subsets straight through; evaluation
    must resolve the same way instead of raising Unknown subset.
    """
    preds_file = tmp_path / "preds.json"
    preds_file.write_text(json.dumps({"i1": _PRED_I1}))
    output_dir = tmp_path / "output"

    mock_swebench = mocker.patch("swebench.run_evaluation", return_value=None)

    run_evaluation(
        preds_file, output_dir, subset="my-org/my-swe-dataset", run_id="custom-test"
    )

    call_kwargs = mock_swebench.call_args[1]
    assert call_kwargs["dataset_name"] == "my-org/my-swe-dataset"


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
    # the skipped entry never reaches the harness input either
    harness_data = json.loads(Path(call_kwargs["predictions_path"]).read_text())
    assert harness_data == {
        "i1": {"instance_id": "i1", "model_name_or_path": "cosh", "model_patch": "diff"},
    }


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
