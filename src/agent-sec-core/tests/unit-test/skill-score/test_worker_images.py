"""The worker only accepts immutable identities bound to its own image roles."""

import hashlib
import json

import pytest
from agent_sec_cli.skill_score.contracts import ContractError
from agent_sec_skill_score_runtime import images


@pytest.fixture
def installed_images(tmp_path, monkeypatch):
    entries = [
        {
            "role": role,
            "id": "sha256:" + config * 64,
            "manifest_id": "sha256:" + image * 64,
        }
        for role, config, image in (("work", "a", "c"), ("egress", "b", "d"))
    ]
    manifest = {
        "schema": "agent-sec-skill-score/runtime-v1",
        "protocol_version": "agent-sec-skill-score/worker-v1",
        "images": entries,
    }
    data = json.dumps(manifest).encode()
    environment = tmp_path / hashlib.sha256(data).hexdigest()
    environment.mkdir()
    (environment / "runtime-manifest.json").write_bytes(data)
    monkeypatch.setattr(images.sys, "argv", [str(environment / "worker-entry.py")])
    return entries


@pytest.mark.parametrize("store", ["id", "manifest_id"])
def test_worker_accepts_each_store_identity(installed_images, store):
    images.validate_prepared_images(
        {item["role"]: item[store] for item in installed_images}
    )


@pytest.mark.parametrize(
    "failure", ["unknown", "swapped", "mutable", "extra", "missing"]
)
def test_worker_rejects_foreign_or_unbound_image(installed_images, failure):
    actual = {item["role"]: item["manifest_id"] for item in installed_images}
    if failure == "unknown":
        actual["work"] = "sha256:" + "e" * 64
    elif failure == "swapped":
        actual["work"], actual["egress"] = actual["egress"], actual["work"]
    elif failure == "mutable":
        actual["work"] = "agent-sec-skill-score:latest"
    elif failure == "extra":
        actual["extra"] = actual["work"]
    else:
        del actual["work"]
    with pytest.raises(ContractError, match="manifest"):
        images.validate_prepared_images(actual)
