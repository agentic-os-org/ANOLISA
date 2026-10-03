#!/usr/bin/env python3
"""Shared hook configuration helpers for Qwen Code hooks."""

import os

_HOOK_POLICIES = frozenset({"observe", "warn", "ask", "block"})
_HOOK_POLICY_ALIASES = {"debug": "observe", "deny": "block"}


def env_flag_enabled(name: str, default: bool = True) -> bool:
    """Read a strict true/false environment flag."""
    value = os.environ.get(name)
    if value is None:
        return default
    normalized = value.strip().lower()
    if normalized == "true":
        return True
    if normalized == "false":
        return False
    return default


def normalize_hook_policy(value: object, default: str) -> str:
    """Normalize a hook policy, including supported compatibility aliases."""
    if not isinstance(value, str):
        return default
    normalized = value.strip().lower()
    normalized = _HOOK_POLICY_ALIASES.get(normalized, normalized)
    return normalized if normalized in _HOOK_POLICIES else default


def env_hook_policy(name: str, default: str) -> str:
    """Read and normalize a four-level hook policy environment variable."""
    return normalize_hook_policy(os.environ.get(name), default)
