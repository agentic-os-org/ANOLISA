"""Build the supported historical installer for mandatory migration acceptance."""

import os
import shutil
import stat
import sys
import tarfile
import tempfile
from pathlib import Path, PurePosixPath
from typing import Callable

LEGACY_SOURCE = "7d277b8dd62eb85784f60fdcb55d3a8d709900a3"
MAX_ARCHIVE_BYTES = 100 * 1024 * 1024


def extract_source(archive: Path, destination: Path) -> None:
    """Extract bounded repository files without links or archive path traversal."""
    total = 0
    with tarfile.open(archive, "r:") as source:
        for count, member in enumerate(source, 1):
            name = PurePosixPath(member.name)
            if (
                count > 10000
                or name.is_absolute()
                or ".." in name.parts
                or not (
                    name.parts[:2] == ("src", "aw")
                    or (member.isdir() and name.parts == ("src",))
                )
                or not (member.isdir() or member.isfile())
                or member.size < 0
            ):
                raise ValueError(f"unsafe legacy source archive entry: {member.name}")
            total += member.size
            if total > MAX_ARCHIVE_BYTES:
                raise ValueError("legacy source archive exceeds its byte limit")
            target = destination.joinpath(*name.parts)
            if member.isdir():
                target.mkdir(parents=True, exist_ok=True)
            else:
                target.parent.mkdir(parents=True, exist_ok=True)
                with source.extractfile(member) as payload, target.open("xb") as output:
                    shutil.copyfileobj(payload, output)
                target.chmod(member.mode & 0o777)


def prepare_legacy_cli(aw: Path, repo: Path, run: Callable[..., str]) -> Path:
    """Build pinned historical code and reject missing evidence instead of skipping."""
    if sys.platform != "linux":
        raise ValueError("historical AW installer acceptance requires Linux")
    resolved = run(
        ["git", "rev-parse", f"{LEGACY_SOURCE}^{{commit}}"],
        repo,
        timeout=30,
        capture=True,
    ).strip()
    if resolved != LEGACY_SOURCE:
        raise ValueError("historical installer source SHA does not match its pin")
    target = aw / "target"
    target.mkdir(exist_ok=True)
    if target.is_symlink():
        raise ValueError("legacy build target must not be a symbolic link")
    build = target / "legacy-cli-build"
    if build.is_symlink():
        raise ValueError("legacy build directory must not be a symbolic link")
    with tempfile.TemporaryDirectory(prefix="legacy-cli-source-", dir=target) as temporary:
        source = Path(temporary)
        archive = source / "source.tar"
        run(
            ["git", "archive", "--format=tar", "--output", str(archive), LEGACY_SOURCE, "src/aw"],
            repo,
            timeout=30,
        )
        extract_source(archive, source)
        manifest = source / "src/aw/Cargo.toml"
        if not manifest.is_file():
            raise ValueError("historical AW workspace manifest is missing")
        run(
            [
                "cargo", "build", "--locked", "--manifest-path", str(manifest),
                "-p", "aw-cli", "--bin", "aw-package", "--bin", "aw",
                "--target-dir", str(build),
                "--config", "profile.dev.package.sha2.opt-level=3",
            ],
            aw,
            timeout=600,
        )
    binary = build / "debug/aw-package"
    for executable in (binary, binary.with_name("aw")):
        metadata = executable.lstat()
        if not stat.S_ISREG(metadata.st_mode) or not os.access(executable, os.X_OK):
            raise ValueError(f"historical AW binary is missing or not executable: {executable}")
    help_text = run([str(binary), "--help"], aw, timeout=10, capture=True)
    if any(f"aw-package {action}" not in help_text for action in ("install", "uninstall", "configure")):
        raise ValueError("historical installer does not expose the supported write commands")
    print(f"Historical installer acceptance source: {LEGACY_SOURCE}", flush=True)
    return binary
