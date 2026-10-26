#!/usr/bin/env python3
"""Bind the standard raw repository and offline images to one fork preview."""

import argparse
import hashlib
import json
import platform
import re
import shutil
import tarfile
import tomllib
from pathlib import Path


def digest(path: Path) -> str:
    """Hash large artifacts without reading them into memory."""
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def prepare(directory: Path, source_commit: str, version: str, components: Path) -> dict:
    """Write install indexes only for the exact staged preview contract."""
    if not re.fullmatch(r"[a-f0-9]{40}", source_commit):
        raise ValueError("source commit must be a full Git SHA")
    if not re.fullmatch(r"\d+\.\d+\.\d+-skillscore\." + source_commit[:12], version):
        raise ValueError("preview version must bind the source commit")
    package = directory / f"sec-core-{version}-linux-x86_64.tar.gz"
    with tarfile.open(package) as archive:
        contracts = [
            m
            for m in archive.getmembers()
            if m.name in {".anolisa/component.toml", "./.anolisa/component.toml"}
        ]
        if len(contracts) != 1 or not contracts[0].isfile():
            raise ValueError("raw package must contain one component contract")
        stream = archive.extractfile(contracts[0])
        if stream is None:
            raise ValueError("raw component contract is unreadable")
        contract_bytes = stream.read()
        contract = tomllib.loads(contract_bytes.decode("utf-8"))
        if (
            contract["component"]["name"] != "sec-core"
            or contract["component"]["version"] != version
        ):
            raise ValueError("raw package identity differs from preview")
        if any(
            m.name.endswith(("skill-score-images.tar", "skill-score-images.tar.gz"))
            for m in archive.getmembers()
        ):
            raise ValueError("Docker image archive must remain outside the raw package")
        if any(m.name.lstrip("./") == "bin/agent-sec-skill-score" for m in archive.getmembers()):
            raise ValueError("raw preview must use the unified agent-sec-cli entrypoint")
    images = json.loads((directory / "images.json").read_text())
    expected = {"agent-sec-skill-score:1", "agent-sec-skill-score-egress:1"}
    details = images.get("image_details", [])
    if len(details) != 2 or {item["name"] for item in details} != expected:
        raise ValueError("preview must bind the work and egress images")
    if set(images.get("images", [])) != expected or any(
        not re.fullmatch(r"sha256:[a-f0-9]{64}", item["id"]) for item in details
    ):
        raise ValueError("every image needs its immutable ID")
    required = [
        package.name,
        "skill-score-images.tar.gz",
        "images.json",
        "tool-inventory.json",
        "source.tar.gz",
    ]
    for name in required:
        path = directory / name
        if path.is_symlink() or not path.is_file() or not 0 < path.stat().st_size < 2 * 1024**3:
            raise ValueError(f"release asset must be a nonempty regular file below 2 GiB: {name}")
    # The installer currently selects the stable channel; the custom repository,
    # explicit prerelease version and GitHub prerelease flag isolate this preview.
    index = f"""schema_version = 1
channel = "stable"
publisher = "asc-fork-preview"

[[entries]]
component = "sec-core"
version = "{version}"
channel = "stable"
artifact_type = "tar_gz"
backend = "raw"
url = "{package.name}"
os = "linux"
arch = "x86_64"
install_modes = ["system"]
sha256 = "{digest(package)}"
manifest_digest = "sha256:{hashlib.sha256(contract_bytes).hexdigest()}"
"""
    for name in ("index-v2.toml", "index.toml"):
        (directory / name).write_text(index)
    shutil.copyfile(components, directory / "components-v2.toml")
    required.extend(("index-v2.toml", "index.toml", "components-v2.toml"))
    manifest = {
        "schema_version": "agent-sec-skill-score/release-v2",
        "source_commit": source_commit,
        "development_build": False,
        "component": "sec-core",
        "version": version,
        "raw_package": package.name,
        "raw_repository": "v1",
        "cli": "agent-sec-cli",
        "command_prefix": ["skillsec", "score"],
        "python_cli": "3.11.6",
        "python_worker": "3.12.11",
        "harbor": "0.22.0",
        "build_libc": dict(zip(("name", "version"), platform.libc_ver())),
        "images": images,
        "artifacts": [
            {
                "file": name,
                "bytes": (directory / name).stat().st_size,
                "sha256": digest(directory / name),
            }
            for name in required
        ],
    }
    (directory / "release-manifest.json").write_text(json.dumps(manifest, indent=2) + "\n")
    return manifest


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("directory", type=Path)
    parser.add_argument("--source-commit", required=True)
    parser.add_argument("--version", required=True)
    parser.add_argument("--components", type=Path, required=True)
    args = parser.parse_args()
    prepare(args.directory, args.source_commit, args.version, args.components)


if __name__ == "__main__":
    main()
