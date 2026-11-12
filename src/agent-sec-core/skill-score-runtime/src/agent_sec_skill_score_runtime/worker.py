"""Versioned private JSON protocol for execution, evidence and owned cleanup."""

import json
import math
import os
import re
import signal
import sys
import time
from pathlib import Path
from types import FrameType
from typing import Any

from agent_sec_cli.skill_score.config import ScoreConfig
from agent_sec_cli.skill_score.contracts import ContractError, Probe
from agent_sec_cli.skill_score.evidence.redact import redact_text, redact_value
from agent_sec_skill_score_runtime.files import atomic_json, read_file
from agent_sec_skill_score_runtime.harbor_runtime import HarborRuntime
from agent_sec_skill_score_runtime.images import validate_prepared_images
from agent_sec_skill_score_runtime.resources import (
    cleanup,
    record_ownership,
    validate_identity,
)
from agent_sec_skill_score_runtime.secrets import scrub_artifacts

SCHEMA = "agent-sec-skill-score/worker-v1"


def interrupted(_signal: int, _frame: FrameType | None) -> None:
    # asyncio.run cancels Trial; Harbor then executes its shielded environment cleanup.
    raise KeyboardInterrupt


def execute(request: dict[str, Any], input_path: Path) -> tuple[dict[str, Any], int]:
    response: dict[str, Any] = {
        "schema_version": SCHEMA,
        "request_id": request.get("request_id", ""),
        "result": None,
        "cleanup_status": "unknown",
        "resources": {},
    }
    recorded = False
    project, request_id = "", ""
    root = input_path.parent
    stage = "request"
    code = 0
    try:
        if request.get("schema_version") != SCHEMA:
            raise ContractError("unsupported worker protocol version")
        request_id, project = request["request_id"], request["compose_project"]
        validate_identity(request_id, project)
        response["resources"] = {"compose_project": project}
        root = Path(request.get("root", str(input_path.parent)))
        if root != input_path.parent or not root.is_absolute():
            raise ContractError("worker root must be the private request directory")
        remaining = (float(request["deadline_unix_ms"]) - time.time() * 1000) / 1000
        timeout = min(float(request["timeout"]), remaining)
        if not math.isfinite(timeout) or timeout <= 0:
            raise ContractError("worker request has no remaining execution budget")
        images = request["image_ids"]
        if set(images) != {"work", "egress"} or any(
            not isinstance(value, str)
            or not re.fullmatch(r"sha256:[a-f0-9]{64}", value)
            for value in images.values()
        ):
            raise ContractError("worker requires both verified image IDs")
        config = ScoreConfig.model_validate(request["config"])
        validate_prepared_images(images)
        config.docker_image = images["work"]
        os.environ["ASC_SKILL_SCORE_EGRESS_IMAGE"] = images["egress"]
        # Execution and controller fallback cleanup must address the same daemon.
        os.environ.pop("DOCKER_CONTEXT", None)
        os.environ["DOCKER_HOST"] = "unix:///var/run/docker.sock"
        runtime = HarborRuntime(config, request_id, project)
        stage = "preflight"
        runtime.preflight()
        record_ownership(root, request_id, project)
        recorded = True
        stage = "execution"
        if request["operation"] == "environment":
            response["result"] = runtime.environment()
        elif request["operation"] == "run":
            response["result"] = runtime.run(
                Path(request["skill_path"]),
                Probe.model_validate(request["probe"]),
                root,
                timeout,
            )
        else:
            raise ContractError("unknown worker operation")
    except KeyboardInterrupt:
        response["error"] = {
            "stage": stage,
            "code": "cancelled",
            "message": "Runtime interrupted",
        }
        code = 1
    except Exception as exc:
        response["error"] = {
            "stage": stage,
            "code": type(exc).__name__,
            "message": redact_text(str(exc)),
        }
        code = 1
    finally:
        # A second termination signal must not interrupt the bounded fallback cleanup.
        signal.signal(signal.SIGTERM, signal.SIG_IGN)
        if recorded:
            status, errors = cleanup(request_id, project)
            response["cleanup_status"] = status
            response["cleanup_errors"] = errors
            atomic_json(
                root / "ownership.json",
                {
                    "schema_version": "agent-sec-skill-score/resources-v1",
                    "request_id": request_id,
                    "compose_project": project,
                    "cleanup_status": status,
                },
            )
            if status != "complete":
                code = 1
        elif stage in {"request", "preflight"}:
            response["cleanup_status"] = "complete"
        try:
            scrub_artifacts(input_path.parent)
        except Exception as exc:
            response["error"] = {
                "stage": "evidence",
                "code": "scrub_failed",
                "message": type(exc).__name__,
            }
            response["result"] = None
            code = 1
    return redact_value(response), code


def main() -> None:
    if len(sys.argv) != 3:
        raise SystemExit("worker expects request.json response.json")
    input_path, output_path = map(Path, sys.argv[1:])
    if output_path.parent != input_path.parent or output_path == input_path:
        raise SystemExit(
            "worker response must use a distinct private request-directory path"
        )
    signal.signal(signal.SIGTERM, interrupted)
    request = json.loads(read_file(input_path, 4 * 1024 * 1024))
    if not isinstance(request, dict):
        raise SystemExit("worker request must be an object")
    response, code = execute(request, input_path)
    atomic_json(output_path, response)
    raise SystemExit(code)


if __name__ == "__main__":
    main()
