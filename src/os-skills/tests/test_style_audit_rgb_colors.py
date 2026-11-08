# SPDX-License-Identifier: MIT
"""Color-role regressions using real SpreadsheetML and audit CLI inputs."""

import importlib.util
import json
import subprocess
import sys
import tempfile
import unittest
import zipfile
from pathlib import Path

SCRIPT = Path(__file__).resolve().parents[1] / "others/xlsx/scripts/style_audit.py"
SPEC = importlib.util.spec_from_file_location("style_audit_rgb", SCRIPT)
AUDIT = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(AUDIT)
NS = "http://schemas.openxmlformats.org/spreadsheetml/2006/main"
REL = "http://schemas.openxmlformats.org/officeDocument/2006/relationships"


def fixture(color: str, formula: bool = True) -> dict[str, str]:
    """Create an audit-ready workbook with an explicitly styled cell."""
    cell = "<f>1+1</f><v>2</v>" if formula else "<v>10</v>"
    return {
        "xl/styles.xml": (
            f'<styleSheet xmlns="{NS}"><fonts count="1"><font>{color}</font>'
            '</fonts><fills count="2"><fill><patternFill patternType="none"/>'
            '</fill><fill><patternFill patternType="gray125"/></fill></fills>'
            '<cellXfs count="1"><xf numFmtId="1" fontId="0" fillId="0" '
            'borderId="0"/></cellXfs></styleSheet>'
        ),
        "xl/worksheets/sheet1.xml": (
            f'<worksheet xmlns="{NS}"><sheetData><row r="1">'
            f'<c r="A1" s="0">{cell}</c></row></sheetData></worksheet>'
        ),
        "xl/workbook.xml": (
            f'<workbook xmlns="{NS}" xmlns:r="{REL}"><sheets>'
            '<sheet name="Sheet1" sheetId="1" r:id="rId1"/></sheets></workbook>'
        ),
        "xl/_rels/workbook.xml.rels": (
            '<Relationships xmlns="http://schemas.openxmlformats.org/package/'
            '2006/relationships"><Relationship Id="rId1" '
            f'Type="{REL}/worksheet" Target="worksheets/sheet1.xml"/>'
            "</Relationships>"
        ),
    }


def audit_color(color: str, formula: bool = True) -> dict:
    files = fixture(color, formula)
    return AUDIT._audit(
        files["xl/styles.xml"].encode(),
        [("Sheet1", files["xl/worksheets/sheet1.xml"].encode())],
    )


class FontColorAuditTests(unittest.TestCase):
    def test_blue_formula_is_detected_for_each_alpha(self) -> None:
        for color in ("000000FF", "FF0000FF", "7f0000ff"):
            with self.subTest(color=color):
                result = audit_color(f'<color rgb="{color}"/>')
                self.assertEqual(
                    [item["type"] for item in result["violations"]],
                    ["formula_cell_blue_font"],
                )

    def test_black_input_is_warned_for_each_alpha(self) -> None:
        for color in ("00000000", "FF000000", "80000000"):
            with self.subTest(color=color):
                result = audit_color(f'<color rgb="{color}"/>', formula=False)
                self.assertEqual(
                    [item["type"] for item in result["warnings"]],
                    ["numeric_input_may_lack_blue"],
                )

    def test_green_formula_stays_valid(self) -> None:
        for color in ("00008000", "FF008000", "80008000"):
            with self.subTest(color=color):
                result = audit_color(f'<color rgb="{color}"/>')
                self.assertEqual(result["violations"], [])
                self.assertEqual(result["warnings"], [])

    def test_unrelated_colors_do_not_become_role_colors(self) -> None:
        for color in ("FFFF0000", "FF0000FE", "FF000001"):
            with self.subTest(color=color):
                result = audit_color(f'<color rgb="{color}"/>', formula=False)
                self.assertEqual(result["warnings"], [])
                self.assertEqual(result["violations"], [])

    def test_theme_black_behavior_is_retained(self) -> None:
        result = audit_color('<color theme="1"/>', formula=False)
        self.assertEqual(result["warnings"][0]["type"], "numeric_input_may_lack_blue")

    def test_malformed_rgb_is_not_reclassified(self) -> None:
        for color in ("0000FF", "GG0000FF", "FF0000FF00"):
            with self.subTest(color=color):
                result = audit_color(f'<color rgb="{color}"/>')
                self.assertEqual(result["violations"], [])

    def run_cli(self, packed: bool, color: str) -> subprocess.CompletedProcess[str]:
        with tempfile.TemporaryDirectory(prefix="style-audit-rgb-") as directory:
            root = Path(directory)
            files = fixture(f'<color rgb="{color}"/>')
            if packed:
                target = root / "colors.xlsx"
                with zipfile.ZipFile(target, "w") as archive:
                    for name, content in files.items():
                        archive.writestr(name, content)
                original = target.read_bytes()
            else:
                target = root / "work"
                for name, content in files.items():
                    path = target / name
                    path.parent.mkdir(parents=True, exist_ok=True)
                    path.write_text(content, encoding="utf-8")
                original = {name: (target / name).read_bytes() for name in files}
            result = subprocess.run(
                [sys.executable, str(SCRIPT), str(target), "--json"],
                capture_output=True,
                text=True,
                encoding="utf-8",
                check=False,
                timeout=20,
            )
            current = (
                target.read_bytes()
                if packed
                else {name: (target / name).read_bytes() for name in files}
            )
            self.assertEqual(
                current, original, "Auditing must keep source bytes intact"
            )
            self.assertEqual(result.stderr, "")
            return result

    def test_packed_cli_rejects_opaque_blue_formula(self) -> None:
        result = self.run_cli(True, "FF0000FF")
        self.assertEqual(result.returncode, 1, result.stdout)
        report = json.loads(result.stdout)
        self.assertEqual(report["violations"][0]["cell"], "A1")
        self.assertEqual(report["violations"][0]["type"], "formula_cell_blue_font")

    def test_unpacked_cli_rejects_opaque_blue_formula(self) -> None:
        result = self.run_cli(False, "FF0000FF")
        self.assertEqual(result.returncode, 1, result.stdout)
        self.assertEqual(len(json.loads(result.stdout)["violations"]), 1)

    def test_cli_valid_formula_still_passes(self) -> None:
        result = self.run_cli(True, "FF008000")
        self.assertEqual(result.returncode, 0, result.stdout)
        self.assertEqual(json.loads(result.stdout)["violations"], [])


if __name__ == "__main__":
    unittest.main()
