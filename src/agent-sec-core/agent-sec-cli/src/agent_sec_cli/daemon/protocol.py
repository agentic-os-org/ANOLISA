"""NDJSON request and response protocol for the agent-sec daemon."""

import json
import uuid
from dataclasses import dataclass, field
from typing import Any

from agent_sec_cli.daemon.errors import (
    BadRequestError,
    DaemonError,
    PayloadTooLargeError,
)

DEFAULT_MAX_REQUEST_BYTES = 4 * 1024 * 1024
DEFAULT_MAX_RESPONSE_BYTES = 4 * 1024 * 1024
DEFAULT_TIMEOUT_MS = 5000
MAX_TIMEOUT_MS = 5 * 60 * 1000


def generate_request_id() -> str:
    """Create a daemon-owned request id for one request."""
    return str(uuid.uuid4())


@dataclass(frozen=True)
class DaemonRequest:
    """Validated daemon request."""

    method: str
    request_id: str = field(default_factory=generate_request_id)
    params: dict[str, Any] = field(default_factory=dict)
    trace_context: dict[str, Any] = field(default_factory=dict)
    caller: str | None = None
    timeout_ms: int | None = None


@dataclass(frozen=True)
class DaemonResponse:
    """Validated daemon response."""

    request_id: str
    ok: bool
    data: Any = field(default_factory=dict)
    stdout: str = ""
    stderr: str = ""
    exit_code: int = 0
    error: dict[str, str] | None = None


class NDJSONFrameParser:
    """Incrementally extracts newline-delimited JSON frames from byte chunks."""

    def __init__(self, max_frame_bytes: int) -> None:
        self._max_frame_bytes = max_frame_bytes
        self._buffer = bytearray()

    def feed(self, chunk: bytes) -> list[bytes]:
        """Append bytes and return all complete frames."""
        if chunk:
            self._buffer.extend(chunk)

        frames: list[bytes] = []
        while True:
            newline_index = self._buffer.find(b"\n")
            if newline_index < 0:
                if len(self._buffer) > self._max_frame_bytes:
                    raise PayloadTooLargeError(self._max_frame_bytes)
                return frames

            frame = bytes(self._buffer[: newline_index + 1])
            if len(frame) > self._max_frame_bytes:
                raise PayloadTooLargeError(self._max_frame_bytes)
            frames.append(frame)
            del self._buffer[: newline_index + 1]

    def flush(self) -> list[bytes]:
        """Return a final EOF-terminated frame, if any."""
        if not self._buffer:
            return []
        if len(self._buffer) > self._max_frame_bytes:
            raise PayloadTooLargeError(self._max_frame_bytes)

        frame = bytes(self._buffer)
        self._buffer.clear()
        return [frame]


def _decode_json_object(line: bytes) -> dict[str, Any]:
    stripped = line.strip()
    if not stripped:
        raise BadRequestError("request must not be empty")

    try:
        payload = json.loads(stripped.decode("utf-8"))
    except UnicodeDecodeError as exc:
        raise BadRequestError("request must be valid UTF-8") from exc
    except json.JSONDecodeError as exc:
        raise BadRequestError("request must be valid JSON") from exc

    if not isinstance(payload, dict):
        raise BadRequestError("request must be a JSON object")
    return payload


def _validate_object_field(payload: dict[str, Any], field_name: str) -> dict[str, Any]:
    value = payload.get(field_name, {})
    if not isinstance(value, dict):
        raise BadRequestError(f"{field_name} must be a JSON object")
    return value


def _validate_timeout_ms(payload: dict[str, Any]) -> int | None:
    if "timeout_ms" not in payload or payload["timeout_ms"] is None:
        return None

    timeout_ms = payload["timeout_ms"]
    if (
        not isinstance(timeout_ms, int)
        or isinstance(timeout_ms, bool)
        or timeout_ms <= 0
    ):
        raise BadRequestError("timeout_ms must be a positive integer")
    if timeout_ms > MAX_TIMEOUT_MS:
        raise BadRequestError(f"timeout_ms must not exceed {MAX_TIMEOUT_MS}")
    return timeout_ms


def parse_request_line(
    line: bytes,
    max_request_bytes: int = DEFAULT_MAX_REQUEST_BYTES,
) -> DaemonRequest:
    """Parse and validate one NDJSON request frame."""
    if len(line) > max_request_bytes:
        raise PayloadTooLargeError(max_request_bytes)

    payload = _decode_json_object(line)
    method = payload.get("method")
    if not isinstance(method, str) or not method.strip():
        raise BadRequestError("method is required")

    caller = payload.get("caller")
    caller = caller.strip() if isinstance(caller, str) and caller.strip() else None

    return DaemonRequest(
        method=method,
        request_id=generate_request_id(),
        params=_validate_object_field(payload, "params"),
        trace_context=_validate_object_field(payload, "trace_context"),
        caller=caller,
        timeout_ms=_validate_timeout_ms(payload),
    )


def request_to_payload(request: DaemonRequest) -> dict[str, Any]:
    """Convert a daemon request to a JSON-serializable payload."""
    payload: dict[str, Any] = {
        "method": request.method,
        "params": request.params,
        "trace_context": request.trace_context,
    }
    if request.caller is not None:
        payload["caller"] = request.caller
    if request.timeout_ms is not None:
        payload["timeout_ms"] = request.timeout_ms
    return payload


def serialize_request(request: DaemonRequest) -> bytes:
    """Serialize a daemon request as one NDJSON frame."""
    return _json_line(request_to_payload(request))


def success_response(
    request_id: str,
    data: Any = None,
    stdout: str = "",
    stderr: str = "",
    exit_code: int = 0,
) -> DaemonResponse:
    """Build a successful daemon response."""
    response_data = {} if data is None else data
    return DaemonResponse(
        request_id=request_id,
        ok=True,
        data=response_data,
        stdout=stdout,
        stderr=stderr,
        exit_code=exit_code,
    )


def error_response(request_id: str, error: DaemonError) -> DaemonResponse:
    """Build a structured daemon error response."""
    return DaemonResponse(
        request_id=request_id,
        ok=False,
        data={},
        stdout="",
        stderr=error.message,
        exit_code=error.exit_code,
        error={"code": error.code, "message": error.message},
    )


def response_to_payload(response: DaemonResponse) -> dict[str, Any]:
    """Convert a daemon response to a JSON-serializable payload."""
    payload: dict[str, Any] = {
        "request_id": response.request_id,
        "ok": response.ok,
        "data": response.data,
        "stdout": response.stdout,
        "stderr": response.stderr,
        "exit_code": response.exit_code,
    }
    if response.error is not None:
        payload["error"] = response.error
    return payload


def serialize_response(response: DaemonResponse) -> bytes:
    """Serialize a daemon response as one NDJSON frame."""
    return _json_line(response_to_payload(response))


def parse_response_line(line: bytes) -> DaemonResponse:
    """Parse and validate one daemon response frame."""
    payload = _decode_json_object(line)

    request_id = payload.get("request_id")
    if not isinstance(request_id, str) or not request_id.strip():
        raise BadRequestError("response request_id must be a non-empty string")

    ok = payload.get("ok")
    if not isinstance(ok, bool):
        raise BadRequestError("response ok must be a boolean")

    stdout = payload.get("stdout", "")
    stderr = payload.get("stderr", "")
    exit_code = payload.get("exit_code", 0)
    if not isinstance(stdout, str):
        raise BadRequestError("response stdout must be a string")
    if not isinstance(stderr, str):
        raise BadRequestError("response stderr must be a string")
    if not isinstance(exit_code, int) or isinstance(exit_code, bool):
        raise BadRequestError("response exit_code must be an integer")

    error = payload.get("error")
    if error is not None:
        if not isinstance(error, dict):
            raise BadRequestError("response error must be a JSON object")
        code = error.get("code")
        message = error.get("message")
        if not isinstance(code, str) or not isinstance(message, str):
            raise BadRequestError("response error code/message must be strings")
        error = {"code": code, "message": message}

    return DaemonResponse(
        request_id=request_id,
        ok=ok,
        data=payload.get("data", {}),
        stdout=stdout,
        stderr=stderr,
        exit_code=exit_code,
        error=error,
    )


def _json_line(payload: dict[str, Any]) -> bytes:
    return (
        json.dumps(payload, ensure_ascii=False, separators=(",", ":")) + "\n"
    ).encode("utf-8")
