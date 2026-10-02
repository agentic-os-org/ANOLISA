"""AgentScope 2.x middleware imports."""

from tokenless_agentscope import _v2
from tokenless_agentscope._contracts import ToolContract

TokenlessMiddleware = _v2.TokenlessMiddleware

__all__ = ["TokenlessMiddleware", "ToolContract"]
