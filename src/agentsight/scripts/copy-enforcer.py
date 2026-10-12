#!/usr/bin/env python3
"""Copy an attested enforcer, checking the bytes written against its SHA256 receipt."""

import hashlib
import os
from pathlib import Path
import re
import stat
import sys
import tempfile


def copy_enforcer(source: Path, destination: Path) -> None:
    # Receipts contain only a digest, not a path that could redirect verification.
    expected = Path(str(source) + ".sha256").read_text(encoding="ascii").strip()
    if not re.fullmatch(r"[0-9a-f]{64}", expected):
        raise ValueError(f"invalid enforcer SHA256 receipt: {source}.sha256")
    with source.open("rb") as stream:
        mode = os.fstat(stream.fileno()).st_mode
        if not stat.S_ISREG(mode) or not mode & 0o111:
            raise ValueError(f"not an executable regular file: {source}")
        binary = stream.read()
    if hashlib.sha256(binary).hexdigest() != expected:
        raise ValueError(f"enforcer SHA256 receipt mismatch: {source}")

    # Publish only the checked buffer, not a second copy from a mutable input path.
    temporary = None
    try:
        with tempfile.NamedTemporaryFile(dir=destination.parent, delete=False) as stream:
            temporary = Path(stream.name)
            stream.write(binary)
            os.fchmod(stream.fileno(), 0o755)
        os.replace(temporary, destination)
    finally:
        if temporary is not None:
            try:
                temporary.unlink()
            except FileNotFoundError:
                pass


if __name__ == "__main__":
    if len(sys.argv) != 3:
        sys.exit(f"usage: {sys.argv[0]} <enforcer> <destination>")
    try:
        copy_enforcer(Path(sys.argv[1]), Path(sys.argv[2]))
    except (OSError, ValueError) as error:
        sys.exit(f"cannot copy attested enforcer: {error}")
