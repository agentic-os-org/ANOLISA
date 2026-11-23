"""验证离线 Codex 符号提取，避免错误指纹或虚拟地址被写入出货 offset 表。"""

import importlib.util
import pathlib
import struct
import tempfile
import unittest
import zlib
from unittest import mock

SCRIPT = (
    pathlib.Path(__file__).resolve().parents[1] / "scripts/extract-codex-offsets.py"
)
SPEC = importlib.util.spec_from_file_location("codex_offsets_tool", SCRIPT)
TOOL = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(TOOL)


class ExtractOffsetsTests(unittest.TestCase):
    def make_elf(self, path: pathlib.Path, flags: int = 5) -> None:
        header = bytearray(64)
        header[:6] = b"\x7fELF\x02\x01"
        struct.pack_into("<Q", header, 32, 64)
        struct.pack_into("<HH", header, 54, 56, 1)
        segment = struct.pack(
            "<IIQQQQQQ", 1, flags, 0x1000, 0x4000, 0, 0x1000, 0x1000, 0x1000
        )
        path.write_bytes(bytes(header) + segment + bytes(0x2000 - 120))

    def test_virtual_addresses_are_converted_through_target_load_segments(self):
        with tempfile.TemporaryDirectory() as directory:
            binary = pathlib.Path(directory) / "codex"
            self.make_elf(binary)
            offsets = {
                "ssl_write": 0x4100,
                "ssl_read": 0x4200,
                "ssl_do_handshake": 0x4300,
                "write_is_ex": True,
                "read_is_ex": True,
            }
            converted = TOOL.file_offsets(str(binary), offsets)
            self.assertEqual(converted["ssl_write"], 0x1100)
            self.assertEqual(converted["ssl_read"], 0x1200)
            self.assertEqual(converted["ssl_do_handshake"], 0x1300)
            self.assertTrue(converted["write_is_ex"])
            self.assertTrue(converted["read_is_ex"])
            self.assertEqual(offsets["ssl_write"], 0x4100)

    def test_non_executable_segments_are_rejected(self):
        with tempfile.TemporaryDirectory() as directory:
            binary = pathlib.Path(directory) / "codex"
            self.make_elf(binary, flags=4)
            with self.assertRaises(ValueError):
                TOOL.file_offsets(
                    str(binary),
                    {
                        "ssl_write": 0x4100,
                        "ssl_read": 0x4200,
                        "ssl_do_handshake": 0x4300,
                    },
                )

    def test_truncated_elf_is_rejected(self):
        with tempfile.TemporaryDirectory() as directory:
            binary = pathlib.Path(directory) / "codex"
            binary.write_bytes(b"\x7fELF\x02\x01")
            with self.assertRaises(ValueError):
                TOOL.file_offsets(str(binary), {})

    def test_ex_variants_keep_byte_count_pointer_flags(self):
        offsets = TOOL.pick_offsets(
            {"SSL_write_ex": 100, "SSL_read_ex": 200, "SSL_do_handshake": 300}
        )
        self.assertTrue(offsets["write_is_ex"])
        self.assertTrue(offsets["read_is_ex"])

    def test_plain_variants_keep_byte_count_return_flags(self):
        offsets = TOOL.pick_offsets(
            {"SSL_write": 100, "SSL_read": 200, "SSL_do_handshake": 300}
        )
        self.assertFalse(offsets["write_is_ex"])
        self.assertFalse(offsets["read_is_ex"])

    def test_debuglink_crc_validates_symbol_files_without_build_ids(self):
        with tempfile.TemporaryDirectory() as directory:
            binary = pathlib.Path(directory) / "codex"
            symbols = pathlib.Path(directory) / "codex.debug"
            symbols.write_bytes(b"matching release symbols")
            checksum = zlib.crc32(symbols.read_bytes()) & 0xFFFFFFFF
            self.make_elf(binary)
            data = bytearray(binary.read_bytes())
            struct.pack_into("<Q", data, 40, 0x2000)
            struct.pack_into("<HHH", data, 58, 64, 3, 1)
            names = b"\0.shstrtab\0.gnu_debuglink\0"
            link = b"codex.debug\0" + struct.pack("<I", checksum)
            sections = (
                bytes(64)
                + struct.pack("<IIQQQQIIQQ", 1, 3, 0, 0, 0x2100, len(names), 0, 0, 1, 0)
                + struct.pack("<IIQQQQIIQQ", 11, 1, 0, 0, 0x2200, len(link), 0, 0, 4, 0)
            )
            data.extend(sections)
            data.extend(bytes(0x2100 - len(data)))
            data.extend(names)
            data.extend(bytes(0x2200 - len(data)))
            data.extend(link)
            binary.write_bytes(data)
            self.assertEqual(TOOL.read_debuglink_crc(str(binary)), checksum)
            with mock.patch.object(TOOL, "read_buildid", return_value=None):
                self.assertTrue(TOOL.matching_symbol_file(str(binary), str(symbols)))
                symbols.write_bytes(b"wrong release symbols")
                self.assertFalse(TOOL.matching_symbol_file(str(binary), str(symbols)))

    def test_missing_symbol_file_reports_a_clean_error(self):
        with tempfile.TemporaryDirectory() as directory:
            binary = pathlib.Path(directory) / "codex"
            self.make_elf(binary)
            missing = pathlib.Path(directory) / "absent.debug"
            with mock.patch(
                "sys.argv", [str(SCRIPT), str(binary), "--symbols", str(missing)]
            ), mock.patch.object(TOOL, "nm_symbols") as nm:
                self.assertEqual(TOOL.main(), 1)
                nm.assert_not_called()

    def test_unreadable_symbol_file_is_not_treated_as_matching(self):
        with tempfile.TemporaryDirectory() as directory:
            binary = pathlib.Path(directory) / "codex"
            self.make_elf(binary)
            absent = pathlib.Path(directory) / "absent.debug"
            with mock.patch.object(
                TOOL, "read_buildid", return_value=None
            ), mock.patch.object(TOOL, "read_debuglink_crc", return_value=0x1234):
                self.assertFalse(TOOL.matching_symbol_file(str(binary), str(absent)))

    def test_mismatched_symbol_build_id_is_rejected_before_nm(self):
        with tempfile.TemporaryDirectory() as directory:
            binary = pathlib.Path(directory) / "codex"
            self.make_elf(binary)
            symbols = pathlib.Path(directory) / "other.debug"
            symbols.write_bytes(b"symbols from a different release")
            with mock.patch(
                "sys.argv", [str(SCRIPT), str(binary), "--symbols", str(symbols)]
            ), mock.patch.object(
                TOOL, "read_buildid", side_effect=["release-a", "release-b"]
            ), mock.patch.object(
                TOOL, "nm_symbols"
            ) as nm:
                self.assertEqual(TOOL.main(), 4)
                nm.assert_not_called()


if __name__ == "__main__":
    unittest.main()
