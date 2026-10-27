"""Opt-in real Harbor upload and source verification; no Agent or model request."""

# ruff: noqa: E402 -- the optional Harbor runtime is required for this integration.

import asyncio
import json
import os
import re
import shlex
import subprocess
import uuid
from pathlib import Path

import pytest

pytest.importorskip("harbor")

from agent_sec_cli.skill_score.contracts import ContractError
from agent_sec_skill_score_runtime import resources, subject
from agent_sec_skill_score_runtime.harbor_environment import (
    ScoreDockerEnvironment,
)
from harbor.agents.installed.claude_code import ClaudeCode
from harbor.models.agent.context import AgentContext
from harbor.models.task.config import EnvironmentConfig
from harbor.models.trial.paths import TrialPaths


@pytest.mark.skipif(
    not os.environ.get("ASC_SCORE_UPLOAD_TEST_IMAGE"),
    reason="Requires an explicitly supplied local immutable work image on Linux",
)
def test_real_harbor_canonical_source_upload(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    image = os.environ["ASC_SCORE_UPLOAD_TEST_IMAGE"]
    assert re.fullmatch(r"sha256:[0-9a-f]{64}", image)
    monkeypatch.setenv("DOCKER_HOST", "unix:///var/run/docker.sock")
    monkeypatch.delenv("DOCKER_CONTEXT", raising=False)
    for name in (
        "ANTHROPIC_API_KEY",
        "ANTHROPIC_AUTH_TOKEN",
        "AGENT_SEC_MODEL_API_KEY",
    ):
        monkeypatch.delenv(name, raising=False)
    inspected = subprocess.run(
        ["docker", "image", "inspect", image],
        check=True,
        capture_output=True,
        text=True,
        timeout=15,
    )
    metadata = json.loads(inspected.stdout)[0]
    assert (metadata["Id"], metadata["Os"], metadata["Architecture"]) == (
        image,
        "linux",
        "amd64",
    )
    source = tmp_path / "source"
    (source / "scripts").mkdir(parents=True)
    (source / "SKILL.md").write_text(
        "# Receipt\nRun scripts/run.py from this Skill directory.\n"
    )
    (source / "scripts/run.py").write_text(
        "#!/usr/bin/env python3\nprint('canonical-receipt')\n"
    )
    (source / "scripts/run.py").chmod(0o711)
    request = uuid.uuid4().hex
    project = "asc-score-" + request + "__env"
    environment_dir = tmp_path / "environment"
    environment_dir.mkdir()
    (environment_dir / "docker-compose.yaml").write_text(
        json.dumps(
            {
                "services": {
                    "main": {
                        "image": image,
                        "pull_policy": "never",
                        "network_mode": "none",
                        "labels": {resources.OWNER_LABEL: request},
                    }
                },
                "networks": {"default": {"labels": {resources.OWNER_LABEL: request}}},
            }
        )
    )
    paths = TrialPaths(tmp_path / "trial")
    paths.mkdir()
    staging = tmp_path / "container-logs"
    staging.mkdir()
    environment = ScoreDockerEnvironment(
        log_staging=str(staging),
        mounts=[
            {"type": "bind", "source": str(path), "target": f"/logs/{name}"}
            for name, path in [
                ("agent", paths.agent_dir),
                ("verifier", paths.verifier_dir),
                ("artifacts", paths.artifacts_dir),
            ]
        ],
        environment_dir=environment_dir,
        environment_name="canonical-source-test",
        session_id=project,
        trial_paths=paths,
        task_env_config=EnvironmentConfig(
            docker_image=image, workdir="/app/work", network_mode="public"
        ),
    )
    resources.record_ownership(tmp_path, request, project)

    async def run() -> None:
        try:
            await environment.start(force_build=False)
            # As in production, Compose owns networking; Harbor's policy sidecar is disabled.
            containers = resources.inventory(project)["container"]
            assert len(containers) == 1
            running = subprocess.run(
                ["docker", "inspect", containers[0]],
                check=True,
                capture_output=True,
                text=True,
                timeout=15,
            )
            inspected = json.loads(running.stdout)[0]
            assert inspected["HostConfig"]["NetworkMode"] == "none"
            assert {
                m["Destination"]: m["Source"] for m in inspected["Mounts"] if m["RW"]
            } == {
                f"/logs/{role}": str(staging / role)
                for role in ("agent", "verifier", "artifacts")
            }
            sentinel = tmp_path / "host-only"
            sentinel.write_text("untouched")
            session = json.dumps(
                {"type": "user", "message": {"role": "user", "content": "receipt"}}
            )
            planted = await environment.exec(
                "mkdir -p /logs/agent/sessions/projects/demo && "
                f"printf '%s\\n' {shlex.quote(session)} > /logs/agent/sessions/projects/demo/session.jsonl && "
                f"ln -s {shlex.quote(str(sentinel))} /logs/agent/trajectory.json",
                timeout_sec=15,
            )
            assert planted.return_code == 0
            await environment.download_dir("/logs/agent", paths.agent_dir)
            ClaudeCode(logs_dir=paths.agent_dir).populate_context_post_run(
                AgentContext()
            )
            assert sentinel.read_text() == "untouched"
            assert (
                json.loads((paths.agent_dir / "trajectory.json").read_text())["steps"][
                    0
                ]["message"]
                == "receipt"
            )
            installed = await subject.install_subject(environment, source)
            assert (
                installed.binding["content_hash"]
                == subject.source_identity(source)["content_hash"]
            )
            executed = await environment.exec(
                "cat /skills/subject/SKILL.md >/dev/null && /skills/subject/scripts/run.py",
                timeout_sec=15,
            )
            assert (
                executed.return_code == 0
                and executed.stdout.strip() == "canonical-receipt"
            )
            after = await subject.recheck_subject(environment, installed)
            assert after["verified"] and after["additional_entries"] == 0
            (tmp_path / "source-binding.json").write_text(
                json.dumps(
                    {
                        "before": installed.binding,
                        "after": after,
                        "stdout": executed.stdout,
                    }
                )
            )
            changed = await environment.exec(
                "printf '\n# changed\n' >> /skills/subject/scripts/run.py",
                timeout_sec=15,
            )
            assert changed.return_code == 0
            with pytest.raises(
                ContractError, match="modified or could not be verified"
            ):
                await subject.recheck_subject(environment, installed)
        finally:
            try:
                await environment.stop(delete=False)
            finally:
                status, errors = resources.cleanup(request, project)
                assert status == "complete", errors
                assert not any(resources.inventory(project).values())

    asyncio.run(asyncio.wait_for(run(), timeout=150))
