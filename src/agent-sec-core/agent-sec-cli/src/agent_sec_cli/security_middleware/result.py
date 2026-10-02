"""ActionResult — unified return type for all backend executions."""

from dataclasses import dataclass, field


@dataclass
class ActionResult:
    """Structured result returned by every backend ``execute()`` call.

    Attributes:
        success:    Whether the backend operation completed without error.
        data:       Backend-specific structured data (e.g. scan findings).
        stdout:     Text output suitable for CLI passthrough / display.
        exit_code:  Numeric exit code (0 = success, non-zero = failure).
        error:      Human-readable error description (empty on success).
        error_type: Structured, message-free product error classification.
    """

    success: bool
    data: dict = field(default_factory=dict)
    stdout: str = ""
    exit_code: int = 0
    error: str = ""
    error_type: str = ""
