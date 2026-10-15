"""Drive the installed Hermes plugin without starting a Hermes host."""

import importlib
import importlib.util
import json
import os
import sys
from pathlib import Path


def _load_raw_plugin():
    plugin_root = Path(os.environ["PII_TEST_HERMES_PLUGIN_ROOT"])
    spec = importlib.util.spec_from_file_location(
        "raw_hermes_plugin",
        plugin_root / "__init__.py",
        submodule_search_locations=[str(plugin_root)],
    )
    assert spec is not None
    assert spec.loader is not None
    package = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = package
    spec.loader.exec_module(package)
    pii_scan = importlib.import_module(f"{spec.name}.capabilities.pii_scan")
    cli_runner = importlib.import_module(f"{spec.name}.cli_runner")
    return pii_scan.PiiScanCapability, cli_runner.record_hermes_observability


if "PII_TEST_HERMES_PLUGIN_ROOT" in os.environ:
    PiiScanCapability, record_hermes_observability = _load_raw_plugin()
else:
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
