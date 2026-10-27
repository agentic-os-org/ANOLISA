"""Request-scoped Docker ownership and cleanup, including interrupted workers."""

import json
import re
import subprocess
from pathlib import Path
from typing import Any

from agent_sec_cli.skill_score.contracts import ContractError
from agent_sec_skill_score_runtime.files import atomic_json

OWNER_LABEL = "io.anolisa.skill-score.request"


def validate_identity(request_id: str, project: str) -> None:
    if not re.fullmatch(r"[A-Za-z0-9_-]{8,80}", request_id):
        raise ContractError("invalid worker request ID")
    if not re.fullmatch(r"asc-score-[a-z0-9_-]{8,64}__env", project):
        raise ContractError("invalid worker Compose project")


def docker(command: list[str]) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        ["docker", *command], check=False, capture_output=True, text=True, timeout=15
    )


def inventory(project: str) -> dict[str, list[str]]:
    result = {}
    for kind in ("container", "network", "volume"):
        args = [kind, "ls", "-q"]
        if kind == "container":
            args.append("-a")
        response = docker(
            [*args, "--filter", f"label=com.docker.compose.project={project}"]
        )
        if response.returncode:
            raise ContractError(f"cannot inspect owned Docker {kind}s")
        result[kind] = response.stdout.split()
    return result


def record_ownership(root: Path, request_id: str, project: str) -> None:
    """Persist discovery coordinates before Harbor can create the first resource."""
    validate_identity(request_id, project)
    if any(inventory(project).values()):
        raise ContractError(
            "Compose project is already in use; refusing resource adoption"
        )
    atomic_json(
        root / "ownership.json",
        {
            "schema_version": "agent-sec-skill-score/resources-v1",
            "request_id": request_id,
            "compose_project": project,
            "cleanup_status": "pending",
        },
    )


def cleanup(request_id: str, project: str) -> tuple[str, list[str]]:
    """Delete only resources carrying both this request and Compose project labels."""
    validate_identity(request_id, project)
    errors = []
    try:
        resources = inventory(project)
        for kind, identifiers in resources.items():
            for identifier in identifiers:
                response = docker([kind, "inspect", identifier])
                if response.returncode:
                    errors.append(f"{kind} inspection failed")
                    continue
                values = json.loads(response.stdout)
                value: dict[str, Any] = values[0]
                labels = (
                    (value.get("Config") or {}).get("Labels")
                    if kind == "container"
                    else value.get("Labels")
                )
                if (
                    not labels
                    or labels.get(OWNER_LABEL) != request_id
                    or labels.get("com.docker.compose.project") != project
                ):
                    errors.append(f"{kind} ownership could not be verified")
                    continue
                arguments = [kind, "rm"]
                if kind == "container":
                    arguments.append("--force")
                if docker([*arguments, identifier]).returncode:
                    errors.append(f"{kind} removal failed")
        remaining = inventory(project)
        return ("pending" if any(remaining.values()) else "complete"), errors
    except (
        OSError,
        ValueError,
        IndexError,
        subprocess.TimeoutExpired,
        ContractError,
    ) as exc:
        return "unknown", [
            *errors,
            f"cleanup verification failed: {type(exc).__name__}",
        ]
