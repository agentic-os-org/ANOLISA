#!/usr/bin/env python3
"""Regression tests for xlsx_pack.py self-inclusion.

Packing into an output path that lives INSIDE the source directory
(the natural ``xlsx_pack.py work/ work/out.xlsx`` flow after editing an
unpacked tree) used to embed the partially-written zip into itself as
an undeclared package part — Excel then flags the workbook for repair.
The packer must skip its own output file.
"""

import importlib.util
import tempfile
import unittest
import zipfile
from pathlib import Path

SCRIPT = (
    Path(__file__).resolve().parents[2]
    / "src"
    / "os-skills"
    / "others"
    / "xlsx"
    / "scripts"
    / "xlsx_pack.py"
)


def load_module():
    spec = importlib.util.spec_from_file_location("xlsx_pack_under_test", SCRIPT)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


class SelfInclusionTest(unittest.TestCase):
    def setUp(self):
        self.module = load_module()
        self.tmp = tempfile.TemporaryDirectory()
        self.work = Path(self.tmp.name) / "work"
        (self.work).mkdir()
        (self.work / "[Content_Types].xml").write_text(
            '<?xml version="1.0"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"/>',
            encoding="utf-8",
        )
        (self.work / "xl").mkdir()
        (self.work / "xl" / "workbook.xml").write_text(
            '<?xml version="1.0"?><workbook/>', encoding="utf-8"
        )

    def tearDown(self):
        self.tmp.cleanup()

    def names_after_pack(self, out: Path):
        self.module.pack(str(self.work), str(out))
        self.assertTrue(out.is_file(), "output file must be written")
        with zipfile.ZipFile(out) as z:
            return z.namelist()

    def test_output_inside_source_dir_is_not_packed(self):
        names = self.names_after_pack(self.work / "out.xlsx")
        self.assertNotIn(
            "out.xlsx",
            names,
            "the packer must not embed its own output file in the archive",
        )
        self.assertIn("[Content_Types].xml", names)
        self.assertIn("xl/workbook.xml", names)

    def test_regular_pack_unaffected(self):
        names = self.names_after_pack(Path(self.tmp.name) / "outside.xlsx")
        self.assertIn("[Content_Types].xml", names)
        self.assertIn("xl/workbook.xml", names)
        self.assertEqual(len(names), 2)


if __name__ == "__main__":
    unittest.main()
