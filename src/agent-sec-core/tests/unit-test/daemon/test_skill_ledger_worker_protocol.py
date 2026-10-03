"""Tests for the Skill Ledger worker NDJSON protocol."""

# ruff: noqa: I001

import json
from pathlib import Path

import pytest
from agent_sec_cli.daemon.jobs.skill_ledger.protocol import (
    MAX_WORKER_FRAME_BYTES,
    SkillFsChange,
    WorkerProtocolError,
    error_worker_response,
    new_worker_request,
    parse_worker_request,
    parse_worker_response,
    serialize_worker_request,
    serialize_worker_response,
    success_worker_response,
)


def make_change(tmp_path: Path) -> SkillFsChange:
    skill_dir = tmp_path / "weather"
    return SkillFsChange(
        canonical_skill_dir=skill_dir,
        reported_skill_id="category/weather",
        event_kinds={"write", "rename"},
        paths={"SKILL.md", "run.sh"},
    )


def test_worker_request_round_trip(tmp_path: Path):
    request = new_worker_request(make_change(tmp_path))

    parsed = parse_worker_request(serialize_worker_request(request))

    assert parsed.request_id == request.request_id
    assert parsed.change == request.change


def test_worker_request_round_trip_without_reported_skill_id(tmp_path: Path):
    change = SkillFsChange(
        canonical_skill_dir=tmp_path / "weather",
        event_kinds={"reconcile"},
        paths=set(),
    )
    request = new_worker_request(change)

    parsed = parse_worker_request(serialize_worker_request(request))

    assert parsed.change == change
    assert parsed.change.reported_skill_id is None


def test_worker_success_response_round_trip():
    response = success_worker_response("request-1", {"status": "processed"})

    parsed = parse_worker_response(serialize_worker_response(response))

    assert parsed.request_id == "request-1"
    assert parsed.ok is True
    assert parsed.result == {"status": "processed"}
    assert parsed.error is None


def test_worker_error_response_round_trip():
    response = error_worker_response("request-1", RuntimeError("scan failed"))

    parsed = parse_worker_response(serialize_worker_response(response))

    assert parsed.ok is False
    assert parsed.result is None
    assert parsed.error is not None
    assert parsed.error.error_type == "RuntimeError"
    assert parsed.error.message == "scan failed"


@pytest.mark.parametrize(
    ("payload", "message"),
    [
        ({}, "schemaVersion"),
        ({"schemaVersion": 1}, "requestId"),
        (
            {"schemaVersion": 1, "requestId": "1", "method": "unknown"},
            "method",
        ),
        (
            {
                "schemaVersion": 1,
                "requestId": "1",
                "method": "process_change",
                "change": {},
            },
            "canonicalSkillDir",
        ),
        (
            {
                "schemaVersion": 1,
                "requestId": "1",
                "method": "process_change",
                "change": {
                    "canonicalSkillDir": "relative",
                    "eventKinds": [],
                    "paths": [],
                },
            },
            "absolute",
        ),
        (
            {
                "schemaVersion": 1,
                "requestId": "1",
                "method": "process_change",
                "change": {
                    "canonicalSkillDir": "//skills/weather",
                    "eventKinds": ["write"],
                    "paths": [],
                },
            },
            "single leading",
        ),
        (
            {
                "schemaVersion": 1,
                "requestId": "1",
                "method": "process_change",
                "change": {
                    "canonicalSkillDir": "/skills/weather",
                    "eventKinds": ["unknown"],
                    "paths": [],
                },
            },
            "eventKinds",
        ),
        (
            {
                "schemaVersion": 1,
                "requestId": "1",
                "method": "process_change",
                "change": {
                    "canonicalSkillDir": "/skills/weather",
                    "eventKinds": ["write"],
                    "paths": ["../escape"],
                },
            },
            "relative paths",
        ),
        (
            {
                "schemaVersion": 1,
                "requestId": "1",
                "method": "process_change",
                "change": {
                    "canonicalSkillDir": "/skills/weather\x00",
                    "eventKinds": ["write"],
                    "paths": [],
                },
            },
            "NUL",
        ),
        (
            {
                "schemaVersion": 1,
                "requestId": "1",
                "method": "process_change",
                "change": {
                    "canonicalSkillDir": "/skills/weather",
                    "eventKinds": ["write"],
                    "paths": ["scripts/run\x00.sh"],
                },
            },
            "NUL",
        ),
        (
            {
                "schemaVersion": 1,
                "requestId": "1",
                "method": "process_change",
                "change": {
                    "canonicalSkillDir": "/skills/weather",
                    "reportedSkillId": "",
                    "eventKinds": ["write"],
                    "paths": [],
                },
            },
            "reportedSkillId",
        ),
        (
            {
                "schemaVersion": 1,
                "requestId": "1",
                "method": "process_change",
                "change": {
                    "canonicalSkillDir": "/skills/weather",
                    "reportedSkillId": {"future": 2},
                    "eventKinds": ["write"],
                    "paths": [],
                },
            },
            "reportedSkillId",
        ),
    ],
)
def test_worker_request_rejects_invalid_payload(payload, message):
    frame = (json.dumps(payload) + "\n").encode()

    with pytest.raises(WorkerProtocolError, match=message):
        parse_worker_request(frame)


@pytest.mark.parametrize("frame", [b"", b"not-json\n", b"[]\n", b"\xff\n"])
def test_worker_protocol_rejects_invalid_frames(frame: bytes):
    with pytest.raises(WorkerProtocolError):
        parse_worker_response(frame)


def test_worker_protocol_rejects_oversized_frame():
    frame = b"x" * (MAX_WORKER_FRAME_BYTES + 1)

    with pytest.raises(WorkerProtocolError, match="exceeds"):
        parse_worker_request(frame)
