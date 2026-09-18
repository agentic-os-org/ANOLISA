"""Tests for Skill Ledger activation daemon integration."""

# ruff: noqa: I001

import asyncio
from pathlib import Path
from typing import Any

import pytest
from agent_sec_cli.daemon.jobs.skill_ledger import (
    SKILL_LEDGER_ACTIVATION_JOB,
    SkillLedgerActivationJob,
)
from agent_sec_cli.daemon.jobs.skill_ledger.processor import (
    process_skill_change,
)
from agent_sec_cli.daemon.jobs.skill_ledger.protocol import SkillFsChange
from agent_sec_cli.daemon.jobs.skill_ledger.worker_client import (
    SkillLedgerWorkerTransportError,
)
from agent_sec_cli.skill_ledger.core.live_root import ResolvedSkillRoot
from agent_sec_cli.skill_ledger.errors import SkillRootResolveError


class FakeWorkerClient:
    """In-process worker client for activation scheduling tests."""

    def __init__(self, process=None):
        self._process = process or (
            lambda change: {"status": "processed", "skill": change.to_dict()}
        )
        self.last_error = None
        self.pid = None
        self.stopped = False

    async def process_change(self, change: SkillFsChange) -> dict[str, Any]:
        return self._process(change)

    async def stop(self) -> None:
        self.stopped = True


def make_skill(tmp_path: Path, name: str = "demo-skill") -> Path:
    """Create a minimal skill directory for daemon tests."""
    skill_dir = tmp_path / name
    skill_dir.mkdir()
    (skill_dir / "SKILL.md").write_text("# Demo Skill\n", encoding="utf-8")
    return skill_dir


def test_activation_job_debounces_same_skill(monkeypatch, tmp_path: Path):
    skill_dir = make_skill(tmp_path, "weather")
    calls = []

    monkeypatch.setattr(
        "agent_sec_cli.daemon.jobs.skill_ledger.activation._resolve_managed_skill_dirs",
        lambda: [],
    )

    def fake_process(change: SkillFsChange) -> dict[str, Any]:
        calls.append(change)
        return {"status": "processed", "skill": change.to_dict()}

    async def scenario():
        job = SkillLedgerActivationJob(
            debounce_seconds=0.05,
            worker_client=FakeWorkerClient(fake_process),
        )
        await job.start()
        try:
            job.enqueue(
                SkillFsChange(
                    canonical_skill_dir=skill_dir.resolve(),
                    event_kinds={"write"},
                    paths={"SKILL.md"},
                )
            )
            job.enqueue(
                SkillFsChange(
                    canonical_skill_dir=skill_dir.resolve(),
                    event_kinds={"rename"},
                    paths={"scripts/run.sh"},
                )
            )
            job.enqueue(
                SkillFsChange(
                    canonical_skill_dir=skill_dir.resolve(),
                    event_kinds={"reconcile"},
                    paths=set(),
                )
            )
            deadline = asyncio.get_running_loop().time() + 1.0
            while len(calls) < 1 and asyncio.get_running_loop().time() < deadline:
                await asyncio.sleep(0.01)
        finally:
            await job.stop()

    asyncio.run(scenario())

    assert len(calls) == 1
    assert calls[0].event_kinds == {"write", "rename", "reconcile"}
    assert calls[0].paths == {"SKILL.md", "scripts/run.sh"}


def test_activation_job_debounces_events_arriving_during_drain(
    monkeypatch,
    tmp_path: Path,
):
    skill_dir = make_skill(tmp_path, "weather")
    calls: list[tuple[set[str], float]] = []

    monkeypatch.setattr(
        "agent_sec_cli.daemon.jobs.skill_ledger.activation._resolve_managed_skill_dirs",
        lambda: [],
    )

    async def scenario():
        job = SkillLedgerActivationJob(debounce_seconds=0.05)

        async def fake_process(change: SkillFsChange) -> None:
            calls.append((set(change.event_kinds), asyncio.get_running_loop().time()))
            if len(calls) == 1:
                job.enqueue(
                    SkillFsChange(
                        canonical_skill_dir=skill_dir.resolve(),
                        event_kinds={"rename"},
                        paths={"scripts/run.sh"},
                    )
                )

        monkeypatch.setattr(job, "_process_change", fake_process)
        await job.start()
        try:
            job.enqueue(
                SkillFsChange(
                    canonical_skill_dir=skill_dir.resolve(),
                    event_kinds={"write"},
                    paths={"SKILL.md"},
                )
            )
            deadline = asyncio.get_running_loop().time() + 1.0
            while len(calls) < 2 and asyncio.get_running_loop().time() < deadline:
                await asyncio.sleep(0.01)
        finally:
            await job.stop()

    asyncio.run(scenario())

    assert [event_kinds for event_kinds, _ in calls] == [{"write"}, {"rename"}]
    assert calls[1][1] - calls[0][1] >= 0.04


def test_drain_pending_requeues_batch_on_cancelled_process(
    monkeypatch,
    tmp_path: Path,
):
    first = make_skill(tmp_path, "weather")
    second = make_skill(tmp_path, "calendar")

    async def scenario():
        job = SkillLedgerActivationJob(debounce_seconds=0)
        job._wake_event = asyncio.Event()
        changes = [
            SkillFsChange(
                canonical_skill_dir=first.resolve(),
                event_kinds={"write"},
                paths={"SKILL.md"},
            ),
            SkillFsChange(
                canonical_skill_dir=second.resolve(),
                event_kinds={"write"},
                paths={"SKILL.md"},
            ),
        ]
        job._pending = {change.canonical_skill_dir: change for change in changes}

        async def fail_process(_change: SkillFsChange) -> None:
            raise asyncio.CancelledError()

        monkeypatch.setattr(job, "_process_change", fail_process)
        with pytest.raises(asyncio.CancelledError):
            await job._drain_pending()
        return job._pending

    pending = asyncio.run(scenario())

    assert set(pending) == {first.resolve(), second.resolve()}


def test_activation_job_records_worker_transport_failure(tmp_path: Path):
    skill_dir = make_skill(tmp_path, "weather")

    def fail_process(_change: SkillFsChange) -> dict[str, Any]:
        raise SkillLedgerWorkerTransportError("worker request timed out after 300s")

    async def scenario():
        job = SkillLedgerActivationJob(
            debounce_seconds=0,
            worker_client=FakeWorkerClient(fail_process),
        )
        job._state = "running"
        await job._process_change(
            SkillFsChange(
                canonical_skill_dir=skill_dir.resolve(),
                event_kinds={"write"},
                paths={"SKILL.md"},
            )
        )
        return job.status(), job.last_processed

    status, last_processed = asyncio.run(scenario())

    assert status.state == "error"
    assert status.last_error == "worker request timed out after 300s"
    assert last_processed is not None
    assert last_processed["status"] == "error"


def test_process_skill_change_resolves_activation_after_scan_error(
    monkeypatch,
    tmp_path: Path,
):
    skill_dir = make_skill(tmp_path, "weather")
    root = ResolvedSkillRoot(skill_dir.resolve(), skill_dir.resolve(), "host")
    backend = object()
    events = []

    def fake_root_resolver(path: Path) -> ResolvedSkillRoot:
        events.append(("root", path))
        return root

    def fake_backend() -> object:
        return backend

    def fail_scan(
        received_root: ResolvedSkillRoot,
        received_backend: object,
    ) -> dict[str, Any]:
        events.append(("scan", received_root, received_backend))
        raise RuntimeError("scanner failed")

    def fake_policy() -> str:
        return "pass_warn_only"

    def fake_resolve(
        received_root: ResolvedSkillRoot,
        received_backend: object,
        policy: str,
    ) -> dict[str, Any]:
        events.append(("resolve", received_root, received_backend, policy))
        return {"target": None}

    monkeypatch.setattr(
        "agent_sec_cli.daemon.jobs.skill_ledger.processor._resolve_skill_root",
        fake_root_resolver,
    )
    monkeypatch.setattr(
        "agent_sec_cli.daemon.jobs.skill_ledger.processor._ensure_default_backend",
        fake_backend,
    )
    monkeypatch.setattr(
        "agent_sec_cli.daemon.jobs.skill_ledger.processor._scan_skill",
        fail_scan,
    )
    monkeypatch.setattr(
        "agent_sec_cli.daemon.jobs.skill_ledger.processor._resolve_activation",
        fake_resolve,
    )
    monkeypatch.setattr(
        "agent_sec_cli.daemon.jobs.skill_ledger.processor._resolve_activation_policy",
        fake_policy,
    )

    result = process_skill_change(
        SkillFsChange(
            canonical_skill_dir=skill_dir.resolve(),
            event_kinds={"write"},
            paths={"SKILL.md"},
        )
    )

    assert result["status"] == "error"
    assert result["error"] == "scanner failed"
    assert result["activation"] == {"target": None}
    assert events == [
        ("root", skill_dir.resolve()),
        ("scan", root, backend),
        ("resolve", root, backend, "pass_warn_only"),
    ]


def test_process_skill_change_skips_resolver_failure(
    monkeypatch,
    tmp_path: Path,
):
    skill_dir = make_skill(tmp_path, "weather")
    resolve_error = SkillRootResolveError(skill_dir.resolve(), "resolver timed out")

    monkeypatch.setattr(
        "agent_sec_cli.daemon.jobs.skill_ledger.processor._resolve_skill_root",
        lambda _path: (_ for _ in ()).throw(resolve_error),
    )

    result = process_skill_change(
        SkillFsChange(
            canonical_skill_dir=skill_dir.resolve(),
            event_kinds={"write"},
            paths={"SKILL.md"},
        )
    )

    assert result["status"] == "skipped"
    assert result["reasonCode"] == "skill_root_resolve_failed"
    assert result["message"] == str(resolve_error)
    assert result["skill"]["skillName"] == "weather"
    assert result["scan"] is None
    assert result["activation"] is None
    assert "error" not in result


def test_process_skill_change_reports_activation_error_per_skill(
    monkeypatch,
    tmp_path: Path,
):
    skill_dir = make_skill(tmp_path, "weather")
    root = ResolvedSkillRoot(skill_dir.resolve(), skill_dir.resolve(), "host")
    backend = object()

    def fake_backend() -> object:
        return backend

    def fake_scan(
        received_root: ResolvedSkillRoot,
        received_backend: object,
    ) -> dict[str, Any]:
        assert received_root is root
        assert received_backend is backend
        return {"status": "noop"}

    def fail_resolve(
        received_root: ResolvedSkillRoot,
        received_backend: object,
        policy: str,
    ) -> dict[str, Any]:
        assert received_root is root
        assert received_backend is backend
        assert policy == "pass_warn_only"
        raise RuntimeError("activation failed")

    monkeypatch.setattr(
        "agent_sec_cli.daemon.jobs.skill_ledger.processor._resolve_skill_root",
        lambda _path: root,
    )

    monkeypatch.setattr(
        "agent_sec_cli.daemon.jobs.skill_ledger.processor._ensure_default_backend",
        fake_backend,
    )
    monkeypatch.setattr(
        "agent_sec_cli.daemon.jobs.skill_ledger.processor._scan_skill",
        fake_scan,
    )
    monkeypatch.setattr(
        "agent_sec_cli.daemon.jobs.skill_ledger.processor._resolve_activation",
        fail_resolve,
    )
    monkeypatch.setattr(
        "agent_sec_cli.daemon.jobs.skill_ledger.processor._resolve_activation_policy",
        lambda: "pass_warn_only",
    )

    result = process_skill_change(
        SkillFsChange(
            canonical_skill_dir=skill_dir.resolve(),
            event_kinds={"write"},
            paths={"SKILL.md"},
        )
    )

    assert result["status"] == "error"
    assert result["error"] == "activation failed"
    assert result["activationError"] == "activation failed"
    assert result["scan"] == {"status": "noop"}
    assert result["activation"] is None


def test_activation_job_keeps_running_after_per_skill_error(
    tmp_path: Path,
):
    skill_dir = make_skill(tmp_path, "weather")
    change = SkillFsChange(canonical_skill_dir=skill_dir.resolve())

    async def scenario() -> SkillLedgerActivationJob:
        job = SkillLedgerActivationJob(
            debounce_seconds=0,
            worker_client=FakeWorkerClient(
                lambda _change: {"status": "error", "error": "scanner failed"}
            ),
        )
        job._state = "running"
        await job._process_change(change)
        return job

    job = asyncio.run(scenario())

    assert job.status().state == "running"
    assert job.status().last_error is None
    assert job.last_processed == {"status": "error", "error": "scanner failed"}


def test_default_job_name_is_stable():
    assert SkillLedgerActivationJob().name == SKILL_LEDGER_ACTIVATION_JOB
