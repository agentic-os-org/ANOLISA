"""Admission and ELF-offset checks for the independent uprobe experiment."""

import hashlib
import json
import os
from pathlib import Path
import struct
import sys
import tempfile
import unittest

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "scripts"))
from correlation import process_ref
from uprobe_counter import load_permit, symbol_offset


def elf() -> bytearray:
    data = bytearray(512)
    data[:6] = b"\x7fELF\x02\x01"
    struct.pack_into("<HHIQQQIHHHHHH", data, 16, 3, 62, 1, 0, 64, 128, 0, 64, 56, 1, 64, 2, 0)
    struct.pack_into("<IIQQQQQQ", data, 64, 1, 5, 256, 4096, 0, 128, 128, 4096)
    struct.pack_into("<IIQQQQIIQQ", data, 128, 0, 11, 0, 0, 256, 24, 1, 0, 8, 24)
    struct.pack_into("<IIQQQQIIQQ", data, 192, 0, 3, 0, 0, 300, 11, 0, 0, 1, 0)
    struct.pack_into("<IBBHQQ", data, 256, 1, 2, 0, 1, 4128, 1)
    data[300:311] = b"\0SSL_write\0"
    return data


class UprobeTests(unittest.TestCase):
    def test_file_offset_uses_load_mapping_not_symbol_address(self) -> None:
        self.assertEqual(symbol_offset(elf(), "SSL_write"), 288)

    def test_unknown_symbol_abi_and_nonexecutable_mapping_are_rejected(self) -> None:
        with self.assertRaisesRegex(ValueError, "symbol"):
            symbol_offset(elf(), "SSL_write_ex")
        wrong = elf()
        struct.pack_into("<H", wrong, 18, 183)
        with self.assertRaisesRegex(ValueError, "abi"):
            symbol_offset(wrong, "SSL_write")
        wrong = elf()
        struct.pack_into("<I", wrong, 68, 4)
        with self.assertRaisesRegex(ValueError, "mapping"):
            symbol_offset(wrong, "SSL_write")

    def test_truncated_elf_rejected(self) -> None:
        for data in (b"", elf()[:32], elf()[:128]):
            with self.assertRaises((ValueError, struct.error)):
                symbol_offset(data, "SSL_write")

    def test_permit_rechecks_live_identity_and_executable(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "permit.json"
            process = process_ref(os.getpid())
            permit = {
                "schema_version": "aw.ptrace-permit-poc/v0",
                "authorized_experiment": True,
                "process": process,
                "exe_sha256": hashlib.sha256(Path("/proc/self/exe").read_bytes()).hexdigest(),
            }
            path.write_text(json.dumps(permit))
            path.chmod(0o600)
            self.assertEqual(load_permit(path, os.getuid()), process)
            permit["exe_sha256"] = "0" * 64
            path.write_text(json.dumps(permit))
            with self.assertRaisesRegex(ValueError, "fingerprint"):
                load_permit(path, os.getuid())
            permit["process"]["start_ticks"] += "0"
            path.write_text(json.dumps(permit))
            with self.assertRaisesRegex(ValueError, "stale"):
                load_permit(path, os.getuid())
            path.chmod(0o644)
            with self.assertRaisesRegex(ValueError, "mode"):
                load_permit(path, os.getuid())


if __name__ == "__main__":
    unittest.main()
