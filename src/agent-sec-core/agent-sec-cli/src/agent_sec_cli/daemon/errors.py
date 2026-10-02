"""Stable daemon error codes and exceptions."""

BAD_REQUEST = "bad_request"
UNKNOWN_METHOD = "unknown_method"
PAYLOAD_TOO_LARGE = "payload_too_large"
TIMEOUT = "timeout"
BUSY = "busy"
UNAVAILABLE = "unavailable"
INTERNAL_ERROR = "internal_error"
SHUTDOWN = "shutdown"


class DaemonError(Exception):
    """Base class for daemon protocol and runtime errors."""

    def __init__(self, code: str, message: str, exit_code: int = 1) -> None:
        super().__init__(message)
        self.code = code
        self.message = message
        self.exit_code = exit_code


class BadRequestError(DaemonError):
    """Request validation failed before dispatch."""

    def __init__(self, message: str) -> None:
        super().__init__(BAD_REQUEST, message)


class UnknownMethodError(DaemonError):
    """Requested method is not present in the allowlist registry."""

    def __init__(self, method: str) -> None:
        super().__init__(UNKNOWN_METHOD, f"unknown daemon method: {method}")


class PayloadTooLargeError(DaemonError):
    """Request payload exceeded the daemon byte limit."""

    def __init__(self, limit_bytes: int) -> None:
        super().__init__(
            PAYLOAD_TOO_LARGE, f"request payload exceeds {limit_bytes} bytes"
        )
        self.limit_bytes = limit_bytes


class ResponseTooLargeError(DaemonError):
    """Response payload exceeded the daemon byte limit."""

    def __init__(self, limit_bytes: int) -> None:
        super().__init__(
            PAYLOAD_TOO_LARGE, f"response payload exceeds {limit_bytes} bytes"
        )
        self.limit_bytes = limit_bytes


class DaemonTimeoutError(DaemonError):
    """Request exceeded its execution deadline."""

    def __init__(self, timeout_ms: int) -> None:
        super().__init__(TIMEOUT, f"daemon request timed out after {timeout_ms} ms")
        self.timeout_ms = timeout_ms


class BusyError(DaemonError):
    """Daemon concurrency or queue limits are exhausted."""

    def __init__(self) -> None:
        super().__init__(BUSY, "daemon is busy")


class UnavailableError(DaemonError):
    """A daemon capability is temporarily unavailable."""

    def __init__(self, message: str) -> None:
        super().__init__(UNAVAILABLE, message)


class InternalDaemonError(DaemonError):
    """Unexpected daemon-side failure."""

    def __init__(self, message: str = "daemon internal error") -> None:
        super().__init__(INTERNAL_ERROR, message)


class ShutdownError(DaemonError):
    """Daemon is shutting down and cannot accept work."""

    def __init__(self) -> None:
        super().__init__(SHUTDOWN, "daemon is shutting down")


class DaemonRuntimePathError(DaemonError):
    """Runtime directory or socket path is invalid for daemon use."""

    def __init__(self, message: str) -> None:
        super().__init__(UNAVAILABLE, message)


class DaemonAlreadyRunningError(DaemonError):
    """Another daemon instance already owns the runtime socket or lock."""

    def __init__(self, message: str = "agent-sec daemon is already running") -> None:
        super().__init__(UNAVAILABLE, message)


class DaemonClientError(Exception):
    """Base class for daemon client transport/protocol failures."""


class DaemonTransportError(DaemonClientError):
    """The daemon socket could not be reached or completed."""


class DaemonProtocolError(DaemonClientError):
    """The daemon returned an invalid protocol response."""


class DaemonClientTimeoutError(DaemonTransportError):
    """The daemon client timed out while connecting or waiting for response."""
