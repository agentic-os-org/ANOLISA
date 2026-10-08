"""Drive the real Hermes capability without claiming native-host acceptance."""

import importlib
import importlib.util
import json
import logging
import os
import sys
from pathlib import Path
from typing import Any, Callable


def _load_raw_plugin() -> type:
    plugin_root = Path(os.environ["SKILLSEC_TEST_HERMES_PLUGIN_ROOT"])
    spec = importlib.util.spec_from_file_location(
        "raw_hermes_skillsec_plugin",
        plugin_root / "__init__.py",
        submodule_search_locations=[str(plugin_root)],
    )
    assert spec is not None
    assert spec.loader is not None
    package = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = package
    spec.loader.exec_module(package)
    skill_ledger = importlib.import_module(f"{spec.name}.capabilities.skill_ledger")
    return skill_ledger.SkillLedgerCapability


if "SKILLSEC_TEST_HERMES_PLUGIN_ROOT" in os.environ:
    SkillLedgerCapability = _load_raw_plugin()
else:
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
