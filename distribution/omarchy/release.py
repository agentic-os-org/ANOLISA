#!/usr/bin/env python3
"""Build and verify one Linux x86_64 Omarchy release from pinned inputs."""

import argparse
import hashlib
import json
import os
import platform
import re
import shutil
import subprocess
import sys
import tarfile
import tempfile
import urllib.request
from pathlib import Path, PurePosixPath

import package
from sbom import write_sbom
from verify_release import verify

HERE = Path(__file__).resolve().parent
REPO = HERE.parents[1]
BRIDGES = {
    "aw-cli": ("src/aw/Cargo.toml", "aw"),
    "aw-provider-sec-core": ("providers/agent-sec-core/Cargo.toml", "aw-provider-sec-core"),
    "aw-provider-tokenless": ("providers/tokenless/Cargo.toml", "aw-provider-tokenless"),
}


def git(*arguments: str) -> str:
    return subprocess.check_output(["git", "-C", str(REPO), *arguments], text=True).strip()


def source_receipt(allow_dirty: bool) -> dict:
    dirty = bool(git("status", "--porcelain", "--untracked-files=normal"))
    if dirty and not allow_dirty:
        raise ValueError(
            "Release builds require a clean checkout; --allow-dirty is only for local review"
        )
    files = set()
    for mode in (("--cached",), ("--others", "--exclude-standard")):
        output = git(
            "ls-files",
            *mode,
            "--",
            ".github",
            "distribution/omarchy",
            "docs/user-guide/en/omarchy",
            "docs/user-guide/zh/omarchy",
            "src/aw",
            "providers",
            "LICENSE",
        )
        files.update(output.splitlines())
    digest = hashlib.sha256()
    for name in sorted(files):
        path = REPO / name
        if path.is_file():
            digest.update(name.encode() + b"\0" + bytes.fromhex(package.digest(path)))
    return {"commit": git("rev-parse", "HEAD"), "dirty": dirty, "tree_sha256": digest.hexdigest()}


def fetch(asset: dict, cache: Path) -> Path:
    """Use a cache only when its content matches the checked-in SHA-256."""
    url, checksum = asset["url"], asset["sha256"]
    if not url.startswith(
        "https://github.com/agentic-os-org/ANOLISA/releases/download/"
    ) or not re.fullmatch(r"[0-9a-f]{64}", checksum):
        raise ValueError("Expected a pinned official release input")
    destination = cache / url.rsplit("/", 1)[-1]
    if (
        destination.is_file()
        and not destination.is_symlink()
        and package.digest(destination) == checksum
    ):
        return destination
    print(f"Downloading {destination.name}", flush=True)
    request = urllib.request.Request(url, headers={"User-Agent": "anolisa-omarchy-packager"})
    descriptor, name = tempfile.mkstemp(dir=cache, prefix=".download-")
    temporary = Path(name)
    try:
        with (
            os.fdopen(descriptor, "wb") as target,
            urllib.request.urlopen(request, timeout=60) as response,
        ):
            if not response.url.startswith("https://"):
                raise ValueError("Release input redirected away from HTTPS")
            shutil.copyfileobj(response, target)
        if package.digest(temporary) != checksum:
            raise ValueError(f"Release input checksum mismatch: {destination.name}")
        temporary.replace(destination)
    finally:
        temporary.unlink(missing_ok=True)
    return destination


def stage_upstream(archive: Path, name: str, payload: Path) -> None:
    """Copy only the native runtime used by this integration, without tar extraction."""
    selected = {
        "sec-core": {"bin/agent-sec-cli", "bin/agent-sec-daemon", "bin/agent-sec-daemon-health"},
        "tokenless": {"bin/tokenless"},
    }
    seen = set()
    with tarfile.open(archive) as tar:
        for member in tar:
            path = PurePosixPath(member.name)
            if path.is_absolute() or ".." in path.parts or "\\" in member.name:
                raise ValueError("Unsafe upstream archive path")
            relative = path.as_posix().removeprefix("./")
            keep = (
                name == "cosh-ng"
                and relative.startswith(("bin/", "libexec/", "share/"))
                or relative in selected.get(name, set())
                or name == "sec-core"
                and relative.startswith("share/")
            )
            if not keep or member.isdir():
                continue
            if not member.isfile() or relative in seen:
                raise ValueError("Unexpected upstream link, special entry or duplicate file")
            seen.add(relative)
            target = payload / relative
            if target.exists():
                raise ValueError(f"Colliding upstream payload file: {relative}")
            target.parent.mkdir(parents=True, exist_ok=True)
            with tar.extractfile(member) as source, target.open("wb") as stream:
                shutil.copyfileobj(source, stream)
            target.chmod(
                0o755
                if relative.startswith("bin/")
                or path.name in {"cosh-core", "cosh-shell", "cosh-gateway"}
                else 0o644
            )


def build_bridges(payload: Path, cache: Path, toolchain: str) -> tuple[dict, dict]:
    """Compile the CLI and independent Provider workspaces with their own lockfiles."""
    environment = dict(os.environ, RUSTUP_TOOLCHAIN=toolchain)
    environment.setdefault("CARGO_TARGET_DIR", str(cache / "target"))
    environment.setdefault("CARGO_PROFILE_RELEASE_DEBUG", "0")
    environment.setdefault("CARGO_PROFILE_RELEASE_LTO", "false")
    environment.setdefault("CARGO_PROFILE_RELEASE_CODEGEN_UNITS", "16")
    metadata, versions = {}, {}
    for name, (manifest, binary) in BRIDGES.items():
        print(f"Building {name}", flush=True)
        command = [
            "cargo",
            "build",
            "--release",
            "--locked",
            "--manifest-path",
            str(REPO / manifest),
            "--bin",
            binary,
        ]
        subprocess.run(command, cwd=REPO, env=environment, check=True)
        document = json.loads(
            subprocess.check_output(
                [
                    "cargo",
                    "metadata",
                    "--locked",
                    "--format-version",
                    "1",
                    "--filter-platform",
                    "x86_64-unknown-linux-gnu",
                    "--manifest-path",
                    str(REPO / manifest),
                ],
                env=environment,
            )
        )
        metadata[name] = document
        versions[binary] = next(
            item["version"] for item in document["packages"] if item["name"] == name
        )
        source = Path(environment["CARGO_TARGET_DIR"]) / "release" / binary
        shutil.copy2(source, payload / "bin" / binary)
    return metadata, versions


def verify_programs(payload: Path, versions: dict) -> None:
    """Check actual native program identity before recording its component version."""
    programs = {
        "aw": "bin/aw",
        "cosh-ng": "libexec/anolisa/cosh-ng/cosh-core",
        "sec-core": "bin/agent-sec-cli",
        "tokenless": "bin/tokenless",
    }
    for name, relative in programs.items():
        path = payload / relative
        with path.open("rb") as stream:
            header = stream.read(20)
        if header[:6] != b"\x7fELF\x02\x01" or header[18:20] != b"\x3e\x00":
            raise ValueError(f"Expected a Linux x86_64 program: {relative}")
        result = subprocess.run(
            [str(path), "--version"], capture_output=True, text=True, timeout=15, check=True
        )
        if versions[name] not in result.stdout:
            raise ValueError(f"Program version differs from its input manifest: {name}")


def verify_inventory(document: dict, component: dict) -> None:
    identity = document.get("metadata", {}).get("component", {})
    hashes = identity.get("hashes", [])
    if (
        document.get("bomFormat") != "CycloneDX"
        or identity.get("version") != component["version"]
        or not any(
            item.get("alg") == "SHA-256"
            and item.get("content") == component["assets"]["archive"]["sha256"]
            for item in hashes
        )
    ):
        raise ValueError("Upstream SBOM does not bind the selected archive and version")


def build(
    output: Path, cache: Path, version: str, tag: str = "", allow_dirty: bool = False
) -> Path:
    config = package.settings()
    if platform.system() != "Linux" or platform.machine() != "x86_64":
        raise ValueError("Build on Linux x86_64")
    if version != config["version"] or not re.fullmatch(r"[0-9]+\.[0-9]+\.[0-9]+", version):
        raise ValueError("Version must match release.json")
    receipt = source_receipt(allow_dirty)
    if tag:
        # Reject an unreviewed worktree before resolving or building any tag.
        if receipt["dirty"] or tag != config["tag_prefix"] + version:
            raise ValueError("Release tag must identify this clean checkout and version")
        if git("rev-parse", tag + "^{commit}") != receipt["commit"]:
            raise ValueError("Release tag does not identify HEAD")
    output, cache = output.resolve(), cache.resolve()
    if output.exists():
        raise ValueError("Use a new output directory; existing releases are never overwritten")
    output.parent.mkdir(parents=True, exist_ok=True)
    cache.mkdir(parents=True, exist_ok=True)
    epoch = int(git("show", "-s", "--format=%ct", "HEAD"))
    with tempfile.TemporaryDirectory(dir=output.parent, prefix=".omarchy-build-") as temporary:
        work = Path(temporary)
        payload = work / "payload"
        payload.mkdir()
        inventories = {}
        for name, component in config["upstream"].items():
            stage_upstream(fetch(component["assets"]["archive"], cache), name, payload)
            inventories[name] = json.loads(
                fetch(component["assets"]["sbom"], cache).read_text(encoding="utf-8")
            )
            verify_inventory(inventories[name], component)
        metadata, versions = build_bridges(payload, cache, config["rust_toolchain"])
        versions.update({name: item["version"] for name, item in config["upstream"].items()})
        verify_programs(payload, versions)
        receipt["inputs"] = config["upstream"]
        receipt["rustc"] = subprocess.check_output(
            ["rustc", "--version"],
            text=True,
            env=dict(os.environ, RUSTUP_TOOLCHAIN=config["rust_toolchain"]),
        ).strip()
        artifacts = work / "artifacts"
        artifacts.mkdir()
        archive, manifest = package.assemble(payload, artifacts, receipt, versions, epoch)
        sbom = write_sbom(archive, manifest, inventories, metadata)
        for path in (archive, sbom):
            Path(str(path) + ".sha256").write_text(
                f"{package.digest(path)}  {path.name}\n", encoding="ascii"
            )
        verify(archive, package.digest(archive), version)
        subprocess.run(
            [
                sys.executable,
                str(REPO / ".github/actions/prebuilt-rust-common/verify-artifacts.py"),
                "--directory",
                str(artifacts),
                "--component",
                "omarchy",
                "--version",
                version,
                "--os",
                "linux",
                "--arch",
                "x86_64",
                "--layout",
                "flat",
            ],
            check=True,
        )
        # Detect source changes that raced the build before exposing any release files.
        after = source_receipt(allow_dirty)
        if any(after[key] != receipt[key] for key in ("commit", "dirty", "tree_sha256")):
            raise ValueError("Source changed during the build")
        artifacts.rename(output)
    print(f"Verified release files: {output}", flush=True)
    return output


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--cache", type=Path, required=True)
    parser.add_argument("--version", required=True)
    parser.add_argument("--tag", default="")
    parser.add_argument(
        "--allow-dirty", action="store_true", help="Build an unpublished local review candidate"
    )
    args = parser.parse_args()
    build(args.output, args.cache, args.version, args.tag, args.allow_dirty)


if __name__ == "__main__":
    main()
