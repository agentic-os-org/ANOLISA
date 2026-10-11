#!/usr/bin/env python3
# SPDX-License-Identifier: MIT
"""Tests for xlsx_unpack.py / xlsx_pack.py safety behavior.

Regression tests for three destructive or corrupting paths:
1. unpack deleted ANY pre-existing output directory (an editing session, or
   a typo like `.`), silently and recursively;
2. unpack rewrote XML members that failed to pretty-print, replacing every
   undecodable byte with U+FFFD, so the unpack/pack round trip corrupted
   the workbook even with no edits;
3. pack stored zip members with os.sep separators (broken on Windows) and
   packed its own output when the target was inside the source directory.
"""

import os
import subprocess
import sys
import tempfile
import unittest
import zipfile

SCRIPTS_DIR = os.path.dirname(os.path.abspath(__file__))
UNPACK = os.path.join(SCRIPTS_DIR, "xlsx_unpack.py")
PACK = os.path.join(SCRIPTS_DIR, "xlsx_pack.py")
MARKER = ".xlsx-unpacked"

NS_MAIN = "http://schemas.openxmlformats.org/spreadsheetml/2006/main"

CONTENT_TYPES_XML = (
    '<?xml version="1.0" encoding="UTF-8" standalone="yes"?>\n'
    '<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"/>'
)

WORKBOOK_XML = (
    '<?xml version="1.0" encoding="UTF-8" standalone="yes"?>\n'
    f'<workbook xmlns="{NS_MAIN}"/>'
)

WORKBOOK_RELS_XML = (
    '<?xml version="1.0" encoding="UTF-8" standalone="yes"?>\n'
    '<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"/>'
)

SHEET1_XML = (
    '<?xml version="1.0" encoding="UTF-8" standalone="yes"?>\n'
    f'<worksheet xmlns="{NS_MAIN}"/>'
)

# Not well-formed XML, despite the .rels extension: exactly the member class
# the old pretty-print fallback corrupted with U+FFFD replacement characters.
BINARY_RELS_BYTES = b"\xff\xfe not xml"


def build_xlsx(path, with_bad_rels=True):
    """Write a minimal 6-member xlsx-like zip fixture."""
    members = {
        "[Content_Types].xml": CONTENT_TYPES_XML,
        "xl/workbook.xml": WORKBOOK_XML,
        "xl/_rels/workbook.xml.rels": WORKBOOK_RELS_XML,
        "xl/styles.xml": SHEET1_XML,
        "xl/worksheets/sheet1.xml": SHEET1_XML,
    }
    with zipfile.ZipFile(path, "w", compression=zipfile.ZIP_DEFLATED) as z:
        for name, text in members.items():
            z.writestr(name, text)
        if with_bad_rels:
            z.writestr("xl/binary.rels", BINARY_RELS_BYTES)
    return path


def run(script, *args):
    return subprocess.run(
        [sys.executable, script, *args],
        capture_output=True,
        text=True,
        cwd=SCRIPTS_DIR,
    )


class UnpackSafety(unittest.TestCase):
    def setUp(self):
        self._tmp = tempfile.TemporaryDirectory()
        self.base = self._tmp.name
        self.xlsx = build_xlsx(os.path.join(self.base, "in.xlsx"))

    def tearDown(self):
        self._tmp.cleanup()

    def test_unpack_refuses_foreign_nonempty_dir(self):
        victim = os.path.join(self.base, "victim")
        os.makedirs(victim)
        with open(os.path.join(victim, "notes.txt"), "w") as f:
            f.write("important editing session")
        result = run(UNPACK, self.xlsx, victim)
        self.assertEqual(result.returncode, 1, result.stderr)
        self.assertIn("Refusing to delete", result.stderr)
        self.assertTrue(os.path.exists(os.path.join(victim, "notes.txt")))

    def test_unpack_dot_does_not_delete_cwd(self):
        # `.` as output_dir must never wipe the caller's working directory,
        # whatever is in it. The run happens with cwd=scratch so the refusal
        # (not the deletion) is what the sentinels observe.
        scratch = os.path.join(self.base, "scratch")
        os.makedirs(scratch)
        with open(os.path.join(scratch, "sentinel.txt"), "w") as f:
            f.write("do not delete me")
        result = subprocess.run(
            [sys.executable, os.path.abspath(UNPACK), self.xlsx, "."],
            capture_output=True,
            text=True,
            cwd=scratch,
        )
        self.assertEqual(result.returncode, 1, result.stderr)
        self.assertIn("Refusing to delete", result.stderr)
        self.assertTrue(os.path.exists(os.path.join(scratch, "sentinel.txt")))
        self.assertFalse(os.path.exists(os.path.join(scratch, MARKER)))

    def test_unpack_force_overwrites(self):
        victim = os.path.join(self.base, "victim")
        os.makedirs(victim)
        stale = os.path.join(victim, "stale.txt")
        with open(stale, "w") as f:
            f.write("old session")
        result = run(UNPACK, self.xlsx, victim, "--force")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertFalse(os.path.exists(stale))
        self.assertTrue(os.path.exists(os.path.join(victim, MARKER)))

    def test_reunpack_after_unpack_allowed(self):
        work = os.path.join(self.base, "work")
        first = run(UNPACK, self.xlsx, work)
        self.assertEqual(first.returncode, 0, first.stderr)
        # An edit made between unpacks must be replaceable by the documented
        # re-unpack path (marker recognized).
        edited = os.path.join(work, "xl", "workbook.xml")
        with open(edited, "w") as f:
            f.write("agent edit")
        second = run(UNPACK, self.xlsx, work)
        self.assertEqual(second.returncode, 0, second.stderr)
        with open(edited) as f:
            self.assertIn("<workbook", f.read())

    def test_unpack_leaves_malformed_member_bytes(self):
        work = os.path.join(self.base, "work")
        result = run(UNPACK, self.xlsx, work)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("left untouched", result.stderr)
        with open(os.path.join(work, "xl", "binary.rels"), "rb") as f:
            on_disk = f.read()
        self.assertEqual(on_disk, BINARY_RELS_BYTES,
                         "malformed member must round-trip byte-identically")

    def test_unpack_refuses_non_directory_output(self):
        blocker = os.path.join(self.base, "blocker")
        with open(blocker, "w") as f:
            f.write("a file")
        result = run(UNPACK, self.xlsx, blocker)
        self.assertEqual(result.returncode, 1, result.stderr)
        with open(blocker) as f:
            self.assertEqual(f.read(), "a file")


class PackSafety(unittest.TestCase):
    def setUp(self):
        self._tmp = tempfile.TemporaryDirectory()
        self.base = self._tmp.name
        self.xlsx = build_xlsx(os.path.join(self.base, "in.xlsx"),
                               with_bad_rels=False)
        work = os.path.join(self.base, "work")
        result = run(UNPACK, self.xlsx, work)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.work = work

    def tearDown(self):
        self._tmp.cleanup()

    def test_pack_roundtrip_forward_slashes(self):
        out = os.path.join(self.base, "out.xlsx")
        result = run(PACK, self.work, out)
        self.assertEqual(result.returncode, 0, result.stderr)
        with zipfile.ZipFile(out) as z:
            names = z.namelist()
        self.assertIn("xl/worksheets/sheet1.xml", names)
        self.assertFalse(any("\\" in name for name in names))

    def test_pack_rejects_output_inside_source(self):
        out = os.path.join(self.work, "out.xlsx")
        result = run(PACK, self.work, out)
        self.assertEqual(result.returncode, 1, result.stderr)
        self.assertIn("outside the source directory", result.stderr)
        self.assertFalse(os.path.exists(out))

    def test_pack_rejects_missing_content_types(self):
        empty = os.path.join(self.base, "empty")
        os.makedirs(empty)
        out = os.path.join(self.base, "empty.xlsx")
        result = run(PACK, empty, out)
        self.assertEqual(result.returncode, 1, result.stderr)
        self.assertIn("[Content_Types].xml", result.stderr)

    def test_packed_members_resolve_for_consumers(self):
        """The packed names must match what sibling scripts look up."""
        out = os.path.join(self.base, "out.xlsx")
        result = run(PACK, self.work, out)
        self.assertEqual(result.returncode, 0, result.stderr)
        with zipfile.ZipFile(out) as z:
            names = set(z.namelist())
        # xlsx_reader.py and friends build "xl" + "/" + ... names.
        self.assertIn("xl/workbook.xml", names)
        self.assertIn("[Content_Types].xml", names)


if __name__ == "__main__":
    unittest.main()
