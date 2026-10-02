"""Unit tests for security_middleware.router — action→backend routing."""

import pytest
from agent_sec_cli.security_middleware import router
from agent_sec_cli.security_middleware.backends.sandbox import SandboxBackend


def test_unknown_action_raises_value_error():
    with pytest.raises(ValueError, match="nonexistent_action"):
        router.get_backend("nonexistent_action")


def test_sandbox_prehook_returns_backend():
    backend = router.get_backend("sandbox_prehook")
    assert isinstance(backend, SandboxBackend)
    assert hasattr(backend, "execute")


def test_all_registered_actions_work():
    """Test that all registered actions can be retrieved."""
    actions = [
        "sandbox_prehook",
        "harden",
        "verify",
        "summary",
        "code_scan",
        "prompt_scan",
        "pii_scan",
        "skill_ledger",
    ]
    for action in actions:
        backend = router.get_backend(action)
        assert hasattr(backend, "execute"), f"Action {action!r} missing execute method"


def test_backend_is_cached():
    b1 = router.get_backend("sandbox_prehook")
    b2 = router.get_backend("sandbox_prehook")
    assert b1 is b2
