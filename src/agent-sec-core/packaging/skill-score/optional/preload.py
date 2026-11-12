"""Export and activate deployment-owned images without a build-time Docker daemon."""

from __future__ import annotations

import gzip
import json
import os
import selectors
import shutil
import signal
import stat
import subprocess
import tempfile
import time
from collections.abc import Callable
from pathlib import Path

import image_build
from lifecycle import directory

SCHEMA = "agent-sec-skill-score/preload-v1"
ARCHIVE = "skill-score-images.tar.gz"
MAX_BYTES = 32 * 1024**3


def trusted_file(path: Path, owner: int, limit: int) -> None:
    metadata = path.lstat()
    if (
        not stat.S_ISREG(metadata.st_mode)
        or metadata.st_nlink != 1
        or metadata.st_uid != owner
        or metadata.st_mode & 0o022
        or not 0 < metadata.st_size <= limit
    ):
        raise ValueError(f"unsafe or oversized preload file: {path.name}")


def read(root: Path, manifest: dict, identity: str) -> tuple[dict, Path]:
    directory(root)
    owner = root.stat().st_uid
    if owner != os.geteuid() or root.stat().st_mode & 0o022:
        raise ValueError(
            "preload directory must be owned by the installer and not writable by others"
        )
    metadata = root / "manifest.json"
    trusted_file(metadata, owner, 1024 * 1024)
    value = json.loads(metadata.read_text())
    if value.get("schema") != SCHEMA:
        raise ValueError("unsupported preload manifest")
    receipt = value["image_build"]
    image_build.validate(receipt, manifest, identity)
    if not image_build.matches_override(receipt):
        raise ValueError(
            "preloaded business base differs from the configured base or recipe"
        )
    expected = value["archive"]
    if expected.get("file") != ARCHIVE:
        raise ValueError("preload archive must use the fixed local filename")
    archive = root / ARCHIVE
    trusted_file(archive, owner, MAX_BYTES)
    if archive.stat().st_size != expected.get("bytes") or image_build.digest(
        archive
    ) != expected.get("sha256"):
        raise ValueError("preload archive size or checksum mismatch")
    return receipt, archive


def stop(process: subprocess.Popen) -> None:
    if process.poll() is None:
        os.killpg(process.pid, signal.SIGTERM)
        try:
            process.wait(timeout=10)
        except subprocess.TimeoutExpired:
            os.killpg(process.pid, signal.SIGKILL)
            process.wait(timeout=5)


def load(archive: Path, check_cancelled: Callable[[], None], logs: Path) -> None:
    # Docker decompresses directly; do not materialize a second, uncompressed tar.
    with image_build.build_log(logs) as log:
        process = subprocess.Popen(
            ["docker", "load", "--input", str(archive)],
            stdout=log,
            stderr=subprocess.STDOUT,
            start_new_session=True,
        )
        try:
            deadline = time.monotonic() + 900
            while process.poll() is None:
                check_cancelled()
                if time.monotonic() >= deadline:
                    raise TimeoutError("preload image loading exceeded 900 seconds")
                time.sleep(0.1)
            check_cancelled()
            if process.returncode:
                raise RuntimeError("preload image loading failed")
        finally:
            stop(process)


def export(
    output: Path,
    receipt: dict,
    image_ids: dict[str, str],
    check_cancelled: Callable[[], None],
) -> None:
    """Create a deployment artifact from a verified installation, never a release payload."""
    directory(output.parent)
    if output.exists() or output.is_symlink():
        raise ValueError("preload export destination must not exist")
    staging = Path(tempfile.mkdtemp(prefix=".preload-", dir=output.parent))
    try:
        archive = staging / ARCHIVE
        process = subprocess.Popen(
            ["docker", "save", image_ids["work"], image_ids["egress"]],
            stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL,
            start_new_session=True,
        )
        try:
            deadline = time.monotonic() + 900
            count = 0
            with (
                process.stdout as source,
                archive.open("wb") as target,
                gzip.GzipFile(
                    filename="", fileobj=target, mode="wb", compresslevel=1, mtime=0
                ) as compressed,
                selectors.DefaultSelector() as selector,
            ):
                selector.register(source, selectors.EVENT_READ)
                while True:
                    check_cancelled()
                    if time.monotonic() >= deadline:
                        raise TimeoutError("preload export exceeded 900 seconds")
                    if not selector.select(timeout=0.1):
                        continue
                    block = os.read(source.fileno(), 1024 * 1024)
                    if not block:
                        break
                    count += len(block)
                    if count > MAX_BYTES:
                        raise ValueError("preload image export exceeds size limit")
                    compressed.write(block)
            if process.wait(timeout=10):
                raise RuntimeError("preload image export failed")
            check_cancelled()
        finally:
            stop(process)
        value = {
            "schema": SCHEMA,
            "image_build": receipt,
            "archive": {
                "file": ARCHIVE,
                "bytes": archive.stat().st_size,
                "sha256": image_build.digest(archive),
            },
        }
        (staging / "manifest.json").write_text(json.dumps(value, indent=2) + "\n")
        staging.chmod(0o755)
        staging.rename(output)
    finally:
        if staging.exists():
            shutil.rmtree(staging)
