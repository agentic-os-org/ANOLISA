"""AgentScope integration for the Tokenless lifecycle."""

import re

import agentscope
from anolisa_tokenless import TokenlessConfig

from tokenless_agentscope._contracts import ToolContract

_VERSION_MATCH = re.match(r"^(\d+)\.(\d+)\.(\d+)", agentscope.__version__)
if _VERSION_MATCH is None:
    raise ImportError(f"Cannot parse AgentScope version {agentscope.__version__!r}")
_VERSION = tuple(int(part) for part in _VERSION_MATCH.groups())

if (1, 0, 11) <= _VERSION < (1, 1, 0):
    from tokenless_agentscope._v1 import TokenlessAgentScope

    __all__ = ["TokenlessAgentScope", "TokenlessConfig", "ToolContract"]
elif (2, 0, 0) <= _VERSION < (2, 1, 0):
    from tokenless_agentscope._v2 import (
        TokenlessAgentScope,
        TokenlessMiddleware,
    )

    __all__ = [
        "TokenlessAgentScope",
        "TokenlessConfig",
        "TokenlessMiddleware",
        "ToolContract",
    ]
else:
    raise ImportError(
        "anolisa-tokenless-agentscope supports AgentScope 1.0.11 through "
        "1.0.x and 2.0.x only, "
        f"not {agentscope.__version__}",
    )
