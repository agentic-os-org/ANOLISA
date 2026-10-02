"""Tests for daemon background job scheduling."""

import asyncio
import logging
import uuid
from typing import Any

import pytest
from agent_sec_cli.correlation_context import (
    TraceContext,
    clear_process_trace_context,
    get_current_trace_context,
)
from agent_sec_cli.daemon.jobs import (
    JobManager,
    JobStatus,
    OneShotBackgroundJob,
    PeriodicBackgroundJob,
)
from agent_sec_cli.daemon.jobs.base import next_cycle_start
from agent_sec_cli.daemon.jobs.registry import register_default_jobs


class RecordingPeriodicJob(PeriodicBackgroundJob):
    """Periodic job used by scheduling tests."""

    name = "recording-periodic-job"

    def __init__(self, interval_seconds: float) -> None:
        super().__init__(interval_seconds=interval_seconds)
        self.run_count = 0
        self.started = asyncio.Event()
        self.trace_contexts: list[TraceContext | None] = []

    async def run_once(self) -> None:
        """Record one scheduled run."""
        self.run_count += 1
        self.trace_contexts.append(get_current_trace_context())
        self.started.set()


class RecordingOneShotJob(OneShotBackgroundJob):
    """One-shot job used by trace-context lifecycle tests."""

    name = "recording-one-shot-job"

    def __init__(self) -> None:
        super().__init__()
        self.trace_contexts: list[tuple[str, TraceContext | None]] = []

    def on_run_started(self, started_at: str) -> None:
        self.trace_contexts.append(("started", get_current_trace_context()))

    async def run_once(self) -> None:
        """Record the active job trace context."""
        self.trace_contexts.append(("run", get_current_trace_context()))

    def on_run_completed(self, finished_at: str) -> None:
        self.trace_contexts.append(("completed", get_current_trace_context()))


class FailingOneShotJob(OneShotBackgroundJob):
    """One-shot job that fails for lifecycle logging tests."""

    name = "failing-one-shot-job"

    async def run_once(self) -> None:
        """Raise a deterministic failure."""
        raise RuntimeError("forced one-shot failure")


def _capture_job_events(monkeypatch) -> list[dict[str, Any]]:
    events: list[dict[str, Any]] = []

    def fake_log_daemon_event(**kwargs) -> None:
        if kwargs["event"].startswith("daemon_job_"):
            events.append(kwargs)

    monkeypatch.setattr(
        "agent_sec_cli.daemon.jobs.base.log_daemon_event",
        fake_log_daemon_event,
    )
    return events


def _assert_uuid(value: str | None) -> None:
    assert value is not None
    assert str(uuid.UUID(value)) == value


def test_next_cycle_start_uses_start_time_interval_boundaries():
    assert next_cycle_start(100.0, 103.0, 10.0) == 110.0
    assert next_cycle_start(100.0, 110.0, 10.0) == 110.0


def test_next_cycle_start_skips_missed_interval_boundaries():
    assert next_cycle_start(100.0, 112.0, 10.0) == 120.0
    assert next_cycle_start(100.0, 125.0, 10.0) == 130.0


def test_next_cycle_start_rejects_invalid_interval():
    with pytest.raises(ValueError, match="interval_seconds must be positive"):
        next_cycle_start(100.0, 101.0, 0.0)


def test_job_status_omits_unset_optional_periodic_fields():
    status = JobStatus(name="job", state="stopped")

    assert status.to_dict() == {
        "name": "job",
        "state": "stopped",
        "last_error": None,
        "last_tick_at": None,
    }


def test_periodic_background_job_runs_and_reports_interval():
    async def scenario():
        job = RecordingPeriodicJob(interval_seconds=3600.0)
        await job.start()
        try:
            await asyncio.wait_for(job.started.wait(), timeout=0.5)
            status = job.status().to_dict()
            run_count = job.run_count
        finally:
            await job.stop()
        return status, run_count

    status, run_count = asyncio.run(scenario())

    assert run_count == 1
    assert status["name"] == "recording-periodic-job"
    assert status["state"] == "running"
    assert status["interval_seconds"] == 3600.0
    assert "last_started_at" in status
    assert "next_run_at" in status


def test_one_shot_background_job_run_has_trace_context_and_resets() -> None:
    async def scenario():
        clear_process_trace_context()
        try:
            job = RecordingOneShotJob()
            await job._run_once_with_lifecycle()
            return (
                job.status().to_dict(),
                list(job.trace_contexts),
                get_current_trace_context(),
            )
        finally:
            clear_process_trace_context()

    status, trace_contexts, after_context = asyncio.run(scenario())

    labels = [label for label, _ctx in trace_contexts]
    contexts = [ctx for _label, ctx in trace_contexts]
    trace_ids = {ctx.trace_id for ctx in contexts if ctx is not None}

    assert labels == ["started", "run", "completed"]
    assert status["state"] == "completed"
    assert len(trace_ids) == 1
    trace_id = trace_ids.pop()
    _assert_uuid(trace_id)
    assert all(ctx is not None for ctx in contexts)
    assert all(ctx.session_id is None for ctx in contexts if ctx is not None)
    assert all(ctx.run_id is None for ctx in contexts if ctx is not None)
    assert all(ctx.call_id is None for ctx in contexts if ctx is not None)
    assert all(ctx.tool_call_id is None for ctx in contexts if ctx is not None)
    assert after_context is None


def test_one_shot_background_job_logs_started_and_completed(monkeypatch) -> None:
    events = _capture_job_events(monkeypatch)

    async def scenario():
        clear_process_trace_context()
        try:
            job = RecordingOneShotJob()
            await job._run_once_with_lifecycle()
            return job.status().to_dict(), get_current_trace_context()
        finally:
            clear_process_trace_context()

    status, after_context = asyncio.run(scenario())

    assert status["state"] == "completed"
    assert after_context is None
    assert [event["event"] for event in events] == [
        "daemon_job_started",
        "daemon_job_completed",
    ]
    assert events[0]["data"]["job_name"] == "recording-one-shot-job"
    assert events[0]["data"]["job_kind"] == "one_shot"
    assert events[0]["data"]["state"] == "running"
    assert events[1]["data"]["state"] == "completed"
    assert isinstance(events[1]["data"]["latency_ms"], int)
    assert events[0]["trace_context"].trace_id == events[1]["trace_context"].trace_id
    _assert_uuid(events[0]["trace_context"].trace_id)


def test_one_shot_background_job_logs_failure(monkeypatch) -> None:
    events = _capture_job_events(monkeypatch)

    async def scenario():
        clear_process_trace_context()
        try:
            job = FailingOneShotJob()
            await job._run_once_with_lifecycle()
            return job.status().to_dict(), get_current_trace_context()
        finally:
            clear_process_trace_context()

    status, after_context = asyncio.run(scenario())

    assert status["state"] == "error"
    assert status["last_error"] == "forced one-shot failure"
    assert after_context is None
    assert [event["event"] for event in events] == [
        "daemon_job_started",
        "daemon_job_failed",
    ]
    failed = events[1]
    assert failed["level"] == logging.ERROR
    assert failed["data"]["job_name"] == "failing-one-shot-job"
    assert failed["data"]["job_kind"] == "one_shot"
    assert failed["data"]["state"] == "error"
    assert failed["data"]["error_type"] == "RuntimeError"
    assert failed["data"]["error_message"] == "forced one-shot failure"
    assert isinstance(failed["data"]["latency_ms"], int)
    assert events[0]["trace_context"].trace_id == failed["trace_context"].trace_id


def test_periodic_background_job_run_gets_new_trace_context_each_tick() -> None:
    async def scenario():
        clear_process_trace_context()
        job = RecordingPeriodicJob(interval_seconds=0.01)
        try:
            await job.start()
            for _attempt in range(50):
                if len(job.trace_contexts) >= 2:
                    break
                await asyncio.sleep(0.01)
            return list(job.trace_contexts[:2]), get_current_trace_context()
        finally:
            await job.stop()
            clear_process_trace_context()

    contexts, after_context = asyncio.run(scenario())

    assert len(contexts) == 2
    assert all(ctx is not None for ctx in contexts)
    trace_ids = [ctx.trace_id for ctx in contexts if ctx is not None]
    for trace_id in trace_ids:
        _assert_uuid(trace_id)
    assert len(set(trace_ids)) == 2
    assert after_context is None


def test_periodic_background_job_logs_started_and_completed(monkeypatch) -> None:
    events = _capture_job_events(monkeypatch)

    async def scenario():
        clear_process_trace_context()
        job = RecordingPeriodicJob(interval_seconds=3600.0)
        try:
            await job.start()
            await asyncio.wait_for(job.started.wait(), timeout=0.5)
            return job.status().to_dict(), get_current_trace_context()
        finally:
            await job.stop()
            clear_process_trace_context()

    status, after_context = asyncio.run(scenario())

    assert status["state"] == "running"
    assert after_context is None
    assert [event["event"] for event in events] == [
        "daemon_job_started",
        "daemon_job_completed",
    ]
    assert events[0]["data"]["job_name"] == "recording-periodic-job"
    assert events[0]["data"]["job_kind"] == "periodic"
    assert events[0]["data"]["state"] == "running"
    assert events[0]["data"]["interval_seconds"] == 3600.0
    assert events[1]["data"]["state"] == "running"
    assert events[1]["data"]["interval_seconds"] == 3600.0
    assert isinstance(events[1]["data"]["latency_ms"], int)
    assert events[0]["trace_context"].trace_id == events[1]["trace_context"].trace_id
    _assert_uuid(events[0]["trace_context"].trace_id)


def test_register_default_jobs_includes_skill_ledger():
    manager = JobManager()
    register_default_jobs(manager)
    assert [job["name"] for job in manager.status()] == ["skill-ledger-activation"]
