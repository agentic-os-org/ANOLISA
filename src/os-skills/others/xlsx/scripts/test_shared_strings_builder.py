#!/usr/bin/env python3
# SPDX-License-Identifier: MIT
"""Round-trip tests for shared-string XML text values."""

import unittest
import xml.etree.ElementTree as ET

import shared_strings_builder as builder


class SharedStringTextTests(unittest.TestCase):
    def test_carriage_returns_survive_xml_parsing(self):
        values = ["first\rlast", "first\r\nlast", "\r", "a\rb\rc"]
        root = ET.fromstring(builder.build_xml(values))
        actual = [node.text for node in root.iter(f"{{{builder.SST_NS}}}t")]
        self.assertEqual(actual, values)

    def test_xml_escaping_and_other_whitespace_are_unchanged(self):
        values = ['a & <b> "quoted"', " first\tlast ", "first\n\nlast", "plain"]
        root = ET.fromstring(builder.build_xml(values))
        nodes = list(root.iter(f"{{{builder.SST_NS}}}t"))
        self.assertEqual([node.text for node in nodes], values)
        self.assertEqual(nodes[1].get("{http://www.w3.org/XML/1998/namespace}space"), "preserve")
        self.assertEqual(root.get("count"), str(len(values)))
        self.assertEqual(root.get("uniqueCount"), str(len(values)))

    def test_literal_character_reference_is_not_interpreted(self):
        value = "literal &#13; and actual\rreturn"
        root = ET.fromstring(builder.build_xml([value]))
        self.assertEqual(next(root.iter(f"{{{builder.SST_NS}}}t")).text, value)


if __name__ == "__main__":
    unittest.main()
