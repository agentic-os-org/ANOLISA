"""Resolve an explicitly selected, fingerprint-pinned static TLS fixture entry.

Offsets are validated against a local TLS receiver before publication. Unknown
builds never fall back to a prologue guess or a user-supplied numeric offset.
"""

import hashlib
from pathlib import Path
import struct

PROFILES = {
    "claude-2.1.161-linux-x64": (
        "1f6a22f387a3bce496b6d869389a35dffb5a69c97d9831833f3bd6dc0e6c6c28",
        68571504,
    ),
}


def checked_offset(data: bytes, digest: str, offset: int) -> int:
    if hashlib.sha256(data).hexdigest() != digest:
        raise ValueError("unsupported_static_fingerprint")
    if len(data) < 64 or data[:6] != b"\x7fELF\x02\x01":
        raise ValueError("unsupported_static_elf")
    kind, machine = struct.unpack_from("<HH", data, 16)
    phoff = struct.unpack_from("<Q", data, 32)[0]
    phsize, phnum = struct.unpack_from("<HH", data, 54)
    if kind not in (2, 3) or machine != 62 or phsize != 56:
        raise ValueError("unsupported_static_abi")
    if phnum == 0 or phoff + phsize * phnum > len(data):
        raise ValueError("invalid_static_segments")
    matches = 0
    for index in range(phnum):
        segment, flags, start, _, _, size, _, _ = struct.unpack_from(
            "<IIQQQQQQ", data, phoff + index * phsize
        )
        if start + size > len(data):
            raise ValueError("invalid_static_segment_bounds")
        if segment == 1 and flags & 1 and start <= offset < start + size:
            matches += 1
    if matches != 1:
        raise ValueError("unsupported_static_entry_mapping")
    return offset


def resolve_static(pid: int, function: str, profile: str) -> tuple[Path, int, str]:
    if profile not in PROFILES or function != "SSL_write":
        raise ValueError("unsupported_static_profile_or_function")
    path = Path(f"/proc/{pid}/exe")
    if path.stat().st_size > 512 << 20:
        raise ValueError("static_binary_size_limit")
    digest, offset = PROFILES[profile]
    checked_offset(path.read_bytes(), digest, offset)
    return path, offset, digest
