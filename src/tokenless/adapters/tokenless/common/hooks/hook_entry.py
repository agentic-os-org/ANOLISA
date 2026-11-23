"""Bootstrap historical hook commands; reusable runtime code is host-independent."""

import importlib.util
import sys
from pathlib import Path

# Extensions carry a compact adapter tree; full installations use siblings.
_here = Path(__file__).resolve().parent
_adapter_root = _here / "adapters"
if not _adapter_root.is_dir():
    _adapter_root = _here.parent.parent
sys.path.insert(0, str(_adapter_root))

from hook_runtime import run_hook
from hook_utils import skip, warn


def main(operation: str) -> None:
    try:
        # Only detached plugins place shared hooks under <plugin>/hooks/shared.
        # An extension's <extension>/hooks must never probe its parent's adapter.
        if _here.name == "shared" and _here.parent.name == "hooks":
            spec = importlib.util.spec_from_file_location(
                "tokenless_local_adapter", _here.parent.parent / "hook_adapter.py"
            )
            adapter = importlib.util.module_from_spec(spec)
            spec.loader.exec_module(adapter)
            agent_id = adapter.AGENT_ID
        else:
            from hook_dispatch import load_adapter, resolve_agent_id

            agent_id = resolve_agent_id()
            adapter = load_adapter(agent_id)
    except (FileNotFoundError, ImportError) as error:
        warn(f"Hook adapter unavailable: {error}. Passing through unchanged.")
        skip()
    run_hook(operation, adapter, agent_id)
