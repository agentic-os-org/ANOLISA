#!/usr/bin/env python3
"""Verify a release binary's architecture and glibc ceiling."""

from __future__ import annotations

import argparse
import re
import subprocess
import sys
from pathlib import Path


GLIBC_RE = re.compile(r"\bGLIBC_(\d+)\.(\d+)(?:\.(\d+))?\b")
MACHINES = {
    "x86_64": "Advanced Micro Devices X86-64",
    "aarch64": "AArch64",
}


def version(value: str) -> tuple[int, ...]:
    """Parse a dotted numeric version."""
    try:
        return tuple(int(item) for item in value.split("."))
    except ValueError as error:
        raise argparse.ArgumentTypeError(f"invalid numeric version: {value}") from error


def format_version(value: tuple[int, ...]) -> str:
    """Format a symbol version without a redundant patch zero."""
    items = list(value)
    while len(items) > 2 and items[-1] == 0:
        items.pop()
    return ".".join(str(item) for item in items)


def readelf(binary: Path, *args: str) -> str:
    """Run readelf and return its standard output."""
    try:
        result = subprocess.run(
            ["readelf", *args, str(binary)],
            check=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
        )
    except FileNotFoundError:
        raise RuntimeError("readelf is required to verify release binaries") from None
    except subprocess.CalledProcessError as error:
        detail = error.stderr.strip() or error.stdout.strip()
        raise RuntimeError(f"readelf failed for {binary}: {detail}") from error
    return result.stdout


def main() -> int:
    """Validate one ELF binary."""
    parser = argparse.ArgumentParser()
    parser.add_argument("binary", type=Path)
    parser.add_argument("--arch", choices=sorted(MACHINES), required=True)
    parser.add_argument("--max", dest="maximum", type=version, required=True)
    args = parser.parse_args()

    if not args.binary.is_file():
        print(f"ERROR: binary does not exist: {args.binary}", file=sys.stderr)
        return 1
    try:
        header = readelf(args.binary, "-h")
        symbols = readelf(args.binary, "--version-info")
    except RuntimeError as error:
        print(f"ERROR: {error}", file=sys.stderr)
        return 1

    machine = re.search(r"^\s*Machine:\s*(.+?)\s*$", header, re.MULTILINE)
    actual_machine = machine.group(1) if machine is not None else ""
    if actual_machine != MACHINES[args.arch]:
        print(
            f"ERROR: {args.binary} machine is {actual_machine!r}, "
            f"expected {MACHINES[args.arch]!r}",
            file=sys.stderr,
        )
        return 1
    required = {
        tuple(int(item) for item in match.groups(default="0"))
        for match in GLIBC_RE.finditer(symbols)
    }
    if not required:
        print(f"ERROR: no GLIBC symbol versions found in {args.binary}", file=sys.stderr)
        return 1
    maximum = max(required)
    padded_limit = args.maximum + (0,) * (len(maximum) - len(args.maximum))
    if maximum > padded_limit:
        print(
            f"ERROR: {args.binary} requires GLIBC_{format_version(maximum)}, "
            f"above GLIBC_{format_version(args.maximum)}",
            file=sys.stderr,
        )
        return 1
    print(f"{args.binary}: arch={args.arch} max_glibc=GLIBC_{format_version(maximum)}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
