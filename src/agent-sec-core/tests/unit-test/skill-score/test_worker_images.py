"""The worker only accepts immutable identities bound to its own image roles."""

import hashlib
import json
from pathlib import Path

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
        "schema": "agent-sec-skill-score/runtime-v2",
        "protocol_version": "agent-sec-skill-score/worker-v1",
        "version": "test",
        "source_commit": "a" * 40,
    }
    data = json.dumps(manifest).encode()
    environment = tmp_path / hashlib.sha256(data).hexdigest()
    environment.mkdir()
    (environment / "runtime-manifest.json").write_bytes(data)
    (environment / "image-build.json").write_text(
        json.dumps(
            {
                "schema": "agent-sec-skill-score/image-build-v1",
                "manifest_sha256": environment.name,
                "source_commit": manifest["source_commit"],
                "version": manifest["version"],
                "images": entries,
            }
        )
    )
    monkeypatch.setattr(images.sys, "argv", [str(environment / "worker-entry.py")])
    return entries


@pytest.mark.parametrize("store", ["id", "manifest_id"])
def test_worker_accepts_each_store_identity(installed_images, store):
    images.validate_prepared_images(
        {item["role"]: item[store] for item in installed_images}
    )


def test_worker_accepts_config_only_legacy_receipt(installed_images):
    receipt_path = Path(images.sys.argv[0]).parent / "image-build.json"
    receipt = json.loads(receipt_path.read_text())
    for item in receipt["images"]:
        del item["manifest_id"]
    receipt_path.write_text(json.dumps(receipt))
    images.validate_prepared_images(
        {item["role"]: item["id"] for item in installed_images}
    )


@pytest.mark.parametrize("value", [None, "", "mutable:tag"])
def test_worker_rejects_malformed_present_manifest_identity(installed_images, value):
    receipt_path = Path(images.sys.argv[0]).parent / "image-build.json"
    receipt = json.loads(receipt_path.read_text())
    receipt["images"][0]["manifest_id"] = value
    receipt_path.write_text(json.dumps(receipt))
    with pytest.raises(ContractError, match="build receipt"):
        images.validate_prepared_images(
            {item["role"]: item["id"] for item in installed_images}
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
    with pytest.raises(ContractError, match="build receipt"):
        images.validate_prepared_images(actual)


def test_worker_rejects_build_from_another_source(installed_images):
    receipt_path = Path(images.sys.argv[0]).parent / "image-build.json"
    receipt = json.loads(receipt_path.read_text())
    receipt["source_commit"] = "f" * 40
    receipt_path.write_text(json.dumps(receipt))
    with pytest.raises(ContractError, match="source release"):
        images.validate_prepared_images(
            {item["role"]: item["id"] for item in installed_images}
        )
