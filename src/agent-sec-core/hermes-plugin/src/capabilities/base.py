"""Abstract base class for all security capabilities."""

from __future__ import annotations

import logging
from abc import ABC, abstractmethod
from typing import Callable, final

from ..registry import safe_hook_wrapper

logger = logging.getLogger("agent-sec-core")


class AgentSecCoreCapability(ABC):
    """Base class for security capabilities.

    Subclasses MUST define:
        id (property)             - unique capability identifier (matches config.toml section)
        name (property)           - human-readable name
        _on_register(config)      - read capability-specific config
        get_hooks_define() -> dict - return hook_name -> callback mapping
    """

    @property
    @abstractmethod
    def id(self) -> str:
        """Unique capability identifier, must match config.toml section name."""
        pass

    @property
    @abstractmethod
    def name(self) -> str:
        """Human-readable capability name."""
        pass

    def __init__(self):
        self._timeout: float  # must be set via config

    @final
    def register(self, ctx, config: dict) -> None:
        """Parse common config and register hooks."""
        if "timeout" not in config:
            raise ValueError(f"[{self.id}] config missing required key 'timeout'")
        self._timeout = config["timeout"]
        self._on_register(config)
        for hook_name, callback_func in self.get_hooks_define().items():
            wrapper_func = safe_hook_wrapper(callback_func, self.id)
            ctx.register_hook(hook_name, wrapper_func)

    @abstractmethod
    def _on_register(self, config: dict) -> None:
        """Read capability-specific config. Subclass must implement.

        If no extra config is needed, simply ``pass``.
        """
        pass

    @abstractmethod
    def get_hooks_define(self) -> dict[str, Callable]:
        """Return mapping of hook_name -> callback method.

        Example::

            def get_hooks_define(self):
                return {"pre_tool_call": self._on_pre_tool_call}
        """
        pass
