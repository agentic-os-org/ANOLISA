#!/usr/bin/env python3
"""Validate ANOLISA CLI release metadata before prebuilt packaging."""

from __future__ import annotations

import argparse
import json
import re
import tomllib
from pathlib import Path


SEMVER = re.compile(
    r"^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)"
    r"(?:-[0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*)?"
    r"(?:\+[0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*)?$"
)
TARGET_PACKAGES = {
    ("linux", "x86_64"): ("linux-x64", "linux", "x64"),
    ("linux", "aarch64"): ("linux-arm64", "linux", "arm64"),
    ("macos", "aarch64"): ("darwin-arm64", "darwin", "arm64"),
}


def read_toml(path: Path) -> dict[str, object]:
    """Read one TOML document with path-aware errors."""
    try:
        with path.open("rb") as stream:
            return tomllib.load(stream)
    except (OSError, tomllib.TOMLDecodeError) as error:
        raise SystemExit(f"ERROR: cannot read {path}: {error}") from error


def read_json(path: Path) -> dict[str, object]:
    """Read one JSON object with path-aware errors."""
    try:
        value = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, UnicodeError, json.JSONDecodeError) as error:
        raise SystemExit(f"ERROR: cannot read {path}: {error}") from error
    if not isinstance(value, dict):
        raise SystemExit(f"ERROR: {path} must contain a JSON object")
    return value


def source_version(root: Path) -> str:
    """Return the authoritative workspace version."""
    cargo = read_toml(root / "Cargo.toml")
    workspace = cargo.get("workspace")
    package = workspace.get("package") if isinstance(workspace, dict) else None
    version = package.get("version") if isinstance(package, dict) else None
    if not isinstance(version, str) or SEMVER.fullmatch(version) is None:
        raise SystemExit(f"ERROR: {root / 'Cargo.toml'} has no valid workspace version")
    return version


def verify_release(root: Path, os_name: str, arch: str) -> str:
    """Return the synchronized version after validating npm release metadata."""
    target = TARGET_PACKAGES.get((os_name, arch))
    if target is None:
        raise SystemExit(f"ERROR: unsupported ANOLISA CLI target: {os_name}/{arch}")
    suffix, npm_os, npm_cpu = target
    version = source_version(root)
    npm_root = root / "npm"
    root_package = read_json(npm_root / "package.json")
    if root_package.get("name") != "@anolisa/cli":
        raise SystemExit(f"ERROR: {npm_root / 'package.json'} has the wrong package name")
    if root_package.get("version") != version:
        raise SystemExit(
            f"ERROR: {npm_root / 'package.json'} version does not match {version}"
        )

    platform_packages: dict[str, str] = {}
    platforms = npm_root / "platforms"
    try:
        package_files = sorted(platforms.glob("*/package.json"))
    except OSError as error:
        raise SystemExit(f"ERROR: cannot list {platforms}: {error}") from error
    if not package_files:
        raise SystemExit(f"ERROR: no npm platform packages found under {platforms}")
    for package_file in package_files:
        package = read_json(package_file)
        name = package.get("name")
        package_version = package.get("version")
        if not isinstance(name, str) or not name.startswith("@anolisa/cli-"):
            raise SystemExit(f"ERROR: {package_file} has the wrong package name")
        if package_version != version:
            raise SystemExit(f"ERROR: {package_file} version does not match {version}")
        platform_packages[name] = version

    optional = root_package.get("optionalDependencies")
    if optional != platform_packages:
        raise SystemExit(
            f"ERROR: {npm_root / 'package.json'} optionalDependencies do not match "
            "the platform packages"
        )

    target_file = platforms / suffix / "package.json"
    target_package = read_json(target_file)
    if (
        target_package.get("name") != f"@anolisa/cli-{suffix}"
        or target_package.get("os") != [npm_os]
        or target_package.get("cpu") != [npm_cpu]
    ):
        raise SystemExit(f"ERROR: {target_file} does not describe {os_name}/{arch}")
    return version


def main() -> int:
    """Print the verified ANOLISA CLI version."""
    parser = argparse.ArgumentParser()
    parser.add_argument("source_root", type=Path)
    parser.add_argument("--os", choices=("linux", "macos"), required=True)
    parser.add_argument("--arch", choices=("x86_64", "aarch64"), required=True)
    args = parser.parse_args()
    print(verify_release(args.source_root.resolve(), args.os, args.arch))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
