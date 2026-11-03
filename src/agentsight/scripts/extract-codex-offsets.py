#!/usr/bin/env python3
"""Extract OpenSSL 3.x function offsets from a codex binary.

Codex CLI links OpenSSL 3.x statically (via reqwest → native-tls → openssl-sys).
When the release binary keeps its symbol table, the offsets of `SSL_write_ex`,
`SSL_read_ex`, and `SSL_do_handshake` can be read directly with `nm`.
Falls back to `SSL_write` / `SSL_read` when the `_ex` variants are absent.

Produces a JSON entry suitable for the `codex_offsets.entries` array in
`agentsight.json` (Tier 3 offset table).

Usage:
  ./extract-codex-offsets.py <binary> [--symbols <symbol-file>]

`<binary>` is the deployed target executable; it supplies the fingerprint and the
executable `PT_LOAD` segments used to convert symbol addresses into file offsets.
Pass `--symbols` when that target is stripped, using the matching symbol file from
the same build (for example the official `codex.debug`). The symbol file must match
the target's Build ID, or its GNU debuglink CRC when no Build ID exists.
Omit `--symbols` only when the target itself still has a usable symbol table.

The output JSON is printed to stdout. Append it to
`agentsight.json -> codex_offsets.entries`.
"""

import argparse
import hashlib
import json
import os
import re
import struct
import subprocess
import sys
import zlib
from typing import Dict, Optional

HEAD_SIZE = 65536

# Preferred symbols (OpenSSL 3.x _ex variants). `SSL_write_ex` and `SSL_read_ex`
# return 0/1 with the actual byte count in `*written` / `*readbytes`; the
# Rust user-space gate routes BPF probes accordingly when `write_is_ex` /
# `read_is_ex` is true.
EX_SYMBOLS = ("SSL_write_ex", "SSL_read_ex", "SSL_do_handshake")
PLAIN_SYMBOLS = ("SSL_write", "SSL_read", "SSL_do_handshake")


def sha256_head(path: str) -> str:
    h = hashlib.sha256()
    with open(path, "rb") as f:
        h.update(f.read(HEAD_SIZE))
    return h.hexdigest()


def file_size(path: str) -> int:
    return os.path.getsize(path)


def read_buildid(path: str) -> Optional[str]:
    try:
        out = subprocess.check_output(
            ["readelf", "-n", path], stderr=subprocess.DEVNULL, universal_newlines=True
        )
    except (subprocess.CalledProcessError, FileNotFoundError):
        return None
    for line in out.splitlines():
        line = line.strip()
        if line.startswith("Build ID:"):
            return line.split(":", 1)[1].strip()
    return None


def detect_codex_version(path: str) -> Optional[str]:
    # Codex embeds its version in the binary as `codex-cli <ver>` or
    # `rust-v<ver>`; either is acceptable for the human-readable label.
    # Read in 1 MiB chunks to avoid loading the entire ~276 MB binary.
    patterns = (r"codex-cli (\d+\.\d+\.\d+)", r"rust-v(\d+\.\d+\.\d+)")
    try:
        with open(path, "rb") as f:
            for chunk in iter(lambda: f.read(1 << 20), b""):
                text = chunk.decode("utf-8", errors="ignore")
                for pat in patterns:
                    m = re.search(pat, text)
                    if m:
                        return m.group(1)
    except OSError:
        return None
    return None


def nm_symbols(binary: str) -> Dict[str, int]:
    """返回已定义代码符号的虚拟地址，尚未转换为 uprobe 文件偏移。

    Tries the regular symbol table first (`nm --defined-only`), then falls
    back to the dynamic symbol table (`nm -D --defined-only`) for release
    binaries that have been stripped of `.symtab` but still expose
    `.dynsym`. The regular table wins on collisions.
    """
    syms = {}  # type: Dict[str, int]
    for args in (
        ["nm", "--defined-only", binary],
        ["nm", "-D", "--defined-only", binary],
    ):
        try:
            out = subprocess.check_output(
                args,
                stderr=subprocess.DEVNULL,
                universal_newlines=True,
                encoding="utf-8",
                errors="replace",
            )
        except (subprocess.CalledProcessError, FileNotFoundError):
            continue
        for line in out.splitlines():
            parts = line.split()
            if len(parts) < 3:
                continue
            addr_str, kind, name = parts[0], parts[1], parts[2]
            if kind not in ("T", "t", "W", "w"):
                continue
            try:
                addr = int(addr_str, 16)
            except ValueError:
                continue
            syms.setdefault(name, addr)
    return syms


def pick_offsets(syms: Dict[str, int]) -> Optional[dict]:
    have_ex = all(name in syms for name in EX_SYMBOLS)
    if have_ex:
        return {
            "ssl_write": syms["SSL_write_ex"],
            "ssl_read": syms["SSL_read_ex"],
            "ssl_do_handshake": syms["SSL_do_handshake"],
            "write_is_ex": True,
            "read_is_ex": True,
        }
    have_plain = all(name in syms for name in PLAIN_SYMBOLS)
    if have_plain:
        return {
            "ssl_write": syms["SSL_write"],
            "ssl_read": syms["SSL_read"],
            "ssl_do_handshake": syms["SSL_do_handshake"],
            "write_is_ex": False,
            "read_is_ex": False,
        }
    return None


def read_debuglink_crc(binary: str) -> Optional[int]:
    """读取 GNU debuglink 校验值；官方 musl 包可能没有 Build ID。"""
    try:
        with open(binary, "rb") as stream:
            header = stream.read(64)
            if len(header) != 64 or header[:6] != b"\x7fELF\x02\x01":
                return None
            shoff = struct.unpack_from("<Q", header, 40)[0]
            shsize, shnum, names_index = struct.unpack_from("<HHH", header, 58)
            if shsize < 64 or names_index >= shnum:
                return None
            sections = []
            for index in range(shnum):
                stream.seek(shoff + index * shsize)
                sections.append(struct.unpack("<IIQQQQIIQQ", stream.read(64)))
            names_section = sections[names_index]
            if names_section[5] > 1024 * 1024:
                return None
            stream.seek(names_section[4])
            names = stream.read(names_section[5])
            for section in sections:
                if names[section[0] :].split(b"\0", 1)[0] != b".gnu_debuglink":
                    continue
                if section[5] > 4096:
                    return None
                stream.seek(section[4])
                data = stream.read(section[5])
                checksum_offset = (data.index(b"\0") + 4) & ~3
                return struct.unpack_from("<I", data, checksum_offset)[0]
    except (OSError, ValueError, struct.error):
        return None
    return None


def matching_symbol_file(binary: str, symbol_file: str) -> bool:
    target_id, symbol_id = read_buildid(binary), read_buildid(symbol_file)
    if target_id and symbol_id:
        return target_id == symbol_id
    expected = read_debuglink_crc(binary)
    if expected is None:
        return False
    checksum = 0
    try:
        with open(symbol_file, "rb") as stream:
            for data in iter(lambda: stream.read(1024 * 1024), b""):
                checksum = zlib.crc32(data, checksum)
    except OSError:
        # 读不到符号文件时不能宣称匹配；main 会统一报错并返回退出码。
        return False
    return checksum & 0xFFFFFFFF == expected


def file_offsets(binary: str, offsets: dict) -> dict:
    """用目标二进制的可执行 PT_LOAD 段转换地址，不能假定地址等于文件偏移。"""
    with open(binary, "rb") as stream:
        header = stream.read(64)
        if len(header) != 64 or header[:6] != b"\x7fELF\x02\x01":
            raise ValueError("expected a little-endian ELF64 target binary")
        phoff = struct.unpack_from("<Q", header, 32)[0]
        phsize, phnum = struct.unpack_from("<HH", header, 54)
        segments = []
        for index in range(phnum):
            stream.seek(phoff + index * phsize)
            raw = stream.read(56)
            if phsize < 56 or len(raw) != 56:
                raise ValueError("truncated ELF program header")
            kind, flags, offset, address, _, size, _, _ = struct.unpack(
                "<IIQQQQQQ", raw
            )
            if kind == 1 and flags & 1:
                segments.append((offset, address, size))
    result = dict(offsets)
    for name in ("ssl_write", "ssl_read", "ssl_do_handshake"):
        address = offsets[name]
        for offset, base, size in segments:
            if base <= address < base + size:
                result[name] = offset + address - base
                break
        else:
            raise ValueError(
                "{} address 0x{:x} is outside executable PT_LOAD segments".format(
                    name, address
                )
            )
    return result


def main() -> int:
    p = argparse.ArgumentParser(description=__doc__.split("\n", 1)[0])
    p.add_argument(
        "binary", help="target codex binary used for fingerprinting and file offsets"
    )
    p.add_argument(
        "--symbols",
        help="matching ELF symbol file (Build ID or GNU debuglink CRC must match)",
    )
    args = p.parse_args()

    binary = args.binary
    if not os.path.isfile(binary):
        print(f"error: not a file: {binary}", file=sys.stderr)
        return 1

    symbol_file = args.symbols or binary
    if args.symbols and not os.path.isfile(symbol_file):
        print(f"error: not a file: {symbol_file}", file=sys.stderr)
        return 1
    if args.symbols and not matching_symbol_file(binary, symbol_file):
        print(
            "error: symbol file must match the target Build ID or GNU debuglink CRC",
            file=sys.stderr,
        )
        return 4
    syms = nm_symbols(symbol_file)
    if not syms:
        print(
            "error: nm produced no symbols — is this a stripped binary?",
            file=sys.stderr,
        )
        return 2

    offsets = pick_offsets(syms)
    if offsets is None:
        missing = [s for s in EX_SYMBOLS if s not in syms] + [
            s for s in PLAIN_SYMBOLS if s not in syms
        ]
        print(
            "error: required SSL_* symbols not found "
            f"(checked {EX_SYMBOLS} and {PLAIN_SYMBOLS}; missing={sorted(set(missing))})",
            file=sys.stderr,
        )
        return 3

    try:
        offsets = file_offsets(binary, offsets)
    except (OSError, ValueError, struct.error) as error:
        print("error: invalid ELF offsets: {}".format(error), file=sys.stderr)
        return 5

    entry = {
        "codex_version": detect_codex_version(binary) or "unknown",
        "fingerprint": {
            "file_size": file_size(binary),
            "head_64k_sha256": sha256_head(binary),
        },
        "offsets": offsets,
    }
    build_id = read_buildid(binary)
    if build_id:
        entry["fingerprint"]["build_id"] = build_id

    json.dump(entry, sys.stdout, indent=2, sort_keys=False)
    sys.stdout.write("\n")
    return 0


if __name__ == "__main__":
    sys.exit(main())
