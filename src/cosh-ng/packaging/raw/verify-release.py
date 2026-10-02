#!/usr/bin/env python3
"""Validate cosh-ng release metadata before raw packaging."""

from __future__ import annotations

import argparse
import re
import tomllib
from pathlib import Path


SEMVER = re.compile(
    r"^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)"
    r"(?:-[0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*)?"
    r"(?:\+[0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*)?$"
)
REQUIRED_SOURCES = {
    ".anolisa/component.toml",
    "bin/cosh-cli",
    "bin/cosh",
    "bin/cosh-switch",
    "libexec/anolisa/cosh-ng/cosh-core",
    "libexec/anolisa/cosh-ng/cosh-shell",
    "share/doc/cosh-ng/LICENSE",
    "share/doc/cosh-ng/README.md",
}
CONFLICT_AWARE_ANOLISA_VERSION = "0.2.17"


def read_toml(path: Path) -> dict[str, object]:
    """Read one TOML document with path-aware errors."""
    try:
        with path.open("rb") as stream:
            return tomllib.load(stream)
    except (OSError, tomllib.TOMLDecodeError) as error:
        raise SystemExit(f"ERROR: cannot read {path}: {error}") from error


def release_key(version: str) -> tuple[int, int, int, bool]:
    """Order SemVer releases against a stable minimum version."""
    core = version.split("+", 1)[0]
    numbers, separator, _prerelease = core.partition("-")
    major, minor, patch = (int(part) for part in numbers.split("."))
    return major, minor, patch, not separator


def verify_release(root: Path, contract_path: Path, os_name: str, arch: str) -> str:
    """Return the synchronized version after validating the raw contract."""
    cargo = read_toml(root / "Cargo.toml")
    workspace = cargo.get("workspace")
    package = workspace.get("package") if isinstance(workspace, dict) else None
    source_version = package.get("version") if isinstance(package, dict) else None
    if not isinstance(source_version, str) or SEMVER.fullmatch(source_version) is None:
        raise SystemExit(f"ERROR: {root / 'Cargo.toml'} has no valid workspace version")

    contract = read_toml(contract_path)
    component = contract.get("component")
    if not isinstance(component, dict):
        raise SystemExit(f"ERROR: {contract_path} has no [component] table")
    if component.get("name") != "cosh-ng":
        raise SystemExit(f"ERROR: {contract_path} is not a cosh-ng contract")
    contract_version = component.get("version")
    if contract_version != source_version:
        raise SystemExit(
            f"ERROR: {contract_path} version {contract_version!r} does not match "
            f"Cargo.toml version {source_version}"
        )

    contract_spec = component.get("contract")
    minimum_version = (
        contract_spec.get("min_anolisa_version")
        if isinstance(contract_spec, dict)
        else None
    )
    if not isinstance(minimum_version, str) or SEMVER.fullmatch(minimum_version) is None:
        raise SystemExit(
            f"ERROR: {contract_path} has no valid component.contract.min_anolisa_version"
        )
    conflicts = component.get("conflicts")
    if (
        isinstance(conflicts, list)
        and conflicts
        and release_key(minimum_version)
        < release_key(CONFLICT_AWARE_ANOLISA_VERSION)
    ):
        raise SystemExit(
            f"ERROR: {contract_path} uses component conflicts but allows ANOLISA "
            f"{minimum_version}; require {CONFLICT_AWARE_ANOLISA_VERSION} or newer"
        )

    platform = component.get("platform")
    operating_systems = platform.get("os") if isinstance(platform, dict) else None
    architectures = platform.get("arch") if isinstance(platform, dict) else None
    if not isinstance(operating_systems, list) or os_name not in operating_systems:
        raise SystemExit(f"ERROR: {contract_path} does not support target OS {os_name}")
    if not isinstance(architectures, list) or arch not in architectures:
        raise SystemExit(f"ERROR: {contract_path} does not support target architecture {arch}")

    layout = component.get("layout")
    files = layout.get("files") if isinstance(layout, dict) else None
    if not isinstance(files, list):
        raise SystemExit(f"ERROR: {contract_path} has no component layout files")
    sources = {
        row.get("source")
        for row in files
        if isinstance(row, dict) and isinstance(row.get("source"), str)
    }
    missing_sources = sorted(REQUIRED_SOURCES - sources)
    if missing_sources:
        raise SystemExit(
            f"ERROR: {contract_path} is missing packaged layout sources: "
            + ", ".join(missing_sources)
        )

    return source_version


def parse_args() -> argparse.Namespace:
    """Parse source, contract, and target identity arguments."""
    parser = argparse.ArgumentParser()
    parser.add_argument("source_root", type=Path)
    parser.add_argument("contract", type=Path)
    parser.add_argument("--os", choices=("linux", "macos"), required=True)
    parser.add_argument("--arch", choices=("x86_64", "aarch64"), required=True)
    return parser.parse_args()


def main() -> int:
    """Print the verified cosh-ng release version."""
    args = parse_args()
    print(
        verify_release(
            args.source_root.resolve(),
            args.contract.resolve(),
            args.os,
            args.arch,
        )
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
