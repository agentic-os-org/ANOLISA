"""Drive the real Hermes capability without claiming native-host acceptance."""

import json
import logging
import sys
from typing import Any, Callable

from src.capabilities.skill_ledger import SkillLedgerCapability


class HookContext:
    """Capture the capability's wrapped callbacks."""

    def __init__(self) -> None:
        self.hooks = {}

    def register_hook(self, name: str, callback: Callable[..., Any]) -> None:
        self.hooks[name] = callback


logging.basicConfig(level=logging.DEBUG)
request = json.load(sys.stdin)
context = HookContext()
SkillLedgerCapability().register(context, {"timeout": 5})
print(json.dumps(context.hooks["pre_tool_call"](**request)))
