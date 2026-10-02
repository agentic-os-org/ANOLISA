"""Shared utility helpers for skill-ledger."""

import os
from datetime import datetime, timezone


def utc_now_iso() -> str:
    """Return the current UTC time as an ISO-8601 string."""
    return datetime.now(timezone.utc).isoformat()


def validate_skill_dir(skill_dir: str) -> None:
    """Validate that *skill_dir* exists, is a directory, and contains ``SKILL.md``.

    Raises :class:`ValueError` on any validation failure.  Callers higher in
    the stack (the middleware backend) catch generic ``Exception`` and convert
    to ``{"status": "error", ...}``, exit 1.  When invoking core functions
    directly (outside the middleware), the caller is responsible for catching
    ``ValueError``.
    """
    if not os.path.isdir(skill_dir):
        raise ValueError(
            f"skill directory does not exist or is not a directory: {skill_dir}"
        )
    if not os.path.isfile(os.path.join(skill_dir, "SKILL.md")):
        raise ValueError(f"SKILL.md not found in skill directory: {skill_dir}")
