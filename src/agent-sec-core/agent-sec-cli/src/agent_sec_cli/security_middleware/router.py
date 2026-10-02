"""Router — action name to backend instance registry with explicit imports."""

from typing import Any

from agent_sec_cli.security_middleware.backends.asset_verify import (
    AssetVerifyBackend,
)
from agent_sec_cli.security_middleware.backends.code_scan import CodeScanBackend
from agent_sec_cli.security_middleware.backends.hardening import (
    HardeningBackend,
)
from agent_sec_cli.security_middleware.backends.pii_scan import PiiScanBackend
from agent_sec_cli.security_middleware.backends.prompt_scan import (
    PromptScanBackend,
)
from agent_sec_cli.security_middleware.backends.sandbox import SandboxBackend
from agent_sec_cli.security_middleware.backends.skill_ledger import (
    SkillLedgerBackend,
)
from agent_sec_cli.security_middleware.backends.summary import SummaryBackend

# ---------------------------------------------------------------------------
# Action → backend class mapping (static, no hot-swapping allowed)
# ---------------------------------------------------------------------------

_BACKEND_CLASSES: dict[str, type] = {
    "sandbox_prehook": SandboxBackend,
    "harden": HardeningBackend,
    "verify": AssetVerifyBackend,
    "summary": SummaryBackend,
    "code_scan": CodeScanBackend,
    "prompt_scan": PromptScanBackend,
    "pii_scan": PiiScanBackend,
    "skill_ledger": SkillLedgerBackend,
}

# Cache of already-instantiated backends keyed by action.
_backend_cache: dict[str, Any] = {}


def get_backend(action: str) -> Any:
    """Return the backend instance responsible for *action*.

    The backend instance is created on first access and cached for subsequent calls.

    Raises:
        ValueError:  If *action* is not found in the registry.
    """
    if action not in _BACKEND_CLASSES:
        registered = ", ".join(sorted(_BACKEND_CLASSES))
        raise ValueError(f"Unknown action {action!r}. Registered actions: {registered}")

    if action in _backend_cache:
        return _backend_cache[action]

    backend_cls = _BACKEND_CLASSES[action]
    instance = backend_cls()

    _backend_cache[action] = instance
    return instance
