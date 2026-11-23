#!/usr/bin/env python3
"""Check an installer archive without executing or extracting its contents."""

import argparse
import hashlib
import json
import re
import tarfile
from pathlib import Path, PurePosixPath

REQUIRED = {
    "bin/aw",
    "bin/aw-provider-sec-core",
    "bin/aw-provider-tokenless",
    "bin/tokenless",
    "bin/agent-sec-cli",
    "bin/agent-sec-daemon",
    "bin/agent-sec-daemon-health",
    "bin/cosh",
    "libexec/anolisa/cosh-ng/cosh-core",
    "libexec/anolisa/cosh-ng/cosh-shell",
    "libexec/anolisa/cosh-ng/cosh-gateway",
    "runtime.py",
    "plugin/manifest.json",
}


def verify(archive: Path, checksum: str | None = None, version: str | None = None) -> dict:
    """Reject damaged, unsafe, incomplete or mismatched installer archives."""
    if checksum is not None:
        with archive.open("rb") as stream:
            actual = hashlib.file_digest(stream, "sha256").hexdigest()
        if not re.fullmatch(r"[0-9a-f]{64}", checksum) or actual != checksum:
            raise ValueError("Archive checksum mismatch")
    with tarfile.open(archive) as tar:
        members = tar.getmembers()
        names = set()
        roots = set()
        for member in members:
            path = PurePosixPath(member.name)
            if (
                path.is_absolute()
                or ".." in path.parts
                or "\\" in member.name
                or not path.parts
                or path.as_posix() != member.name.rstrip("/")
            ):
                raise ValueError("Unsafe archive path")
            if member.name in names or not (member.isfile() or member.isdir()):
                raise ValueError("Duplicate path, link or special archive entry")
            names.add(member.name)
            roots.add(path.parts[0])
            mode = (
                0o755
                if member.isdir()
                or path.name == "install"
                or "/payload/bin/" in member.name
                or path.name in {"cosh-core", "cosh-shell", "cosh-gateway"}
                else 0o644
            )
            if member.uid or member.gid or member.mode != mode:
                raise ValueError("Unexpected archive ownership or permissions")
        if len(roots) != 1:
            raise ValueError("Expected one installer root")
        root = roots.pop()
        manifest_name = root + "/bundle.json"
        if manifest_name not in names or not tar.getmember(manifest_name).isfile():
            raise ValueError("Missing installer manifest")
        manifest = json.load(tar.extractfile(manifest_name))
        if not isinstance(manifest, dict):
            raise ValueError("Invalid installer manifest")
        if manifest.get("schema") != 1 or manifest.get("platform") != "linux-x86_64":
            raise ValueError("Unsupported installer format or architecture")
        actual_version = str(manifest.get("version", ""))
        if (
            not re.fullmatch(r"[0-9]+\.[0-9]+\.[0-9]+", actual_version)
            or root != f"anolisa-omarchy-{actual_version}"
            or version
            and actual_version != version
        ):
            raise ValueError("Installer version mismatch")
        for field in ("files", "install_files"):
            entries = manifest.get(field)
            if not isinstance(entries, dict) or not entries:
                raise ValueError("Missing installer file inventory")
            for name, value in entries.items():
                path = PurePosixPath(name)
                if (
                    not path.parts
                    or path.is_absolute()
                    or ".." in path.parts
                    or path.as_posix() != name
                    or "\\" in name
                    or not isinstance(value, str)
                    or not re.fullmatch(r"[0-9a-f]{64}", value)
                ):
                    raise ValueError("Invalid installer file inventory")
        expected = {root + "/payload/" + name: value for name, value in manifest["files"].items()}
        expected.update(
            {root + "/" + name: value for name, value in manifest["install_files"].items()}
        )
        actual_files = {member.name for member in members if member.isfile()}
        if actual_files != set(expected) | {root + "/bundle.json"}:
            raise ValueError("Installer files differ from the manifest")
        if not {"install", "install.py", "verify_release.py", "LICENSE"} <= set(
            manifest["install_files"]
        ):
            raise ValueError("Missing installer entry points or license")
        if not REQUIRED <= set(manifest["files"]) or "bin/qodercli" in manifest["files"]:
            raise ValueError("Missing required program or unexpectedly bundled Qoder")
        for name, expected_hash in expected.items():
            actual_hash = hashlib.sha256(tar.extractfile(name).read()).hexdigest()
            if actual_hash != expected_hash:
                raise ValueError(f"Installer file checksum mismatch: {name}")
        return manifest


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("archive", type=Path)
    parser.add_argument("--sha256")
    parser.add_argument("--version")
    args = parser.parse_args()
    metadata = verify(args.archive, args.sha256, args.version)
    print(f"Verified ANOLISA Omarchy {metadata['version']} ({metadata['platform']})")
