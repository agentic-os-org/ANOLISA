#!/usr/bin/env python3
"""Verify one flat prebuilt package or a complete Actions Artifact download."""

from __future__ import annotations

import argparse
import hashlib
import json
import re
import sys
import tarfile
from pathlib import Path
from typing import Any

from targets import TARGETS


def die(message: str) -> None:
    """Exit with one actionable diagnostic."""
    print(f"ERROR: {message}", file=sys.stderr)
    raise SystemExit(1)


def sha256_file(path: Path) -> str:
    """Return a file's SHA-256 digest."""
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def read_sidecar(path: Path, filename: str) -> str:
    """Read a strict sha256sum sidecar for one adjacent file."""
    try:
        lines = path.read_text(encoding="utf-8").splitlines()
    except OSError as error:
        die(f"cannot read checksum sidecar {path}: {error}")
    if len(lines) != 1:
        die(f"checksum sidecar must contain exactly one line: {path}")
    match = re.fullmatch(r"([0-9a-fA-F]{64})  ([^/]+)", lines[0])
    if match is None or match.group(2) != filename:
        die(f"checksum sidecar does not identify {filename}: {path}")
    return match.group(1).lower()


def cargo_sbom_tool_present(metadata: dict[str, Any]) -> bool:
    """Accept both CycloneDX list and object representations of metadata.tools."""
    tools = metadata.get("tools")
    candidates: list[Any] = []
    if isinstance(tools, list):
        candidates.extend(tools)
    elif isinstance(tools, dict):
        for value in tools.values():
            if isinstance(value, list):
                candidates.extend(value)
    return any(
        isinstance(tool, dict)
        and tool.get("name") == "cargo-sbom"
        and tool.get("version") == "0.10.0"
        for tool in candidates
    )


def component_refs(component: dict[str, Any]) -> list[str]:
    """Collect non-empty bom-ref values from one nested component tree."""
    references: list[str] = []
    bom_ref = component.get("bom-ref")
    if isinstance(bom_ref, str) and bom_ref:
        references.append(bom_ref)
    nested = component.get("components")
    if isinstance(nested, list):
        for item in nested:
            if isinstance(item, dict):
                references.extend(component_refs(item))
    return references


def expected_files(
    component_name: str, version: str, target_os: str, target_arch: str
) -> set[str]:
    """Return the exact four release asset names for one target."""
    archive = f"{component_name}-{version}-{target_os}-{target_arch}.tar.gz"
    return {
        archive,
        f"{archive}.sha256",
        f"{archive}.cdx.json",
        f"{archive}.cdx.json.sha256",
    }


def validate_directory(
    directory: Path,
    component_name: str,
    version: str,
    target_os: str,
    target_arch: str,
) -> None:
    """Validate one target's exact four-file output directory."""
    expected = expected_files(component_name, version, target_os, target_arch)
    try:
        entries = list(directory.iterdir())
    except OSError as error:
        die(f"cannot list artifact directory {directory}: {error}")
    actual = {entry.name for entry in entries}
    if any(not entry.is_file() for entry in entries) or actual != expected:
        missing = sorted(expected - actual)
        extra = sorted(actual - expected)
        die(
            f"unexpected files for {target_os}/{target_arch}; "
            f"missing={missing or 'none'} extra={extra or 'none'}"
        )

    archive_name = f"{component_name}-{version}-{target_os}-{target_arch}.tar.gz"
    archive = directory / archive_name
    archive_sha = sha256_file(archive)
    if read_sidecar(Path(f"{archive}.sha256"), archive.name) != archive_sha:
        die(f"archive checksum mismatch: {archive}")
    try:
        with tarfile.open(archive, "r:gz") as package:
            if not package.getmembers():
                die(f"archive is empty: {archive}")
    except (OSError, tarfile.TarError) as error:
        die(f"cannot read archive {archive}: {error}")

    sbom = Path(f"{archive}.cdx.json")
    sbom_sha = sha256_file(sbom)
    if read_sidecar(Path(f"{sbom}.sha256"), sbom.name) != sbom_sha:
        die(f"SBOM checksum mismatch: {sbom}")
    try:
        data = json.loads(sbom.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        die(f"cannot read SBOM {sbom}: {error}")
    if not isinstance(data, dict):
        die(f"SBOM must contain a JSON object: {sbom}")
    if data.get("bomFormat") != "CycloneDX" or data.get("specVersion") != "1.6":
        die(f"SBOM is not CycloneDX 1.6: {sbom}")
    metadata = data.get("metadata")
    component = metadata.get("component") if isinstance(metadata, dict) else None
    artifact_id = f"{component_name}-{version}-{target_os}-{target_arch}-tar"
    if not isinstance(metadata, dict) or not isinstance(component, dict):
        die(f"SBOM is missing metadata.component: {sbom}")
    if (
        component.get("name") != component_name
        or component.get("version") != version
        or component.get("bom-ref") != artifact_id
    ):
        die(f"SBOM component identity mismatch: {sbom}")
    hashes = component.get("hashes")
    if not isinstance(hashes, list) or not any(
        isinstance(item, dict)
        and item.get("alg") == "SHA-256"
        and item.get("content") == archive_sha
        for item in hashes
    ):
        die(f"SBOM does not bind the archive checksum: {sbom}")
    properties = component.get("properties")
    property_map = {
        item.get("name"): item.get("value")
        for item in properties or []
        if isinstance(item, dict)
    }
    if (
        property_map.get("anolisa:artifact:name") != archive.name
        or property_map.get("anolisa:target:os") != target_os
        or property_map.get("anolisa:target:arch") != target_arch
    ):
        die(f"SBOM target properties mismatch: {sbom}")
    if not cargo_sbom_tool_present(metadata):
        die(f"SBOM does not identify cargo-sbom 0.10.0: {sbom}")

    components = data.get("components")
    dependencies = data.get("dependencies")
    if not isinstance(components, list) or not isinstance(dependencies, list):
        die(f"SBOM components and dependencies must be arrays: {sbom}")
    references = component_refs(component)
    for item in components:
        if not isinstance(item, dict):
            die(f"SBOM contains an invalid component: {sbom}")
        references.extend(component_refs(item))
    if len(references) != len(set(references)):
        die(f"SBOM contains duplicate component bom-ref values: {sbom}")
    dependency_refs = [
        item.get("ref") for item in dependencies if isinstance(item, dict)
    ]
    if (
        len(dependency_refs) != len(dependencies)
        or any(not isinstance(value, str) or not value for value in dependency_refs)
        or len(dependency_refs) != len(set(dependency_refs))
    ):
        die(f"SBOM contains invalid or duplicate dependency refs: {sbom}")
    known_refs = set(references)
    for dependency in dependencies:
        depends_on = dependency.get("dependsOn")
        if dependency.get("ref") not in known_refs:
            die(f"SBOM dependency ref does not resolve to a component: {sbom}")
        if not isinstance(depends_on, list) or any(
            not isinstance(value, str) or value not in known_refs
            for value in depends_on
        ):
            die(f"SBOM dependency target does not resolve to a component: {sbom}")


def main() -> int:
    """Validate flat or downloaded Actions Artifact layout."""
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--directory", required=True, type=Path)
    parser.add_argument("--component", required=True)
    parser.add_argument("--version", required=True)
    parser.add_argument("--layout", choices=("flat", "actions"), required=True)
    parser.add_argument("--os", dest="target_os")
    parser.add_argument("--arch", dest="target_arch")
    args = parser.parse_args()
    if not re.fullmatch(r"[a-z0-9][a-z0-9-]*", args.component):
        parser.error("component must be a lowercase release component name")
    root = args.directory.resolve()
    if args.layout == "flat":
        if not args.target_os or not args.target_arch:
            parser.error("flat layout requires --os and --arch")
        validate_directory(
            root,
            args.component,
            args.version,
            args.target_os,
            args.target_arch,
        )
        return 0
    if args.target_os or args.target_arch:
        parser.error("actions layout does not accept --os or --arch")

    component_targets = TARGETS.get(args.component)
    if component_targets is None:
        parser.error(f"component has no prebuilt target matrix: {args.component}")
    expected_directories = {
        f"{args.component}-prebuilt-{args.version}-"
        f"{target['target-os']}-{target['target-arch']}"
        for target in component_targets
    }
    try:
        entries = list(root.iterdir())
    except OSError as error:
        die(f"cannot list Actions Artifact root {root}: {error}")
    actual_directories = {entry.name for entry in entries}
    if any(not entry.is_dir() for entry in entries) or actual_directories != expected_directories:
        missing = sorted(expected_directories - actual_directories)
        extra = sorted(actual_directories - expected_directories)
        die(
            "unexpected Actions Artifacts; "
            f"missing={missing or 'none'} extra={extra or 'none'}"
        )
    for target in component_targets:
        target_os = target["target-os"]
        target_arch = target["target-arch"]
        directory = (
            root
            / f"{args.component}-prebuilt-{args.version}-{target_os}-{target_arch}"
        )
        validate_directory(
            directory, args.component, args.version, target_os, target_arch
        )
    print(f"Verified {4 * len(component_targets)} {args.component} prebuilt release assets under {root}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
