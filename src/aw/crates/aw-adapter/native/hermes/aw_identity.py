"""Verify the immutable source distribution before using revision-specific Hermes APIs."""

from functools import lru_cache
import hashlib
import os
from pathlib import Path
import re
import stat
import sys

REVISION = "3b421578588243f920886e6fe79a6faa87ea026e"
# SHA-256 over paths, executable bits and file contents in the official Git tree.
SOURCE_DIGEST = "b80ef3ed83512b44cf56b353eeeb4aeb5fffdbb894d06e1431ce33a720d26405"


def _trusted(path, *, ancestor=False):
    metadata = path.lstat()
    if metadata.st_uid not in (0, os.geteuid()) or (
        metadata.st_mode & 0o022 and not (
            ancestor and stat.S_ISDIR(metadata.st_mode) and metadata.st_mode & stat.S_ISVTX
        )
    ):
        raise ValueError(f"unprotected Hermes source path: {path}")
    return metadata.st_mode


def source_digest(source):
    root = Path(source)
    if not root.is_absolute() or root.resolve(strict=True) != root:
        raise ValueError("Hermes source must be an absolute canonical directory")
    for parent in root.parents:
        if not stat.S_ISDIR(_trusted(parent, ancestor=True)):
            raise ValueError("invalid Hermes source parent")
    digest = hashlib.sha256()

    def visit(directory):
        _trusted(directory)
        stamp_seen = False
        for path in sorted(directory.iterdir(), key=lambda child: os.fsencode(child.name)):
            if path == root / ".git":
                continue
            mode = _trusted(path)
            if directory == root and re.fullmatch(r"\.[a-z0-9_-]{1,32}-revision", path.name):
                if stamp_seen:
                    raise ValueError("multiple Hermes packaged revision stamps")
                if not stat.S_ISREG(mode) or path.stat().st_size != 41:
                    raise ValueError("invalid Hermes packaged revision stamp")
                if path.read_bytes() != (REVISION + "\n").encode():
                    raise ValueError("Hermes packaged revision differs")
                stamp_seen = True
                continue
            if stat.S_ISDIR(mode):
                if path.name == "__pycache__":
                    # A fresh external cache isolates imports from packaged bytecode.
                    for cache in path.iterdir():
                        if (not sys.pycache_prefix or cache.suffix != ".pyc"
                                or not stat.S_ISREG(_trusted(cache))):
                            raise ValueError("invalid Hermes bytecode cache")
                else:
                    visit(path)
            elif stat.S_ISREG(mode):
                if path == root / ".bytecode-fingerprint":
                    continue
                digest.update(os.fsencode(path.relative_to(root)) + b"\0")
                digest.update(b"x" if mode & stat.S_IXUSR else b"-")
                with path.open("rb") as stream:
                    digest.update(hashlib.file_digest(stream, "sha256").digest())
            else:
                raise ValueError(f"unsupported Hermes source file: {path}")

    visit(root)
    return digest.hexdigest()


def verified_revision(source):
    if source_digest(source) != SOURCE_DIGEST:
        raise ValueError("Hermes source differs from the verified 0.23.1 distribution")
    return REVISION


@lru_cache(maxsize=1)
def installed_revision(source):
    return verified_revision(source)


def runtime_revision():
    from hermes_cli import version_info

    source = Path(version_info.__file__).resolve().parent.parent
    if (source / ".git").exists():
        return version_info.get_version_info().commit
    return installed_revision(source)
