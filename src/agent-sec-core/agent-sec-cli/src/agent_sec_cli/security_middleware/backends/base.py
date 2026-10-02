"""Abstract base class for all security middleware backends."""

import copy
from abc import ABC, abstractmethod
from typing import Any

from agent_sec_cli.security_middleware.context import RequestContext
from agent_sec_cli.security_middleware.result import ActionResult


class BaseBackend(ABC):
    """All backend implementations must inherit from this class."""

    @abstractmethod
    def execute(self, ctx: RequestContext, **kwargs: Any) -> ActionResult:
        """Execute the backend action and return a unified ActionResult."""
        pass

    def build_event_details(
        self, result: ActionResult, kwargs: dict[str, Any]
    ) -> dict[str, Any]:
        """Build success audit details for the lifecycle event."""
        details = {
            "request": copy.deepcopy(kwargs),
            "result": copy.deepcopy(result.data),
        }
        if not result.success and result.error:
            details["error"] = result.error
        return details

    def build_error_details(
        self, exception: Exception, kwargs: dict[str, Any]
    ) -> dict[str, Any]:
        """Build failure audit details for the lifecycle event."""
        return {
            "request": copy.deepcopy(kwargs),
            "error": str(exception),
            "error_type": type(exception).__name__,
        }
