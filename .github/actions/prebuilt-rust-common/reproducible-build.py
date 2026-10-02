#!/usr/bin/env python3
"""Run a Rust build command with stable source paths and timestamps."""

from __future__ import annotations

import argparse
import os
import subprocess
import sys
from pathlib import Path


ENCODED_SEPARATOR = "\x1f"


def rustflags_from_environment(env: dict[str, str]) -> list[str]:
    """Return the Rust flags Cargo would use before reproducibility flags."""
    encoded = env.get("CARGO_ENCODED_RUSTFLAGS")
    if encoded is not None:
        return [item for item in encoded.split(ENCODED_SEPARATOR) if item]
    return env.get("RUSTFLAGS", "").split()


def rust_sysroot(env: dict[str, str]) -> Path:
    """Resolve the active host toolchain root for path remapping."""
    try:
        result = subprocess.run(
            ["rustc", "--print", "sysroot"],
            check=True,
            env=env,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
        )
    except (OSError, subprocess.CalledProcessError) as error:
        detail = getattr(error, "stderr", "") or str(error)
        raise RuntimeError(f"failed to query rustc sysroot: {detail.strip()}") from error
    value = result.stdout.strip()
    if not value:
        raise RuntimeError("rustc returned an empty sysroot")
    return Path(value).resolve()


def reproducible_environment(
    source_root: Path,
    source_date_epoch: str,
    base_env: dict[str, str],
) -> dict[str, str]:
    """Build an environment that removes host-specific Rust source prefixes."""
    env = base_env.copy()
    flags = rustflags_from_environment(env)
    cargo_home = Path(env.get("CARGO_HOME", str(Path.home() / ".cargo"))).resolve()
    registry_sources = cargo_home / "registry" / "src"
    try:
        source_ids = sorted(path.name for path in registry_sources.iterdir() if path.is_dir())
    except FileNotFoundError:
        source_ids = []
    except OSError as error:
        raise RuntimeError(f"cannot list Cargo registry sources: {error}") from error
    mappings = []
    for source_id in source_ids:
        canonical = "/cargo/registry/src/canonical"
        mappings.extend(
            [
                (registry_sources / source_id, canonical),
                (Path("/cargo/registry/src") / source_id, canonical),
            ]
        )
    mappings.extend(
        [
            (source_root.resolve(), "/workspace"),
            (Path("/project"), "/workspace"),
            (cargo_home, "/cargo"),
            (rust_sysroot(env), "/rust-toolchain"),
        ]
    )
    for source, destination in mappings:
        flag = f"--remap-path-prefix={source}={destination}"
        if flag not in flags:
            flags.append(flag)
    env["CARGO_ENCODED_RUSTFLAGS"] = ENCODED_SEPARATOR.join(flags)
    env.pop("RUSTFLAGS", None)
    env["SOURCE_DATE_EPOCH"] = source_date_epoch
    return env


def parse_args(argv: list[str]) -> argparse.Namespace:
    """Parse the fixed source root, epoch, and delegated command."""
    parser = argparse.ArgumentParser()
    parser.add_argument("--source-root", required=True, type=Path)
    parser.add_argument("--source-date-epoch", required=True)
    parser.add_argument("command", nargs=argparse.REMAINDER)
    args = parser.parse_args(argv)
    if args.command and args.command[0] == "--":
        args.command = args.command[1:]
    if not args.command:
        parser.error("a command is required after --")
    if not args.source_date_epoch.isdigit():
        parser.error("--source-date-epoch must be a non-negative integer")
    if not args.source_root.is_dir():
        parser.error(f"--source-root is not a directory: {args.source_root}")
    return args


def main(argv: list[str]) -> int:
    """Replace this process with the delegated reproducible build command."""
    args = parse_args(argv)
    try:
        env = reproducible_environment(
            args.source_root,
            args.source_date_epoch,
            os.environ,
        )
    except RuntimeError as error:
        print(f"ERROR: {error}", file=sys.stderr)
        return 1
    try:
        os.execvpe(args.command[0], args.command, env)
    except OSError as error:
        print(f"ERROR: failed to execute {args.command[0]}: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
