#!/usr/bin/env python3
"""Build selected AW and Provider programs, then assemble independent archives."""

from __future__ import annotations

import argparse
import os
import platform
import sys
from pathlib import Path

from common import Cancellation, run
from package import assemble, components, validate

SOURCE = Path(__file__).resolve().parents[3]


def build_targets(
    repo: Path, selection: str | list[str] | tuple[str, ...]
) -> list[tuple[Path, tuple[str, ...]]]:
    """Select framework and bridge programs without backend source builds."""
    selected = components(selection)
    targets = []
    if "core" in selected:
        targets.append((repo / "src/aw", ("aw-cli",)))
    if "sec-core" in selected:
        targets.append((repo / "providers/agent-sec-core", ("aw-provider-sec-core",)))
    if "tokenless" in selected:
        targets.append((repo / "providers/tokenless", ("aw-provider-tokenless",)))
    return targets


def source_commit(repo: Path, cancel: Cancellation) -> str:
    """Require a clean source tree before any program is compiled."""
    if run(["git", "status", "--porcelain", "--untracked-files=normal"], repo, cancel, 10).strip():
        raise ValueError("commit the source tree before building")
    return run(["git", "rev-parse", "HEAD"], repo, cancel, 10).decode().strip()


def recheck_source(repo: Path, source: str, cancel: Cancellation) -> None:
    """Reject source changes before publishing an artifact."""
    if (
        run(["git", "status", "--porcelain", "--untracked-files=normal"], repo, cancel, 10).strip()
        or run(["git", "rev-parse", "HEAD"], repo, cancel, 10).decode().strip() != source
    ):
        raise ValueError("source checkout changed during the build")


def compile_target(
    directory: Path, packages: tuple[str, ...], cancel: Cancellation, target: Path | None = None
) -> None:
    """Compile one locked source workspace into its selected build directory."""
    command = [
        os.environ.get("CARGO", "cargo"),
        "+1.97.1",
        "build",
        "--release",
        "--locked",
        "--bins",
        "--target-dir",
        str(target or directory / "target"),
    ]
    for package in packages:
        command += ["-p", package]
    run(command, directory, cancel)


def build(
    repo: Path,
    version: str,
    selection: str | list[str] | tuple[str, ...],
    output: Path,
    cancel: Cancellation,
) -> list[Path]:
    """Build only the selected package programs from one unchanged source commit."""
    if platform.system() != "Linux":
        raise ValueError("AW builds require native Linux x86_64 or aarch64")
    arch = platform.machine()
    validate(version, selection, arch, "0" * 40)
    if not output.is_absolute():
        raise ValueError("output must be absolute")
    if output.exists() and any(output.iterdir()):
        raise ValueError("output directory must be empty")
    source = source_commit(repo, cancel)
    validate(version, selection, arch, source)
    for directory, packages in build_targets(repo, selection):
        compile_target(directory, packages, cancel)
    recheck_source(repo, source, cancel)
    return assemble(repo, version, source, output, selection, arch, cancel)


def main() -> int:
    try:
        for argument in sys.argv[1:]:
            argument.encode("utf-8")
    except UnicodeEncodeError:
        print("aw-packages: process arguments must be UTF-8", file=sys.stderr)
        return 1
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--version", required=True)
    parser.add_argument("--components", nargs="+", default=["core"])
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    cancel = Cancellation()
    cancel.register()
    try:
        for archive in build(SOURCE, args.version, args.components, args.output, cancel):
            print(archive)
    except (OSError, RuntimeError, ValueError) as error:
        print(f"aw-packages: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
