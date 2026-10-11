#!/usr/bin/env python3
# SPDX-License-Identifier: MIT
"""Serialize workbook parts without discarding whitespace in text values."""

import xml.etree.ElementTree as ET


def write_tree(tree: ET.ElementTree, path: str) -> None:
    """Indent structural XML while preserving blank lines and carriage returns in text."""
    ET.indent(tree, space="  ")
    # XML normalizes raw carriage returns even inside xml:space-preserved text.
    content = ET.tostring(tree.getroot(), encoding="unicode").replace("\r", "&#13;")
    with open(path, "w", encoding="utf-8", newline="\n") as output:
        output.write('<?xml version="1.0" encoding="utf-8"?>\n')
        output.write(content)
        output.write("\n")
