"""Tests for daemon protocol parsing and dispatch."""

import asyncio
import uuid
from pathlib import Path

import pytest
from agent_sec_cli.daemon.errors import BadRequestError, PayloadTooLargeError
from agent_sec_cli.daemon.protocol import (
    MAX_TIMEOUT_MS,
    DaemonRequest,
    NDJSONFrameParser,
    parse_request_line,
    request_to_payload,
)
from agent_sec_cli.daemon.registry import (
    HandlerResult,
    MethodRegistry,
    MethodSpec,
    dispatch_request,
)
from agent_sec_cli.daemon.runtime import DaemonRuntime


def test_parse_request_rejects_malformed_json():
    with pytest.raises(BadRequestError, match="valid JSON"):
        parse_request_line(b"{bad-json}\n")


def test_parse_request_rejects_non_object_request():
    with pytest.raises(BadRequestError, match="JSON object"):
        parse_request_line(b'["daemon.health"]\n')


def test_frame_parser_handles_partial_and_coalesced_lines():
    parser = NDJSONFrameParser(max_frame_bytes=1024)

    assert parser.feed(b'{"method":"daemon.health"') == []
    frames = parser.feed(b'}\n{"method":"daemon.health"}\n')
    requests = [parse_request_line(frame) for frame in frames]

    assert [request.method for request in requests] == [
        "daemon.health",
        "daemon.health",
    ]
    assert requests[0].request_id != requests[1].request_id
    uuid.UUID(requests[0].request_id)
    uuid.UUID(requests[1].request_id)


def test_frame_parser_rejects_oversized_payload():
    parser = NDJSONFrameParser(max_frame_bytes=10)

    with pytest.raises(PayloadTooLargeError):
        parser.feed(b"a" * 11)


def test_parse_request_generates_missing_request_id():
    request = parse_request_line(b'{"method":"daemon.health"}\n')

    uuid.UUID(request.request_id)
    assert request.method == "daemon.health"
    assert request.params == {}
    assert request.trace_context == {}
    assert request.caller is None


@pytest.mark.parametrize(
    "payload",
    [
        b'{"id":"req-1","method":"daemon.health"}\n',
        b'{"request_id":"req-1","method":"daemon.health"}\n',
    ],
)
def test_parse_request_ignores_caller_provided_request_id(payload: bytes):
    request = parse_request_line(payload)

    assert request.request_id != "req-1"
    uuid.UUID(request.request_id)


def test_parse_request_accepts_caller():
    request = parse_request_line(b'{"method":"scan-prompt","caller":" cli "}\n')

    assert request.caller == "cli"


@pytest.mark.parametrize("caller", ['"   "', "42", "false"])
def test_parse_request_ignores_invalid_optional_caller(caller: str):
    request = parse_request_line(
        f'{{"method":"scan-prompt","caller":{caller}}}\n'.encode()
    )

    assert request.caller is None


def test_request_to_payload_includes_caller_when_present():
    payload = request_to_payload(
        DaemonRequest(
            method="scan-prompt",
            request_id="req-1",
            caller="cli",
        )
    )

    assert "id" not in payload
    assert "request_id" not in payload
    assert payload["caller"] == "cli"


def test_parse_request_accepts_timeout_ms_at_max():
    request = parse_request_line(
        f'{{"method":"daemon.health","timeout_ms":{MAX_TIMEOUT_MS}}}\n'.encode()
    )

    assert request.timeout_ms == MAX_TIMEOUT_MS


def test_parse_request_rejects_timeout_ms_above_max():
    over_max = MAX_TIMEOUT_MS + 1
    with pytest.raises(BadRequestError, match="must not exceed"):
        parse_request_line(
            f'{{"method":"daemon.health","timeout_ms":{over_max}}}\n'.encode()
        )


def test_dispatch_rejects_unknown_method(tmp_path: Path):
    async def scenario():
        request = DaemonRequest(
            method="unknown.method",
            request_id="req-unknown",
        )
        response = await dispatch_request(
            request,
            MethodRegistry(),
            DaemonRuntime(socket_path=tmp_path / "daemon.sock"),
        )
        return response

    response = asyncio.run(scenario())

    assert response.request_id == "req-unknown"
    assert response.ok is False
    assert response.error == {
        "code": "unknown_method",
        "message": "unknown daemon method: unknown.method",
    }


def test_dispatch_applies_request_timeout(tmp_path: Path):
    async def slow_handler(
        _request: DaemonRequest, _runtime: DaemonRuntime
    ) -> HandlerResult:
        await asyncio.sleep(0.05)
        return HandlerResult(data={"done": True})

    async def scenario():
        registry = MethodRegistry()
        registry.register(
            MethodSpec(
                method="slow",
                handler=slow_handler,
                lifecycle="test",
                timeout_ms=1000,
            )
        )
        request = DaemonRequest(
            method="slow",
            request_id="req-timeout",
            timeout_ms=1,
        )
        response = await dispatch_request(
            request,
            registry,
            DaemonRuntime(socket_path=tmp_path / "daemon.sock"),
        )
        return response

    response = asyncio.run(scenario())

    assert response.request_id == "req-timeout"
    assert response.ok is False
    assert response.error == {
        "code": "timeout",
        "message": "daemon request timed out after 1 ms",
    }
