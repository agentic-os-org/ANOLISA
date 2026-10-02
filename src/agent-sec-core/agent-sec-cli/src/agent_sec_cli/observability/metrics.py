"""Hook metric allowlist for observability record payloads.

The allowlist is derived from the typed schema models, not a runtime
``allow-list.conf``. Changing accepted metrics changes the public wire
contract and should go through the schema code path.
"""

from collections.abc import Mapping

from agent_sec_cli.observability.schema import (
    observability_hook_metric_allowlist,
)

HOOK_METRIC_ALLOWLIST: Mapping[str, frozenset[str]] = (
    observability_hook_metric_allowlist()
)


def allowed_metrics_for_hook(hook: str) -> frozenset[str]:
    """Return the metric names allowed for *hook*, or an empty set."""
    return HOOK_METRIC_ALLOWLIST.get(hook, frozenset())
