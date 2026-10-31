"""Exercise the complete unpack CLI on real temporary OOXML ZIP packages.

Run on the real host; no path/platform replacement. Windows is the regression
host, while POSIX provides the existing behavior control for the same assertions.

Run from the repository root:
    python tests/os-skills/test_xlsx_unpack_inventory.py -v

On Windows, unchanged main has two failures and one passing control; the fix
passes all three tests. This manual regression is outside the current CI test
discovery. POSIX and Python 3.8 runtime validation have not been performed.
"""

import os
import subprocess
import sys
import tempfile
import unittest
import xml.etree.ElementTree as ET
import zipfile
from pathlib import Path

SOURCE_ROOT = Path(
    os.environ.get("ANOLISA_SOURCE_ROOT", Path(__file__).resolve().parents[2])
)
SCRIPT = SOURCE_ROOT / "src/os-skills/others/xlsx/scripts/xlsx_unpack.py"
NS = "http://schemas.openxmlformats.org/spreadsheetml/2006/main"
REL = "http://schemas.openxmlformats.org/officeDocument/2006/relationships"
PACKAGE_REL = "http://schemas.openxmlformats.org/package/2006/relationships"
BINARY = b"\x00\xffopaque macro fixture\r\n"


class UnpackInventoryTest(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)

    def run_fixture(self, parts, extension="xlsx"):
        source = self.root / ("input." + extension)
        output = self.root / "unpacked workbook"
        types = ET.Element(
            "Types",
            xmlns="http://schemas.openxmlformats.org/package/2006/content-types",
        )
        ET.SubElement(
            types,
            "Default",
            Extension="rels",
            ContentType="application/vnd.openxmlformats-package.relationships+xml",
        )
        ET.SubElement(types, "Default", Extension="xml", ContentType="application/xml")
        workbook_type = (
            "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"
        )
        if extension == "xlsm":
            workbook_type = "application/vnd.ms-excel.sheet.macroEnabled.main+xml"
        ET.SubElement(
            types, "Override", PartName="/xl/workbook.xml", ContentType=workbook_type
        )
        workbook = ET.Element("workbook", xmlns=NS)
        sheets = ET.SubElement(workbook, "sheets")
        relationships = ET.Element("Relationships", xmlns=PACKAGE_REL)
        root_relationships = ET.Element("Relationships", xmlns=PACKAGE_REL)
        ET.SubElement(
            root_relationships,
            "Relationship",
            Id="rId1",
            Type=REL + "/officeDocument",
            Target="xl/workbook.xml",
        )
        members = {}
        for index, part in enumerate(parts, 1):
            rid = "rId" + str(index)
            ET.SubElement(
                sheets,
                "sheet",
                name="Data " + str(index),
                sheetId=str(index),
                attrib={"{" + REL + "}id": rid},
            )
            ET.SubElement(
                relationships,
                "Relationship",
                Id=rid,
                Type=REL + "/worksheet",
                Target=part[len("xl/") :],
            )
            ET.SubElement(
                types,
                "Override",
                PartName="/" + part,
                ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml",
            )
            members[part] = (
                f'<worksheet xmlns="{NS}"><sheetData><row r="1"><c r="A1"><v>{index}</v></c></row></sheetData></worksheet>'.encode()
            )
        members.update(
            {
                "[Content_Types].xml": ET.tostring(types),
                "_rels/.rels": ET.tostring(root_relationships),
                "xl/workbook.xml": ET.tostring(workbook),
                "xl/_rels/workbook.xml.rels": ET.tostring(relationships),
            }
        )
        if extension == "xlsm":
            ET.SubElement(
                relationships,
                "Relationship",
                Id="macros",
                Type="http://schemas.microsoft.com/office/2006/relationships/vbaProject",
                Target="vbaProject.bin",
            )
            ET.SubElement(
                types,
                "Override",
                PartName="/xl/vbaProject.bin",
                ContentType="application/vnd.ms-office.vbaProject",
            )
            members["xl/_rels/workbook.xml.rels"] = ET.tostring(relationships)
            members["[Content_Types].xml"] = ET.tostring(types)
            members["xl/vbaProject.bin"] = BINARY
        with zipfile.ZipFile(source, "w", compression=zipfile.ZIP_DEFLATED) as archive:
            for name, content in members.items():
                archive.writestr(name, content)
        env = dict(os.environ, PYTHONIOENCODING="utf-8")
        result = subprocess.run(
            [sys.executable, str(SCRIPT), str(source), str(output)],
            cwd=self.root,
            env=env,
            capture_output=True,
            check=False,
            text=True,
            encoding="utf-8",
            timeout=30,
        )
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        return result.stdout, output

    def assert_worksheet_inventory(self, stdout, parts):
        self.assertIn("[Worksheets]", stdout)
        inventory = stdout.split("[Worksheets]", 1)[1].split(
            "[High-risk content detected:]", 1
        )[0]
        for part in parts:
            self.assertIn(part + "  (", inventory)
            self.assertNotIn(part + "  (not found)", inventory)

    def test_standard_worksheet_is_listed_after_real_extraction(self):
        parts = ["xl/worksheets/sheet1.xml"]
        stdout, output = self.run_fixture(parts)
        self.assertTrue((output / parts[0]).is_file())
        self.assert_worksheet_inventory(stdout, parts)

    def test_multiple_worksheet_parts_are_listed(self):
        parts = ["xl/worksheets/sheet1.xml", "xl/worksheets/sheet2.xml"]
        stdout, output = self.run_fixture(parts)
        for part in parts:
            self.assertEqual(
                len(ET.parse(output / part).findall(".//{" + NS + "}c")), 1
            )
        self.assert_worksheet_inventory(stdout, parts)

    def test_package_inventory_and_macro_bytes_control(self):
        stdout, output = self.run_fixture(["xl/worksheets/sheet1.xml"], "xlsm")
        self.assertIn("[Package root]", stdout)
        self.assertIn("[Workbook]", stdout)
        self.assertIn("xl/vbaProject.bin", stdout)
        self.assertEqual((output / "xl/vbaProject.bin").read_bytes(), BINARY)
        self.assertEqual(
            ET.parse(output / "xl/worksheets/sheet1.xml").find(".//{" + NS + "}v").text,
            "1",
        )


if __name__ == "__main__":
    unittest.main()
