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

"""Adapter for OpenClaw local runs with one profile per SWE-bench instance."""

from __future__ import annotations

import logging
import shutil
import threading
from pathlib import Path

from swe_runner.agents import AgentAdapter, PreparedAgentRun, register_agent
from swe_runner.agents.openclaw.artifacts import OpenClawArtifacts
from swe_runner.agents.openclaw.client import OpenClawClient
from swe_runner.agents.openclaw.identifiers import (
    build_openclaw_agent_id,
    build_openclaw_session_id,
    safe_session_component,
)
from swe_runner.agents.openclaw.profile import OpenClawCaseProfileManager
from swe_runner.agents.openclaw.prompts import build_openclaw_agents_text, build_openclaw_prompt
from swe_runner.agents.openclaw.sandbox import (
    OpenClawSandboxManager,
    OpenClawSandboxSpec,
)
from swe_runner.agents.openclaw.tokenless_evidence import write_tokenless_evidence
from swe_runner.common.models import AgentResult, Settings, SWEInstance
from swe_runner.run.io.artifacts import RunArtifacts, merge_metadata
from swe_runner.run.prompting.prompt_resources import load_optional_builtin_skill_text
from swe_runner.run.workspace.docker import (
    prepare_workspace_from_image,
)
from swe_runner.run.workspace.docker_refs import default_workspace_root, get_docker_image_name
from swe_runner.run.workspace.git import get_git_revision

logger = logging.getLogger(__name__)
_DEFAULT_LOCAL_AGENT_ID = "swebench"


def clean_openclaw_workspace_root(workspace_root: Path) -> None:
    """Remove stale OpenClaw workspace state before preparing an instance."""
    if workspace_root.exists():
        shutil.rmtree(workspace_root, ignore_errors=True)


def _agent_id_from_model(model: str) -> str:
    if model and model != "openclaw":
        if "/" in model:
            return model.rsplit("/", 1)[-1]
        return model
    return _DEFAULT_LOCAL_AGENT_ID


def _write_openclaw_error_log(output_dir: Path, instance_id: str, session_id: str, raw_output: str) -> Path | None:
    error_dir = output_dir / "openclaw-errors"
    error_path = error_dir / f"{safe_session_component(instance_id)}.log"
    try:
        error_dir.mkdir(parents=True, exist_ok=True)
        error_path.write_text(
            f"instance_id={instance_id}\nsession_id={session_id}\n\n{raw_output}",
            encoding="utf-8",
        )
    except OSError:
        logger.exception("OPENCLAW_ERROR_LOG_WRITE_FAILED instance=%s file=%s", instance_id, error_path)
        return None
    return error_path


def _string_bool(value: bool) -> str:
    return "true" if value else "false"


def _resolve_openclaw_agents_text(settings: Settings) -> tuple[str, str | None]:
    if settings.agent.use_skill and settings.agent.per_case_prompt:
        raise RuntimeError("OpenClaw supports only one prompt guidance mode: --use-skill or --per-case-prompt")
    if settings.agent.use_skill:
        skill_text = load_optional_builtin_skill_text(skills_dir=settings.agent.skills_dir)
        if skill_text:
            return "skill", build_openclaw_agents_text(skill_text=skill_text)
        return "skill-unavailable", build_openclaw_agents_text()
    if settings.agent.per_case_prompt:
        return "per-case-prompt", build_openclaw_agents_text()
    return "common", build_openclaw_agents_text()


class OpenClawAdapter(AgentAdapter):
    """Run OpenClaw locally with one isolated profile per instance."""

    def __init__(
        self,
        *,
        model: str = "openclaw",
        base_config_path: Path | None = None,
        cli_path: str = "openclaw",
        profile_link_root: Path | None = None,
    ) -> None:
        self._agent_id = _agent_id_from_model(model)
        self._base_config_path = base_config_path
        self._cli_path = cli_path
        self._profile_link_root = profile_link_root

    @property
    def name(self) -> str:
        return "openclaw"

    @property
    def agent_id(self) -> str:
        return self._agent_id

    def prepare(self, instance: SWEInstance, settings: Settings) -> PreparedAgentRun:
        image_name = get_docker_image_name(instance)
        workspace_root = default_workspace_root(instance.instance_id)
        openclaw_workspace_root = workspace_root / "openclaw-workspace"
        clean_openclaw_workspace_root(workspace_root)
        work_dir = prepare_workspace_from_image(
            image_name,
            instance_id=instance.instance_id,
            work_dir=workspace_root / "repo",
            pull_registry=settings.agent.docker_pull_registry,
        )
        base_revision = get_git_revision(work_dir)
        prompt = build_openclaw_prompt(
            instance,
            use_per_case_prompt=settings.agent.per_case_prompt,
            prompts_dir=settings.agent.prompts_dir,
        )
        injection_mode, agents_text = _resolve_openclaw_agents_text(settings)

        profile_manager = OpenClawCaseProfileManager(
            output_dir=settings.output.output_dir,
            base_config_path=self._base_config_path,
            profile_link_root=self._profile_link_root,
        )
        profile = profile_manager.prepare(instance.instance_id)
        runtime_agent_id = build_openclaw_agent_id(instance.instance_id)

        sandbox_manager = OpenClawSandboxManager(
            config_path=profile.config_path,
            profile=profile.name,
            cli_path=self._cli_path,
            tokenless=settings.agent.tokenless,
        )
        sandbox_manager.configure(
            OpenClawSandboxSpec(
                agent_id=runtime_agent_id,
                image_name=image_name,
                workspace_root=openclaw_workspace_root,
                testbed_dir=work_dir,
                agents_text=agents_text,
            )
        )
        session_id = build_openclaw_session_id(instance.instance_id)

        def remove_profile_link() -> None:
            profile_manager.cleanup_link(profile)

        def remove_sandbox() -> None:
            sandbox_manager.remove_agent_containers(runtime_agent_id)

        def remove_workspace() -> None:
            if workspace_root.exists():
                shutil.rmtree(workspace_root, ignore_errors=True)

        run_artifacts = RunArtifacts(
            agent_id=runtime_agent_id,
            docker_image_name=image_name,
            session_id=session_id,
        )
        openclaw_artifacts = OpenClawArtifacts(
            base_agent_id=self._agent_id,
            openclaw_profile=profile.name,
            openclaw_profile_dir=str(profile.directory),
            openclaw_config_path=str(profile.config_path),
            openclaw_workspace_root=str(openclaw_workspace_root),
            openclaw_injection_mode=injection_mode,
            openclaw_tokenless_requested=_string_bool(settings.agent.tokenless),
        )
        if agents_text is not None:
            openclaw_artifacts = openclaw_artifacts.with_updates(
                openclaw_agents_path=str(openclaw_workspace_root / "AGENTS.md")
            )

        return PreparedAgentRun(
            instance=instance,
            settings=settings,
            work_dir=work_dir,
            prompt=prompt,
            timeout=settings.agent.timeout,
            max_turns=settings.agent.step_limit,
            base_revision=base_revision,
            metadata=merge_metadata(run_artifacts.to_metadata(), openclaw_artifacts.to_metadata()),
            cleanup_callbacks=[remove_workspace, remove_profile_link, remove_sandbox],
        )

    def run(self, prepared: PreparedAgentRun) -> AgentResult:
        prompt = prepared.prompt
        instance_id = prepared.instance.instance_id
        timeout = prepared.timeout
        max_turns = prepared.max_turns
        prepared_run_artifacts = RunArtifacts.from_metadata(prepared.metadata)
        prepared_openclaw_artifacts = OpenClawArtifacts.from_metadata(prepared.metadata)
        effective_agent_id = prepared_run_artifacts.agent_id or self._agent_id
        profile_name = prepared_openclaw_artifacts.openclaw_profile
        if profile_name is None:
            raise RuntimeError("OpenClaw prepared run is missing openclaw_profile")
        session_id = prepared_run_artifacts.session_id or build_openclaw_session_id(instance_id)
        thread_name = threading.current_thread().name

        logger.info(
            "OPENCLAW_REQUEST_START instance=%s session=%s profile=%s thread=%s agent_id=%s timeout=%ss step_limit_ignored=%s",
            instance_id,
            session_id,
            profile_name,
            thread_name,
            effective_agent_id,
            timeout,
            max_turns,
        )

        client = OpenClawClient(
            profile=profile_name,
            agent_id=effective_agent_id,
            cli_path=self._cli_path,
        )
        outcome = client.run_prompt(
            prompt,
            session_id=session_id,
            timeout=timeout,
            max_steps=max_turns,
        )

        logger.info(
            "OPENCLAW_REQUEST_END instance=%s session=%s profile=%s thread=%s returncode=%s duration=%.2fs",
            instance_id,
            session_id,
            profile_name,
            thread_name,
            outcome.returncode,
            outcome.duration_seconds,
        )

        metadata = RunArtifacts(
            agent_id=str(effective_agent_id),
            session_id=str(session_id),
        ).to_metadata()
        metadata = merge_metadata(
            metadata,
            OpenClawArtifacts(
                openclaw_profile=str(profile_name),
                openclaw_returncode=str(outcome.returncode),
            ).to_metadata(),
        )
        error = outcome.error
        if outcome.returncode != 0:
            error_log = _write_openclaw_error_log(
                prepared.settings.output.output_dir,
                instance_id,
                str(session_id),
                outcome.raw_output,
            )
            if error_log is not None:
                metadata["openclaw_error_log"] = str(error_log)
                error = f"OpenClaw local exited with return code {outcome.returncode}; see {error_log}"
            logger.error(
                "OPENCLAW_REQUEST_FAILED instance=%s session=%s profile=%s returncode=%s error_log=%s",
                instance_id,
                session_id,
                profile_name,
                outcome.returncode,
                error_log,
            )

        if prepared_openclaw_artifacts.openclaw_tokenless_requested == "true":
            try:
                metadata.update(
                    write_tokenless_evidence(
                        output_dir=prepared.settings.output.output_dir,
                        instance_id=instance_id,
                        metadata={**prepared.metadata, **metadata},
                        raw_output=outcome.raw_output,
                    )
                )
            except Exception:
                logger.exception("OPENCLAW_TOKENLESS_EVIDENCE_FAILED instance=%s", instance_id)
                metadata["openclaw_tokenless_evidence_error"] = "failed"

        return AgentResult(
            raw_output=outcome.raw_output,
            patch=None,
            success=outcome.returncode == 0,
            duration_seconds=outcome.duration_seconds,
            error=error,
            metadata=metadata,
        )


register_agent("openclaw", OpenClawAdapter)
