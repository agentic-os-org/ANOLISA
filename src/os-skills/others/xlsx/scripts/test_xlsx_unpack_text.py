#!/usr/bin/env python3
# SPDX-License-Identifier: MIT
"""Regression tests for preserving cell text during XLSX unpacking."""

import contextlib
import io
import tempfile
import unittest
import xml.etree.ElementTree as ET
import zipfile
from pathlib import Path

import xlsx_pack
import xlsx_unpack


class UnpackTextTests(unittest.TestCase):
    def test_text_blank_lines_are_preserved(self):
        for value in ("first\n\nlast", "\n\nfirst\n\n", " \n \n "):
            with self.subTest(value=value):
                raw = f'<sst><si><t xml:space="preserve">{value}</t></si></sst>'
                formatted = xlsx_unpack.pretty_print_xml(raw.encode())
                self.assertEqual(ET.fromstring(formatted).find("si/t").text, value)

    def test_rich_text_runs_are_preserved(self):
        raw = b'<si><r><t>first\n\nrun</t></r><r><t>second\n\nrun</t></r></si>'
        before = [node.text for node in ET.fromstring(raw).iter("t")]
        after = [node.text for node in ET.fromstring(
            xlsx_unpack.pretty_print_xml(raw)).iter("t")]
        self.assertEqual(after, before)

    def test_markup_is_still_pretty_printed(self):
        formatted = xlsx_unpack.pretty_print_xml(b'<root><child value="1"/></root>')
        self.assertIn('\n  <child value="1"/>\n', formatted)
        self.assertEqual(ET.fromstring(formatted).find("child").get("value"), "1")

    def test_unpack_pack_preserves_inline_cell_text_and_binary(self):
        namespace = "http://schemas.openxmlformats.org/spreadsheetml/2006/main"
        worksheet = (
            f'<worksheet xmlns="{namespace}"><sheetData><row r="1">'
            '<c r="A1" t="inlineStr"><is><t xml:space="preserve">'
            'first\n\nlast</t></is></c></row></sheetData></worksheet>'
        ).encode()
        with tempfile.TemporaryDirectory() as directory:
            folder = Path(directory)
            source = folder / "input.xlsx"
            with zipfile.ZipFile(source, "w") as archive:
                archive.writestr("[Content_Types].xml", '<Types/>')
                archive.writestr("xl/worksheets/sheet1.xml", worksheet)
                archive.writestr("xl/vbaProject.bin", b"\x00\x01macro fixture")
            with contextlib.redirect_stdout(io.StringIO()):
                xlsx_unpack.unpack(str(source), str(folder / "work"))
                xlsx_pack.pack(str(folder / "work"), str(folder / "output.xlsx"))
            with zipfile.ZipFile(folder / "output.xlsx") as archive:
                parsed = ET.fromstring(archive.read("xl/worksheets/sheet1.xml"))
                self.assertEqual(parsed.find(f'.//{{{namespace}}}t').text, "first\n\nlast")
                self.assertEqual(archive.read("xl/vbaProject.bin"), b"\x00\x01macro fixture")


if __name__ == "__main__":
    unittest.main()
