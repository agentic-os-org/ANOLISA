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

"""Per-instance input manifest generation."""

from __future__ import annotations

import json
import logging
from functools import lru_cache
from pathlib import Path
from typing import Any

from swe_runner.agents import PreparedAgentRun
from swe_runner.agents.input_manifest import collect_input_manifest
from swe_runner.run.io.artifacts import RunArtifacts
from swe_runner.run.io.manifest_records import (
    canonical_json_bytes,
    file_record,
    sha256_bytes,
    sha256_text,
    text_record,
)
from swe_runner.run.prompting.prompt_resources import resolve_builtin_skill_path, resolve_custom_prompt_path
from swe_runner.run.workspace.docker_refs import get_docker_image_name
from swe_runner.run.workspace.git import get_git_revision, get_git_status_porcelain

logger = logging.getLogger(__name__)

_INPUT_MANIFESTS_DIR = "input-manifests"
_INPUT_MANIFEST_SCHEMA_VERSION = 1


def _safe_manifest_component(value: str) -> str:
    chars = [char if char.isalnum() or char in "._-" else "-" for char in value]
    safe = "".join(chars).strip("-._")
    return safe or "instance"


@lru_cache(maxsize=1)
def _runner_git_info() -> dict[str, Any]:
    repo_root = Path(__file__).resolve().parents[3]
    status = get_git_status_porcelain(repo_root, timeout=5)
    return {
        "repo_root": str(repo_root),
        "commit": get_git_revision(repo_root, timeout=5),
        "dirty": bool(status),
        "status_porcelain_sha256": sha256_text(status or ""),
    }


def _manifest_path(output_dir: Path, instance_id: str) -> Path:
    return output_dir / _INPUT_MANIFESTS_DIR / _safe_manifest_component(instance_id) / "input_manifest.json"


def build_input_manifest(*, agent_name: str, prepared: PreparedAgentRun, manifest_path: Path) -> dict[str, Any]:
    """Build the input manifest payload for one prepared instance."""
    instance = prepared.instance
    settings = prepared.settings
    instance_payload = instance.model_dump(mode="json")
    agent_settings = settings.agent.model_dump(mode="json")
    dataset_settings = settings.dataset.model_dump(mode="json")
    metadata = dict(prepared.metadata)
    artifacts = RunArtifacts.from_metadata(metadata)
    agent_manifest = collect_input_manifest(prepared, metadata)

    files: dict[str, Any] = {
        "per_case_prompt": file_record(resolve_custom_prompt_path(instance.instance_id, settings.agent.prompts_dir))
        if settings.agent.per_case_prompt
        else {"path": None, "exists": False},
        "builtin_skill": file_record(resolve_builtin_skill_path(settings.agent.skills_dir))
        if settings.agent.use_skill
        else {"path": None, "exists": False},
    }
    files.update(agent_manifest.files)

    manifest: dict[str, Any] = {
        "schema_version": _INPUT_MANIFEST_SCHEMA_VERSION,
        "manifest_path": str(manifest_path),
        "instance_id": instance.instance_id,
        "agent_name": agent_name,
        "dataset": {
            "row_sha256": sha256_bytes(canonical_json_bytes(instance_payload)),
            "repo": instance.repo,
            "version": instance.version,
            "base_commit": instance.base_commit,
            "problem_statement": text_record(instance.problem_statement),
            "reference_patch": text_record(instance.patch),
            "reference_test_patch": text_record(instance.test_patch),
        },
        "settings": {
            "agent": agent_settings,
            "dataset": dataset_settings,
        },
        "runtime": {
            "work_dir": str(prepared.work_dir),
            "base_revision": prepared.base_revision,
            "docker_image_name": artifacts.docker_image_name or get_docker_image_name(instance),
            "timeout": prepared.timeout,
            "max_turns": prepared.max_turns,
        },
        "prompt": text_record(prepared.prompt),
        "files": files,
        "metadata": metadata,
        "runner": _runner_git_info(),
    }
    manifest.update(agent_manifest.sections)
    return manifest


def write_input_manifest(output_dir: Path, *, agent_name: str, prepared: PreparedAgentRun) -> Path:
    """Write ``input-manifests/<instance_id>.json`` and return its path."""
    output_dir.mkdir(parents=True, exist_ok=True)
    path = _manifest_path(output_dir, prepared.instance.instance_id)
    path.parent.mkdir(parents=True, exist_ok=True)
    payload = build_input_manifest(agent_name=agent_name, prepared=prepared, manifest_path=path)
    path.write_text(json.dumps(payload, indent=2, ensure_ascii=False) + "\n", encoding="utf-8")
    logger.info("INPUT_MANIFEST_WRITE instance=%s file=%s", prepared.instance.instance_id, path)
    return path
