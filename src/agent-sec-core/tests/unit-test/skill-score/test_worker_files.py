"""Malformed container artifacts never cause an out-of-bound controller read."""

import hashlib
import json
import os

import pytest
from agent_sec_cli.skill_score.contracts import ContractError
from agent_sec_skill_score_runtime import files, resources
from agent_sec_skill_score_runtime.egress import prepare_egress
from agent_sec_skill_score_runtime.secrets import scrub_artifacts


@pytest.mark.parametrize("kind", ["symlink", "hardlink", "directory-link", "fifo"])
def test_snapshot_rejects_unsafe_artifacts(tmp_path, kind):
    root = tmp_path / "source"
    root.mkdir()
    secret = tmp_path / "unrelated"
    secret.write_text("must not leave its file")
    if kind == "symlink":
        (root / "artifact").symlink_to(secret)
    elif kind == "hardlink":
        os.link(secret, root / "artifact")
    elif kind == "directory-link":
        (root / "artifact").symlink_to(tmp_path, target_is_directory=True)
    else:
        os.mkfifo(root / "artifact")
    with pytest.raises((ContractError, OSError)):
        files.snapshot_tree(root, tmp_path / "copy")
    assert secret.read_text() == "must not leave its file"


def test_snapshot_hashes_private_regular_copy_and_enforces_limits(
    tmp_path, monkeypatch
):
    root = tmp_path / "source"
    root.mkdir()
    (root / "events.json").write_text('{"event":"observed"}')
    manifest = files.snapshot_tree(root, tmp_path / "copy")
    assert manifest == [
        {
            "path": "events.json",
            "size": 20,
            "sha256": hashlib.sha256(b'{"event":"observed"}').hexdigest(),
        }
    ]
    assert (tmp_path / "copy/events.json").stat().st_mode & 0o777 == 0o600
    monkeypatch.setattr(files, "TREE_LIMIT", 1)
    with pytest.raises(ContractError, match="aggregate"):
        files.snapshot_tree(root, tmp_path / "oversized")


def test_bounded_enumeration_and_no_follow_ancestors(tmp_path, monkeypatch):
    root = tmp_path / "source"
    root.mkdir()
    (root / "a").touch()
    (root / "b").touch()
    monkeypatch.setattr(files, "ENTRY_LIMIT", 1)
    with pytest.raises(ContractError, match="entry limit"):
        files.snapshot_tree(root, tmp_path / "copy")
    (tmp_path / "alias").symlink_to(root, target_is_directory=True)
    with pytest.raises(OSError):
        files.read_file(tmp_path / "alias/a")


def test_scrub_does_not_follow_links_and_removes_actual_resolved_keys(
    tmp_path, monkeypatch
):
    monkeypatch.setenv("ANTHROPIC_API_KEY", "exact-test-secret")
    (tmp_path / "log").write_text("exact-test-secret\x00binary")
    scrub_artifacts(tmp_path)
    assert (tmp_path / "log").read_bytes() == b"[REDACTED]\x00binary"
    (tmp_path / "link").symlink_to(tmp_path / "log")
    with pytest.raises(OSError):
        scrub_artifacts(tmp_path)


def test_secret_in_artifact_name_cannot_be_published(tmp_path, monkeypatch):
    monkeypatch.setenv("ANTHROPIC_API_KEY", "exact-test-secret")
    (tmp_path / "exact-test-secret.txt").write_text("no secret in content")
    with pytest.raises(ContractError, match="filename"):
        scrub_artifacts(tmp_path)


def test_resource_labels_cover_work_sidecar_and_network(tmp_path, monkeypatch):
    monkeypatch.setenv("ASC_SKILL_SCORE_EGRESS_IMAGE", "sha256:" + "b" * 64)
    prepare_egress(tmp_path / "task", "example.com", ["192.0.2.1"], "request-12345")
    compose = json.loads(
        (tmp_path / "task/environment/docker-compose.yaml").read_text()
    )
    for service in compose["services"].values():
        assert service["labels"][resources.OWNER_LABEL] == "request-12345"
    assert (
        compose["networks"]["default"]["labels"][resources.OWNER_LABEL]
        == "request-12345"
    )


def test_cleanup_does_not_remove_foreign_resource(monkeypatch):
    project = "asc-score-12345678__env"
    monkeypatch.setattr(
        resources,
        "inventory",
        lambda _: {"container": ["foreign"], "network": [], "volume": []},
    )
    calls = []

    def docker(args):
        calls.append(args)
        return resources.subprocess.CompletedProcess(
            args,
            0,
            json.dumps(
                [
                    {
                        "Config": {
                            "Labels": {
                                "com.docker.compose.project": project,
                                resources.OWNER_LABEL: "another-request",
                            }
                        }
                    }
                ]
            ),
            "",
        )

    monkeypatch.setattr(resources, "docker", docker)
    status, errors = resources.cleanup("request-12345", project)
    assert status == "pending" and errors
    assert all("rm" not in call for call in calls)


def test_owner_record_exists_before_execution_and_rejects_collision(
    tmp_path, monkeypatch
):
    monkeypatch.setattr(
        resources, "inventory", lambda _: {"container": [], "network": [], "volume": []}
    )
    resources.record_ownership(tmp_path, "request-12345", "asc-score-12345678__env")
    assert (
        json.loads((tmp_path / "ownership.json").read_text())["cleanup_status"]
        == "pending"
    )
    monkeypatch.setattr(resources, "inventory", lambda _: {"container": ["existing"]})
    with pytest.raises(ContractError, match="adoption"):
        resources.record_ownership(tmp_path, "request-12345", "asc-score-12345678__env")
