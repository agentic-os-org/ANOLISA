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

"""Evaluation service – runner and report generation."""

from __future__ import annotations

import inspect
import json
import logging
import os
import tarfile
from collections.abc import Iterator
from contextlib import contextmanager
from io import BytesIO, StringIO
from pathlib import Path, PurePosixPath
from typing import Any

from rich.console import Console
from rich.table import Table

from swe_runner.common.dataset_registry import DATASET_MAPPING, get_dataset_name
from swe_runner.evaluation.models import EvalReport

logger = logging.getLogger(__name__)


@contextmanager
def _pushd(path: Path) -> Iterator[None]:
    previous_cwd = Path.cwd()
    path.mkdir(parents=True, exist_ok=True)
    os.chdir(path)
    try:
        yield
    finally:
        os.chdir(previous_cwd)


# ---------------------------------------------------------------------------
# Prediction loading
# ---------------------------------------------------------------------------

def _load_normalized_predictions(preds_path: Path) -> dict[str, dict]:
    """Load predictions and return the instance_id-keyed entries worth evaluating.

    swebench's own parser accepts both an instance_id-keyed mapping and a
    list of prediction records, and it validates every entry before the
    instance_ids filter applies — so the wrapper must hand the harness the
    same data it counted, or an entry the wrapper skipped (a null
    prediction, an empty patch) still crashes the harness that just
    reported a successful skip. Normalize the shapes, diagnose every
    skipped entry once, and evaluate the survivors; the caller writes the
    result out and keeps the user's file untouched.
    """
    if not preds_path.exists():
        raise FileNotFoundError(f"Predictions file not found: {preds_path}")

    if preds_path.suffix == ".jsonl":
        # swebench's parser accepts .jsonl records the same way; one
        # JSON object per line.
        with open(preds_path) as f:
            data = [json.loads(line) for line in f if line.strip()]
    else:
        with open(preds_path) as f:
            data = json.load(f)

    if not data:
        return {}

    if isinstance(data, list):
        keyed: dict[str, dict] = {}
        for pred in data:
            if not isinstance(pred, dict):
                logger.warning(
                    "EVAL_SKIP_MALFORMED_PREDICTION type=%s", type(pred).__name__
                )
                continue
            iid = pred.get("instance_id")
            if not iid:
                logger.warning(
                    "EVAL_SKIP_PREDICTION_MISSING_ID keys=%s", sorted(pred)
                )
                continue
            keyed[iid] = pred
        data = keyed
    elif not isinstance(data, dict):
        raise ValueError(
            "Predictions must be a JSON object keyed by instance_id or a "
            f"list of prediction records, got {type(data).__name__}"
        )

    normalized: dict[str, dict] = {}
    for iid, pred in data.items():
        # A null/None entry follows the empty-patch skip path instead of
        # crashing on .get — swebench would reject it just the same.
        if not isinstance(pred, dict) or not pred.get("model_patch", ""):
            logger.warning("EVAL_SKIP_EMPTY_PATCH instance=%s", iid)
            continue
        normalized[iid] = pred
    return normalized


# ---------------------------------------------------------------------------
# Evaluation runner
# ---------------------------------------------------------------------------

def _normalize_tar_owner(tar_info: tarfile.TarInfo) -> tarfile.TarInfo:
    tar_info.uid = 0
    tar_info.gid = 0
    tar_info.uname = "root"
    tar_info.gname = "root"
    return tar_info


def _copy_to_container_normalized_owner(container: Any, src: Path, dst: Path | PurePosixPath) -> None:
    """Copy files into Docker without preserving host UID/GID.

    Rootless Docker can only chown files to mapped IDs. The SWE-bench harness
    archives patches with the host user's UID/GID, which fails on rootless
    daemons when those IDs are not mapped inside the container.
    """
    if os.path.dirname(str(dst)) == "":
        raise ValueError(f"Destination path parent directory cannot be empty!, dst: {dst}")

    with BytesIO() as archive:
        with tarfile.open(fileobj=archive, mode="w") as tar:
            tar.add(src, arcname=dst.name, filter=_normalize_tar_owner)

        container.exec_run(f"mkdir -p {dst.parent}")
        container.put_archive(os.path.dirname(str(dst)), archive.getvalue())


def _install_swebench_rootless_copy_patch() -> None:
    import swebench.harness.docker_utils as docker_utils
    import swebench.harness.run_evaluation as run_evaluation_module

    docker_utils.copy_to_container = _copy_to_container_normalized_owner
    run_evaluation_module.copy_to_container = _copy_to_container_normalized_owner


def _ensure_docker_host_for_rootless_context(
    docker_config: Path | None = None,
    rootless_socket: Path | None = None,
) -> None:
    if os.environ.get("DOCKER_HOST"):
        return

    docker_config = docker_config or Path.home() / ".docker" / "config.json"
    # os.getuid 在原生 Windows 等平台不存在：默认 socket 探测按平台跳过，
    # 而不是 AttributeError 打断每次 evaluate 调用
    if rootless_socket is None:
        if not hasattr(os, "getuid"):
            return
        rootless_socket = Path(f"/run/user/{os.getuid()}/docker.sock")

    try:
        # A docker config is optional and user-owned: it can be missing, be
        # written in the platform code page, or hold a bare ``null``. Probing it
        # must never raise out of evaluation setup, so every decode and shape
        # failure maps to "no rootless context to detect".
        config = json.loads(docker_config.read_text(encoding="utf-8"))
    except (OSError, ValueError):
        return

    if not isinstance(config, dict):
        return
    if config.get("currentContext") != "rootless" or not rootless_socket.exists():
        return

    os.environ["DOCKER_HOST"] = f"unix://{rootless_socket}"
    logger.info("EVAL_DOCKER_HOST_ROOTLESS docker_host=%s", os.environ["DOCKER_HOST"])

def run_evaluation(
    preds_path: Path,
    output_dir: Path,
    *,
    subset: str = "lite",
    split: str = "test",
    workers: int = 4,
    timeout: int = 1800,
    run_id: str = "eval",
    cache_level: str = "env",
    namespace: str | None = "swebench",
) -> None:
    from swebench import run_evaluation as swebench_run_evaluation

    _install_swebench_rootless_copy_patch()
    _ensure_docker_host_for_rootless_context()

    output_dir = output_dir.resolve()
    preds_path = preds_path.resolve()

    # The harness must see exactly the predictions counted here: entries
    # the wrapper skipped (null, empty patch, malformed records) never
    # reach it, or its parser rejects input the wrapper just claimed to
    # skip successfully. The user's file stays untouched; the normalized
    # subset is materialized next to the reports as an audit artifact of
    # what was actually evaluated.
    predictions = _load_normalized_predictions(preds_path)
    instance_ids = list(predictions)
    if not instance_ids:
        logger.warning("EVAL_NO_VALID_PREDICTIONS preds_path=%s", preds_path)

    output_dir.mkdir(parents=True, exist_ok=True)
    normalized_path = output_dir / "predictions.normalized.json"
    normalized_path.write_text(
        json.dumps(predictions, indent=2), encoding="utf-8"
    )

    # run/dataset.py passes custom subset paths straight through; the
    # evaluator must resolve the same way so a dataset the run command
    # accepts can also be evaluated.
    dataset_name = get_dataset_name(subset) if subset in DATASET_MAPPING else subset

    # swebench's entry point is not a stable API: 5.x dropped cache_level,
    # force_rebuild, clean and namespace, and passing any of them raises TypeError
    # before a single instance is evaluated. Filter against the installed signature
    # instead of pinning, and log what was dropped so a silently ignored knob shows
    # up in the run log rather than as an unexplained result difference.
    candidate_kwargs = {
        "dataset_name": dataset_name,
        "split": split,
        "instance_ids": instance_ids,
        "predictions_path": str(normalized_path),
        "max_workers": workers,
        "run_id": run_id,
        "timeout": timeout,
        "cache_level": cache_level,
        "force_rebuild": False,
        "clean": False,
        "open_file_limit": 4096,
        "namespace": namespace,
        "rewrite_reports": False,
        "modal": False,
        "report_dir": str(output_dir),
    }
    accepted = inspect.signature(swebench_run_evaluation).parameters
    if not any(p.kind is inspect.Parameter.VAR_KEYWORD for p in accepted.values()):
        dropped = sorted(set(candidate_kwargs) - set(accepted))
        if dropped:
            logger.warning(
                "EVAL_SWEBENCH_ARGS_DROPPED unsupported_by_installed_swebench=%s",
                ",".join(dropped),
            )
        missing = sorted(
            name
            for name, p in accepted.items()
            if p.default is inspect.Parameter.empty and name not in candidate_kwargs
        )
        if missing:
            raise RuntimeError(
                "installed swebench requires arguments this runner does not supply: "
                f"{', '.join(missing)}"
            )
        candidate_kwargs = {k: v for k, v in candidate_kwargs.items() if k in accepted}

    with _pushd(output_dir):
        swebench_run_evaluation(**candidate_kwargs)


# ---------------------------------------------------------------------------
# Report generation
# ---------------------------------------------------------------------------

def generate_report_text(report: EvalReport) -> str:
    table = Table(title="SWE-bench Evaluation Results")
    table.add_column("Instance ID", style="cyan")
    table.add_column("Patch Applied", justify="center")
    table.add_column("Resolution Status", justify="center")

    for result in report.instance_results:
        patch_str = "Yes" if result.patch_applied else "No"
        status_str = "[green]RESOLVED_FULL[/green]" if result.resolved else "[red]RESOLVED_NO[/red]"
        table.add_row(result.instance_id, patch_str, status_str)

    buf = StringIO()
    console = Console(file=buf, force_terminal=True)
    console.print(table)
    console.print()
    console.print(f"Total: {report.total_instances}")
    console.print(f"Resolved (Full): {report.resolved_full}")
    console.print(f"Resolved (Partial): {report.resolved_partial}")
    console.print(f"Failed: {report.resolved_no}")
    console.print(f"Resolution Rate: {report.resolution_rate:.1%}")
    return buf.getvalue()


def save_report_json(report: EvalReport, output_dir: Path) -> Path:
    output_dir.mkdir(parents=True, exist_ok=True)
    report_path = output_dir / "eval_report.json"
    report_path.write_text(report.model_dump_json(indent=2))
    return report_path
