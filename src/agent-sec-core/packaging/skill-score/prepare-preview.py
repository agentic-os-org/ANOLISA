#!/usr/bin/env python3
"""Bind the base and optional raw components to one immutable fork release."""

import argparse
import hashlib
import json
import platform
import re
import shutil
import tarfile
import tomllib
from pathlib import Path

COMPONENTS = ("sec-core", "sec-core-skill-score")
INSTALLER = "anolisa-cli-0.3.20-linux-x86_64.tar.gz"
# These are standard-library contents of the pinned adapter-only CPython archive.
BASE_ENSUREPIP_WHEELS = {
    "lib/anolisa/sec-core/python3.11/runtime/lib/python3.11/ensurepip/_bundled/" + wheel
    for wheel in ("pip-23.2.1-py3-none-any.whl", "setuptools-65.5.0-py3-none-any.whl")
}


def digest(path: Path) -> str:
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def package_contract(
    path: Path, component: str, version: str, runtime_bytes: bytes
) -> bytes:
    with tarfile.open(path) as archive:
        members = archive.getmembers()
        contracts = [
            m
            for m in members
            if m.name in {".anolisa/component.toml", "./.anolisa/component.toml"}
        ]
        if len(contracts) != 1 or not contracts[0].isfile():
            raise ValueError("raw package must contain one component contract")
        stream = archive.extractfile(contracts[0])
        if stream is None:
            raise ValueError("raw component contract is unreadable")
        data = stream.read()
        contract = tomllib.loads(data.decode())
        if (
            contract["component"]["name"] != component
            or contract["component"]["version"] != version
        ):
            raise ValueError("raw package identity differs from preview")
        for member in members:
            name = member.name.removeprefix("./")
            standard_wheel = (
                component == "sec-core"
                and member.isfile()
                and name in BASE_ENSUREPIP_WHEELS
            )
            if (
                name.endswith(
                    ("skill-score-images.tar", "skill-score-images.tar.gz", ".whl")
                )
                and not standard_wheel
            ):
                raise ValueError(
                    "third-party dependencies/images must remain outside the raw package"
                )
            if name == "bin/agent-sec-skill-score":
                raise ValueError(
                    "raw preview must use the unified agent-sec-cli entrypoint"
                )
            if component == "sec-core" and (
                "/skill-score/" in name
                or name == "lib/anolisa/sec-core-skill-score"
                or name.startswith("lib/anolisa/sec-core-skill-score/")
            ):
                raise ValueError(
                    "base raw package cannot contain the optional scoring runtime"
                )
            if component == "sec-core-skill-score" and (
                "/python/" in name or "/site-packages/" in name
            ):
                raise ValueError(
                    "optional raw package cannot bundle third-party runtime contents"
                )
        if component == "sec-core-skill-score":
            entry = archive.extractfile(
                "lib/anolisa/sec-core-skill-score/runtime-manifest.json"
            )
            if entry is None or entry.read() != runtime_bytes:
                raise ValueError("optional raw runtime manifest differs from release")
            if contract["component"]["contract"]["min_anolisa_version"] != "0.3.20":
                raise ValueError(
                    "optional component must require the preparation-capable installer"
                )
        return data


def prepare(
    directory: Path, source_commit: str, version: str, components: Path
) -> dict:
    if not re.fullmatch(r"[a-f0-9]{40}", source_commit):
        raise ValueError("source commit must be a full Git SHA")
    if not re.fullmatch(
        r"\d+\.\d+\.\d+-skillscore\."
        + source_commit[:12]
        + r"(?:\.base\.[a-f0-9]{12})?",
        version,
    ):
        raise ValueError("preview version must bind the source commit")
    required = [
        "runtime-manifest.json",
        "skill-score-images.tar.gz",
        "images.json",
        "tool-inventory.json",
        "source.tar.gz",
        INSTALLER,
    ]
    packages = {
        component: f"{component}-{version}-linux-x86_64.tar.gz"
        for component in COMPONENTS
    }
    required.extend(packages.values())
    for name in required:
        path = directory / name
        if (
            path.is_symlink()
            or not path.is_file()
            or not 0 < path.stat().st_size < 2 * 1024**3
        ):
            raise ValueError(
                f"release asset must be a nonempty regular file below 2 GiB: {name}"
            )
    runtime_bytes = (directory / "runtime-manifest.json").read_bytes()
    runtime = json.loads(runtime_bytes)
    if runtime["version"] != version or runtime["source_commit"] != source_commit:
        raise ValueError("runtime manifest identity differs from release")
    base = runtime["work_base_image"]
    if (
        json.loads((directory / "images.json").read_text()).get("work_base_image")
        != base
        or json.loads((directory / "tool-inventory.json").read_text()).get(
            "work_base_image"
        )
        != base
        or ".base." in version
        and not version.endswith(".base." + base["image_id"][7:19])
    ):
        raise ValueError("work base image identity differs across release metadata")
    archive = runtime["image_archive"]
    image_path = directory / archive["file"]
    if (
        archive["sha256"] != digest(image_path)
        or archive["bytes"] != image_path.stat().st_size
    ):
        raise ValueError("runtime image archive differs from release")
    index = 'schema_version = 1\nchannel = "stable"\npublisher = "asc-fork-preview"\n'
    for component, name in packages.items():
        contract = package_contract(directory / name, component, version, runtime_bytes)
        index += f"""\n[[entries]]
component = "{component}"
version = "{version}"
channel = "stable"
artifact_type = "tar_gz"
backend = "raw"
url = "{name}"
os = "linux"
arch = "x86_64"
install_modes = ["system"]
sha256 = "{digest(directory / name)}"
manifest_digest = "sha256:{hashlib.sha256(contract).hexdigest()}"
"""
    for name in ("index-v2.toml", "index.toml"):
        (directory / name).write_text(index)
    shutil.copyfile(components, directory / "components-v2.toml")
    required.extend(("index-v2.toml", "index.toml", "components-v2.toml"))
    manifest = {
        "schema_version": "agent-sec-skill-score/release-v3",
        "source_commit": source_commit,
        "development_build": False,
        "component": "sec-core",
        "version": version,
        "raw_packages": packages,
        "raw_repository": "v1",
        "cli": "agent-sec-cli",
        "command_prefix": ["skillsec", "score"],
        "runtime_manifest": "runtime-manifest.json",
        "installer": {
            "file": INSTALLER,
            "version": "0.3.20",
            "sha256": digest(directory / INSTALLER),
        },
        "python_worker": runtime["python"]["version"],
        "harbor": runtime["harbor_version"],
        "build_libc": dict(zip(("name", "version"), platform.libc_ver())),
        "images": json.loads((directory / "images.json").read_text()),
        "artifacts": [
            {
                "file": name,
                "bytes": (directory / name).stat().st_size,
                "sha256": digest(directory / name),
            }
            for name in required
        ],
    }
    (directory / "release-manifest.json").write_text(
        json.dumps(manifest, indent=2) + "\n"
    )
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
