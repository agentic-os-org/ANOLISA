"""Reject unverified static TLS builds and offsets outside executable segments."""

import hashlib
from pathlib import Path
import struct
import sys
import unittest

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "scripts"))
from static_tls import checked_offset, resolve_static


def executable() -> bytearray:
    data = bytearray(512)
    data[:6] = b"\x7fELF\x02\x01"
    struct.pack_into("<HHIQQQIHHHHHH", data, 16, 2, 62, 1, 0, 64, 0, 0, 64, 56, 1, 0, 0, 0)
    struct.pack_into("<IIQQQQQQ", data, 64, 1, 5, 256, 4096, 0, 128, 128, 4096)
    return data


class StaticTlsTests(unittest.TestCase):
    def test_pinned_entry_uses_file_offset(self) -> None:
        data = executable()
        self.assertEqual(checked_offset(data, hashlib.sha256(data).hexdigest(), 288), 288)

    def test_changed_binary_is_rejected_before_elf_parsing(self) -> None:
        data = executable()
        digest = hashlib.sha256(data).hexdigest()
        data[-1] = 1
        with self.assertRaisesRegex(ValueError, "fingerprint"):
            checked_offset(data, digest, 288)

    def test_nonexecutable_segment_and_virtual_address_rejected(self) -> None:
        data = executable()
        with self.assertRaisesRegex(ValueError, "mapping"):
            checked_offset(data, hashlib.sha256(data).hexdigest(), 4128)
        struct.pack_into("<I", data, 68, 4)
        with self.assertRaisesRegex(ValueError, "mapping"):
            checked_offset(data, hashlib.sha256(data).hexdigest(), 288)

    def test_invalid_abi_and_truncated_segments_rejected(self) -> None:
        data = executable()
        struct.pack_into("<H", data, 18, 183)
        with self.assertRaisesRegex(ValueError, "abi"):
            checked_offset(data, hashlib.sha256(data).hexdigest(), 288)
        data = executable()[:96]
        with self.assertRaisesRegex(ValueError, "segments"):
            checked_offset(data, hashlib.sha256(data).hexdigest(), 288)

    def test_unknown_profile_or_api_never_reads_process(self) -> None:
        for function, profile in (
            ("SSL_write", "unknown"),
            ("SSL_write_ex", "claude-2.1.161-linux-x64"),
        ):
            with self.assertRaisesRegex(ValueError, "profile_or_function"):
                resolve_static(-1, function, profile)


if __name__ == "__main__":
    unittest.main()
