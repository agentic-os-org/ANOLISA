#!/usr/bin/env python3
"""Validate Tokenless source metadata before assembling a raw package."""

from __future__ import annotations

import argparse
import json
import re
from pathlib import Path


WORKSPACE_VERSION = re.compile(
    r"(?ms)^\[workspace\.package\]\s*$.*?^version\s*=\s*\"([^\"]+)\""
)
COMPONENT = re.compile(r"(?ms)^\[component\]\s*$.*?(?=^\[|\Z)")
ADAPTER_BLOCK = re.compile(r"(?ms)^\[\[adapters\]\]\s*$.*?(?=^\[|\Z)")
FRAMEWORK = re.compile(r'(?m)^framework\s*=\s*"([^"]+)"')
FIELD = r"(?m)^{}\s*=\s*\"([^\"]+)\""


def match_field(text: str, name: str, path: Path) -> str:
    """Read one string field from a scoped TOML table."""
    match = re.search(FIELD.format(re.escape(name)), text)
    if match is None:
        raise SystemExit(f"ERROR: {path} has no {name} field")
    return match.group(1)


def read_json_version(path: Path) -> str:
    """Read one generated adapter's top-level JSON version."""
    try:
        document = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        raise SystemExit(f"ERROR: cannot read {path}: {error}") from error
    version = document.get("version") if isinstance(document, dict) else None
    if not isinstance(version, str) or not version:
        raise SystemExit(f"ERROR: {path} has no string version")
    return version


def read_hermes_version(path: Path) -> str:
    """Read the generated Hermes manifest's simple version field."""
    try:
        text = path.read_text(encoding="utf-8")
    except OSError as error:
        raise SystemExit(f"ERROR: cannot read {path}: {error}") from error
    match = re.search(r'(?m)^version:\s*["\']?([^"\'\s]+)', text)
    if match is None:
        raise SystemExit(f"ERROR: {path} has no version")
    return match.group(1)


def read_manifest_targets(path: Path) -> set[str]:
    """Return the framework targets declared by the generated manifest."""
    try:
        document = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        raise SystemExit(f"ERROR: cannot read {path}: {error}") from error
    targets = document.get("targets") if isinstance(document, dict) else None
    if targets is None:
        return set()
    if not isinstance(targets, dict):
        raise SystemExit(f"ERROR: {path} has a non-object targets table")
    return set(targets)


def read_contract_frameworks(contract: Path) -> set[str]:
    """Return every framework named by an [[adapters]] block in the contract."""
    try:
        text = contract.read_text(encoding="utf-8")
    except OSError as error:
        raise SystemExit(f"ERROR: cannot read {contract}: {error}") from error
    frameworks = set()
    for block in ADAPTER_BLOCK.finditer(text):
        match = FRAMEWORK.search(block.group(0))
        if match is None:
            raise SystemExit(f"ERROR: {contract} has an [[adapters]] block without framework")
        frameworks.add(match.group(1))
    return frameworks


def verify_versions(root: Path, contract: Path) -> str:
    """Return the source version after checking packaged release metadata."""
    cargo_path = root / "Cargo.toml"
    try:
        cargo_text = cargo_path.read_text(encoding="utf-8")
        contract_text = contract.read_text(encoding="utf-8")
    except OSError as error:
        raise SystemExit(f"ERROR: cannot read release metadata: {error}") from error

    cargo_match = WORKSPACE_VERSION.search(cargo_text)
    if cargo_match is None:
        raise SystemExit(f"ERROR: {cargo_path} has no workspace package version")
    expected = cargo_match.group(1)

    component_match = COMPONENT.search(contract_text)
    if component_match is None:
        raise SystemExit(f"ERROR: {contract} has no [component] table")
    component_text = component_match.group(0)
    if match_field(component_text, "name", contract) != "tokenless":
        raise SystemExit(f"ERROR: {contract} is not a tokenless contract")
    contract_version = match_field(component_text, "version", contract)
    if contract_version != expected:
        raise SystemExit(
            f"ERROR: {contract} version {contract_version} does not match "
            f"Cargo.toml version {expected}"
        )

    adapters = root / "adapters" / "tokenless"
    json_manifests = (
        adapters / "manifest.json",
        adapters / "openclaw" / "package.json",
        adapters / "openclaw" / "openclaw.plugin.json",
        adapters / "dsh" / "package.json",
        adapters / "qoder" / ".qoder-plugin" / "plugin.json",
        adapters / "claude-code" / ".claude-plugin" / "plugin.json",
        adapters / "codex" / ".codex-plugin" / "plugin.json",
        adapters / "qwencode" / "qwen-extension.json",
        adapters / "qwenpaw" / "plugin.json",
    )
    versions = {str(path.relative_to(root)): read_json_version(path) for path in json_manifests}
    hermes = adapters / "hermes" / "plugin.yaml"
    versions[str(hermes.relative_to(root))] = read_hermes_version(hermes)
    drift = [f"{path}={version}" for path, version in versions.items() if version != expected]
    if drift:
        raise SystemExit(
            f"ERROR: generated adapter versions do not match {expected}: "
            + ", ".join(drift)
        )
    # The QwenPaw requirements file is stamped too, but as wheel URLs: a
    # stale or unstamped version there only fails inside `qwenpaw plugin install`.
    requirements = adapters / "qwenpaw" / "requirements.txt"
    text = requirements.read_text(encoding="utf-8")
    referenced = set(re.findall(r"/tokenless/v([^/]+)/", text)) | set(
        re.findall(r"anolisa_tokenless-([^-]+)-", text)
    )
    if "@VERSION@" in text or referenced != {expected}:
        raise SystemExit(
            f"ERROR: {requirements.relative_to(root)} does not reference "
            f"tokenless {expected}: {sorted(referenced) or 'no wheel URL'}"
        )
    return expected


def verify_adapter_coverage(root: Path, contract: Path) -> None:
    """Fail when the install contract and the adapter manifest disagree.

    The raw backend lays exactly the payload the contract names, so an adapter
    that ships in the archive but has no [[adapters]] entry is silently absent
    after install. Compare both directions: an undeclared target loses its
    files, and a declared framework with no target installs a dead bundle.
    """
    manifest = root / "adapters" / "tokenless" / "manifest.json"
    targets = read_manifest_targets(manifest)
    frameworks = read_contract_frameworks(contract)
    undeclared = sorted(targets - frameworks)
    if undeclared:
        names = ", ".join(undeclared)
        raise SystemExit(f"ERROR: {contract} declares no [[adapters]] entry for: {names}")
    unshipped = sorted(frameworks - targets)
    if unshipped:
        names = ", ".join(unshipped)
        raise SystemExit(
            f"ERROR: {contract} declares adapters that {manifest.relative_to(root)} "
            f"does not ship: {names}"
        )


def parse_args() -> argparse.Namespace:
    """Parse source and contract paths."""
    parser = argparse.ArgumentParser()
    parser.add_argument("source_root", type=Path)
    parser.add_argument("contract", type=Path)
    return parser.parse_args()


def main() -> int:
    """Print the verified release version for the packaging shell script."""
    args = parse_args()
    root = args.source_root.resolve()
    contract = args.contract.resolve()
    version = verify_versions(root, contract)
    verify_adapter_coverage(root, contract)
    print(version)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
