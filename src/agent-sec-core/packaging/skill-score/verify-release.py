"""Validate optional package ownership and bind assessment release artifacts."""

import argparse
import hashlib
import json
import platform
import posixpath
import re
import subprocess
import tarfile
from collections import deque
from pathlib import Path


def digest(path: Path) -> str:
    """Hash a release artifact without loading image archives into memory."""
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def rpm_files(path: Path) -> list[str]:
    """Read package ownership before installing either optional package."""
    return subprocess.run(
        ["rpm", "-qlp", str(path)], check=True, capture_output=True, text=True
    ).stdout.splitlines()


def validate_paths(paths: list[str], prefix: str) -> None:
    """Reject packages that would overwrite system AgentSecCore entrypoints."""
    if not paths or any(
        path != prefix and not path.startswith(prefix + "/") for path in paths
    ):
        raise ValueError(f"package must own only {prefix}")


def validate_archive(path: Path) -> None:
    """Limit the shared Ubuntu/RPM payload to the two private prefixes."""
    prefixes = ("opt/agent-sec/skill-score-cli", "opt/agent-sec/skill-score-runtime")

    def within_prefix(name: str) -> bool:
        return any(
            name == prefix or name.startswith(prefix + "/") for prefix in prefixes
        )

    with tarfile.open(path) as archive:
        members = archive.getmembers()
        entries = {member.name: member for member in members}
        if len(entries) != len(members):
            raise ValueError("payload contains duplicate paths")
        for member in members:
            if member.name in {"opt", "opt/agent-sec"} and member.isdir():
                continue
            normalized = posixpath.normpath(member.name)
            if (
                member.name.startswith("/")
                or normalized != member.name
                or not within_prefix(normalized)
            ):
                raise ValueError("payload archive escapes assessment prefixes")
            if member.issym() or member.islnk():
                target = (
                    member.linkname
                    if member.islnk()
                    else posixpath.join(posixpath.dirname(member.name), member.linkname)
                )
                if not within_prefix(posixpath.normpath(target)):
                    raise ValueError("payload link escapes assessment prefixes")
            elif not (member.isdir() or member.isfile()):
                raise ValueError("payload contains a special filesystem object")
            if member.mode & 0o6000:
                raise ValueError("payload contains setuid or setgid bits")

        # Resolve links before processing '..', as the kernel does. Lexical
        # normalization alone misses a -> '.', b -> 'a/../outside'.
        for member in members:
            pending = deque(member.name.split("/"))
            resolved: list[str] = []
            links = 0
            while pending:
                part = pending.popleft()
                if part in {"", "."}:
                    continue
                if part == "..":
                    if not resolved:
                        raise ValueError("payload link escapes assessment prefixes")
                    resolved.pop()
                else:
                    resolved.append(part)
                name = "/".join(resolved)
                if name not in {"", "opt", "opt/agent-sec"} and not within_prefix(name):
                    raise ValueError("payload link escapes assessment prefixes")
                entry = entries.get(name)
                if part != ".." and entry and (entry.issym() or entry.islnk()):
                    links += 1
                    if links > 40 or entry.linkname.startswith("/"):
                        raise ValueError("payload contains a cyclic or absolute link")
                    if entry.islnk():
                        resolved.clear()
                    else:
                        resolved.pop()
                    pending.extendleft(reversed(entry.linkname.split("/")))
            if not member.isdir() and not within_prefix("/".join(resolved)):
                raise ValueError("payload link escapes assessment prefixes")


def main() -> None:
    """Write the release manifest only after all required artifacts validate."""
    parser = argparse.ArgumentParser()
    parser.add_argument("directory", type=Path, nargs="?")
    parser.add_argument("--source-commit")
    parser.add_argument("--payload-only", type=Path)
    parser.add_argument("--development", action="store_true")
    args = parser.parse_args()
    if args.payload_only is not None:
        if (
            args.directory is not None
            or args.source_commit is not None
            or args.development
        ):
            parser.error("--payload-only cannot be combined with release arguments")
        validate_archive(args.payload_only)
        return
    if args.directory is None or args.source_commit is None:
        parser.error("directory and --source-commit are required for a release")
    if not re.fullmatch(r"[a-f0-9]{40}", args.source_commit):
        raise ValueError("source commit must be a full Git SHA")
    packages = list(args.directory.glob("*.rpm"))
    if len(packages) != 2:
        raise ValueError("exactly two assessment RPMs are required")
    for kind in ("cli", "runtime"):
        selected = [
            p for p in packages if p.name.startswith(f"agent-sec-skill-score-{kind}-")
        ]
        if len(selected) != 1:
            raise ValueError(f"exactly one {kind} RPM is required")
        validate_paths(rpm_files(selected[0]), f"/opt/agent-sec/skill-score-{kind}")
    validate_archive(args.directory / "skill-score-payload.tar.gz")
    required = [
        "source.tar.gz",
        "skill-score-payload.tar.gz",
        "skill-score-images.tar",
        "images.json",
        "tool-inventory.json",
    ]
    paths = packages + [args.directory / name for name in required]
    images = json.loads((args.directory / "images.json").read_text())
    if not images.get("image_details") or not images.get("versions"):
        raise ValueError("image identity and runtime versions are required")
    result = {
        "schema_version": "agent-sec-skill-score/release-v1",
        "source_commit": args.source_commit,
        "development_build": args.development,
        "python_cli": "3.11.6",
        "python_worker": "3.12.11",
        "harbor": "0.22.0",
        "build_libc": dict(zip(("name", "version"), platform.libc_ver())),
        "images": images,
        "artifacts": [
            {"file": path.name, "bytes": path.stat().st_size, "sha256": digest(path)}
            for path in paths
        ],
    }
    (args.directory / "release-manifest.json").write_text(
        json.dumps(result, indent=2) + "\n"
    )


if __name__ == "__main__":
    main()
