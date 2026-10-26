"""Shared target definitions for prebuilt release packages."""

from __future__ import annotations

import runpy
from pathlib import Path

TARGETS = {
    "agent-sec-core": (
        {"target-os": "linux", "target-arch": "x86_64", "profile": "gnu2.28-x86_64"},
    ),
    "os-skills": (
        {"target-os": "linux", "target-arch": "x86_64", "profile": "static-linux"},
        {"target-os": "linux", "target-arch": "aarch64", "profile": "static-linux"},
    ),
    "anolisa": (
        {
            "target-os": "linux",
            "target-arch": "x86_64",
            "profile": "gnu2.17-x86_64",
        },
        {
            "target-os": "linux",
            "target-arch": "aarch64",
            "profile": "gnu2.17-aarch64",
        },
        {
            "target-os": "macos",
            "target-arch": "aarch64",
            "profile": "darwin11-aarch64",
        },
        {
            "target-os": "macos",
            "target-arch": "x86_64",
            "profile": "darwin11-x86_64",
        },
    ),
    "ws-ckpt": (
        {
            "target-os": "linux",
            "target-arch": "x86_64",
            "profile": "gnu2.28-x86_64",
        },
        {
            "target-os": "linux",
            "target-arch": "aarch64",
            "profile": "gnu2.28-aarch64",
        },
    ),
    "skillfs": (
        {
            "target-os": "linux",
            "target-arch": "x86_64",
            "profile": "gnu2.28-x86_64",
        },
        {
            "target-os": "linux",
            "target-arch": "aarch64",
            "profile": "gnu2.28-aarch64",
        },
    ),
    "cosh-ng": (
        {
            "target-os": "linux",
            "target-arch": "x86_64",
            "profile": "gnu2.28-x86_64",
        },
        {
            "target-os": "linux",
            "target-arch": "aarch64",
            "profile": "gnu2.28-aarch64",
        },
        {
            "target-os": "macos",
            "target-arch": "aarch64",
            "profile": "darwin11-aarch64",
        },
        {
            "target-os": "macos",
            "target-arch": "x86_64",
            "profile": "darwin11-x86_64",
        },
    ),
    "agent-memory": (
        {"target-os": "linux", "target-arch": "x86_64", "profile": "gnu2.28-x86_64"},
        {"target-os": "linux", "target-arch": "aarch64", "profile": "gnu2.28-aarch64"},
    ),
    "agentsight": (
        {"target-os": "linux", "target-arch": "x86_64", "profile": "gnu2.28-x86_64"},
        {"target-os": "linux", "target-arch": "aarch64", "profile": "gnu2.28-aarch64"},
        {"target-os": "macos", "target-arch": "aarch64", "profile": "darwin11-aarch64"},
    ),
    "tokenless": (
        {
            "target-os": "linux",
            "target-arch": "x86_64",
            "profile": "gnu2.17-x86_64",
        },
        {
            "target-os": "linux",
            "target-arch": "aarch64",
            "profile": "gnu2.17-aarch64",
        },
        {
            "target-os": "macos",
            "target-arch": "aarch64",
            "profile": "darwin11-aarch64",
        },
        {
            "target-os": "macos",
            "target-arch": "x86_64",
            "profile": "darwin11-x86_64",
        },
        {
            "target-os": "windows",
            "target-arch": "x86_64",
            "profile": "windows-msvc-x86_64",
        },
    ),
}


def component_targets(
    component: str, tokenless_source_root: Path | None = None
) -> tuple[dict[str, str], ...]:
    """Keep historical Tokenless previews bound to their source target matrix."""
    targets = TARGETS.get(component, ())
    if component != "tokenless" or tokenless_source_root is None:
        return targets
    if not (tokenless_source_root / "src/tokenless/Cargo.toml").is_file():
        raise ValueError(f"Tokenless source manifest is missing under {tokenless_source_root}")
    source_targets = tokenless_source_root / ".github/actions/prebuilt-rust-common/targets.py"
    if source_targets.is_file():
        return tuple(runpy.run_path(str(source_targets))["TARGETS"]["tokenless"])
    # Older tags predate the shared matrix and ship the original three targets.
    return tuple(
        target for target in targets
        if target["profile"] not in ("darwin11-x86_64", "windows-msvc-x86_64")
    )
