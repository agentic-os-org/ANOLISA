"""Canonical path syntax shared by Skill Ledger entry points."""

import os
from pathlib import Path
from typing import Any


def validate_path_root_syntax(value: str, *, subject: str = "path") -> None:
    """Reject POSIX paths whose leading separators have ambiguous semantics."""
    if value.startswith("//"):
        raise ValueError(f"{subject} must use a single leading '/'")


def normalize_canonical_skill_dir(skill_dir: str | Path) -> Path:
    """Return an absolute, lexical canonical path without resolving symlinks."""
    raw = os.path.expanduser(os.fspath(skill_dir))
    if not raw:
        raise ValueError("canonical skill directory must not be empty")
    if "\x00" in raw:
        raise ValueError("canonical skill directory must not contain NUL")
    validate_path_root_syntax(raw, subject="canonical skill directory")
    return Path(os.path.abspath(os.path.normpath(raw)))


def validate_canonical_skill_dir(value: Any) -> Path:
    """Validate a protocol canonical path without touching the filesystem."""
    if not isinstance(value, str) or not value:
        raise ValueError("must be a non-empty string")
    if "\x00" in value:
        raise ValueError("must not contain NUL")
    validate_path_root_syntax(value, subject="canonical path")
    if not Path(value).is_absolute():
        raise ValueError("must be an absolute path")
    if os.path.normpath(value) != value:
        raise ValueError("must be lexically normalized")
    return Path(value)
