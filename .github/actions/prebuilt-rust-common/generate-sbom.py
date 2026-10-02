#!/usr/bin/env python3
"""Generate a deterministic CycloneDX 1.6 sidecar for a Rust archive."""

from __future__ import annotations

import argparse
import datetime as dt
import hashlib
import json
import os
import re
import subprocess
import sys
import tempfile
import uuid
from pathlib import Path
from typing import Any


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


def cargo_sbom_version(tool: str) -> str:
    """Return the installed cargo-sbom version."""
    try:
        result = subprocess.run(
            [tool, "--version"],
            check=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
        )
    except FileNotFoundError:
        die(f"cargo-sbom command not found: {tool}")
    except subprocess.CalledProcessError as error:
        detail = (error.stderr or error.stdout or "").strip()
        die(f"failed to query cargo-sbom version: {detail or error.returncode}")
    output = result.stdout.strip() or result.stderr.strip()
    match = re.search(r"(?:^|\s)cargo-sbom\s+([^\s]+)", output)
    if match is None:
        die(f"unexpected cargo-sbom --version output: {output!r}")
    return match.group(1)


def cargo_metadata(project_dir: Path, target: str) -> dict[str, Any]:
    """Return Cargo's default-feature dependency graph for one target."""
    try:
        result = subprocess.run(
            [
                "cargo",
                "metadata",
                "--format-version",
                "1",
                "--locked",
                "--filter-platform",
                target,
                "--manifest-path",
                str(project_dir / "Cargo.toml"),
            ],
            check=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
        )
    except FileNotFoundError:
        die("cargo command not found")
    except subprocess.CalledProcessError as error:
        detail = (error.stderr or error.stdout or "").strip()
        die(f"cargo metadata failed: {detail or error.returncode}")
    try:
        data = json.loads(result.stdout)
    except json.JSONDecodeError as error:
        die(f"cargo metadata returned invalid JSON: {error}")
    if not isinstance(data, dict):
        die("cargo metadata output must be a JSON object")
    return data


def resolved_package_identities(data: dict[str, Any]) -> set[tuple[str, str]]:
    """Return normal dependencies reachable from default workspace members."""
    packages = data.get("packages")
    workspace_members = data.get("workspace_members")
    workspace_default_members = data.get("workspace_default_members")
    resolve = data.get("resolve")
    nodes = resolve.get("nodes") if isinstance(resolve, dict) else None
    if not isinstance(packages, list) or not isinstance(workspace_members, list):
        die("cargo metadata is missing packages or workspace_members")
    if not isinstance(workspace_default_members, list):
        workspace_default_members = workspace_members
    if not isinstance(nodes, list):
        die("cargo metadata is missing the resolved dependency graph")

    packages_by_id: dict[str, dict[str, Any]] = {}
    for package in packages:
        if not isinstance(package, dict) or not isinstance(package.get("id"), str):
            die("cargo metadata contains an invalid package")
        packages_by_id[package["id"]] = package
    nodes_by_id: dict[str, dict[str, Any]] = {}
    for node in nodes:
        if not isinstance(node, dict) or not isinstance(node.get("id"), str):
            die("cargo metadata contains an invalid resolve node")
        nodes_by_id[node["id"]] = node

    pending = list(workspace_default_members)
    reachable: set[str] = set()
    while pending:
        package_id = pending.pop()
        if not isinstance(package_id, str) or package_id in reachable:
            continue
        package = packages_by_id.get(package_id)
        node = nodes_by_id.get(package_id)
        if package is None or node is None:
            die(f"cargo metadata cannot resolve package {package_id}")
        reachable.add(package_id)
        dependencies = node.get("deps")
        if not isinstance(dependencies, list):
            die(f"cargo metadata package has invalid dependencies: {package_id}")
        for dependency in dependencies:
            if not isinstance(dependency, dict):
                die(f"cargo metadata package has an invalid dependency: {package_id}")
            dependency_id = dependency.get("pkg")
            kinds = dependency.get("dep_kinds")
            if not isinstance(dependency_id, str) or not isinstance(kinds, list):
                die(f"cargo metadata package has an invalid dependency: {package_id}")
            if any(
                isinstance(kind, dict) and kind.get("kind") is None
                for kind in kinds
            ):
                pending.append(dependency_id)

    identities: set[tuple[str, str]] = set()
    for package_id in reachable:
        package = packages_by_id[package_id]
        name = package.get("name")
        version = package.get("version")
        if not isinstance(name, str) or not isinstance(version, str):
            die(f"cargo metadata package has no name or version: {package_id}")
        identities.add((name, version))
    return identities


def component_identity(component: dict[str, Any]) -> tuple[str, str] | None:
    """Return the Cargo name/version identity represented by a component."""
    name = component.get("name")
    version = component.get("version")
    if isinstance(name, str) and isinstance(version, str):
        return (name, version)
    return None


def component_identities(components: list[Any]) -> set[tuple[str, str]]:
    """Collect identities from a nested component list."""
    identities: set[tuple[str, str]] = set()
    for component in components:
        if not isinstance(component, dict):
            die("cargo-sbom output contains an invalid component")
        identity = component_identity(component)
        if identity is not None:
            identities.add(identity)
        nested = component.get("components")
        if isinstance(nested, list):
            identities.update(component_identities(nested))
    return identities


def filter_components(
    components: list[Any], allowed: set[tuple[str, str]]
) -> list[dict[str, Any]]:
    """Keep only components in the resolved target dependency graph."""
    retained: list[dict[str, Any]] = []
    for component in components:
        if not isinstance(component, dict):
            die("cargo-sbom output contains an invalid component")
        identity = component_identity(component)
        if identity not in allowed:
            continue
        nested = component.get("components")
        if isinstance(nested, list):
            component["components"] = filter_components(nested, allowed)
        retained.append(component)
    return retained


def component_refs(components: list[Any]) -> set[str]:
    """Collect bom-ref values from a nested component list."""
    refs: set[str] = set()
    for component in components:
        if not isinstance(component, dict):
            die("cargo-sbom output contains an invalid component")
        bom_ref = component.get("bom-ref")
        if isinstance(bom_ref, str) and bom_ref:
            refs.add(bom_ref)
        nested = component.get("components")
        if isinstance(nested, list):
            refs.update(component_refs(nested))
    return refs


def filter_sbom_for_target(
    data: dict[str, Any], allowed: set[tuple[str, str]]
) -> dict[str, Any]:
    """Remove packages and edges not used by the selected Cargo target."""
    metadata = data.get("metadata")
    root = metadata.get("component") if isinstance(metadata, dict) else None
    components = data.get("components")
    dependencies = data.get("dependencies")
    root_components = root.get("components") if isinstance(root, dict) else None
    if not isinstance(root_components, list) or not isinstance(components, list):
        die("cargo-sbom output is missing component collections")
    if not isinstance(dependencies, list):
        die("cargo-sbom output is missing dependencies")

    represented = component_identities(root_components)
    represented.update(component_identities(components))
    missing = sorted(allowed - represented)
    if missing:
        name, version = missing[0]
        die(f"cargo-sbom omitted resolved package {name} {version}")

    root["components"] = filter_components(root_components, allowed)
    data["components"] = filter_components(components, allowed)
    refs = component_refs(root["components"])
    refs.update(component_refs(data["components"]))
    retained_dependencies: list[dict[str, Any]] = []
    for dependency in dependencies:
        if not isinstance(dependency, dict):
            die("cargo-sbom output contains an invalid dependency")
        dependency_ref = dependency.get("ref")
        if dependency_ref not in refs:
            continue
        depends_on = dependency.get("dependsOn")
        if isinstance(depends_on, list):
            dependency["dependsOn"] = [value for value in depends_on if value in refs]
        retained_dependencies.append(dependency)
    data["dependencies"] = retained_dependencies
    return data


def canonical_key(value: Any) -> str:
    """Render a stable sort key for arbitrary JSON values."""
    return json.dumps(value, ensure_ascii=False, sort_keys=True, separators=(",", ":"))


def component_key(component: Any) -> tuple[str, str, str]:
    """Return a stable identity-first component sort key."""
    if not isinstance(component, dict):
        return ("", "", canonical_key(component))
    return (
        str(component.get("bom-ref", "")),
        str(component.get("name", "")),
        f"{component.get('version', '')}\0{canonical_key(component)}",
    )


def normalize_component(component: dict[str, Any]) -> None:
    """Sort nested component collections in place."""
    nested = component.get("components")
    if isinstance(nested, list):
        for item in nested:
            if isinstance(item, dict):
                normalize_component(item)
        nested.sort(key=component_key)
    for key in ("externalReferences", "hashes", "licenses", "properties"):
        values = component.get(key)
        if isinstance(values, list):
            values.sort(key=canonical_key)


def deduplicate_component_tree(component: dict[str, Any], seen: set[str]) -> None:
    """Remove repeated bom-ref values while retaining first occurrence order."""
    bom_ref = component.get("bom-ref")
    if isinstance(bom_ref, str) and bom_ref:
        seen.add(bom_ref)
    nested = component.get("components")
    if not isinstance(nested, list):
        return
    unique: list[Any] = []
    for item in nested:
        if not isinstance(item, dict):
            unique.append(item)
            continue
        item_ref = item.get("bom-ref")
        if isinstance(item_ref, str) and item_ref and item_ref in seen:
            continue
        deduplicate_component_tree(item, seen)
        unique.append(item)
    component["components"] = unique


def merge_sboms(documents: list[dict[str, Any]]) -> dict[str, Any]:
    """Merge filtered cargo-sbom documents for every shipped Rust project."""
    if not documents:
        die("at least one cargo-sbom document is required")
    merged = documents[0]
    metadata = merged.get("metadata")
    root = metadata.get("component") if isinstance(metadata, dict) else None
    root_components = root.get("components") if isinstance(root, dict) else None
    components = merged.get("components")
    dependencies = merged.get("dependencies")
    if not all(isinstance(value, list) for value in (root_components, components, dependencies)):
        die("cargo-sbom output is missing mergeable component collections")

    for document in documents[1:]:
        other_metadata = document.get("metadata")
        other_root = (
            other_metadata.get("component")
            if isinstance(other_metadata, dict)
            else None
        )
        other_root_components = (
            other_root.get("components") if isinstance(other_root, dict) else None
        )
        other_components = document.get("components")
        other_dependencies = document.get("dependencies")
        if not all(
            isinstance(value, list)
            for value in (other_root_components, other_components, other_dependencies)
        ):
            die("cargo-sbom output is missing mergeable component collections")
        root_components.extend(other_root_components)
        components.extend(other_components)
        dependencies.extend(other_dependencies)
    return merged


def normalize_sbom(
    data: dict[str, Any],
    *,
    component_name: str,
    version: str,
    target_os: str,
    target_arch: str,
    artifact_name: str,
    artifact_sha256: str,
    epoch: int,
) -> dict[str, Any]:
    """Bind cargo-sbom output to the immutable release archive."""
    if data.get("bomFormat") != "CycloneDX" or data.get("specVersion") != "1.6":
        die("cargo-sbom did not produce CycloneDX 1.6 JSON")
    metadata = data.get("metadata")
    root = metadata.get("component") if isinstance(metadata, dict) else None
    if not isinstance(metadata, dict) or not isinstance(root, dict):
        die("cargo-sbom output is missing metadata.component")

    artifact_id = f"{component_name}-{version}-{target_os}-{target_arch}-tar"
    root.update(
        {
            "type": "application",
            "name": component_name,
            "version": version,
            "bom-ref": artifact_id,
            "hashes": [{"alg": "SHA-256", "content": artifact_sha256}],
        }
    )
    properties = [
        value
        for value in root.get("properties", [])
        if isinstance(value, dict)
        and not str(value.get("name", "")).startswith("anolisa:")
    ]
    properties.extend(
        [
            {"name": "anolisa:artifact:id", "value": artifact_id},
            {"name": "anolisa:artifact:name", "value": artifact_name},
            {"name": "anolisa:target:arch", "value": target_arch},
            {"name": "anolisa:target:os", "value": target_os},
        ]
    )
    root["properties"] = properties
    timestamp = dt.datetime.fromtimestamp(epoch, tz=dt.timezone.utc)
    metadata["timestamp"] = timestamp.isoformat(timespec="seconds").replace(
        "+00:00", "Z"
    )
    metadata["authors"] = [{"name": "ANOLISA Release Pipeline"}]
    serial_seed = (
        f"anolisa:{component_name}:{version}:{target_os}:{target_arch}:"
        f"sha256:{artifact_sha256}"
    )
    data["serialNumber"] = f"urn:uuid:{uuid.uuid5(uuid.NAMESPACE_URL, serial_seed)}"
    data["version"] = 1

    normalize_component(root)
    seen: set[str] = set()
    deduplicate_component_tree(root, seen)
    components = data.get("components")
    dependencies = data.get("dependencies")
    if not isinstance(components, list) or not isinstance(dependencies, list):
        die("cargo-sbom components and dependencies must be arrays")
    for component in components:
        if isinstance(component, dict):
            normalize_component(component)
    components.sort(key=component_key)
    unique: list[Any] = []
    for component in components:
        if not isinstance(component, dict):
            unique.append(component)
            continue
        bom_ref = component.get("bom-ref")
        if isinstance(bom_ref, str) and bom_ref and bom_ref in seen:
            continue
        deduplicate_component_tree(component, seen)
        unique.append(component)
    data["components"] = unique
    merged_dependencies: dict[str, set[str]] = {}
    for dependency in dependencies:
        if not isinstance(dependency, dict):
            die("cargo-sbom output contains an invalid dependency")
        reference = dependency.get("ref")
        depends_on = dependency.get("dependsOn")
        if not isinstance(reference, str) or not isinstance(depends_on, list):
            die("cargo-sbom output contains an invalid dependency edge")
        targets = merged_dependencies.setdefault(reference, set())
        for target in depends_on:
            if not isinstance(target, str):
                die("cargo-sbom output contains an invalid dependency target")
            targets.add(target)
    data["dependencies"] = [
        {"ref": reference, "dependsOn": sorted(targets)}
        for reference, targets in sorted(merged_dependencies.items())
    ]
    return data


def atomic_write(path: Path, payload: bytes) -> None:
    """Atomically replace one generated sidecar."""
    path.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.NamedTemporaryFile(
        dir=path.parent, prefix=f".{path.name}.", delete=False
    ) as stream:
        temporary = Path(stream.name)
        stream.write(payload)
    try:
        os.replace(temporary, path)
    finally:
        temporary.unlink(missing_ok=True)


def main() -> int:
    """Generate the normalized SBOM and its checksum sidecar."""
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--artifact", required=True, type=Path)
    parser.add_argument("--component", required=True)
    parser.add_argument("--version", required=True)
    parser.add_argument("--os", dest="target_os", required=True)
    parser.add_argument("--arch", dest="target_arch", required=True)
    parser.add_argument("--target", required=True)
    parser.add_argument(
        "--project-dir", required=True, action="append", type=Path
    )
    parser.add_argument("--source-date-epoch", required=True, type=int)
    parser.add_argument("--tool", default="cargo-sbom")
    args = parser.parse_args()

    artifact = args.artifact.resolve()
    project_dirs = [project_dir.resolve() for project_dir in args.project_dir]
    if not artifact.is_file() or any(
        not project_dir.is_dir() for project_dir in project_dirs
    ):
        die("artifact must be a file and every project-dir must be a directory")
    if not re.fullmatch(r"[a-z0-9][a-z0-9-]*", args.component):
        die("component must be a lowercase release component name")
    if args.source_date_epoch < 0:
        die("source-date-epoch must not be negative")
    expected_target = {
        ("linux", "x86_64"): "x86_64-unknown-linux-gnu",
        ("linux", "aarch64"): "aarch64-unknown-linux-gnu",
        ("macos", "aarch64"): "aarch64-apple-darwin",
        ("macos", "x86_64"): "x86_64-apple-darwin",
    }.get((args.target_os, args.target_arch))
    if expected_target is None or args.target != expected_target:
        die("target triple does not match the requested OS and architecture")
    actual_tool_version = cargo_sbom_version(args.tool)
    if actual_tool_version != "0.10.0":
        die(f"cargo-sbom 0.10.0 is required, got {actual_tool_version}")
    artifact_sha256 = sha256_file(artifact)
    if read_sidecar(Path(f"{artifact}.sha256"), artifact.name) != artifact_sha256:
        die(f"artifact checksum sidecar does not match {artifact}")

    documents: list[dict[str, Any]] = []
    for project_dir in project_dirs:
        try:
            result = subprocess.run(
                [
                    args.tool,
                    "--output-format",
                    "cyclone_dx_json_1_6",
                    "--project-directory",
                    str(project_dir),
                ],
                check=True,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                text=True,
            )
        except FileNotFoundError:
            die(f"cargo-sbom command not found: {args.tool}")
        except subprocess.CalledProcessError as error:
            detail = (error.stderr or error.stdout or "").strip()
            die(f"cargo-sbom failed: {detail or error.returncode}")
        try:
            raw = json.loads(result.stdout)
        except json.JSONDecodeError as error:
            die(f"cargo-sbom returned invalid JSON: {error}")
        if not isinstance(raw, dict):
            die("cargo-sbom output must be a JSON object")
        documents.append(
            filter_sbom_for_target(
                raw,
                resolved_package_identities(cargo_metadata(project_dir, args.target)),
            )
        )
    raw = merge_sboms(documents)
    normalized = normalize_sbom(
        raw,
        component_name=args.component,
        version=args.version,
        target_os=args.target_os,
        target_arch=args.target_arch,
        artifact_name=artifact.name,
        artifact_sha256=artifact_sha256,
        epoch=args.source_date_epoch,
    )
    output = Path(f"{artifact}.cdx.json")
    payload = (
        json.dumps(normalized, ensure_ascii=False, indent=2, sort_keys=True) + "\n"
    ).encode()
    atomic_write(output, payload)
    digest = hashlib.sha256(payload).hexdigest()
    atomic_write(Path(f"{output}.sha256"), f"{digest}  {output.name}\n".encode())
    print(output)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
