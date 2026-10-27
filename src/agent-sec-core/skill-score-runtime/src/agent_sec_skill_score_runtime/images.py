"""Versioned images delivered by the optional Skill Score runtime."""

import hashlib
import json
import os
import re
import sys
from pathlib import Path

from agent_sec_cli.skill_score.contracts import ContractError
from agent_sec_skill_score_runtime.files import read_file


def validate_prepared_images(images: dict[str, str]) -> None:
    """Independently bind the controller's IDs to this worker installation."""
    environment = Path(sys.argv[0]).absolute().parent
    # A candidate and an active worker both bind to their own immutable manifest.
    # The installer writes readiness only after this environment check succeeds.
    manifest_bytes = read_file(environment / "runtime-manifest.json")
    digest = hashlib.sha256(manifest_bytes).hexdigest()
    manifest = json.loads(manifest_bytes)
    if (
        environment.name != digest
        or manifest.get("schema") != "agent-sec-skill-score/runtime-v1"
        or manifest.get("protocol_version") != "agent-sec-skill-score/worker-v1"
    ):
        raise ContractError(
            "worker environment differs from its manifest; repair sec-core-skill-score"
        )
    entries = manifest["images"]
    expected = {item["role"]: (item["id"], item["manifest_id"]) for item in entries}
    if (
        len(entries) != 2
        or set(expected) != {"work", "egress"}
        or set(images) != set(expected)
        or any(
            not re.fullmatch(r"sha256:[a-f0-9]{64}", identity)
            for pair in expected.values()
            for identity in pair
        )
        or any(identity not in expected[role] for role, identity in images.items())
    ):
        raise ContractError("worker image IDs differ from the installed manifest")


def sidecar_image() -> str:
    """Use the validated immutable image selected by the installed runtime manifest."""
    identity = os.environ.get("ASC_SKILL_SCORE_EGRESS_IMAGE", "")
    if not re.fullmatch(r"sha256:[a-f0-9]{64}", identity):
        raise ContractError(
            "validated egress image ID is missing; repair sec-core-skill-score"
        )
    return identity
