#!/usr/bin/env python3
# SPDX-License-Identifier: MIT
"""Round-trip tests for shared_strings_builder.py text escaping.

XML parsers normalize a literal carriage return in text content to a line feed,
even under xml:space="preserve", so a CR written literally silently rewrites the
cell value. These tests parse the generated part with the standard library and
compare the text the parser reports.
"""

import sys
import unittest
import xml.etree.ElementTree as ET
from pathlib import Path

sys.dont_write_bytecode = True

sys.path.insert(
    0,
    str(Path(__file__).resolve().parents[1] / "others" / "xlsx" / "scripts"),
)

from shared_strings_builder import build_xml  # noqa: E402

NS_MAIN = "http://schemas.openxmlformats.org/spreadsheetml/2006/main"


def round_trip(value):
    root = ET.fromstring(build_xml([value]))
    element = root.find(f"{{{NS_MAIN}}}si/{{{NS_MAIN}}}t")
    return element.text, element.get("{http://www.w3.org/XML/1998/namespace}space")


class TestTextRoundTrip(unittest.TestCase):
    def test_carriage_return_survives(self):
        text, _ = round_trip("first\rlast")

        self.assertEqual(text, "first\rlast")

    def test_crlf_pair_survives(self):
        text, _ = round_trip("first\r\nlast")

        self.assertEqual(text, "first\r\nlast")

    def test_line_feed_is_unchanged(self):
        text, _ = round_trip("first\nlast")

        self.assertEqual(text, "first\nlast")

    def test_tab_is_unchanged(self):
        text, _ = round_trip("first\tlast")

        self.assertEqual(text, "first\tlast")

    def test_leading_and_trailing_space_keeps_preserve(self):
        text, space = round_trip("  padded  ")

        self.assertEqual(text, "  padded  ")
        self.assertEqual(space, "preserve")

    def test_xml_special_characters_still_escape(self):
        text, _ = round_trip("<a> & \"b\"")

        self.assertEqual(text, '<a> & "b"')

    def test_plain_text_gets_no_preserve_attribute(self):
        text, space = round_trip("plain")

        self.assertEqual(text, "plain")
        self.assertIsNone(space)


if __name__ == "__main__":
    unittest.main()
