#!/usr/bin/env python3
# SPDX-License-Identifier: MIT
"""Namespace-binding regression tests for the XLSX editors.

`mc:Ignorable` and `mc:Choice/@Requires` name prefixes inside attribute values.
ElementTree only declares namespaces bound to element or attribute names, so a
compatibility-only prefix lost its declaration while the value kept using it:
the edit exited 0 and produced a part that no longer defines the prefix it
references.
"""

import importlib.util
import os
import subprocess
import sys
import tempfile
import unittest
import xml.etree.ElementTree as ET
from pathlib import Path

sys.dont_write_bytecode = True

NS_SPREADSHEET = "http://schemas.openxmlformats.org/spreadsheetml/2006/main"
NS_MC = "http://schemas.openxmlformats.org/markup-compatibility/2006"
NS_AC = "http://schemas.microsoft.com/office/spreadsheetml/2009/9/ac"
NS_X16 = "http://schemas.microsoft.com/office/spreadsheetml/2014/11/main"

SCRIPTS_DIR = os.path.join(
    str(Path(__file__).resolve().parents[1]), "others", "xlsx", "scripts"
)
SHIFT_ROWS = os.path.join(SCRIPTS_DIR, "xlsx_shift_rows.py")

sys.path.insert(0, SCRIPTS_DIR)

# The prefix appears only in mc:Ignorable; no element or attribute is named
# with it, which is exactly the shape ElementTree cannot round-trip.
# `x14ac:dyDescent` is deliberately absent: an attribute *named* with the prefix
# would make ElementTree declare it on its own, and `register()` would then cover
# for `restore()` -- letting either half be deleted with the tests still green.
COMPATIBILITY_PART = f"""<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<worksheet xmlns="{NS_SPREADSHEET}" xmlns:mc="{NS_MC}" xmlns:x14ac="{NS_AC}"
           mc:Ignorable="x14ac">
  <sheetFormatPr defaultRowHeight="15"/>
  <sheetData><row r="2"><c r="A2"><v>10</v></c></row></sheetData>
</worksheet>
"""

PLAIN_PART = f"""<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<worksheet xmlns="{NS_SPREADSHEET}">
  <sheetData><row r="2"><c r="A2"><v>10</v></c></row></sheetData>
</worksheet>
"""


# Unlike COMPATIBILITY_PART this one *names* an element attribute with the
# prefix, which is the only way to constrain `register()`: without it
# ElementTree emits `ns1:dyDescent` and the declared prefix is a generated one.
NAMED_PREFIX_PART = f"""<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<worksheet xmlns="{NS_SPREADSHEET}" xmlns:mc="{NS_MC}" xmlns:x14ac="{NS_AC}"
           mc:Ignorable="x14ac">
  <sheetFormatPr defaultRowHeight="15" x14ac:dyDescent="0.25"/>
  <sheetData><row r="2"><c r="A2"><v>10</v></c></row></sheetData>
</worksheet>
"""


# `xl/tables/*.xml` parts have an empty root, so this is the only shape whose
# serialized root closes with `/>`. Both named prefixes appear only in
# `mc:Ignorable`: declaring them is what makes the value mean anything, and
# naming no element with them is what makes ElementTree drop them.
SELF_CLOSING_TABLE_PART = f"""<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<table xmlns="{NS_SPREADSHEET}" xmlns:mc="{NS_MC}" xmlns:x14ac="{NS_AC}"
       xmlns:x16="{NS_X16}" mc:Ignorable="x14ac x16"
       id="1" name="T" displayName="T" ref="A1:D20"/>
"""


def declared_prefixes(path):
    """Maps every prefix the part declares to its URI."""
    return dict(value for _event, value in ET.iterparse(path, events=("start-ns",)))


def load_script(name):
    """Loads a sibling editor script as a module, without invoking its CLI entry point.

    The module body does run: `exec_module` is the point. That is why the editors'
    import-time `ET.register_namespace` calls leak into every later test in this
    process, and why the assertions below never depend on the global registry.
    """
    spec = importlib.util.spec_from_file_location(
        name.replace("-", "_"), os.path.join(SCRIPTS_DIR, f"{name}.py")
    )
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


class ShiftRowsCliTest(unittest.TestCase):
    def test_shift_rows_keeps_a_compatibility_only_prefix(self):
        # The reproduction from the report, end to end: the command used to exit
        # 0 while dropping the declaration `mc:Ignorable` still named.
        with tempfile.TemporaryDirectory() as directory:
            sheet = os.path.join(directory, "xl", "worksheets", "sheet1.xml")
            os.makedirs(os.path.dirname(sheet))
            with open(sheet, "w", encoding="utf-8") as handle:
                handle.write(COMPATIBILITY_PART)
            result = subprocess.run(
                [sys.executable, "-B", SHIFT_ROWS, directory, "insert", "2", "1"],
                capture_output=True,
                text=True,
                check=False,
                env={**os.environ, "PYTHONDONTWRITEBYTECODE": "1"},
            )
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
            self.assertEqual(declared_prefixes(sheet).get("x14ac"), NS_AC)
            root = ET.parse(sheet).getroot()
            self.assertEqual(root.get(f"{{{NS_MC}}}Ignorable"), "x14ac")

    def test_a_self_closing_table_root_keeps_its_declarations(self):
        # The shape the regression actually had: `xl/tables/*.xml` roots have no
        # children, so the serialized root closes with `/>` and the declarations
        # have to land before the solidus. `RestoreTest` pins `restore()` on text
        # it builds itself; only a real CLI run pins the `tree.write` ->
        # pretty-print -> `restore()` chain, whose root spelling differs by
        # branch (`/>` from minidom, ` />` when pretty-print falls back).
        with tempfile.TemporaryDirectory() as directory:
            table = os.path.join(directory, "xl", "tables", "table1.xml")
            os.makedirs(os.path.dirname(table))
            with open(table, "w", encoding="utf-8") as handle:
                handle.write(SELF_CLOSING_TABLE_PART)
            result = subprocess.run(
                [sys.executable, "-B", SHIFT_ROWS, directory, "insert", "2", "1"],
                capture_output=True,
                text=True,
                check=False,
                env={**os.environ, "PYTHONDONTWRITEBYTECODE": "1"},
            )
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
            with open(table, encoding="utf-8") as handle:
                written = handle.read()
            self.assertTrue(written.rstrip().endswith("/>"), written)
            # Re-parsing is the assertion that matters: a solidus swallowed by the
            # insertion point leaves the part unparseable, not merely undeclared.
            self.assertEqual(ET.parse(table).getroot().get("ref"), "A1:D21")
            prefixes = declared_prefixes(table)
            self.assertEqual(prefixes.get("x14ac"), NS_AC, written)
            self.assertEqual(prefixes.get("x16"), NS_X16, written)

    def test_a_plain_workbook_gains_no_declarations(self):
        # The control: restoring bindings must not invent a compatibility
        # prefix for a workbook that never declared one.
        with tempfile.TemporaryDirectory() as directory:
            sheet = os.path.join(directory, "xl", "worksheets", "sheet1.xml")
            os.makedirs(os.path.dirname(sheet))
            with open(sheet, "w", encoding="utf-8") as handle:
                handle.write(PLAIN_PART)
            result = subprocess.run(
                [sys.executable, "-B", SHIFT_ROWS, directory, "insert", "2", "1"],
                capture_output=True,
                text=True,
                check=False,
                env={**os.environ, "PYTHONDONTWRITEBYTECODE": "1"},
            )
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
            uris = set(declared_prefixes(sheet).values())
            self.assertNotIn(NS_MC, uris)
            self.assertNotIn(NS_AC, uris)
            self.assertIn(NS_SPREADSHEET, uris)


class RestoreTest(unittest.TestCase):
    """`restore` inserts into the root start tag, so it must find the right spot.

    The shapes a real part reaches are covered: a prolog, a comment before the
    root, a declaration quoting with `'`, and a self-closing root. The two
    malformed-input exits (`close < 0` for an unterminated `<?`/`<!`, and the
    loop running out on an unterminated root tag) are deliberately untested --
    they are not reachable from a part that parses. The last one is the dangerous case -- inserting before
    the `>` of `<x/>` leaves the solidus inside the tag and the part stops
    parsing, which is worse than the undeclared prefix being fixed.
    """

    def setUp(self):
        self.module = load_script("xlsx_namespaces")

    def assert_restores(self, text, expected):
        restored = self.module.restore(text, {"mc": NS_MC})
        self.assertEqual(restored, expected)
        # Whatever the shape, the result has to stay parseable.
        ET.fromstring(restored)

    def test_root_tag_after_a_prolog(self):
        self.assert_restores(
            '<?xml version="1.0"?><worksheet mc:Ignorable="x"><v/></worksheet>',
            '<?xml version="1.0"?><worksheet mc:Ignorable="x" xmlns:mc="%s">'
            "<v/></worksheet>" % NS_MC,
        )

    def test_root_tag_after_a_comment(self):
        self.assert_restores(
            '<?xml version="1.0"?><!-- note --><worksheet mc:Ignorable="x"/>',
            '<?xml version="1.0"?><!-- note --><worksheet mc:Ignorable="x" '
            'xmlns:mc="%s"/>' % NS_MC,
        )

    def test_a_single_quoted_attribute_does_not_end_the_tag(self):
        self.assert_restores(
            "<worksheet a='>' mc:Ignorable=\"x\"><v/></worksheet>",
            "<worksheet a='>' mc:Ignorable=\"x\" xmlns:mc=\"%s\">"
            "<v/></worksheet>" % NS_MC,
        )

    def test_a_self_closing_root_stays_well_formed(self):
        # The regression: the declaration used to land after the solidus.
        self.assert_restores(
            '<worksheet a="1" mc:Ignorable="x"/>',
            '<worksheet a="1" mc:Ignorable="x" xmlns:mc="%s"/>' % NS_MC,
        )

    def test_a_prefix_the_text_already_declares_is_left_alone(self):
        text = '<worksheet xmlns:mc="%s" mc:Ignorable="x"/>' % NS_MC
        self.assertEqual(self.module.restore(text, {"mc": NS_MC}), text)

    def test_a_document_without_a_root_is_returned_unchanged(self):
        text = "<?xml version=\"1.0\"?>"
        self.assertEqual(self.module.restore(text, {"mc": NS_MC}), text)


class RegisterTest(unittest.TestCase):
    """`register()` keeps a prefix the file chose instead of an invented `ns1`."""

    def test_shift_rows_preserves_a_named_prefix(self):
        with tempfile.TemporaryDirectory() as directory:
            sheet = os.path.join(directory, "xl", "worksheets", "sheet1.xml")
            os.makedirs(os.path.dirname(sheet))
            with open(sheet, "w", encoding="utf-8") as handle:
                handle.write(NAMED_PREFIX_PART)
            result = subprocess.run(
                [sys.executable, "-B", SHIFT_ROWS, directory, "insert", "2", "1"],
                capture_output=True,
                text=True,
                check=False,
                env={**os.environ, "PYTHONDONTWRITEBYTECODE": "1"},
            )
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
            with open(sheet, encoding="utf-8") as handle:
                written = handle.read()
        self.assertIn("x14ac:dyDescent", written)
        self.assertNotIn("ns0:dyDescent", written)

    def test_the_other_editors_preserve_a_named_prefix(self):
        for script_name in ("xlsx_insert_row", "xlsx_add_column"):
            with self.subTest(script=script_name):
                module = load_script(script_name)
                with tempfile.TemporaryDirectory() as directory:
                    sheet = os.path.join(directory, "sheet1.xml")
                    with open(sheet, "w", encoding="utf-8") as handle:
                        handle.write(NAMED_PREFIX_PART)
                    module._write_tree(ET.parse(sheet), sheet)
                    with open(sheet, encoding="utf-8") as handle:
                        written = handle.read()
                self.assertIn("x14ac:dyDescent", written)
                self.assertNotIn("ns0:dyDescent", written)


class WriterTest(unittest.TestCase):
    """The other two editors share the same write path; exercise it directly."""

    def assert_prefix_survives(self, script_name):
        module = load_script(script_name)
        with tempfile.TemporaryDirectory() as directory:
            sheet = os.path.join(directory, "sheet1.xml")
            with open(sheet, "w", encoding="utf-8") as handle:
                handle.write(COMPATIBILITY_PART)
            module._write_tree(ET.parse(sheet), sheet)
            self.assertEqual(declared_prefixes(sheet).get("x14ac"), NS_AC)

    def test_insert_row_keeps_a_compatibility_only_prefix(self):
        self.assert_prefix_survives("xlsx_insert_row")

    def test_add_column_keeps_a_compatibility_only_prefix(self):
        self.assert_prefix_survives("xlsx_add_column")


class CaptureTest(unittest.TestCase):
    """`capture()` keeps the outermost binding for each prefix.

    `restore()`'s argument that it only widens scope depends on this
    direction, so a capture that took the innermost redeclaration would make
    the restore cases pass for the wrong reason.
    """

    def test_the_outermost_binding_wins(self):
        # `restore()` is only correct if a prefix keeps the meaning it had in the
        # widest scope. Capturing the innermost redeclaration would silently rebind
        # every part to a narrower meaning, and nothing else in this file pins the
        # direction: flipping `setdefault` to plain assignment leaves the other
        # cases green.
        xlsx_namespaces = load_script("xlsx_namespaces")

        part = (
            f'<worksheet xmlns="{NS_SPREADSHEET}" xmlns:x14ac="{NS_AC}">'
            f'<sheetFormatPr xmlns:x14ac="urn:other"/>'
            f"</worksheet>"
        )

        self.assertEqual(xlsx_namespaces.capture(part)["x14ac"], NS_AC)


if __name__ == "__main__":
    unittest.main()
