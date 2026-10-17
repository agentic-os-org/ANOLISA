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


def _referenced_wheel_versions(text: str) -> set:
    """Return the tokenless wheel versions referenced as wheel URLs.

    Anchored on the wheel tag (cp/py) so pre-release versions such as
    0.9.0-rc.1 are captured whole; the previous pattern stopped at the
    first dash and truncated them, making the gate reject every
    pre-release with a false drift.
    """
    return set(re.findall(r"anolisa_tokenless-(.+?)-(?:cp|py)\d", text))


# PEP 440 version grammar, from the appendix of the spec. Wheel filenames
# carry the *normalized* form of the project version -- the pinned maturin
# 1.15.0 names the wheel of a Cargo 0.9.0-rc.1 "anolisa_tokenless-0.9.0rc1-
# ...", and pip rejects filenames that keep an unnormalized spelling --
# while the Cargo workspace version keeps its original form. This script
# runs under the bare python3 that packaging/raw/package.sh provides, so
# only the stdlib is available and the normalization below is hand-rolled
# instead of delegating to packaging.version.Version.
_PEP440_VERSION = re.compile(
    r"""
    ^v?
    (?:(?P<epoch>[0-9]+)!)?
    (?P<release>[0-9]+(?:\.[0-9]+)*)
    (?P<pre>[-_.]?(?P<pre_l>alpha|a|beta|b|preview|pre|c|rc)[-_.]?(?P<pre_n>[0-9]+)?)?
    (?P<post>(?:-(?P<post_n1>[0-9]+))|(?:[-_.]?(?P<post_l>post|rev|r)[-_.]?(?P<post_n2>[0-9]+)?))?
    (?P<dev>[-_.]?dev[-_.]?(?P<dev_n>[0-9]+)?)?
    (?:\+(?P<local>[a-z0-9]+(?:[-_.][a-z0-9]+)*))?
    $
    """,
    re.IGNORECASE | re.VERBOSE,
)
_PRE_ALIASES = {"alpha": "a", "beta": "b", "c": "rc", "pre": "rc", "preview": "rc"}


def _normalize_pep440(version: str) -> str:
    """Return the PEP 440 normalized spelling of a version string.

    Normalization drops the separators around pre/post/dev segments, folds
    the long pre-release spellings (alpha/beta/preview/pre/c) into their
    single letters, lowercases the remainder and canonicalizes local-version
    separators to dots -- exactly what str(packaging.version.Version) does,
    reproduced here for a stdlib-only environment. A version the grammar
    cannot parse is returned unchanged, so the caller falls back to exact
    string comparison, which is the only contract such a spelling ever had.
    """
    match = _PEP440_VERSION.match(version)
    if match is None:
        return version
    parts = []
    if match.group("epoch") is not None:
        parts.append(f"{int(match.group('epoch'))}!")
    parts.append(
        ".".join(str(int(piece)) for piece in match.group("release").split("."))
    )
    if match.group("pre_l") is not None:
        letter = _PRE_ALIASES.get(
            match.group("pre_l").lower(), match.group("pre_l").lower()
        )
        parts.append(f"{letter}{int(match.group('pre_n') or 0)}")
    if match.group("post_n1") is not None:
        parts.append(f".post{int(match.group('post_n1'))}")
    elif match.group("post_l") is not None:
        parts.append(f".post{int(match.group('post_n2') or 0)}")
    if match.group("dev") is not None:
        parts.append(f".dev{int(match.group('dev_n') or 0)}")
    if match.group("local") is not None:
        local = ".".join(re.split(r"[-_.]", match.group("local").lower()))
        parts.append(f"+{local}")
    return "".join(parts)


def _verify_requirements_references(relative: str, expected: str, text: str) -> None:
    """Fail unless a stamped requirements file pins this release's wheels.

    The release URL keeps the raw workspace version in its tag path
    (/tokenless/v0.9.0-rc.1/) while the wheel filename carries the PEP 440
    normalized form (anolisa_tokenless-0.9.0rc1-cp311-...). Each position
    must use its own spelling: the release workflow tags the workspace
    version verbatim, so a normalized tag path would point installers at a
    tag the workflow never creates, while a wheel filename that keeps the
    unnormalized spelling is a form pip refuses to install and would break
    `qwenpaw plugin install` on every host. The producer side of the pair is
    the Makefile rule that stamps adapters/tokenless/qwenpaw/requirements.txt
    through packaging/raw/wheel-version.py.
    """
    # The template carries two placeholders -- @VERSION@ for the tag path and
    # @WHEEL_VERSION@ for the filenames -- so report any surviving stamp
    # marker instead of only the one this gate used to know about.
    placeholders = sorted(set(re.findall(r"@[A-Z0-9_]+@", text)))
    if placeholders:
        raise SystemExit(
            f"ERROR: {relative} is not stamped: {', '.join(placeholders)} remains"
        )
    path_versions = set(re.findall(r"/tokenless/v([^/]+)/", text))
    wheel_versions = _referenced_wheel_versions(text)
    normalized = _normalize_pep440(expected)
    unnormalized = sorted(v for v in wheel_versions if _normalize_pep440(v) != v)
    if unnormalized:
        raise SystemExit(
            f"ERROR: {relative} pins wheel filename version(s) {unnormalized} "
            f"in a form pip rejects: wheel filenames must carry the PEP 440 "
            f"normalized version ({normalized})"
        )
    # The release workflow creates the tag from the workspace version
    # verbatim, so the tag path must use that exact spelling; the normalized
    # Python spelling would point requirements at a different tag.
    stale_tags = sorted(v for v in path_versions if v != expected)
    if stale_tags:
        raise SystemExit(
            f"ERROR: {relative} pins release tag version(s) {stale_tags}: the "
            f"release tag must use the workspace spelling {expected}"
        )
    # Wheel filenames carry the PEP 440 normalized spelling of the same
    # version; any other value is a different release.
    wrong_wheels = sorted(v for v in wheel_versions if v != normalized)
    if wrong_wheels:
        raise SystemExit(
            f"ERROR: {relative} does not reference tokenless {expected}: "
            f"{wrong_wheels}"
        )
    if not path_versions and not wheel_versions:
        raise SystemExit(
            f"ERROR: {relative} does not reference tokenless {expected}: "
            "no wheel URL"
        )


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
        adapters / "qwenwork" / ".qoder-plugin" / "plugin.json",
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
    _verify_requirements_references(
        str(requirements.relative_to(root)), expected, text
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
