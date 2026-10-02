"""Skill Ledger activation job scheduling."""

import asyncio
import contextlib
from pathlib import Path
from typing import Any

from agent_sec_cli.daemon.errors import UnavailableError
from agent_sec_cli.daemon.jobs.base import BackgroundJob, JobStatus, utc_now
from agent_sec_cli.daemon.jobs.skill_ledger.protocol import SkillFsChange
from agent_sec_cli.daemon.jobs.skill_ledger.worker_client import (
    SkillLedgerWorkerClient,
)
from agent_sec_cli.skill_ledger.config import resolve_managed_skill_dirs
from agent_sec_cli.skill_ledger.path_identity import (
    normalize_canonical_skill_dir,
)

SKILL_LEDGER_ACTIVATION_JOB = "skill-ledger-activation"
DEFAULT_DEBOUNCE_SECONDS = 0.5


class SkillLedgerActivationJob(BackgroundJob):
    """Debounced Skill Ledger scanner/activation worker."""

    name = SKILL_LEDGER_ACTIVATION_JOB

    def __init__(
        self,
        debounce_seconds: float = DEFAULT_DEBOUNCE_SECONDS,
        worker_client: SkillLedgerWorkerClient | None = None,
    ) -> None:
        if debounce_seconds < 0:
            raise ValueError("debounce_seconds must be non-negative")
        self.debounce_seconds = debounce_seconds
        self._task: asyncio.Task[None] | None = None
        self._wake_event: asyncio.Event | None = None
        self._pending: dict[Path, SkillFsChange] = {}
        self._state = "stopped"
        self._last_error: str | None = None
        self._last_tick_at: str | None = None
        self._last_processed: dict[str, Any] | None = None
        self._worker_client = worker_client or SkillLedgerWorkerClient()

    async def start(self) -> None:
        """Start the activation worker and enqueue startup reconciliation."""
        if self._task is not None and not self._task.done():
            return
        self._wake_event = asyncio.Event()
        self._state = "running"
        self._task = asyncio.create_task(self._run_loop())
        self._enqueue_reconcile()

    async def stop(self) -> None:
        """Stop the activation worker."""
        try:
            if self._task is not None:
                self._task.cancel()
                with contextlib.suppress(asyncio.CancelledError):
                    await self._task
                self._task = None
        finally:
            await self._worker_client.stop()
            self._wake_event = None
            self._state = "stopped"

    def status(self) -> JobStatus:
        """Return current job status."""
        last_error = self._last_error or self._worker_client.last_error
        return JobStatus(
            name=self.name,
            state="error" if last_error and self._state == "running" else self._state,
            last_error=last_error,
            last_tick_at=self._last_tick_at,
        )

    def enqueue(self, change: SkillFsChange) -> bool:
        """Queue a SkillFS change. Returns whether it was newly queued."""
        if self._wake_event is None:
            raise UnavailableError("skill-ledger activation job is not running")
        existing = self._pending.get(change.canonical_skill_dir)
        newly_queued = existing is None
        if existing is None:
            self._pending[change.canonical_skill_dir] = change
        else:
            existing.merge(change)
        self._wake_event.set()
        return newly_queued

    @property
    def last_processed(self) -> dict[str, Any] | None:
        """Return the last processed result for tests and diagnostics."""
        return self._last_processed

    @property
    def worker_pid(self) -> int | None:
        """Return the worker PID for diagnostics."""
        return self._worker_client.pid

    async def _run_loop(self) -> None:
        while True:
            if self._wake_event is None:
                return
            await self._wake_event.wait()
            self._wake_event.clear()
            if self.debounce_seconds:
                await asyncio.sleep(self.debounce_seconds)
            await self._drain_pending()

    async def _drain_pending(self) -> None:
        pending = self._pending
        self._pending = {}
        changes = list(pending.values())
        for index, change in enumerate(changes):
            try:
                await self._process_change(change)
            except asyncio.CancelledError:
                self._requeue_changes(changes[index:])
                raise

    def _requeue_changes(self, changes: list[SkillFsChange]) -> None:
        for change in changes:
            existing = self._pending.get(change.canonical_skill_dir)
            if existing is None:
                self._pending[change.canonical_skill_dir] = change
            else:
                existing.merge(change)
        if changes and self._wake_event is not None:
            self._wake_event.set()

    async def _process_change(self, change: SkillFsChange) -> None:
        self._last_tick_at = utc_now()
        try:
            result = await self._worker_client.process_change(change)
            self._last_processed = result
            self._last_error = None
            self._state = "running"
        except asyncio.CancelledError:
            raise
        except Exception as exc:
            self._last_error = str(exc)
            self._last_processed = {
                "canonicalSkillDir": str(change.canonical_skill_dir),
                "skillName": change.skill_name,
                "status": "error",
                "error": str(exc),
            }
            self._state = "error"

    def _enqueue_reconcile(self) -> None:
        try:
            for skill_dir in _resolve_managed_skill_dirs():
                self.enqueue(
                    SkillFsChange(
                        canonical_skill_dir=normalize_canonical_skill_dir(skill_dir),
                        event_kinds={"reconcile"},
                        paths=set(),
                    )
                )
        except Exception as exc:
            self._last_error = str(exc)
            self._state = "error"


def _resolve_managed_skill_dirs() -> list[Path]:
    return resolve_managed_skill_dirs()
