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

import json
import logging
import os
import tarfile
from collections.abc import Iterator
from contextlib import contextmanager
from io import StringIO
from pathlib import Path, PurePosixPath
from typing import Any

from rich.console import Console
from rich.table import Table

from swe_runner.common.dataset_registry import get_dataset_name
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
# Instance-id helpers
# ---------------------------------------------------------------------------

def _get_instance_ids(preds_path: Path) -> list[str]:
    """Read a predictions JSON file and return instance IDs with non-empty patches."""
    if not preds_path.exists():
        raise FileNotFoundError(f"Predictions file not found: {preds_path}")

    with open(preds_path) as f:
        data = json.load(f)

    if not data:
        return []

    instance_ids: list[str] = []
    for iid, pred in data.items():
        if not pred.get("model_patch", ""):
            logger.warning("EVAL_SKIP_EMPTY_PATCH instance=%s", iid)
            continue
        instance_ids.append(iid)
    return instance_ids


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

    tar_path = src.with_suffix(".tar")
    try:
        with tarfile.open(tar_path, "w") as tar:
            tar.add(src, arcname=dst.name, filter=_normalize_tar_owner)

        data = tar_path.read_bytes()
        container.exec_run(f"mkdir -p {dst.parent}")
        container.put_archive(os.path.dirname(str(dst)), data)
    finally:
        tar_path.unlink(missing_ok=True)


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
    rootless_socket = rootless_socket or Path(f"/run/user/{os.getuid()}/docker.sock")

    try:
        config = json.loads(docker_config.read_text())
    except (FileNotFoundError, json.JSONDecodeError, OSError):
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

    instance_ids = _get_instance_ids(preds_path)
    if not instance_ids:
        logger.warning("EVAL_NO_VALID_PREDICTIONS preds_path=%s", preds_path)

    dataset_name = get_dataset_name(subset)

    output_dir = output_dir.resolve()
    preds_path = preds_path.resolve()
    with _pushd(output_dir):
        swebench_run_evaluation(
            dataset_name=dataset_name,
            split=split,
            instance_ids=instance_ids,
            predictions_path=str(preds_path),
            max_workers=workers,
            run_id=run_id,
            timeout=timeout,
            cache_level=cache_level,
            force_rebuild=False,
            clean=False,
            open_file_limit=4096,
            namespace=namespace,
            rewrite_reports=False,
            modal=False,
            report_dir=str(output_dir),
        )


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
