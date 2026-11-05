#!/usr/bin/env python3
# SPDX-License-Identifier: MIT
"""Regression tests for preserving cell text during XLSX unpacking."""

import contextlib
import io
import subprocess
import sys
import tempfile
import unittest
import xml.etree.ElementTree as ET
import zipfile
from pathlib import Path

import xlsx_add_column
import xlsx_insert_row
import xlsx_pack
import xlsx_shift_rows
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

    def test_formatting_is_stable_across_repeated_round_trips(self) -> None:
        raw = b'<sst><si><t xml:space="preserve">first\n\nlast</t></si></sst>'
        formatted = xlsx_unpack.pretty_print_xml(raw)
        for _ in range(4):
            formatted_again = xlsx_unpack.pretty_print_xml(formatted.encode())
            self.assertEqual(formatted_again, formatted)
            self.assertEqual(
                ET.fromstring(formatted_again).find("si/t").text,
                "first\n\nlast",
            )

    def test_whitespace_only_leaf_text_survives_repeated_formatting(self) -> None:
        for value in (" ", "\n\n", " \n \n "):
            with self.subTest(value=value):
                raw = f'<sst><si><t xml:space="preserve">{value}</t></si></sst>'
                formatted = xlsx_unpack.pretty_print_xml(raw.encode())
                self.assertEqual(
                    ET.fromstring(formatted).find("si/t").text, value
                )
                self.assertEqual(
                    xlsx_unpack.pretty_print_xml(formatted.encode()), formatted
                )

    def test_helper_writers_preserve_text_and_stable_formatting(self) -> None:
        for helper in (xlsx_add_column, xlsx_insert_row, xlsx_shift_rows):
            with self.subTest(helper=helper.__name__):
                with tempfile.TemporaryDirectory() as directory:
                    path = Path(directory) / "strings.xml"
                    path.write_text(
                        '<sst><si><t xml:space="preserve">'
                        'first\n\nlast</t></si></sst>', encoding="utf-8"
                    )
                    helper._write_tree(ET.parse(path), str(path))
                    first = path.read_bytes()
                    self.assertEqual(
                        ET.parse(path).find("si/t").text, "first\n\nlast"
                    )
                    for _ in range(4):
                        helper._write_tree(ET.parse(path), str(path))
                        self.assertEqual(path.read_bytes(), first)

    def test_documented_edit_commands_preserve_shared_and_inline_text(self) -> None:
        scripts = Path(__file__).resolve().parent
        template = scripts.parent / "templates" / "minimal_xlsx"
        namespace = xlsx_add_column.NS_SS
        worksheet = (
            f'<worksheet xmlns="{namespace}"><dimension ref="A1:B2"/>'
            '<sheetData><row r="1"><c r="A1" t="s"><v>0</v></c>'
            '<c r="B1" t="inlineStr"><is><t xml:space="preserve">'
            'inline\n\ntext</t></is></c></row></sheetData></worksheet>'
        )
        shared = (
            f'<sst xmlns="{namespace}" count="1" uniqueCount="1">'
            '<si><t xml:space="preserve">first\n\nlast</t></si></sst>'
        )
        commands = (
            ("xlsx_add_column.py", "--col", "C", "--header", "New header"),
            ("xlsx_insert_row.py", "--at", "1", "--text", "A=New row"),
        )
        for command in commands:
            with self.subTest(command=command[0]):
                with tempfile.TemporaryDirectory() as directory:
                    folder = Path(directory)
                    source = folder / "input.xlsx"
                    with zipfile.ZipFile(source, "w") as archive:
                        for path in template.rglob("*"):
                            if path.is_file():
                                name = path.relative_to(template).as_posix()
                                if name not in (
                                    "xl/sharedStrings.xml",
                                    "xl/worksheets/sheet1.xml",
                                    "xl/_rels/workbook.xml.rels",
                                ):
                                    archive.write(path, name)
                        archive.writestr("xl/sharedStrings.xml", shared)
                        archive.writestr("xl/worksheets/sheet1.xml", worksheet)
                        archive.writestr(
                            "xl/_rels/workbook.xml.rels",
                            '<Relationships xmlns="http://schemas.'
                            'openxmlformats.org/package/2006/relationships">'
                            '<Relationship Id="rId1" '
                            'Type="http://schemas.openxmlformats.org/'
                            'officeDocument/2006/relationships/worksheet" '
                            'Target="/xl/worksheets/sheet1.xml"/>'
                            '</Relationships>',
                        )
                        archive.writestr("xl/vbaProject.bin", b"macro fixture")
                    work = folder / "work"
                    output = folder / "output.xlsx"
                    with contextlib.redirect_stdout(io.StringIO()):
                        xlsx_unpack.unpack(str(source), str(work))
                    result = subprocess.run(
                        [sys.executable, "-X", "utf8",
                         str(scripts / command[0]),
                         str(work), *command[1:]],
                        capture_output=True, encoding="utf-8", check=False,
                    )
                    self.assertEqual(
                        result.returncode, 0, result.stdout + result.stderr
                    )
                    with contextlib.redirect_stdout(io.StringIO()):
                        xlsx_pack.pack(str(work), str(output))
                    with zipfile.ZipFile(output) as archive:
                        strings = ET.fromstring(
                            archive.read("xl/sharedStrings.xml")
                        )
                        sheet = ET.fromstring(
                            archive.read("xl/worksheets/sheet1.xml")
                        )
                        self.assertEqual(
                            strings.find(
                                f"{{{namespace}}}si/{{{namespace}}}t"
                            ).text,
                            "first\n\nlast",
                        )
                        self.assertEqual(
                            sheet.find(f".//{{{namespace}}}t").text,
                            "inline\n\ntext",
                        )
                        self.assertEqual(
                            archive.read("xl/vbaProject.bin"), b"macro fixture"
                        )

    def test_invalid_xml_keeps_the_original_fallback(self) -> None:
        raw = b"<not valid xml"
        self.assertEqual(xlsx_unpack.pretty_print_xml(raw), raw.decode())

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
