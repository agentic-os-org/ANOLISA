"""Keep markup-compatibility namespace bindings through real XLSX edits."""

import shutil
import subprocess
import sys
import tempfile
import unittest
import xml.dom.minidom as DOM
import xml.etree.ElementTree as ET
from pathlib import Path

XLSX = Path(__file__).resolve().parents[1] / "others/xlsx"
SCRIPTS = XLSX / "scripts"
SS = "http://schemas.openxmlformats.org/spreadsheetml/2006/main"
MC = "http://schemas.openxmlformats.org/markup-compatibility/2006"
AC = "http://schemas.microsoft.com/office/spreadsheetml/2009/9/ac"


def compatibility_bindings(path):
    """Resolve MC prefix/QName values against the document's actual scope."""
    bindings = []
    document = DOM.parse(str(path))
    for element in document.getElementsByTagName("*"):
        for attribute in element.attributes.values():
            if attribute.namespaceURI == MC or (
                element.namespaceURI == MC
                and element.localName == "Choice"
                and attribute.name == "Requires"
            ):
                values = []
                for token in attribute.value.split():
                    prefix, separator, local = token.partition(":")
                    node = element
                    while node.nodeType == node.ELEMENT_NODE:
                        name = f"xmlns:{prefix}"
                        if node.hasAttribute(name):
                            values.append((node.getAttribute(name), local if separator else ""))
                            break
                        node = node.parentNode
                    else:
                        raise AssertionError(
                            f"{path.name}: {attribute.name} references undeclared prefix {prefix!r}"
                        )
                bindings.append((element.localName, attribute.localName, tuple(values)))
    return bindings


class XlsxNamespaceTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.directory = Path(self.temporary.name)
        self.work = self.directory / "work"
        shutil.copytree(XLSX / "templates/minimal_xlsx", self.work)
        # Absolute OPC targets avoid the separate, already reported relative-target bug.
        relationships = self.work / "xl/_rels/workbook.xml.rels"
        relationships.write_text(
            relationships.read_text().replace('Target="worksheets/', 'Target="/xl/worksheets/'),
            encoding="utf-8",
        )
        self.sheet = self.work / "xl/worksheets/sheet1.xml"
        self.sheet.write_text(
            f'''<worksheet xmlns="{SS}" xmlns:mc="{MC}" xmlns:x14ac="{AC}"
              xmlns:unused="urn:unused" xmlns:ns0="urn:existing-ns0"
              xmlns:xlsx0="urn:existing-xlsx0"
              mc:Ignorable="x14ac unused ns0 xlsx0" mc:PreserveAttributes="x14ac:dyDescent">
              <dimension ref="A1:A2"/>
              <sheetFormatPr defaultRowHeight="15" x14ac:dyDescent="0.25"/>
              <sheetData>
                <row r="1"><c r="A1"><v>5</v></c></row>
                <row r="2"><c r="A2"><v>10</v></c></row>
              </sheetData>
              <extLst><ext uri="urn:test">
                <ns0:outer xmlns:p="urn:outer" mc:Ignorable="p" p:value="outer">
                  <p:inner xmlns:p="urn:inner" mc:Ignorable="p" p:value="inner"/>
                </ns0:outer>
                <group xmlns="urn:default" xmlns:d="urn:default" mc:Ignorable="d">
                  <child/><plain xmlns=""/>
                </group>
                <mc:AlternateContent><mc:Choice Requires="unused"><unused:item/></mc:Choice>
                  <mc:Fallback><ns0:item/></mc:Fallback></mc:AlternateContent>
              </ext></extLst>
            </worksheet>''',
            encoding="utf-8",
        )

    def run_script(self, name, *arguments):
        result = subprocess.run(
            [sys.executable, str(SCRIPTS / name), str(self.work), *arguments],
            cwd=self.directory,
            capture_output=True,
            text=True,
            timeout=15,
        )
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)

    def assert_extensions(self):
        root = ET.parse(self.sheet).getroot()
        self.assertEqual(root.find(f"{{{SS}}}sheetFormatPr").get(f"{{{AC}}}dyDescent"), "0.25")
        self.assertEqual(root.find(".//{urn:existing-ns0}outer").get("{urn:outer}value"), "outer")
        self.assertEqual(root.find(".//{urn:inner}inner").get("{urn:inner}value"), "inner")
        self.assertIsNotNone(root.find(".//{urn:default}child"))
        self.assertIsNotNone(root.find(".//plain"))

    def test_shift_rows_preserves_compatibility_and_extensions(self):
        before = compatibility_bindings(self.sheet)
        self.run_script("xlsx_shift_rows.py", "insert", "2", "1")
        self.assertEqual(compatibility_bindings(self.sheet), before)
        root = ET.parse(self.sheet).getroot()
        self.assertEqual(root.find(f".//{{{SS}}}c[@r='A3']/{{{SS}}}v").text, "10")
        self.assert_extensions()

    def test_insert_row_preserves_compatibility_and_extensions(self):
        before = compatibility_bindings(self.sheet)
        self.run_script("xlsx_insert_row.py", "--at", "2", "--values", "A=7")
        self.assertEqual(compatibility_bindings(self.sheet), before)
        root = ET.parse(self.sheet).getroot()
        self.assertEqual(root.find(f".//{{{SS}}}c[@r='A2']/{{{SS}}}v").text, "7")
        self.assertEqual(root.find(f".//{{{SS}}}c[@r='A3']/{{{SS}}}v").text, "10")
        self.assert_extensions()

    def test_add_column_preserves_all_rewritten_parts(self):
        parts = [self.sheet, self.work / "xl/styles.xml", self.work / "xl/sharedStrings.xml"]
        for path in parts[1:]:
            text = path.read_text(encoding="utf-8")
            root_tag = "styleSheet" if path.name == "styles.xml" else "sst"
            text = text.replace(
                f"<{root_tag}",
                f'<{root_tag} xmlns:mc="{MC}" xmlns:unused="urn:unused" mc:Ignorable="unused"',
                1,
            )
            path.write_text(text, encoding="utf-8")
        before = [compatibility_bindings(path) for path in parts]
        self.run_script(
            "xlsx_add_column.py", "--col", "B", "--header", "Total",
            "--formula", "A{row}*2", "--formula-rows", "2:2", "--numfmt", "0.00",
        )
        self.assertEqual([compatibility_bindings(path) for path in parts], before)
        root = ET.parse(self.sheet).getroot()
        self.assertEqual(root.find(f".//{{{SS}}}c[@r='B2']/{{{SS}}}f").text, "A2*2")
        self.assert_extensions()

    def test_shift_table_and_pivot_cache_keep_unused_prefixes(self):
        paths = []
        for folder, name, content in (
            ("tables", "table1.xml", '<table ref="A1:A2"/>'),
            ("pivotCaches", "pivotCacheDefinition1.xml", '<pivotCacheDefinition><cacheSource><worksheetSource ref="A1:A2"/></cacheSource></pivotCacheDefinition>'),
        ):
            path = self.work / "xl" / folder / name
            path.parent.mkdir()
            content = content.replace(
                ">", f' xmlns="{SS}" xmlns:mc="{MC}" xmlns:unused="urn:unused" mc:Ignorable="unused">', 1
            ) if folder == "pivotCaches" else (
                f'<table xmlns="{SS}" xmlns:mc="{MC}" xmlns:unused="urn:unused" mc:Ignorable="unused" ref="A1:A2"/>'
            )
            path.write_text(content, encoding="utf-8")
            paths.append(path)
        before = [compatibility_bindings(path) for path in paths]
        self.run_script("xlsx_shift_rows.py", "insert", "2", "1")
        self.assertEqual([compatibility_bindings(path) for path in paths], before)
        for path in paths:
            self.assertIn('ref="A1:A3"', path.read_text(encoding="utf-8"))

    def test_plain_workbook_still_shifts(self):
        self.sheet.write_text(
            f'<worksheet xmlns="{SS}"><sheetData><row r="2"><c r="A2"><v>10</v></c></row></sheetData></worksheet>',
            encoding="utf-8",
        )
        self.run_script("xlsx_shift_rows.py", "insert", "2", "1")
        root = ET.parse(self.sheet).getroot()
        self.assertEqual(root.find(f".//{{{SS}}}c[@r='A3']/{{{SS}}}v").text, "10")

    def test_repeated_edits_reuse_generated_prefixes(self):
        self.run_script("xlsx_shift_rows.py", "insert", "2", "1")
        before = DOM.parse(str(self.sheet)).documentElement.attributes.keys()
        bindings = compatibility_bindings(self.sheet)
        self.run_script("xlsx_shift_rows.py", "insert", "2", "1")
        after = DOM.parse(str(self.sheet)).documentElement.attributes.keys()
        self.assertEqual(set(after), set(before))
        self.assertEqual(compatibility_bindings(self.sheet), bindings)
        self.assert_extensions()


if __name__ == "__main__":
    unittest.main()
