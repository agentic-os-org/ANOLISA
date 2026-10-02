#!/usr/bin/env python3
"""Extract OpenSSL 3.x function offsets from a codex binary.

Codex CLI links OpenSSL 3.x statically (via reqwest → native-tls → openssl-sys).
When the release binary keeps its symbol table, the offsets of `SSL_write_ex`,
`SSL_read_ex`, and `SSL_do_handshake` can be read directly with `nm`.
Falls back to `SSL_write` / `SSL_read` when the `_ex` variants are absent.

Produces a JSON entry suitable for the `codex_offsets.entries` array in
`agentsight.json` (Tier 3 offset table).

Usage:
  ./extract-codex-offsets.py <binary>

`<binary>` should be a codex executable that still has a usable symbol
table (release builds with their dynsym intact, or a `*.symbols` package
downloaded from the codex release).

The output JSON is printed to stdout. Append it to
`agentsight.json -> codex_offsets.entries`.
"""

import argparse
import hashlib
import json
import os
import re
import subprocess
import sys
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
    """Return a {symbol_name: file_offset} map for defined text symbols.

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
                args, stderr=subprocess.DEVNULL, universal_newlines=True
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


def main() -> int:
    p = argparse.ArgumentParser(description=__doc__.split("\n", 1)[0])
    p.add_argument("binary", help="codex binary (with symbol table)")
    args = p.parse_args()

    binary = args.binary
    if not os.path.isfile(binary):
        print(f"error: not a file: {binary}", file=sys.stderr)
        return 1

    syms = nm_symbols(binary)
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
