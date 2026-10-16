#!/usr/bin/env python3
"""Compose pinned AW and independent AgentSight archives without repackaging them."""

import argparse
import hashlib
import json
import os
import re
import shutil
import sys
import tarfile
import tempfile
from pathlib import Path, PurePosixPath

if sys.version_info < (3, 11):
    raise SystemExit("AW composition release tooling requires Python 3.11 or newer")

import tomllib

MAX_MEMBERS = 4096
MAX_BYTES = 2 * 1024**3
MAX_METADATA = 1024**2
AW_COMPONENTS = {"aw-core", "aw-provider-sec-core", "aw-provider-tokenless"}
AW_COMPONENT_FIELDS = {
    "format", "component", "version", "source_commit", "os", "arch",
    "provider_protocol", "requires", "files",
}


def require(condition: bool, message: str) -> None:
    """Reject invalid release inputs before publishing any output."""
    if not condition:
        raise ValueError(message)


def digest(path: Path) -> str:
    """Hash the original archive bytes; no extraction or component execution."""
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def members(archive: tarfile.TarFile) -> dict[str, tarfile.TarInfo]:
    """Bound archive expansion and reject ambiguous names, links and special files."""
    result = {}
    size = 0
    for member in archive:
        require(len(result) < MAX_MEMBERS, "too many archive members")
        path = PurePosixPath(member.name)
        require(
            not path.is_absolute() and ".." not in path.parts,
            f"unsafe archive path: {member.name}",
        )
        name = str(path)
        require(name not in result, f"duplicate archive path: {name}")
        require((member.isfile() or member.isdir()) and member.size >= 0, f"nonregular archive member: {name}")
        size += member.size
        require(size <= MAX_BYTES, "archive exceeds expanded size limit")
        result[name] = member
    return result


def metadata(archive: tarfile.TarFile, entries: dict, name: str) -> bytes:
    """Read one bounded metadata file after validating the complete archive layout."""
    entry = entries.get(name)
    require(entry is not None and entry.isfile(), f"missing archive file: {name}")
    require(entry.size <= MAX_METADATA, f"metadata exceeds size limit: {name}")
    with archive.extractfile(entry) as stream:
        return stream.read()


def inspect_aw(path: Path, arch: str) -> list[dict]:
    """Keep the original all-in-one receipt identity; AW install remains authoritative."""
    with tarfile.open(path, "r:gz") as archive:
        entries = members(archive)
        names = [name for name in entries if name.endswith("/manifest.json")]
        require(len(names) == 1, "AW archive must contain one manifest")
        manifest = json.loads(metadata(archive, entries, names[0]))
        require(
            isinstance(manifest, dict) and set(manifest) == {"format", "components"}
            and type(manifest["format"]) is int and manifest["format"] == 1,
            "unsupported AW manifest",
        )
        components = manifest["components"]
        require(
            isinstance(components, list)
            and all(isinstance(item, dict) and set(item) == AW_COMPONENT_FIELDS
                    for item in components),
            "AW component fields must match the closed composition schema",
        )
        require(
            len(components) == len(AW_COMPONENTS)
            and {item.get("component") for item in components} == AW_COMPONENTS,
            "composition requires the existing AW all-in-one package",
        )
        identities = {(item.get("version"), item.get("source_commit")) for item in components}
        require(len(identities) == 1, "AW components must retain matching version and source")
        for item in components:
            require(
                isinstance(item.get("source_commit"), str)
                and re.fullmatch(r"[0-9a-f]{40}", item["source_commit"])
                and isinstance(item.get("version"), str)
                and re.fullmatch(r"[0-9]+\.[0-9]+\.[0-9]+-preview\.[0-9]+", item["version"]),
                "invalid AW version or source commit",
            )
            require(
                type(item.get("format")) is int and item["format"] == 1
                and item.get("os") == "linux"
                and item.get("arch") == arch
                and item.get("provider_protocol") == "aw-provider/v1alpha1",
                "incompatible AW component",
            )
            dependencies = {} if item["component"] == "aw-core" else {"aw-core": item["version"]}
            require(item.get("requires") == dependencies, "incompatible AW dependency")
            require(isinstance(item["files"], dict), "AW files must be an object")
            for name, payload in item["files"].items():
                parts = name.split("/")
                require(parts[0] in {"bin", "libexec", "share"}
                        and all(part not in {"", ".", ".."} for part in parts),
                        f"invalid AW payload path: {name}")
                require(isinstance(payload, dict) and set(payload) == {"sha256", "mode"}
                        and isinstance(payload["sha256"], str)
                        and re.fullmatch(r"[0-9a-f]{64}", payload["sha256"])
                        and type(payload["mode"]) is int
                        and payload["mode"] in {0o644, 0o755},
                        f"invalid AW payload metadata: {name}")
        return components


def inspect_sight(path: Path, arch: str, version: str) -> dict:
    """Read AgentSight's own raw contract and check the packaged binary architecture."""
    with tarfile.open(path, "r:gz") as archive:
        entries = members(archive)
        contract = tomllib.loads(metadata(archive, entries, ".anolisa/component.toml").decode())
        component = contract.get("component", {})
        require(
            component.get("name") == "agentsight" and component.get("version") == version,
            "AgentSight release does not match its pinned identity",
        )
        platform = component.get("platform", {})
        require(
            "linux" in platform.get("os", []) and arch in platform.get("arch", []),
            "incompatible AgentSight contract platform",
        )
        expected_machine = {"x86_64": 62, "aarch64": 183}[arch]
        for name in ["bin/agentsight", "bin/agentsight-enforcer"]:
            entry = entries.get(name)
            require(entry is not None and entry.isfile(), f"missing archive file: {name}")
            with archive.extractfile(entry) as stream:
                header = stream.read(20)
            require(
                len(header) == 20 and header[:6] == b"\x7fELF\x02\x01"
                and int.from_bytes(header[18:20], "little") == expected_machine
                and entry.mode & 0o111 != 0,
                f"incompatible AgentSight executable: {name}",
            )
        for name in [
            "bin/agentsight-start",
            "share/anolisa/agentsight/agentsight.service",
            "share/anolisa/agentsight/agentsight-enforcer.service",
        ]:
            require(name in entries and entries[name].isfile(), f"missing archive file: {name}")
        return component


def compose(aw: Path, aw_sha256: str, sight: Path, sight_sha256: str,
            sight_version: str, sight_release: str, arch: str, output: Path) -> None:
    """Publish original archives plus a combination record, never install or start them."""
    require(arch in {"x86_64", "aarch64"}, "unsupported Linux architecture")
    require(re.fullmatch(r"\d+\.\d+\.\d+(?:[-+][0-9A-Za-z.-]+)?", sight_version),
            "invalid AgentSight version")
    require(
        sight_release in {
            f"https://github.com/agentic-os-org/ANOLISA/releases/tag/sight/v{sight_version}",
            f"https://github.com/alibaba/anolisa/releases/tag/sight/v{sight_version}",
        }, "AgentSight source must reference its pinned release tag",
    )
    require(not output.exists(), "output already exists")
    for path, expected in [(aw, aw_sha256), (sight, sight_sha256)]:
        require(re.fullmatch(r"[0-9a-f]{64}", expected), "invalid archive SHA-256")
        require(path.is_file() and not path.is_symlink(), f"expected regular archive: {path}")
        require(path.stat().st_size <= MAX_BYTES, f"archive exceeds size limit: {path}")
    # Validate the staged copies so input changes cannot bypass the final digest check.
    with tempfile.TemporaryDirectory(prefix=".aw-compose-", dir=output.parent) as temporary:
        stage = Path(temporary) / "composition"
        stage.mkdir()
        for source, name, expected in [(aw, "aw.tar.gz", aw_sha256),
                                       (sight, "agentsight.tar.gz", sight_sha256)]:
            shutil.copyfile(source, stage / name)
            require(digest(stage / name) == expected, f"archive checksum mismatch: {source}")
        components = inspect_aw(stage / "aw.tar.gz", arch)
        component = inspect_sight(stage / "agentsight.tar.gz", arch, sight_version)
        record = {
            "format": 1,
            "platform": {"os": "linux", "arch": arch},
            "aw": {"archive": "aw.tar.gz", "sha256": aw_sha256, "components": components},
            "agentsight": {
                "archive": "agentsight.tar.gz", "sha256": sight_sha256,
                "version": component["version"], "release": sight_release,
                "contract": ".anolisa/component.toml",
                "installer": "anolisa", "ownership": "independent",
            },
        }
        (stage / "composition.json").write_text(json.dumps(record, indent=2) + "\n", encoding="utf-8")
        # mkdir is exclusive: concurrent composers must not replace each other's output.
        expected_digests = {
            "aw.tar.gz": aw_sha256, "agentsight.tar.gz": sight_sha256,
            "composition.json": digest(stage / "composition.json"),
        }
        output.mkdir()
        published = []
        try:
            for name in ["aw.tar.gz", "agentsight.tar.gz", "composition.json"]:
                os.link(stage / name, output / name)
                published.append(name)
        except BaseException:
            for name in reversed(published):
                destination = output / name
                try:
                    if (destination.samefile(stage / name)
                            and digest(destination) == expected_digests[name]):
                        destination.unlink()
                    else:
                        print(f"AW composition: retained modified output {destination}", file=sys.stderr)
                except OSError as error:
                    print(f"AW composition: rollback {destination}: {error}", file=sys.stderr)
            try:
                output.rmdir()
            except OSError as error:
                print(f"AW composition: retained output {output}: {error}", file=sys.stderr)
            raise


def main() -> None:
    """Keep composition in release tooling, outside the everyday aw run workflow."""
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--aw-archive", type=Path, required=True)
    parser.add_argument("--aw-sha256", required=True)
    parser.add_argument("--sight-archive", type=Path, required=True)
    parser.add_argument("--sight-sha256", required=True)
    parser.add_argument("--sight-version", required=True)
    parser.add_argument("--sight-release", required=True)
    parser.add_argument("--arch", choices=("x86_64", "aarch64"), required=True)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    try:
        compose(args.aw_archive, args.aw_sha256, args.sight_archive, args.sight_sha256,
                args.sight_version, args.sight_release, args.arch, args.output)
    except (OSError, ValueError, KeyError, TypeError, EOFError, tarfile.TarError) as error:
        parser.exit(1, f"AW composition: {error}\n")


if __name__ == "__main__":
    main()
