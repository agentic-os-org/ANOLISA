"""Tests for the SkillFS daemon notification handler."""

# ruff: noqa: I001

import asyncio
from pathlib import Path
from typing import Any
from unittest.mock import Mock

import pytest
from agent_sec_cli.daemon.errors import BadRequestError
from agent_sec_cli.daemon.handlers.skill_ledger import (
    METHOD_SKILLFS_NOTIFY_CHANGE,
    parse_skillfs_change,
    register_skill_ledger_methods,
    skillfs_notify_change_handler,
)
from agent_sec_cli.daemon.jobs.skill_ledger import SkillLedgerActivationJob
from agent_sec_cli.daemon.jobs.skill_ledger.protocol import SkillFsChange
from agent_sec_cli.daemon.protocol import DaemonRequest
from agent_sec_cli.daemon.registry import MethodRegistry
from agent_sec_cli.daemon.runtime import DaemonRuntime


class FakeWorkerClient:
    """In-process worker client for handler queueing tests."""

    def __init__(self) -> None:
        self.last_error = None
        self.pid = None

    async def process_change(self, change: SkillFsChange) -> dict[str, Any]:
        return {"status": "processed", "skill": change.to_dict()}

    async def stop(self) -> None:
        pass


def make_skill(tmp_path: Path, name: str = "demo-skill") -> Path:
    """Create a minimal skill directory for daemon tests."""
    skill_dir = tmp_path / name
    skill_dir.mkdir()
    (skill_dir / "SKILL.md").write_text("# Demo Skill\n", encoding="utf-8")
    return skill_dir


def request_for(skill_dir: Path, **overrides: Any) -> DaemonRequest:
    """Build a daemon request for SkillFS notify tests."""
    params: dict[str, Any] = {
        "schemaVersion": 2,
        "canonicalSkillDir": str(skill_dir),
        "skillId": f"category/{skill_dir.name}",
        "eventKind": "write",
        "paths": ["SKILL.md"],
    }
    params.update(overrides)
    return DaemonRequest(
        method=METHOD_SKILLFS_NOTIFY_CHANGE,
        params=params,
    )


def test_register_skill_ledger_methods():
    registry = MethodRegistry()

    register_skill_ledger_methods(registry)

    spec = registry.get(METHOD_SKILLFS_NOTIFY_CHANGE)
    assert spec.handler is skillfs_notify_change_handler
    assert spec.queue == "skill_ledger"


def test_parse_skillfs_change_validates_request(tmp_path: Path):
    skill_dir = make_skill(tmp_path, "weather")

    change = parse_skillfs_change(request_for(skill_dir).params)

    assert change.canonical_skill_dir == skill_dir
    assert change.skill_name == "weather"
    assert change.reported_skill_id == "category/weather"
    assert change.event_kinds == {"write"}
    assert change.paths == {"SKILL.md"}


def test_parse_skillfs_change_accepts_reconcile_with_empty_paths(tmp_path: Path):
    skill_dir = make_skill(tmp_path, "weather")

    change = parse_skillfs_change(
        request_for(skill_dir, eventKind="reconcile", paths=[]).params
    )

    assert change.canonical_skill_dir == skill_dir
    assert change.skill_name == "weather"
    assert change.event_kinds == {"reconcile"}
    assert change.paths == set()


@pytest.mark.parametrize(
    ("unknown_field", "value"),
    [
        ("exists", False),
        ("exists", True),
        ("futureField", {"semantics": "unsupported"}),
    ],
)
def test_parse_skillfs_change_rejects_unknown_fields(
    tmp_path: Path,
    unknown_field: str,
    value: Any,
):
    skill_dir = make_skill(tmp_path, "weather")
    params = request_for(skill_dir).params
    params[unknown_field] = value

    with pytest.raises(BadRequestError, match=f"unknown fields: {unknown_field}"):
        parse_skillfs_change(params)


@pytest.mark.parametrize(
    ("overrides", "message"),
    [
        ({"schemaVersion": 1}, "schemaVersion"),
        ({"canonicalSkillDir": "relative-skill"}, "absolute path"),
        ({"canonicalSkillDir": "~/relative-to-home"}, "absolute path"),
        ({"canonicalSkillDir": "//skills/weather"}, "single leading"),
        ({"canonicalSkillDir": "/skills/bad\x00name"}, "must not contain NUL"),
        ({"skillId": None}, "skillId"),
        ({"skillId": ""}, "skillId"),
        ({"skillId": 7}, "skillId"),
        ({"skillId": []}, "skillId"),
        ({"skillId": {}}, "skillId"),
        ({"eventKind": "chmod"}, "eventKind"),
        ({"paths": "/absolute"}, "paths"),
        ({"paths": ["/absolute"]}, "relative paths"),
        ({"paths": ["../escape"]}, "relative paths"),
        ({"paths": ["."]}, "relative paths"),
        ({"paths": ["scripts/run\x00.sh"]}, "NUL"),
    ],
)
def test_parse_skillfs_change_rejects_invalid_params(
    tmp_path: Path,
    overrides: dict[str, Any],
    message: str,
):
    skill_dir = make_skill(tmp_path, "weather")

    with pytest.raises(BadRequestError, match=message):
        parse_skillfs_change(request_for(skill_dir, **overrides).params)


def test_parse_skillfs_change_preserves_opaque_skill_id(
    tmp_path: Path,
):
    skill_dir = tmp_path / "hidden" / "apple" / "notes"

    change = parse_skillfs_change(
        request_for(skill_dir, skillId="reported/id-does-not-match").params
    )

    assert change.canonical_skill_dir == skill_dir
    assert change.reported_skill_id == "reported/id-does-not-match"


def test_parse_skillfs_change_accepts_flat_skill_id(tmp_path: Path):
    skill_dir = tmp_path / "hidden" / "weather"

    change = parse_skillfs_change(request_for(skill_dir, skillId="weather").params)

    assert change.canonical_skill_dir == skill_dir
    assert change.reported_skill_id == "weather"


@pytest.mark.parametrize(
    "field",
    [
        "schemaVersion",
        "canonicalSkillDir",
        "skillId",
        "eventKind",
        "paths",
    ],
)
def test_parse_skillfs_change_rejects_missing_required_fields(
    tmp_path: Path,
    field: str,
):
    skill_dir = tmp_path / "hidden" / "weather"
    params = request_for(skill_dir).params
    params.pop(field)

    with pytest.raises(BadRequestError, match=f"required fields: {field}"):
        parse_skillfs_change(params)


def test_parse_skillfs_change_rejects_non_normalized_canonical(tmp_path: Path):
    skill_dir = make_skill(tmp_path, "weather")
    non_normalized = f"{skill_dir}/../weather"

    with pytest.raises(BadRequestError, match="lexically normalized"):
        parse_skillfs_change(
            request_for(skill_dir, canonicalSkillDir=non_normalized).params
        )


def test_metadata_only_notification_is_accepted_and_ignored(tmp_path: Path):
    skill_dir = make_skill(tmp_path, "weather")
    runtime = DaemonRuntime(socket_path=tmp_path / "daemon.sock")

    response = skillfs_notify_change_handler(
        request_for(skill_dir, paths=[".skill-meta/latest.json"]),
        runtime,
    )

    assert response.data["schemaVersion"] == 2
    assert response.data["accepted"] is True
    assert response.data["ignored"] is True
    assert response.data["reason"] == "metadata-only change"


def test_metadata_only_notification_still_requires_skill_id(tmp_path: Path):
    skill_dir = make_skill(tmp_path, "weather")
    runtime = DaemonRuntime(socket_path=tmp_path / "daemon.sock")
    request = request_for(skill_dir, paths=[".skill-meta/latest.json"])
    request.params.pop("skillId")

    with pytest.raises(BadRequestError, match="skillId"):
        skillfs_notify_change_handler(request, runtime)


@pytest.mark.parametrize(
    ("paths", "unknown_fields"),
    [
        (["SKILL.md"], {"exists": False}),
        (["SKILL.md"], {"exists": True}),
        ([".skill-meta/latest.json"], {"futureField": "unsupported"}),
    ],
)
def test_notify_rejects_unknown_fields_before_ignore_or_enqueue(
    monkeypatch,
    tmp_path: Path,
    paths: list[str],
    unknown_fields: dict[str, Any],
):
    skill_dir = make_skill(tmp_path, "weather")
    runtime = DaemonRuntime(socket_path=tmp_path / "daemon.sock")
    job = SkillLedgerActivationJob()
    enqueue = Mock(return_value=True)
    monkeypatch.setattr(job, "enqueue", enqueue)
    runtime.jobs.register(job)

    with pytest.raises(BadRequestError, match="unknown fields"):
        skillfs_notify_change_handler(
            request_for(skill_dir, paths=paths, **unknown_fields),
            runtime,
        )

    enqueue.assert_not_called()


def test_notify_enqueues_registered_activation_job(monkeypatch, tmp_path: Path):
    skill_dir = make_skill(tmp_path, "weather")
    runtime = DaemonRuntime(socket_path=tmp_path / "daemon.sock")
    job = SkillLedgerActivationJob(
        debounce_seconds=0,
        worker_client=FakeWorkerClient(),
    )
    runtime.jobs.register(job)
    monkeypatch.setattr(
        "agent_sec_cli.daemon.jobs.skill_ledger.activation._resolve_managed_skill_dirs",
        lambda: [],
    )

    async def scenario():
        await job.start()
        try:
            response = skillfs_notify_change_handler(request_for(skill_dir), runtime)
        finally:
            await job.stop()
        return response

    response = asyncio.run(scenario())

    assert response.data["schemaVersion"] == 2
    assert response.data["accepted"] is True
    assert response.data["ignored"] is False
    assert response.data["queued"] is True
    assert response.data["coalesced"] is False
    assert response.data["skill"]["skillName"] == "weather"
    assert response.data["skill"]["canonicalSkillDir"] == str(skill_dir)
    assert response.data["skill"]["reportedSkillId"] == "category/weather"


def test_notify_enqueues_reconcile_with_empty_paths(monkeypatch, tmp_path: Path):
    skill_dir = make_skill(tmp_path, "weather")
    runtime = DaemonRuntime(socket_path=tmp_path / "daemon.sock")
    job = SkillLedgerActivationJob(
        debounce_seconds=0,
        worker_client=FakeWorkerClient(),
    )
    runtime.jobs.register(job)
    monkeypatch.setattr(
        "agent_sec_cli.daemon.jobs.skill_ledger.activation._resolve_managed_skill_dirs",
        lambda: [],
    )

    async def scenario():
        await job.start()
        try:
            response = skillfs_notify_change_handler(
                request_for(skill_dir, eventKind="reconcile", paths=[]),
                runtime,
            )
        finally:
            await job.stop()
        return response

    response = asyncio.run(scenario())

    assert response.data["schemaVersion"] == 2
    assert response.data["accepted"] is True
    assert response.data["ignored"] is False
    assert response.data["queued"] is True
    assert response.data["coalesced"] is False
    assert response.data["skill"]["eventKinds"] == ["reconcile"]
    assert response.data["skill"]["paths"] == []
