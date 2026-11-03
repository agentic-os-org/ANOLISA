#!/usr/bin/env python3
"""Print the wheel-filename spelling of a Cargo workspace version.

The QwenPaw requirements template pins GitHub Release asset URLs: the tag path
keeps the raw Cargo version (/tokenless/v0.9.0-rc.1/) while the wheel filename
carries its PEP 440 normalized form (anolisa_tokenless-0.9.0rc1-cp311-...),
which is what the pinned maturin 1.15.0 names it and the only spelling pip
installs from. verify-release.py owns that contract and the normalization
behind it, so the stamping rule in the Makefile asks here rather than carrying
a second implementation that could drift from the gate it has to satisfy.
"""

from __future__ import annotations

import importlib.util
import sys
from pathlib import Path


def load_verifier():
    """Import the sibling verify-release.py, whose name is not an identifier."""
    path = Path(__file__).resolve().with_name("verify-release.py")
    spec = importlib.util.spec_from_file_location("verify_release", path)
    if spec is None or spec.loader is None:
        raise SystemExit(f"ERROR: cannot import {path}")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def main(argv: list[str]) -> int:
    """Print the normalized spelling of exactly one version."""
    if len(argv) != 2 or not argv[1].strip():
        print("usage: wheel-version.py <cargo-version>", file=sys.stderr)
        return 2
    normalized = load_verifier()._normalize_pep440(argv[1].strip())
    if not normalized:
        raise SystemExit(f"ERROR: {argv[1]} normalizes to an empty version")
    print(normalized)
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv))
