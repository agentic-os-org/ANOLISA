"""Drive the installed Hermes plugin without starting a Hermes host."""

import json
import sys

from src.capabilities.pii_scan import PiiScanCapability
from src.cli_runner import record_hermes_observability


class HookContext:
    """Capture the real capability's registered, wrapped callbacks."""

    def __init__(self):
        self.hooks = {}

    def register_hook(self, name, callback):
        self.hooks[name] = callback


request = json.load(sys.stdin)
if request["hook"] == "observability":
    result = record_hermes_observability(request["event"])
    assert result.exit_code == 0, result.stderr
    print("null")
else:
    context = HookContext()
    PiiScanCapability().register(context, {"timeout": 10})
    print(json.dumps(context.hooks[request["hook"]](**request["event"])))
