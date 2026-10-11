"""Characterize column conversions and their use by the complete editor caller set."""

import importlib.util
import os
import shutil
import subprocess
import sys
import tempfile
import unittest
import xml.etree.ElementTree as ET
from pathlib import Path
from types import ModuleType

ROOT = Path(__file__).resolve().parents[2]
SCRIPTS = Path(
    os.environ.get("XLSX_COORDINATE_SCRIPTS", ROOT / "src/os-skills/others/xlsx/scripts")
)
NAMESPACE = "http://schemas.openxmlformats.org/spreadsheetml/2006/main"
TAG = f"{{{NAMESPACE}}}"
CALLERS = ("xlsx_shift_rows", "xlsx_insert_row", "xlsx_add_column")
BOUNDARIES = {1: "A", 26: "Z", 27: "AA", 52: "AZ", 53: "BA", 702: "ZZ", 703: "AAA", 16384: "XFD"}


def load_module(name: str) -> ModuleType:
    specification = importlib.util.spec_from_file_location(
        f"coordinate_test_{name}", SCRIPTS / f"{name}.py"
    )
    assert specification is not None and specification.loader is not None
    module = importlib.util.module_from_spec(specification)
    specification.loader.exec_module(module)
    return module


def fixture(directory: Path) -> Path:
    """Build an unpacked workbook with columns crossing multiple base-26 boundaries."""
    worksheet = directory / "xl/worksheets/sheet1.xml"
    worksheet.parent.mkdir(parents=True)
    (directory / "xl/_rels").mkdir()
    (directory / "xl/workbook.xml").write_text(
        f'<workbook xmlns="{NAMESPACE}" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships">'
        '<sheets><sheet name="Budget" sheetId="1" r:id="rId1"/></sheets></workbook>',
        encoding="utf-8",
    )
    (directory / "xl/_rels/workbook.xml.rels").write_text(
        '<Relationships><Relationship Id="rId1" Target="/xl/worksheets/sheet1.xml"/></Relationships>',
        encoding="utf-8",
    )
    worksheet.write_text(
        f'<worksheet xmlns="{NAMESPACE}"><dimension ref="A1:ZZ4"/>'
        '<cols><col min="1" max="702" width="12"/></cols><sheetData>'
        '<row r="1"><c r="A1"><v>1</v></c><c r="ZZ1"><v>1</v></c></row>'
        '<row r="2"><c r="A2"><v>2</v></c><c r="ZZ2"><v>2</v></c></row>'
        '<row r="3"><c r="A3"><v>3</v></c></row>'
        '<row r="4"><c r="A4"><v>4</v></c></row>'
        "</sheetData></worksheet>",
        encoding="utf-8",
    )
    return worksheet


class CoordinateTests(unittest.TestCase):
    def test_existing_module_aliases_keep_boundaries_and_case(self) -> None:
        for name in CALLERS:
            module = load_module(name)
            for number, letters in BOUNDARIES.items():
                with self.subTest(caller=name, column=letters):
                    self.assertEqual(module.col_number(letters), number)
                    self.assertEqual(module.col_number(letters.lower()), number)
                    if hasattr(module, "col_letter"):
                        self.assertEqual(module.col_letter(number), letters)

    def test_existing_aliases_keep_empty_and_unbounded_behavior(self) -> None:
        for name in CALLERS:
            module = load_module(name)
            self.assertEqual(module.col_number(""), 0)
            self.assertEqual(module.col_number("AAAA"), 18279)
            if hasattr(module, "col_letter"):
                self.assertEqual(module.col_letter(0), "")
                self.assertEqual(module.col_letter(-1), "")
                self.assertEqual(module.col_letter(18279), "AAAA")

    def test_existing_aliases_round_trip_every_supported_excel_column(self) -> None:
        shift = load_module("xlsx_shift_rows")
        add = load_module("xlsx_add_column")
        insert = load_module("xlsx_insert_row")
        for number in range(1, 16385):
            letters = shift.col_letter(number)
            self.assertEqual(add.col_letter(number), letters)
            for module in (shift, add, insert):
                self.assertEqual(module.col_number(letters), number)

    def test_shared_module_exposes_compatible_conversions(self) -> None:
        module = load_module("xlsx_coordinates")
        for number, letters in BOUNDARIES.items():
            self.assertEqual(module.col_letter(number), letters)
            self.assertEqual(module.col_number(letters.lower()), number)
        self.assertEqual(module.col_number(""), 0)
        self.assertEqual(module.col_letter(-1), "")

    def run_script(self, scripts: Path, name: str, arguments: list[str], cwd: Path) -> str:
        result = subprocess.run(
            [sys.executable, str(scripts / f"{name}.py"), *arguments],
            capture_output=True,
            text=True,
            cwd=cwd,
            timeout=20,
        )
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertNotIn("Traceback", result.stderr)
        return result.stdout

    def test_actual_insert_cli_orders_cross_boundary_columns(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            worksheet = fixture(root / "workbook")
            self.run_script(
                SCRIPTS,
                "xlsx_insert_row",
                [str(root / "workbook"), "--at", "2", "--values", "ZZ=7", "B=8", "AA=9"],
                root,
            )
            tree = ET.parse(worksheet)
            row = tree.find(f"{TAG}sheetData/{TAG}row[@r='2']")
            self.assertEqual([cell.get("r") for cell in row], ["B2", "AA2", "ZZ2"])
            self.assertEqual([cell.findtext(f"{TAG}v") for cell in row], ["8", "9", "7"])
            self.assertEqual(tree.find(f"{TAG}dimension").get("ref"), "A1:ZZ5")

    def test_actual_add_cli_expands_columns_and_dimension(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            worksheet = fixture(root / "workbook")
            self.run_script(
                SCRIPTS,
                "xlsx_add_column",
                [
                    str(root / "workbook"),
                    "--col",
                    "aaa",
                    "--formula",
                    "=A{row}*2",
                    "--formula-rows",
                    "2:3",
                ],
                root,
            )
            tree = ET.parse(worksheet)
            self.assertEqual(tree.find(f"{TAG}dimension").get("ref"), "A1:AAA4")
            for number in (2, 3):
                cell = tree.find(f"{TAG}sheetData/{TAG}row[@r='{number}']/{TAG}c[@r='AAA{number}']")
                self.assertEqual(cell.findtext(f"{TAG}f"), f"A{number}*2")
            columns = tree.findall(f"{TAG}cols/{TAG}col")
            self.assertTrue(any(column.get("min") == "703" for column in columns))

    def test_copied_assets_run_outside_repository(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            installed = root / "installed assets"
            shutil.copytree(SCRIPTS, installed, ignore=shutil.ignore_patterns("__pycache__"))
            unrelated = root / "unrelated cwd"
            unrelated.mkdir()
            worksheet = fixture(root / "workbook")
            self.run_script(
                installed,
                "xlsx_shift_rows",
                [str(root / "workbook"), "insert", "2", "1"],
                unrelated,
            )
            self.run_script(
                installed,
                "xlsx_add_column",
                [
                    str(root / "workbook"),
                    "--col",
                    "AAA",
                    "--formula",
                    "=A{row}*2",
                    "--formula-rows",
                    "3:3",
                ],
                unrelated,
            )
            tree = ET.parse(worksheet)
            cell = tree.find(f"{TAG}sheetData/{TAG}row[@r='3']/{TAG}c[@r='AAA3']")
            self.assertEqual(cell.findtext(f"{TAG}f"), "A3*2")

    def test_direct_help_for_both_editors(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            for name in ("xlsx_insert_row", "xlsx_add_column"):
                output = self.run_script(SCRIPTS, name, ["--help"], Path(temporary))
                self.assertIn("usage:", output)


if __name__ == "__main__":
    unittest.main()
