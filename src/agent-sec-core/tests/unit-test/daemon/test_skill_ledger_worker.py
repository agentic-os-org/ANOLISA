"""Regression tests for the Skill Ledger worker's oversized-response fallback.

The worker must answer a result that exceeds the frame cap with a bounded
error frame and keep serving — dying instead reads as a transport failure,
gets the worker restarted and the change reprocessed before failing
identically. These tests are the committed regression for that fallback,
including the oversized-exception-message case where the error response
itself is what exceeds the cap.
"""

import io
import sys
import types
from pathlib import Path

from agent_sec_cli.daemon.jobs.skill_ledger import worker as worker_module
from agent_sec_cli.daemon.jobs.skill_ledger.protocol import (
    MAX_WORKER_FRAME_BYTES,
    SkillFsChange,
    new_worker_request,
    parse_worker_response,
    serialize_worker_request,
)


def make_change(tmp_path: Path) -> SkillFsChange:
    return SkillFsChange(
        canonical_skill_dir=tmp_path / "weather",
        event_kinds={"write"},
        paths={"SKILL.md"},
    )


class FakeStdinBuffer:
    """Feeds request frames; an empty read ends the worker loop."""

    def __init__(self, frames: list[bytes]) -> None:
        self._frames = list(frames)

    def readline(self, _size: int = -1) -> bytes:
        return self._frames.pop(0) if self._frames else b""


def run_worker(monkeypatch, requests, handler) -> tuple[int, list[bytes]]:
    frames = [serialize_worker_request(new_worker_request(change)) for change in requests]
    for frame in frames:
        assert len(frame) <= MAX_WORKER_FRAME_BYTES, "test requests must fit the cap"

    monkeypatch.setattr(worker_module, "process_skill_change", handler)
    out = io.BytesIO()
    monkeypatch.setattr(sys, "stdin", types.SimpleNamespace(buffer=FakeStdinBuffer(frames)))
    code = worker_module._run(out)
    return code, [line for line in out.getvalue().splitlines() if line]


def test_oversized_result_answers_bounded_error_and_survives(monkeypatch, tmp_path: Path):
    """A result above the frame cap becomes a bounded error, not a dead worker.

    The worker keeps its loop: the NEXT request is answered normally on the
    same process, which is precisely what a dead-worker read (transport
    failure -> restart -> reprocess -> identical failure) would not do.
    """

    def handler(change):
        if change.paths == {"SKILL.md"}:
            # A valid but undeliverable result: ~4.5 MiB of payload.
            return {"echo": ["x" * 64] * 72000}
        return {"status": "processed", "paths": sorted(change.paths)}

    code, lines = run_worker(
        monkeypatch,
        [make_change(tmp_path), SkillFsChange(
            canonical_skill_dir=tmp_path / "weather",
            event_kinds={"write"},
            paths={"README.md"},
        )],
        handler,
    )

    assert code == 0
    assert len(lines) == 2, f"expected bounded error + next success, got {len(lines)} frames"

    bounded = parse_worker_response(lines[0])
    assert bounded.ok is False
    assert bounded.error.error_type == "WorkerResultTooLargeError"
    assert str(MAX_WORKER_FRAME_BYTES) in bounded.error.message
    assert len(lines[0]) <= MAX_WORKER_FRAME_BYTES

    nxt = parse_worker_response(lines[1])
    assert nxt.ok is True
    assert nxt.result == {"status": "processed", "paths": ["README.md"]}


def test_oversized_exception_message_also_answers_bounded_error(monkeypatch, tmp_path: Path):
    """An exception whose own message exceeds the cap still gets a bounded frame.

    The error response carries the exception message; when THAT response is
    the oversized one, the fallback must replace it with the short bounded
    error rather than die on the second serialization.
    """

    def handler(_change):
        raise RuntimeError("boom: " + "y" * (MAX_WORKER_FRAME_BYTES + 1024))

    code, lines = run_worker(monkeypatch, [make_change(tmp_path)], handler)

    assert code == 0
    assert len(lines) == 1

    bounded = parse_worker_response(lines[0])
    assert bounded.ok is False
    assert bounded.error.error_type == "WorkerResultTooLargeError"
    assert str(MAX_WORKER_FRAME_BYTES) in bounded.error.message
    assert len(lines[0]) <= MAX_WORKER_FRAME_BYTES


def test_normal_results_still_answer_success(monkeypatch, tmp_path: Path):
    """The fallback changes nothing for deliverable results."""

    def handler(change):
        return {"status": "processed", "paths": sorted(change.paths)}

    code, lines = run_worker(monkeypatch, [make_change(tmp_path)], handler)

    assert code == 0
    response = parse_worker_response(lines[0])
    assert response.ok is True
    assert response.result == {"status": "processed", "paths": ["SKILL.md"]}


def test_processing_exception_answers_structured_error(monkeypatch, tmp_path: Path):
    """A normal-sized processing exception keeps its identity in the error frame."""

    def handler(_change):
        raise RuntimeError("scan failed")

    code, lines = run_worker(monkeypatch, [make_change(tmp_path)], handler)

    assert code == 0
    response = parse_worker_response(lines[0])
    assert response.ok is False
    assert response.error.error_type == "RuntimeError"
    assert response.error.message == "scan failed"
