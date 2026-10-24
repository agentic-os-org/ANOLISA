#!/usr/bin/env python3
# SPDX-License-Identifier: MIT
"""Tests for xlsx_shift_rows.py insert/delete semantics.

Regression tests for delete mode, which used to relabel the deleted row
onto the row above it (producing duplicate <row r="N"> elements and
keeping the doomed content) instead of dropping its content and shifting
only the rows below the deleted block.
"""

import os
import re
import shutil
import subprocess
import sys
import tempfile
import unittest
import xml.etree.ElementTree as ET

NS_MAIN = "http://schemas.openxmlformats.org/spreadsheetml/2006/main"
NS_REL = "http://schemas.openxmlformats.org/officeDocument/2006/relationships"
NS_PKG_REL = ("http://schemas.openxmlformats.org/package/2006/relationships")

SCRIPTS_DIR = os.path.dirname(os.path.abspath(__file__))
SCRIPT = os.path.join(SCRIPTS_DIR, "xlsx_shift_rows.py")

WORKSHEET_XML = f"""<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<worksheet xmlns="{NS_MAIN}">
  <dimension ref="A1:A9"/>
  <sheetData>
    <row r="7"><c r="A7"><v>7</v></c></row>
    <row r="8"><c r="A8"><v>8</v></c></row>
    <row r="9"><c r="A9"><v>9</v></c><c r="B9"><f>A9*2</f></c></row>
  </sheetData>
</worksheet>
"""

MERGE_XML = f"""<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<worksheet xmlns="{NS_MAIN}">
  <dimension ref="A1:B7"/>
  <sheetData>
    <row r="5"><c r="A5"><v>5</v></c><c r="B5"><v>5</v></c></row>
    <row r="6"><c r="A6"><v>6</v></c><c r="B6"><v>6</v></c></row>
    <row r="7"><c r="A7"><v>7</v></c><c r="B7"><v>7</v></c></row>
  </sheetData>
  <mergeCells count="2">
    <mergeCell ref="A6:B6"/>
    <mergeCell ref="A7:B7"/>
  </mergeCells>
</worksheet>
"""


def _tag(local: str) -> str:
    return f"{{{NS_MAIN}}}{local}"


def _rows_of(ws_path):
    root = ET.parse(ws_path).getroot()
    sheet_data = root.find(_tag("sheetData"))
    return [int(row.get("r")) for row in sheet_data] if sheet_data is not None else []


class TestXlsxShiftRows(unittest.TestCase):
    def setUp(self):
        self.work_dir = tempfile.mkdtemp(prefix="xlsx_shift_rows_test_")
        ws_dir = os.path.join(self.work_dir, "xl", "worksheets")
        os.makedirs(ws_dir)
        self.ws_path = os.path.join(ws_dir, "sheet1.xml")
        with open(self.ws_path, "w", encoding="utf-8") as fh:
            fh.write(WORKSHEET_XML)

    def tearDown(self):
        shutil.rmtree(self.work_dir, ignore_errors=True)

    def run_script(self, *args, expect_ok=True):
        result = subprocess.run(
            [sys.executable, SCRIPT, self.work_dir, *args],
            capture_output=True,
            text=True,
        )
        if expect_ok:
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        return result

    def read_rows(self):
        root = ET.parse(self.ws_path).getroot()
        sheet_data = root.find(_tag("sheetData"))
        return [
            (int(row.get("r")),
             [c.get("r") for c in row],
             [c.find(_tag("f")).text
              if c.find(_tag("f")) is not None else None
              for c in row],
             [c.find(_tag("v")).text
              if c.find(_tag("v")) is not None else None
              for c in row])
            for row in sheet_data
        ]

    def test_delete_row_drops_content_without_duplicate(self):
        self.run_script("delete", "8", "1")
        rows = self.read_rows()
        numbers = [r[0] for r in rows]
        # No duplicate r attributes and the deleted content is gone.
        self.assertEqual(numbers, [7, 8])
        self.assertEqual([c for c, *_ in rows if c == 7], [7])
        # Old row 9 shifted up to 8 with its cell address, formula and value;
        # the deleted row's value ("8") no longer appears anywhere.
        self.assertEqual(rows[1][1], ["A8", "B8"])
        self.assertEqual(rows[1][2], [None, "A8*2"])
        self.assertEqual(rows[0][3], ["7"])
        self.assertEqual(rows[1][3], ["9", None])

    def test_delete_multiple_rows(self):
        self.run_script("delete", "7", "2")
        rows = self.read_rows()
        numbers = [r[0] for r in rows]
        self.assertEqual(numbers, [7])
        self.assertEqual(rows[0][1], ["A7", "B7"])
        self.assertEqual(rows[0][2], [None, "A7*2"])
        self.assertEqual(rows[0][3], ["9", None])

    def test_insert_mode_unchanged(self):
        self.run_script("insert", "8", "1")
        rows = self.read_rows()
        numbers = [r[0] for r in rows]
        self.assertEqual(numbers, [7, 9, 10])
        self.assertEqual(rows[2][1], ["A10", "B10"])
        self.assertEqual(rows[2][2], [None, "A10*2"])

    def test_row_order_stays_ascending(self):
        self.run_script("delete", "8", "1")
        with open(self.ws_path, encoding="utf-8") as fh:
            raw = fh.read()
        numbers = [int(m) for m in re.findall(r'<row r="(\d+)"', raw)]
        self.assertEqual(numbers, sorted(numbers))

    def test_rejects_bad_row_and_count(self):
        # at < 1 or count < 1 previously produced duplicate r="1" / no-op runs.
        self.assertEqual(self.run_script("delete", "0", "1", expect_ok=False).returncode, 1)
        self.assertEqual(self.run_script("delete", "3", "-1", expect_ok=False).returncode, 1)


class TestMergeClamp(unittest.TestCase):
    """Blocking #1: merge/dimension endpoints inside the deleted block."""

    def setUp(self):
        self.work_dir = tempfile.mkdtemp(prefix="xlsx_merge_test_")
        ws_dir = os.path.join(self.work_dir, "xl", "worksheets")
        os.makedirs(ws_dir)
        self.ws_path = os.path.join(ws_dir, "sheet1.xml")
        with open(self.ws_path, "w", encoding="utf-8") as fh:
            fh.write(MERGE_XML)

    def tearDown(self):
        shutil.rmtree(self.work_dir, ignore_errors=True)

    def run_script(self, *args):
        result = subprocess.run(
            [sys.executable, SCRIPT, self.work_dir, *args],
            capture_output=True, text=True,
        )
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)

    def test_merge_clamped_collapsed_and_counted(self):
        # delete row 6: A6:B6 collapses (removed), A7:B7 -> A6:B6, count 2 -> 1.
        self.run_script("delete", "6", "1")
        root = ET.parse(self.ws_path).getroot()
        mc_parent = root.find(_tag("mergeCells"))
        refs = [mc.get("ref") for mc in mc_parent]
        self.assertEqual(refs, ["A6:B6"], "collapsed merge dropped, survivor shifted")
        self.assertEqual(mc_parent.get("count"), "1", "mergeCells count decremented")
        # no merge may overlap another after the operation
        self.assertEqual(len(refs), len(set(refs)))

    def test_dimension_recomputed_to_true_extent(self):
        # data rows 5,6,7; delete row 7 -> surviving 5,6. The ORIGINAL start
        # endpoint is clamped (stays A1) and only the end shrinks to the true
        # extent; dragging the start down to min(surviving) would silently
        # lose rows the writer never emitted.
        self.run_script("delete", "7", "1")
        root = ET.parse(self.ws_path).getroot()
        dim = root.find(_tag("dimension")).get("ref")
        self.assertEqual(dim, "A1:B6")


class TestSheetTargeting(unittest.TestCase):
    """Blocking #2: delete must not destroy rows on every worksheet."""

    def setUp(self):
        self.work_dir = tempfile.mkdtemp(prefix="xlsx_multi_test_")
        ws_dir = os.path.join(self.work_dir, "xl", "worksheets")
        os.makedirs(ws_dir)
        os.makedirs(os.path.join(self.work_dir, "xl", "_rels"))

        def sheet_xml(prefix):
            rows = "".join(
                f'<row r="{n}"><c r="A{n}"><v>{prefix}-{n}</v></c></row>'
                for n in range(1, 6)
            )
            return (f'<?xml version="1.0" encoding="UTF-8" standalone="yes"?>\n'
                    f'<worksheet xmlns="{NS_MAIN}"><dimension ref="A1:A5"/>'
                    f'<sheetData>{rows}</sheetData></worksheet>\n')

        for i, pfx in enumerate(("S1", "S2"), start=1):
            with open(os.path.join(ws_dir, f"sheet{i}.xml"), "w", encoding="utf-8") as fh:
                fh.write(sheet_xml(pfx))

        wb = (f'<?xml version="1.0" encoding="UTF-8" standalone="yes"?>\n'
              f'<workbook xmlns="{NS_MAIN}" xmlns:r="{NS_REL}"><sheets>'
              f'<sheet name="One" sheetId="1" r:id="rId1"/>'
              f'<sheet name="Two" sheetId="2" r:id="rId2"/>'
              f'</sheets></workbook>\n')
        with open(os.path.join(self.work_dir, "xl", "workbook.xml"), "w", encoding="utf-8") as fh:
            fh.write(wb)
        rels = (f'<?xml version="1.0" encoding="UTF-8" standalone="yes"?>\n'
                f'<Relationships xmlns="{NS_PKG_REL}">'
                f'<Relationship Id="rId1" Type="x" Target="worksheets/sheet1.xml"/>'
                f'<Relationship Id="rId2" Type="x" Target="worksheets/sheet2.xml"/>'
                f'</Relationships>\n')
        with open(os.path.join(self.work_dir, "xl", "_rels", "workbook.xml.rels"), "w", encoding="utf-8") as fh:
            fh.write(rels)

        self.s1 = os.path.join(ws_dir, "sheet1.xml")
        self.s2 = os.path.join(ws_dir, "sheet2.xml")

    def tearDown(self):
        shutil.rmtree(self.work_dir, ignore_errors=True)

    def run_script(self, *args):
        return subprocess.run(
            [sys.executable, SCRIPT, self.work_dir, *args],
            capture_output=True, text=True,
        )

    def test_refuses_multi_sheet_without_target(self):
        r = self.run_script("delete", "3", "1")
        self.assertEqual(r.returncode, 1)
        self.assertIn("multiple worksheets", r.stdout + r.stderr)
        # nothing mutated
        self.assertEqual(_rows_of(self.s1), [1, 2, 3, 4, 5])
        self.assertEqual(_rows_of(self.s2), [1, 2, 3, 4, 5])

    def test_targets_only_named_sheet(self):
        r = self.run_script("delete", "3", "1", "--sheet", "Two")
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
        # rows 4,5 on sheet Two shift up; sheet One is untouched.
        self.assertEqual(_rows_of(self.s2), [1, 2, 3, 4])
        self.assertEqual(_rows_of(self.s1), [1, 2, 3, 4, 5])

    def test_targets_by_index(self):
        r = self.run_script("delete", "3", "1", "--sheet", "1")
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
        self.assertEqual(_rows_of(self.s1), [1, 2, 3, 4])
        self.assertEqual(_rows_of(self.s2), [1, 2, 3, 4, 5])


def _write_book(work_dir, sheet_xmls):
    """Write worksheets + workbook.xml + rels for [(name, xml), ...]."""
    ws_dir = os.path.join(work_dir, "xl", "worksheets")
    os.makedirs(ws_dir, exist_ok=True)
    os.makedirs(os.path.join(work_dir, "xl", "_rels"), exist_ok=True)
    entries, rels = [], []
    for i, (name, xml) in enumerate(sheet_xmls, start=1):
        fn = f"sheet{i}.xml"
        with open(os.path.join(ws_dir, fn), "w", encoding="utf-8") as fh:
            fh.write(xml)
        rid = f"rId{i}"
        entries.append(f'<sheet name="{name}" sheetId="{i}" r:id="{rid}"/>')
        rels.append(f'<Relationship Id="{rid}" Type="x" '
                    f'Target="/xl/worksheets/{fn}"/>')
    with open(os.path.join(work_dir, "xl", "workbook.xml"), "w",
              encoding="utf-8") as fh:
        fh.write('<?xml version="1.0" encoding="UTF-8" standalone="yes"?>\n'
                 f'<workbook xmlns="{NS_MAIN}" xmlns:r="{NS_REL}"><sheets>'
                 + "".join(entries) + "</sheets></workbook>\n")
    with open(os.path.join(work_dir, "xl", "_rels", "workbook.xml.rels"),
              "w", encoding="utf-8") as fh:
        fh.write('<?xml version="1.0" encoding="UTF-8" standalone="yes"?>\n'
                 f'<Relationships xmlns="{NS_PKG_REL}">'
                 + "".join(rels) + "</Relationships>\n")


def _formulas_of(ws_path):
    """{cell_ref: (f_text, t, si, ref)} for every cell with an <f>."""
    root = ET.parse(ws_path).getroot()
    out = {}
    for c in root.iter(_tag("c")):
        f = c.find(_tag("f"))
        if f is not None:
            out[c.get("r")] = (f.text, f.get("t"), f.get("si"), f.get("ref"))
    return out


class TestCollapsedRangeEndpoints(unittest.TestCase):
    """Round-2 🔴3: the "" collapse contract must be honoured at EVERY
    adjust_range_ref consumer — never write sqref=""/ref="" into an attribute.
    """

    CF_DV_XML = f"""<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<worksheet xmlns="{NS_MAIN}">
  <dimension ref="A1:B7"/>
  <sheetData>
    <row r="5"><c r="A5"><v>5</v></c></row>
    <row r="6"><c r="A6"><v>6</v></c></row>
    <row r="7"><c r="A7"><v>7</v></c></row>
  </sheetData>
  <conditionalFormatting sqref="A6:B6">
    <cfRule type="cellIs" dxfId="0" operator="greaterThan"><formula>0</formula></cfRule>
  </conditionalFormatting>
  <dataValidations count="1">
    <dataValidation type="whole" operator="between" sqref="A6:A6"/>
  </dataValidations>
  <mergeCells count="1">
    <mergeCell ref="A6:B6"/>
  </mergeCells>
</worksheet>
"""

    def setUp(self):
        self.work_dir = tempfile.mkdtemp(prefix="xlsx_collapse_test_")
        ws_dir = os.path.join(self.work_dir, "xl", "worksheets")
        os.makedirs(ws_dir)
        self.ws_path = os.path.join(ws_dir, "sheet1.xml")
        with open(self.ws_path, "w", encoding="utf-8") as fh:
            fh.write(self.CF_DV_XML)

    def tearDown(self):
        shutil.rmtree(self.work_dir, ignore_errors=True)

    def run_script(self, *args):
        result = subprocess.run(
            [sys.executable, SCRIPT, self.work_dir, *args],
            capture_output=True, text=True,
        )
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        return result

    def test_cf_dv_merge_collapse_drops_elements(self):
        self.run_script("delete", "6", "1")
        root = ET.parse(self.ws_path).getroot()
        self.assertIsNone(root.find(_tag("conditionalFormatting")),
                          "collapsed conditionalFormatting dropped")
        self.assertIsNone(root.find(_tag("dataValidations")),
                          "emptied dataValidations parent dropped")
        self.assertIsNone(root.find(_tag("mergeCells")),
                          "emptied mergeCells parent dropped")
        raw = open(self.ws_path, encoding="utf-8").read()
        self.assertNotIn('sqref=""', raw)
        self.assertNotIn('ref=""', raw)

    def test_dimension_omitted_when_no_rows_survive(self):
        self.run_script("delete", "5", "3")
        root = ET.parse(self.ws_path).getroot()
        dim = root.find(_tag("dimension"))
        self.assertNotIn("ref", dim.attrib,
                         "ref must be omitted, not written empty")
        sheet_data = root.find(_tag("sheetData"))
        self.assertEqual(len(list(sheet_data)), 0)


class TestChartCollapse(unittest.TestCase):
    """Round-2 🔴3: a collapsed chart series range must not become `Sheet1!`."""

    def setUp(self):
        self.work_dir = tempfile.mkdtemp(prefix="xlsx_chart_test_")
        ws_dir = os.path.join(self.work_dir, "xl", "worksheets")
        os.makedirs(ws_dir)
        charts_dir = os.path.join(self.work_dir, "xl", "charts")
        os.makedirs(charts_dir)
        rows = "".join(
            f'<row r="{n}"><c r="A{n}"><v>{n}</v></c></row>'
            for n in range(1, 4))
        with open(os.path.join(ws_dir, "sheet1.xml"), "w", encoding="utf-8") as fh:
            fh.write(f'<?xml version="1.0" encoding="UTF-8" standalone="yes"?>\n'
                     f'<worksheet xmlns="{NS_MAIN}"><dimension ref="A1:A3"/>'
                     f'<sheetData>{rows}</sheetData></worksheet>\n')
        self.chart = os.path.join(charts_dir, "chart1.xml")
        with open(self.chart, "w", encoding="utf-8") as fh:
            fh.write('<c:chart xmlns:c="x"><c:ser><c:val><c:numRef>'
                     '<c:f>Sheet1!$A$1:$A$3</c:f></c:numRef></c:val></c:ser>'
                     '</c:chart>\n')

    def tearDown(self):
        shutil.rmtree(self.work_dir, ignore_errors=True)

    def test_collapsed_chart_range_kept_with_warning(self):
        r = subprocess.run(
            [sys.executable, SCRIPT, self.work_dir, "delete", "1", "3"],
            capture_output=True, text=True,
        )
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
        content = open(self.chart, encoding="utf-8").read()
        self.assertIn("Sheet1!$A$1:$A$3", content,
                      "collapsed range kept intact, not written as Sheet1!")
        self.assertNotIn("<c:f>Sheet1!</c:f>", content)
        self.assertIn("WARNING", r.stderr)


class TestCrossSheetFormulas(unittest.TestCase):
    """Round-2 🔴2: formula shifting must be sheet-aware in both directions."""

    S1_XML = f"""<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<worksheet xmlns="{NS_MAIN}">
  <dimension ref="A1:B7"/>
  <sheetData>
    <row r="5"><c r="A5"><v>1</v></c><c r="B5"><f>S2!A7</f></c></row>
    <row r="6"><c r="A6"><v>2</v></c></row>
    <row r="7"><c r="A7"><v>3</v></c><c r="B7"><f>'S2'!A7</f></c></row>
  </sheetData>
</worksheet>
"""

    S2_XML = f"""<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<worksheet xmlns="{NS_MAIN}">
  <dimension ref="A1:C7"/>
  <sheetData>
    <row r="5"><c r="A5"><v>9</v></c><c r="B5"><f>S1!A7</f></c></row>
    <row r="6"><c r="A6"><v>10</v></c><c r="B6"><f>'S1'!A7</f></c></row>
    <row r="7"><c r="A7"><v>11</v></c><c r="C7"><f>A7*2</f></c></row>
  </sheetData>
</worksheet>
"""

    def setUp(self):
        self.work_dir = tempfile.mkdtemp(prefix="xlsx_xsheet_test_")
        _write_book(self.work_dir, [("S1", self.S1_XML), ("S2", self.S2_XML)])
        ws_dir = os.path.join(self.work_dir, "xl", "worksheets")
        self.s1 = os.path.join(ws_dir, "sheet1.xml")
        self.s2 = os.path.join(ws_dir, "sheet2.xml")

    def tearDown(self):
        shutil.rmtree(self.work_dir, ignore_errors=True)

    def test_delete_on_target_sheet(self):
        r = subprocess.run(
            [sys.executable, SCRIPT, self.work_dir, "delete", "6", "1",
             "--sheet", "S1"],
            capture_output=True, text=True,
        )
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
        # (a) refs on S1 qualified with S2 stay: S2's rows did not move
        f1 = _formulas_of(self.s1)
        self.assertEqual(f1["B5"][0], "S2!A7")
        self.assertEqual(f1["B6"][0], "'S2'!A7")
        # (b) refs on S2 qualified with S1 follow the move: old row 7 -> 6
        f2 = _formulas_of(self.s2)
        self.assertEqual(f2["B5"][0], "S1!A6")
        self.assertEqual(f2["B6"][0], "'S1'!A6")
        # bare refs on S2 point at S2 itself — untouched
        self.assertEqual(f2["C7"][0], "A7*2")
        self.assertEqual(_rows_of(self.s2), [5, 6, 7])


class TestSharedFormulaPrimary(unittest.TestCase):
    """Round-2 🔴4: deleting the primary's row must not orphan the group."""

    SHARED_XML = f"""<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<worksheet xmlns="{NS_MAIN}">
  <dimension ref="A1:B9"/>
  <sheetData>
    <row r="5"><c r="A5"><v>5</v></c></row>
    <row r="6"><c r="A6"><v>6</v></c><c r="B6"><f t="shared" ref="B6:B9" si="0">SUM($A6:$A6)</f></c></row>
    <row r="7"><c r="A7"><v>7</v></c><c r="B7"><f t="shared" si="0"/></c></row>
    <row r="9"><c r="A9"><v>9</v></c><c r="B9"><f t="shared" si="0"/></c></row>
  </sheetData>
</worksheet>
"""

    def setUp(self):
        self.work_dir = tempfile.mkdtemp(prefix="xlsx_shared_test_")
        ws_dir = os.path.join(self.work_dir, "xl", "worksheets")
        os.makedirs(ws_dir)
        self.ws_path = os.path.join(ws_dir, "sheet1.xml")
        with open(self.ws_path, "w", encoding="utf-8") as fh:
            fh.write(self.SHARED_XML)

    def tearDown(self):
        shutil.rmtree(self.work_dir, ignore_errors=True)

    def test_follower_promoted_to_primary(self):
        r = subprocess.run(
            [sys.executable, SCRIPT, self.work_dir, "delete", "6", "1"],
            capture_output=True, text=True,
        )
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
        self.assertEqual(_rows_of(self.ws_path), [5, 6, 8])
        f = _formulas_of(self.ws_path)
        # the first surviving follower (old row 7 -> 6) became the primary:
        # text materialised from the old primary, offset by the row distance
        self.assertEqual(f["B6"], ("SUM($A6:$A6)", "shared", "0", "B6:B8"))
        # the remaining follower keeps pointing at the group
        self.assertEqual(f["B8"][0], None)
        self.assertEqual(f["B8"][1], "shared")


class TestHyperlink(unittest.TestCase):
    """Round-2 🟡6: links on deleted rows must not re-attach to new content."""

    HL_XML = f"""<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<worksheet xmlns="{NS_MAIN}">
  <dimension ref="A1:B7"/>
  <sheetData>
    <row r="5"><c r="A5"><v>5</v></c></row>
    <row r="6"><c r="A6"><v>6</v></c></row>
    <row r="7"><c r="A7"><v>7</v></c></row>
  </sheetData>
  <hyperlinks>
    <hyperlink ref="A6"/>
    <hyperlink ref="A7"/>
  </hyperlinks>
</worksheet>
"""

    def setUp(self):
        self.work_dir = tempfile.mkdtemp(prefix="xlsx_hlink_test_")
        ws_dir = os.path.join(self.work_dir, "xl", "worksheets")
        os.makedirs(ws_dir)
        self.ws_path = os.path.join(ws_dir, "sheet1.xml")
        with open(self.ws_path, "w", encoding="utf-8") as fh:
            fh.write(self.HL_XML)

    def tearDown(self):
        shutil.rmtree(self.work_dir, ignore_errors=True)

    def test_deleted_row_link_dropped_survivor_shifted(self):
        r = subprocess.run(
            [sys.executable, SCRIPT, self.work_dir, "delete", "6", "1"],
            capture_output=True, text=True,
        )
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
        root = ET.parse(self.ws_path).getroot()
        refs = [hl.get("ref") for hl in root.iter(_tag("hyperlink"))]
        self.assertEqual(refs, ["A6"],
                         "A6 link dropped with its row; A7 followed the shift")


class TestDigitNamedSheet(unittest.TestCase):
    """Round-2 🟡7: --sheet must match the name before the index."""

    def setUp(self):
        self.work_dir = tempfile.mkdtemp(prefix="xlsx_digit_test_")

        def sheet_xml(pfx):
            rows = "".join(
                f'<row r="{n}"><c r="A{n}"><v>{pfx}-{n}</v></c></row>'
                for n in range(1, 6))
            return (f'<?xml version="1.0" encoding="UTF-8" standalone="yes"?>\n'
                    f'<worksheet xmlns="{NS_MAIN}"><dimension ref="A1:A5"/>'
                    f'<sheetData>{rows}</sheetData></worksheet>\n')

        _write_book(self.work_dir, [("2024", sheet_xml("Y")),
                                    ("S2", sheet_xml("Z"))])
        ws_dir = os.path.join(self.work_dir, "xl", "worksheets")
        self.s1 = os.path.join(ws_dir, "sheet1.xml")
        self.s2 = os.path.join(ws_dir, "sheet2.xml")

    def tearDown(self):
        shutil.rmtree(self.work_dir, ignore_errors=True)

    def test_targets_digit_named_sheet_by_name(self):
        r = subprocess.run(
            [sys.executable, SCRIPT, self.work_dir, "delete", "3", "1",
             "--sheet", "2024"],
            capture_output=True, text=True,
        )
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
        self.assertEqual(_rows_of(self.s1), [1, 2, 3, 4])
        self.assertEqual(_rows_of(self.s2), [1, 2, 3, 4, 5])

    def test_hash_forces_index(self):
        r = subprocess.run(
            [sys.executable, SCRIPT, self.work_dir, "delete", "3", "1",
             "--sheet", "#2"],
            capture_output=True, text=True,
        )
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
        self.assertEqual(_rows_of(self.s2), [1, 2, 3, 4])
        self.assertEqual(_rows_of(self.s1), [1, 2, 3, 4, 5])


class TestInsertRowMultiSheet(unittest.TestCase):
    """Round-2 🔴1: xlsx_insert_row.py must forward --sheet to the shift."""

    def setUp(self):
        self.work_dir = tempfile.mkdtemp(prefix="xlsx_insert_ms_test_")

        def sheet_xml(pfx):
            rows = "".join(
                f'<row r="{n}"><c r="A{n}"><v>{pfx}-{n}</v></c></row>'
                for n in range(1, 4))
            return (f'<?xml version="1.0" encoding="UTF-8" standalone="yes"?>\n'
                    f'<worksheet xmlns="{NS_MAIN}"><dimension ref="A1:A3"/>'
                    f'<sheetData>{rows}</sheetData></worksheet>\n')

        _write_book(self.work_dir, [("S1", sheet_xml("A")),
                                    ("S2", sheet_xml("B"))])
        with open(os.path.join(self.work_dir, "xl", "sharedStrings.xml"),
                  "w", encoding="utf-8") as fh:
            fh.write(f'<?xml version="1.0" encoding="UTF-8" standalone="yes"?>\n'
                     f'<sst xmlns="{NS_MAIN}" count="0" uniqueCount="0"/>\n')
        ws_dir = os.path.join(self.work_dir, "xl", "worksheets")
        self.s1 = os.path.join(ws_dir, "sheet1.xml")
        self.s2 = os.path.join(ws_dir, "sheet2.xml")

    def tearDown(self):
        shutil.rmtree(self.work_dir, ignore_errors=True)

    def test_insert_into_second_sheet(self):
        script = os.path.join(SCRIPTS_DIR, "xlsx_insert_row.py")
        r = subprocess.run(
            [sys.executable, script, self.work_dir, "--at", "2",
             "--sheet", "S2", "--text", "A=New", "--values", "B=7"],
            capture_output=True, text=True,
        )
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
        # S2: rows 2,3 shifted down, new row at 2
        self.assertEqual(_rows_of(self.s2), [1, 2, 3, 4])
        root = ET.parse(self.s2).getroot()
        new_row = [row for row in root.find(_tag("sheetData"))
                   if row.get("r") == "2"][0]
        cells = {c.get("r"): c for c in new_row}
        self.assertIn("A2", cells)
        self.assertEqual(cells["A2"].get("t"), "s")
        self.assertEqual(cells["B2"].find(_tag("v")).text, "7")
        # S1 untouched
        self.assertEqual(_rows_of(self.s1), [1, 2, 3])


if __name__ == "__main__":
    unittest.main()
